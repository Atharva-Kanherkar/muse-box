//! Time-synced lyrics from LRCLIB.
//!
//! Spotify does not expose lyrics, so they come from LRCLIB: free, no key, and
//! crucially human-timed. That last part matters — a language model cannot time
//! lyrics. It has no audio, so anything it produced would be a plausible guess
//! that drifts visibly. Real alignment needs the audio file, which never reaches
//! this backend; Spotify streams to the playback device.
//!
//! Clients need no clock negotiation: each line carries an offset into the
//! track, and the same `progress_ms + (now - server_ts)` that drives the
//! progress bar picks the current line.
//!
//! Shared across every account rather than kept per-account: a track's lyrics
//! do not depend on who is listening, so the first person to play a song
//! looks it up for everyone after them. One file per track
//! (`<store_dir>/<track_id>.json`) rather than one growing blob, so a cache
//! miss writes a few hundred bytes instead of rewriting every track this
//! process has ever looked up — the whole-file rewrite this replaced was fine
//! at "one person's library" and a real write-amplification problem once many
//! concurrent strangers are missing on many distinct tracks.

use std::path::{Path, PathBuf};
use std::{collections::HashMap, ffi::OsString};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::AppError;

const LRCLIB_GET_URL: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";
/// A release within this much of the track's length is treated as the same
/// recording. Different masters and edits drift by a few seconds.
const DURATION_TOLERANCE_MS: u64 = 8_000;
/// LRCLIB asks callers to identify themselves rather than pretend to be a
/// browser.
const USER_AGENT: &str = concat!(
    "muse-box/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/Atharva-Kanherkar/muse-box)"
);
/// Tracks to keep hot in memory. The disk store behind it is unbounded — this
/// only bounds how many avoid a disk read for whatever is currently playing.
const MAX_CACHED_TRACKS: usize = 256;

/// One lyric line and where it falls in the track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

/// Lyrics for one track. `synced` is false when only a plain text version
/// exists, so a client can show it without pretending to follow along.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Deserialize)]
struct LrclibResponse {
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    /// Seconds, as a float. Only present on search results.
    #[serde(default)]
    duration: Option<f64>,
}

/// Looks up lyrics and remembers what it found, including the misses.
pub struct LyricsIndex {
    http: reqwest::Client,
    get_url: String,
    search_url: String,
    /// Hot cache only. `None` marks a track already known to have no lyrics,
    /// so a track on repeat does not re-ask on every play. The durable copy
    /// of both is on disk, one file per track, under `store_dir`.
    cache: RwLock<HashMap<String, Option<Lyrics>>>,
    store_dir: PathBuf,
}

impl LyricsIndex {
    pub fn new(store_dir: PathBuf) -> Self {
        Self {
            http: crate::spotify::http_client(),
            get_url: LRCLIB_GET_URL.to_string(),
            search_url: LRCLIB_SEARCH_URL.to_string(),
            cache: RwLock::new(HashMap::new()),
            store_dir,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_urls(store_dir: PathBuf, get_url: String, search_url: String) -> Self {
        Self {
            http: crate::spotify::http_client(),
            get_url,
            search_url,
            cache: RwLock::new(HashMap::new()),
            store_dir,
        }
    }

    /// Count tracks already cached on disk, for the boot log. Nothing is read
    /// into memory: the disk store is the durable copy, and the in-memory map
    /// is only ever a hot cache for whatever is currently playing, so there is
    /// nothing to gain by loading every track this process has ever seen.
    pub async fn load(&self) -> usize {
        let Ok(mut entries) = tokio::fs::read_dir(&self.store_dir).await else {
            return 0;
        };
        let mut count = 0;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                count += 1;
            }
        }
        count
    }

