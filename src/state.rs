//! Shared playback state and meaningful-change detection for the SSE feed.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::{error::AppError, render::DitherMode, spotify::PlaybackObservation};

const MIN_RENDER_DIMENSION: u32 = 16;
const MAX_RENDER_DIMENSION: u32 = 1024;
const DEFAULT_RENDER_DIMENSION: u32 = 400;
const SEEK_THRESHOLD_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderParams {
    pub width: u32,
    pub height: u32,
    pub dither: DitherMode,
}

impl Default for RenderParams {
    fn default() -> Self {
        Self {
            width: DEFAULT_RENDER_DIMENSION,
            height: DEFAULT_RENDER_DIMENSION,
            dither: DitherMode::Bayer,
        }
    }
}

impl RenderParams {
    pub fn from_query(query: &HashMap<String, String>) -> Result<Self, AppError> {
        let defaults = Self::default();
        let width = parse_dimension(query.get("w"), "w", defaults.width)?;
        let height = parse_dimension(query.get("h"), "h", defaults.height)?;
        let dither = match query.get("dither").map(String::as_str) {
            None | Some("bayer") => DitherMode::Bayer,
            Some("atkinson") => DitherMode::Atkinson,
            Some(value) => {
                return Err(AppError::BadRequest(format!(
                    "invalid dither mode: {value}"
                )));
            }
        };
        Ok(Self {
            width,
            height,
            dither,
        })
    }
}

pub fn is_meaningful_change(
    published: &PlaybackObservation,
    observed: &PlaybackObservation,
) -> bool {
    if published.track_id != observed.track_id || published.is_playing != observed.is_playing {
        return true;
    }
    observed
        .progress_ms
        .abs_diff(interpolated_progress(published, observed.observed_at))
        > SEEK_THRESHOLD_MS
}

pub fn interpolated_progress(published: &PlaybackObservation, at: DateTime<Utc>) -> u64 {
    if !published.is_playing {
        return published.progress_ms;
    }
    let elapsed_ms = (at - published.observed_at).num_milliseconds().max(0) as u64;
    published
        .progress_ms
        .saturating_add(elapsed_ms)
        .min(published.duration_ms)
}

fn parse_dimension(value: Option<&String>, name: &str, default: u32) -> Result<u32, AppError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u32>()
        .map_err(|_| AppError::BadRequest(format!("invalid {name}: {value}")))?;
    if !(MIN_RENDER_DIMENSION..=MAX_RENDER_DIMENSION).contains(&parsed) {
        return Err(AppError::BadRequest(format!(
            "{name} must be between {MIN_RENDER_DIMENSION} and {MAX_RENDER_DIMENSION}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[test]
    fn render_params_apply_defaults_and_validate_bounds() {
        assert_eq!(
            RenderParams::from_query(&HashMap::new()).unwrap(),
            RenderParams::default()
        );
        let valid = HashMap::from([
            ("w".to_string(), "16".to_string()),
            ("h".to_string(), "1024".to_string()),
            ("dither".to_string(), "atkinson".to_string()),
        ]);
        assert_eq!(
            RenderParams::from_query(&valid).unwrap(),
            RenderParams {
                width: 16,
                height: 1024,
                dither: DitherMode::Atkinson,
            }
        );

        for invalid in [
            HashMap::from([("w".to_string(), "15".to_string())]),
            HashMap::from([("h".to_string(), "1025".to_string())]),
            HashMap::from([("w".to_string(), "nope".to_string())]),
            HashMap::from([("dither".to_string(), "floyd".to_string())]),
        ] {
            assert!(RenderParams::from_query(&invalid).is_err());
        }
    }

    #[test]
    fn meaningful_change_detects_track_playback_and_seek_only() {
        let start = Utc::now();
        let published = observation(start, "track-a", true, 10_000);

        let steady = observation(start + Duration::seconds(2), "track-a", true, 12_000);
        assert!(!is_meaningful_change(&published, &steady));

        let boundary = observation(start + Duration::seconds(2), "track-a", true, 14_000);
        assert!(!is_meaningful_change(&published, &boundary));

        let seek = observation(start + Duration::seconds(2), "track-a", true, 14_001);
        assert!(is_meaningful_change(&published, &seek));

        let paused = observation(start + Duration::seconds(2), "track-a", false, 12_000);
        assert!(is_meaningful_change(&published, &paused));

        let changed = observation(start + Duration::seconds(2), "track-b", true, 12_000);
        assert!(is_meaningful_change(&published, &changed));
    }

    fn observation(
        observed_at: DateTime<Utc>,
        track_id: &str,
        is_playing: bool,
        progress_ms: u64,
    ) -> PlaybackObservation {
        PlaybackObservation {
            observed_at,
            track_id: Some(track_id.to_string()),
            track: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            art_url: Some("http://example.test/art.png".to_string()),
            is_playing,
            progress_ms,
            duration_ms: 300_000,
        }
    }
}
