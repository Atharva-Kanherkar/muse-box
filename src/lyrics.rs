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

use std::{collections::HashMap, path::PathBuf};

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
/// Tracks to remember. A lookup is cheap but not free, and the same album gets
/// played through repeatedly.
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
    /// `None` marks a track we already know has no lyrics, so a track on repeat
    /// does not re-ask on every play.
    cache: RwLock<HashMap<String, Option<Lyrics>>>,
    store_path: PathBuf,
}

impl LyricsIndex {
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            http: crate::spotify::http_client(),
            cache: RwLock::new(HashMap::new()),
            store_path,
        }
    }

    pub async fn load(&self) -> usize {
        let Ok(bytes) = tokio::fs::read(&self.store_path).await else {
            return 0;
        };
        match serde_json::from_slice::<HashMap<String, Option<Lyrics>>>(&bytes) {
            Ok(cache) => {
                let count = cache.len();
                *self.cache.write().await = cache;
                count
            }
            Err(error) => {
                tracing::warn!(%error, "lyrics cache is unreadable; starting empty");
                0
            }
        }
    }

    /// Lyrics for a track, from cache or LRCLIB. A miss is remembered as a miss.
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

        let found = match self.fetch(track, artist, album, duration_ms).await {
            Ok(found) => found,
            Err(error) => {
                // A lookup failure must never disturb playback, so it is not
                // cached either: the next play tries again.
                tracing::warn!(%error, track, "lyrics lookup failed");
                return None;
            }
        };

        let mut cache = self.cache.write().await;
        if !cache.contains_key(track_id) && cache.len() >= MAX_CACHED_TRACKS {
            cache.clear();
        }
        cache.insert(track_id.to_string(), found.clone());
        let snapshot = cache.clone();
        drop(cache);

        // Best effort: losing the cache costs a lookup, not correctness.
        if let Ok(bytes) = serde_json::to_vec(&snapshot) {
            let _ = tokio::fs::write(&self.store_path, bytes).await;
        }
        found
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
            .get(LRCLIB_GET_URL)
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
            .get(LRCLIB_SEARCH_URL)
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
}