    /// Lyrics for a track, from the hot cache, then disk, then LRCLIB. A
    /// confirmed miss is cached like a hit; a failed lookup is not, so the
    /// next play tries again rather than remembering a blip forever.
    pub async fn for_track(
        &self,
        track_id: &str,
        track: &str,
        artist: &str,
        album: &str,
        duration_ms: u64,
    ) -> Option<Lyrics> {
        if let Some(cached) = self.cache.read().await.get(track_id) {
            return cached.clone();
        }
        if let Some(found) = self.read_from_disk(track_id).await {
            self.remember(track_id, found.clone()).await;
            return found;
        }

        let found = match self.fetch(track, artist, album, duration_ms).await {
            Ok(found) => found,
            Err(error) => {
                // A lookup failure must never disturb playback, so it is not
                // cached either: the next play tries again.
                tracing::warn!(%error, track, "lyrics lookup failed");
                return None;
            }
        };
        self.remember(track_id, found.clone()).await;
        self.persist(track_id, &found).await;
        found
    }

    async fn read_from_disk(&self, track_id: &str) -> Option<Option<Lyrics>> {
        let bytes = tokio::fs::read(self.track_path(track_id)).await.ok()?;
        // A corrupt single-track file is cheap to lose: it is treated as a
        // cache miss and looked up again, rather than logged as a failure.
        serde_json::from_slice(&bytes).ok()
    }

    async fn remember(&self, track_id: &str, found: Option<Lyrics>) {
        let mut cache = self.cache.write().await;
        if !cache.contains_key(track_id) && cache.len() >= MAX_CACHED_TRACKS {
            cache.clear();
        }
        cache.insert(track_id.to_string(), found);
    }

    /// Best effort: losing this costs a disk read's worth of a re-lookup next
    /// time this track plays, not correctness.
    async fn persist(&self, track_id: &str, found: &Option<Lyrics>) {
        let Ok(bytes) = serde_json::to_vec(found) else {
            return;
        };
        if let Err(error) = write_track_file(&self.track_path(track_id), &bytes).await {
            tracing::warn!(%error, track_id, "failed to cache lyrics to disk");
        }
    }

    fn track_path(&self, track_id: &str) -> PathBuf {
        self.store_dir.join(format!("{track_id}.json"))
    }

    /// Exact lookup first, then fuzzy search.
    ///
    /// `/api/get` needs the track, artist, album and duration to line up
    /// exactly, which they often do not — Spotify's album name for a film
    /// soundtrack rarely matches. `/api/search` is forgiving, so the fallback is
    /// what actually finds most tracks.
    async fn fetch(
        &self,
        track: &str,
        artist: &str,
        album: &str,
        duration_ms: u64,
    ) -> Result<Option<Lyrics>, AppError> {
        if let Some(found) = self.fetch_exact(track, artist, album, duration_ms).await? {
            return Ok(Some(found));
        }
        self.fetch_by_search(track, artist, duration_ms).await
    }

    async fn fetch_exact(
        &self,
        track: &str,
        artist: &str,
        album: &str,
        duration_ms: u64,
    ) -> Result<Option<Lyrics>, AppError> {
        let response = self
            .http
            .get(&self.get_url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .query(&[
                ("track_name", track),
                ("artist_name", artist),
                ("album_name", album),
                ("duration", &(duration_ms / 1000).to_string()),
            ])
            .send()
            .await
            .map_err(|error| AppError::Spotify(format!("lyrics request failed: {error}")))?;

        // 404 is the normal answer here, so it is not an error.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(AppError::Spotify(format!(
                "lyrics request failed with {}",
                response.status()
            )));
        }
        let body: LrclibResponse = response
            .json()
            .await
            .map_err(|error| AppError::Spotify(format!("lyrics response unreadable: {error}")))?;
        Ok(into_lyrics(&body))
    }

    async fn fetch_by_search(
        &self,
        track: &str,
        artist: &str,
        duration_ms: u64,
    ) -> Result<Option<Lyrics>, AppError> {
        let response = self
            .http
            .get(&self.search_url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .query(&[("track_name", track), ("artist_name", artist)])
            .send()
            .await
            .map_err(|error| AppError::Spotify(format!("lyrics search failed: {error}")))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let results: Vec<LrclibResponse> = response
            .json()
            .await
            .map_err(|error| AppError::Spotify(format!("lyrics search unreadable: {error}")))?;

        Ok(pick_best(&results, duration_ms).and_then(into_lyrics))
    }
}

