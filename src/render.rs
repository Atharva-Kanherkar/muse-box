use serde::{Deserialize, Serialize};

/// Versioned render document emitted by the backend.
/// Web UI and ESP32 both consume this exact shape.
pub const RENDER_DOCUMENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    #[default]
    Idle,
    Playing,
    Paused,
    /// A voice command is being processed (broadcast the moment an upload lands).
    Thinking,
    /// Reserved for the streaming-voice mode (phase 2.5).
    Listening,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DitherMode {
    Bayer,
    Atkinson,
}

/// Dithered artwork (album cover or idle frame), ready to blit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Art {
    pub w: u32,
    pub h: u32,
    pub dither: DitherMode,
    /// Base64 of packed 1-bit data: row-major, MSB-first within each byte,
    /// each row padded to a whole byte, 1 = foreground (ink). Not a PNG.
    pub bits: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RenderDoc {
    pub version: u32,
    pub state: PlaybackState,
    /// When this document was built. Clients interpolate progress from it:
    /// rendered_progress = progress_ms + (now - server_ts) while playing.
    pub server_ts: Option<chrono::DateTime<chrono::Utc>>,
    /// Spotify track ID; clients may use it to cache decoded art.
    pub track_id: Option<String>,
    pub track: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art: Option<Art>,
    /// Exactly two colors: [dominant background, clamped accent].
    pub palette: Vec<String>,
    /// Playback position at server_ts.
    pub progress_ms: u64,
    pub duration_ms: u64,
    /// Recent voice command transcript log.
    pub voice_log: Vec<VoiceLogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceLogEntry {
    pub transcript: String,
    pub action: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl RenderDoc {
    pub fn idle() -> Self {
        Self {
            version: RENDER_DOCUMENT_VERSION,
            state: PlaybackState::Idle,
            server_ts: Some(chrono::Utc::now()),
            track_id: None,
            track: None,
            artist: None,
            album: None,
            art: None,
            palette: vec!["#1a1a1a".to_string(), "#e0e0e0".to_string()],
            progress_ms: 0,
            duration_ms: 0,
            voice_log: vec![],
        }
    }
}
