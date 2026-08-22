//! Personal music taste: the listener's own library, embedded and searchable.
//!
//! A blind Spotify search answers "play Thunderstruck" well and "play something
//! calm" badly, because the second question is about *this* listener. So the
//! library — saved tracks, top tracks, recent plays, and every playlist — is
//! embedded once and searched semantically, and requests that are about taste
//! resolve against music the listener already chose.
//!
//! There is no vector database here on purpose. A personal library is a few
//! thousand vectors; a linear cosine scan over that is well under a millisecond
//! and costs no infrastructure. If this ever indexed many listeners, that trade
//! would flip.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{error::AppError, spotify::LibraryTrack};

const EMBEDDING_URL: &str = "https://api.openai.com/v1/embeddings";
const EMBEDDING_MODEL: &str = "text-embedding-3-small";
/// Shortened from the model's native 1536. Retrieval quality holds up well and
/// it cuts the stored index and the scan by two thirds.
const EMBEDDING_DIMENSIONS: usize = 512;
/// The embeddings endpoint accepts batches; this keeps requests a sane size.
const EMBEDDING_BATCH: usize = 96;
/// Ceiling on how much library to index, so a listener with a huge account does
/// not turn startup into a long job.
pub const MAX_INDEXED_TRACKS: usize = 3_000;
/// Cosine floor for calling a library track a match.
///
/// Without this, the nearest neighbour was returned however far away it was, so
/// naming a song the listener does not own played whatever in their library
/// happened to sit closest in embedding space — "I like the way you kiss me"
/// became a different pop song entirely. Below the floor the caller gets nothing
/// and falls back to a real Spotify search, which is the right answer for a
/// track they do not have.
const MIN_MATCH_SCORE: f32 = 0.42;

/// One indexed track and its embedding.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexedTrack {
    track: LibraryTrack,
    embedding: Vec<f32>,
}

/// The on-disk form, so a restart does not re-embed the whole library.
#[derive(Debug, Serialize, Deserialize)]
struct StoredIndex {
    model: String,
    dimensions: usize,
    tracks: Vec<IndexedTrack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TasteStats {
    pub tracks: usize,
}

/// A searchable view of one listener's music.
pub struct TasteIndex {
    api_key: String,
    http: reqwest::Client,
    store_path: PathBuf,
    tracks: RwLock<Vec<IndexedTrack>>,
}

impl TasteIndex {
    pub fn new(api_key: impl Into<String>, store_path: PathBuf) -> Self {
        Self {
            api_key: api_key.into(),
            http: crate::spotify::http_client(),
            store_path,
            tracks: RwLock::new(Vec::new()),
        }
    }

    pub async fn stats(&self) -> TasteStats {
        TasteStats {
            tracks: self.tracks.read().await.len(),
        }
    }

    pub async fn is_empty(&self) -> bool {
        self.tracks.read().await.is_empty()
    }

    /// Load a previously built index, if one exists and still matches the model.
    pub async fn load(&self) -> Result<bool, AppError> {
        let bytes = match tokio::fs::read(&self.store_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "failed to read taste index: {error}"
                )));
            }
        };
        let stored: StoredIndex = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            // A corrupt or older index is not worth failing over; it just gets
            // rebuilt.
            Err(error) => {
                tracing::warn!(%error, "taste index is unreadable; it will be rebuilt");
                return Ok(false);
            }
        };
        if stored.model != EMBEDDING_MODEL || stored.dimensions != EMBEDDING_DIMENSIONS {
            tracing::info!("taste index was built with a different embedding; rebuilding");
            return Ok(false);
        }
        let count = stored.tracks.len();
        *self.tracks.write().await = stored.tracks;
        tracing::info!(tracks = count, "loaded taste index");
        Ok(count > 0)
    }

    /// Embed a library and replace the index, then persist it.
    pub async fn rebuild(&self, library: Vec<LibraryTrack>) -> Result<TasteStats, AppError> {
        let mut indexed = Vec::with_capacity(library.len());
        for batch in library.chunks(EMBEDDING_BATCH) {
            let inputs: Vec<String> = batch.iter().map(LibraryTrack::embedding_text).collect();
            let embeddings = self.embed(&inputs).await?;
            if embeddings.len() != batch.len() {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "embedding count {} does not match batch size {}",
                    embeddings.len(),
                    batch.len()
                )));
            }
            for (track, embedding) in batch.iter().zip(embeddings) {
                indexed.push(IndexedTrack {
                    track: track.clone(),
                    embedding: normalize(embedding),
                });
            }
        }

        let count = indexed.len();
        *self.tracks.write().await = indexed;
        self.persist().await?;
        tracing::info!(tracks = count, "rebuilt taste index");
        Ok(TasteStats { tracks: count })
    }

    /// Best matches for a natural-language description of what to play.
    pub async fn search(
        &self,
        description: &str,
        limit: usize,
    ) -> Result<Vec<LibraryTrack>, AppError> {
        let tracks = self.tracks.read().await;
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let query = self
            .embed(std::slice::from_ref(&description.to_string()))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("embedding response carried no vector"))
            })?;
        let query = normalize(query);

        let mut scored: Vec<(f32, &IndexedTrack)> = tracks
            .iter()
            // Both sides are unit length, so the dot product is the cosine.
            .map(|entry| (dot(&query, &entry.embedding), entry))
            .collect();
        scored.sort_by(|left, right| right.0.total_cmp(&left.0));
        if let Some((best, entry)) = scored.first() {
            tracing::debug!(
                score = best,
                track = %entry.track.name,
                accepted = *best >= MIN_MATCH_SCORE,
                "closest library match"
            );
        }
        Ok(scored
            .into_iter()
            .take(limit)
            .filter(|(score, _)| *score >= MIN_MATCH_SCORE)
            .map(|(_, entry)| entry.track.clone())
            .collect())
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AppError> {
        #[derive(Deserialize)]
        struct EmbeddingResponse {
            data: Vec<EmbeddingDatum>,
        }
        #[derive(Deserialize)]
        struct EmbeddingDatum {
            index: usize,
            embedding: Vec<f32>,
        }

        let response = self
            .http
            .post(EMBEDDING_URL)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": EMBEDDING_MODEL,
                "dimensions": EMBEDDING_DIMENSIONS,
                "input": inputs,
            }))
            .send()
            .await
            .map_err(|error| AppError::Voice(format!("embedding request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(AppError::Voice(format!(
                "embedding request failed with {}",
                response.status()
            )));
        }
        let mut data = response
            .json::<EmbeddingResponse>()
            .await
            .map_err(|error| AppError::Voice(format!("embedding response unreadable: {error}")))?
            .data;
        // The API documents order but returns an index; honour the index.
        data.sort_by_key(|datum| datum.index);
        Ok(data.into_iter().map(|datum| datum.embedding).collect())
    }

    async fn persist(&self) -> Result<(), AppError> {
        let stored = StoredIndex {
            model: EMBEDDING_MODEL.to_string(),
            dimensions: EMBEDDING_DIMENSIONS,
            tracks: self.tracks.read().await.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("failed to serialize taste index: {error}"))
        })?;
        write_atomically(&self.store_path, &bytes).await
    }
}