/// Same discipline as the token store: a fresh sibling temp file is written,
/// then renamed over the destination, so a crash mid-write leaves either the
/// old file or the new one, never a half-written one.
async fn write_track_file(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to create lyrics cache directory: {error}"
            ))
        })?;
    }
    let mut temporary: OsString = path.file_name().unwrap_or_default().to_os_string();
    temporary.push(".tmp");
    let temporary = path.with_file_name(temporary);
    tokio::fs::write(&temporary, bytes).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "failed to write lyrics cache file: {error}"
        ))
    })?;
    tokio::fs::rename(&temporary, path).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "failed to commit lyrics cache file: {error}"
        ))
    })
}

/// Prefer a synced result whose length matches the track, then any synced one,
/// then anything at all. Length matters: the same song has many masters and a
/// mismatched one drifts audibly.
fn pick_best(results: &[LrclibResponse], duration_ms: u64) -> Option<&LrclibResponse> {
    let close = |candidate: &LrclibResponse| match candidate.duration {
        Some(seconds) => {
            let candidate_ms = (seconds * 1000.0) as u64;
            candidate_ms.abs_diff(duration_ms) <= DURATION_TOLERANCE_MS
        }
        None => false,
    };
    results
        .iter()
        .find(|candidate| candidate.synced_lyrics.is_some() && close(candidate))
        .or_else(|| {
            results
                .iter()
                .find(|candidate| candidate.synced_lyrics.is_some())
        })
        .or_else(|| results.first())
}

fn into_lyrics(body: &LrclibResponse) -> Option<Lyrics> {
    {
        if let Some(synced) = body.synced_lyrics.as_deref() {
            let lines = parse_lrc(synced);
            if !lines.is_empty() {
                return Some(Lyrics {
                    synced: true,
                    lines,
                });
            }
        }
        if let Some(plain) = body.plain_lyrics.as_deref() {
            let lines: Vec<_> = plain
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| LyricLine {
                    at_ms: 0,
                    text: line.to_string(),
                })
                .collect();
            if !lines.is_empty() {
                // Shown as text, never scrolled: inventing timings would drift
                // visibly and look broken.
                return Some(Lyrics {
                    synced: false,
                    lines,
                });
            }
        }
        None
    }
}

/// Parse LRC timestamps: `[mm:ss.xx] text`, with several stamps allowed per
/// line and blank lines kept as instrumental gaps.
fn parse_lrc(source: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in source.lines() {
        let mut rest = raw;
        let mut stamps = Vec::new();
        // A line may carry several timestamps for a repeated chorus.
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else { break };
            let stamp = &rest[1..close];
            if let Some(at_ms) = parse_timestamp(stamp) {
                stamps.push(at_ms);
            }
            rest = &rest[close + 1..];
        }
        if stamps.is_empty() {
            continue;
        }
        let text = rest.trim().to_string();
        for at_ms in stamps {
            lines.push(LyricLine {
                at_ms,
                text: text.clone(),
            });
        }
    }
    lines.sort_by_key(|line| line.at_ms);
    lines
}

/// `mm:ss.xx`, `mm:ss.xxx` or `mm:ss`. Metadata tags like `[ar:Artist]` have no
/// numeric minutes and fall out as `None`.
fn parse_timestamp(stamp: &str) -> Option<u64> {
    let (minutes, rest) = stamp.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, fraction),
        None => (rest, ""),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;
    let fraction = fraction.trim();
    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction.get(..3)?.parse::<u64>().ok()?,
    };
    Some(minutes * 60_000 + seconds * 1_000 + millis)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
    use serde_json::json;

    use super::*;

    fn candidate(seconds: Option<f64>, synced: bool) -> LrclibResponse {
        LrclibResponse {
            plain_lyrics: Some("plain".to_string()),
            synced_lyrics: synced.then(|| "[00:01.00] line".to_string()),
            duration: seconds,
        }
    }

    #[test]
    fn the_closest_synced_release_wins() {
        // The same song has many masters; a mismatched one drifts audibly.
        let results = [
            candidate(Some(200.0), true),
            candidate(Some(293.0), true),
            candidate(Some(291.0), true),
        ];
        let best = pick_best(&results, 292_000).expect("a match");
        assert_eq!(best.duration, Some(293.0));
    }

    #[test]
    fn synced_beats_close_when_nothing_close_is_synced() {
        let results = [candidate(Some(292.0), false), candidate(Some(180.0), true)];
        let best = pick_best(&results, 292_000).expect("a match");
        // Following the wrong-length words still beats not following at all.
        assert!(best.synced_lyrics.is_some());
    }

    #[test]
    fn something_unsynced_is_better_than_nothing() {
        let results = [candidate(None, false)];
        assert!(pick_best(&results, 292_000).is_some());
        assert!(pick_best(&[], 292_000).is_none());
    }

    #[test]
    fn plain_only_results_are_marked_unsynced() {
        let lyrics = into_lyrics(&candidate(Some(200.0), false)).expect("lyrics");
        assert!(!lyrics.synced, "must not claim to follow along");
        assert_eq!(lyrics.lines.len(), 1);
        assert_eq!(lyrics.lines[0].at_ms, 0);
    }

    #[test]
    fn timestamps_parse_in_every_shape_lrclib_emits() {
        assert_eq!(parse_timestamp("00:00.00"), Some(0));
        assert_eq!(parse_timestamp("01:02.50"), Some(62_500));
        assert_eq!(parse_timestamp("01:02.5"), Some(62_500));
        assert_eq!(parse_timestamp("01:02.500"), Some(62_500));
        assert_eq!(parse_timestamp("02:03"), Some(123_000));
        assert_eq!(parse_timestamp("10:00.00"), Some(600_000));
        // Metadata tags share the bracket syntax and must not become lines.
        assert_eq!(parse_timestamp("ar:Some Artist"), None);
        assert_eq!(parse_timestamp("by:someone"), None);
        assert_eq!(parse_timestamp(""), None);
    }

    #[test]
    fn lrc_parses_into_ordered_lines() {
        let lines = parse_lrc(
            "[ar:KK]\n[00:12.00] First line\n[00:20.50]Second line\n[00:05.00] Earlier\n",
        );
        assert_eq!(
            lines,
            vec![
                LyricLine {
                    at_ms: 5_000,
                    text: "Earlier".to_string()
                },
                LyricLine {
                    at_ms: 12_000,
                    text: "First line".to_string()
                },
                LyricLine {
                    at_ms: 20_500,
                    text: "Second line".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_repeated_chorus_lands_at_every_timestamp() {
        let lines = parse_lrc("[00:10.00][01:10.00][02:10.00] Chorus\n");
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.text == "Chorus"));
        assert_eq!(
            lines.iter().map(|line| line.at_ms).collect::<Vec<_>>(),
            vec![10_000, 70_000, 130_000]
        );
    }

    #[test]
    fn an_instrumental_gap_keeps_its_place() {
        // An empty timed line is how LRC marks a break; dropping it would make
        // the previous line appear to hold until the next vocal.
        let lines = parse_lrc("[00:10.00] Sung\n[00:14.00]\n[00:30.00] Again\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "");
        assert_eq!(lines[1].at_ms, 14_000);
    }

    #[test]
    fn untimed_junk_yields_nothing_rather_than_line_zero() {
        assert!(parse_lrc("just some words\nand more\n").is_empty());
        assert!(parse_lrc("").is_empty());
    }

    /// A mock LRCLIB: `/get` for the exact lookup, `/search` for the fallback.
    /// `hits` counts every request either endpoint receives.
    async fn spawn_lrclib(
        get_status: StatusCode,
        get_body: serde_json::Value,
    ) -> (String, String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let app = Router::new()
            .route(
                "/get",
                get(move || {
                    let counted = counted.clone();
                    let status = get_status;
                    let body = get_body.clone();
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        (status, Json(body)).into_response()
                    }
                }),
            )
            .route("/search", get(|| async { Json(json!([])).into_response() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock LRCLIB");
        let address = listener.local_addr().expect("mock LRCLIB address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock LRCLIB");
        });
        (
            format!("http://{address}/get"),
            format!("http://{address}/search"),
            hits,
        )
    }

    #[tokio::test]
    async fn a_miss_that_finds_lyrics_writes_exactly_one_track_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (get_url, search_url, hits) = spawn_lrclib(
            StatusCode::OK,
            json!({ "plainLyrics": "one\ntwo", "syncedLyrics": null }),
        )
        .await;
        let index =
            LyricsIndex::with_test_urls(directory.path().to_path_buf(), get_url, search_url);

        let found = index
            .for_track("track-1", "Song", "Artist", "Album", 200_000)
            .await
            .expect("lyrics");
        assert!(!found.synced);
        assert_eq!(found.lines.len(), 2);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let files: Vec<_> = std::fs::read_dir(directory.path())
            .expect("read cache dir")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].file_name(), "track-1.json");
    }

    #[tokio::test]
    async fn a_confirmed_miss_is_cached_and_not_re_fetched() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (get_url, search_url, hits) = spawn_lrclib(StatusCode::NOT_FOUND, json!({})).await;
        let index =
            LyricsIndex::with_test_urls(directory.path().to_path_buf(), get_url, search_url);

        let first = index
            .for_track("track-2", "Song", "Artist", "Album", 200_000)
            .await;
        assert!(first.is_none());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        // Same process, same in-memory cache: must not ask LRCLIB again.
        let second = index
            .for_track("track-2", "Song", "Artist", "Album", 200_000)
            .await;
        assert!(second.is_none());

        // A fresh index (as if the process restarted), pointed at nothing
        // reachable, must still find the miss on disk rather than dialing out.
        let reloaded = LyricsIndex::with_test_urls(
            directory.path().to_path_buf(),
            "http://127.0.0.1:1/get".to_string(),
            "http://127.0.0.1:1/search".to_string(),
        );
        let third = reloaded
            .for_track("track-2", "Song", "Artist", "Album", 200_000)
            .await;
        assert!(third.is_none());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the confirmed miss must be served from disk, not looked up again"
        );
    }

    #[tokio::test]
    async fn a_network_failure_is_not_cached_so_the_next_play_retries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // Nothing is listening on this port, so every request fails.
        let index = LyricsIndex::with_test_urls(
            directory.path().to_path_buf(),
            "http://127.0.0.1:1/get".to_string(),
            "http://127.0.0.1:1/search".to_string(),
        );

        let found = index
            .for_track("track-3", "Song", "Artist", "Album", 200_000)
            .await;
        assert!(found.is_none());
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            0,
            "a failed lookup must not be cached as a confirmed miss"
        );
    }

    #[tokio::test]
    async fn load_counts_track_files_without_populating_the_hot_cache() {
        let directory = tempfile::tempdir().expect("temporary directory");
        tokio::fs::write(
            directory.path().join("track-a.json"),
            serde_json::to_vec(&Some(Lyrics {
                synced: false,
                lines: vec![],
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(directory.path().join("track-b.json"), b"null")
            .await
            .unwrap();
        // An orphaned temp file from a crash mid-write must not count.
        tokio::fs::write(directory.path().join("track-c.json.tmp"), b"null")
            .await
            .unwrap();

        let index = LyricsIndex::new(directory.path().to_path_buf());
        assert_eq!(index.load().await, 2);
    }

    #[tokio::test]
    async fn a_pre_seeded_disk_entry_is_served_without_any_network_access() {
        let directory = tempfile::tempdir().expect("temporary directory");
        tokio::fs::write(
            directory.path().join("track-a.json"),
            serde_json::to_vec(&Some(Lyrics {
                synced: true,
                lines: vec![LyricLine {
                    at_ms: 1_000,
                    text: "hello".to_string(),
                }],
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        // Points at nothing reachable: this must never be dialed.
        let index = LyricsIndex::with_test_urls(
            directory.path().to_path_buf(),
            "http://127.0.0.1:1/get".to_string(),
            "http://127.0.0.1:1/search".to_string(),
        );

        let found = index
            .for_track("track-a", "Song", "Artist", "Album", 200_000)
            .await
            .expect("lyrics served from disk");
        assert!(found.synced);
        assert_eq!(found.lines[0].text, "hello");
    }
}