/// Same temp-file-and-rename discipline as the token store: a half-written
/// index would otherwise have to be rebuilt from scratch.
async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to create taste index directory: {error}"
            ))
        })?;
    }
    let mut temporary = path.file_name().unwrap_or_default().to_os_string();
    temporary.push(".tmp");
    let temporary = path.with_file_name(temporary);
    tokio::fs::write(&temporary, bytes).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!("failed to write taste index: {error}"))
    })?;
    tokio::fs::rename(&temporary, path).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!("failed to commit taste index: {error}"))
    })
}

fn normalize(mut vector: Vec<f32>) -> Vec<f32> {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for value in &mut vector {
            *value /= magnitude;
        }
    }
    vector
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, name: &str, artists: &str, sources: &[&str]) -> LibraryTrack {
        LibraryTrack {
            id: id.to_string(),
            name: name.to_string(),
            artists: artists.to_string(),
            album: "Album".to_string(),
            sources: sources.iter().map(|source| source.to_string()).collect(),
        }
    }

    #[test]
    fn embedding_text_leads_with_the_song_and_artist() {
        let text = track("1", "Tadap Tadap", "KK", &["Late night", "saved"]).embedding_text();
        assert!(text.starts_with("Tadap Tadap by KK"), "{text}");
        // Playlist names are strong taste signal and must survive into the text.
        assert!(text.contains("Late night"), "{text}");
        assert!(text.contains("saved"), "{text}");
    }

    #[test]
    fn normalized_vectors_score_by_cosine() {
        let a = normalize(vec![3.0, 4.0]);
        assert!((dot(&a, &a) - 1.0).abs() < 1e-6);
        let b = normalize(vec![-4.0, 3.0]);
        // Orthogonal inputs must score zero, or ranking is meaningless.
        assert!(dot(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn normalizing_a_zero_vector_does_not_produce_nan() {
        // An all-zero embedding would otherwise divide by zero and poison every
        // comparison with NaN, which sorts unpredictably.
        let zero = normalize(vec![0.0, 0.0, 0.0]);
        assert!(zero.iter().all(|value| value.is_finite()));
        assert_eq!(dot(&zero, &zero), 0.0);
    }

    #[tokio::test]
    async fn search_returns_nothing_before_the_index_is_built() {
        let index = TasteIndex::new("key", PathBuf::from("unused"));
        assert!(index.is_empty().await);
        // Must not call the embeddings API just to answer "I know nothing yet".
        assert!(index.search("something calm", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_index_file_is_not_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let index = TasteIndex::new("key", directory.path().join("taste.json"));
        assert!(!index.load().await.unwrap());
    }

    #[tokio::test]
    async fn a_corrupt_index_rebuilds_instead_of_failing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("taste.json");
        tokio::fs::write(&path, b"not json at all").await.unwrap();
        let index = TasteIndex::new("key", path);
        assert!(!index.load().await.unwrap(), "corrupt index must not error");
    }
}
