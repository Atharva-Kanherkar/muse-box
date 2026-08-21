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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RenderDoc {
    pub version: u32,
    pub state: PlaybackState,
    pub track: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Base64-encoded 1-bit (or half-block) dithered album art.
    pub art_1bit: Option<String>,
    /// Extracted dominant / accent palette.
    pub palette: Vec<String>,
    pub progress_ms: u64,
    pub duration_ms: u64,
    /// Optional FFT band values for LED reactivity (0.0 - 1.0).
    pub fft_bands: Vec<f32>,
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
            track: None,
            artist: None,
            album: None,
            art_1bit: None,
            palette: vec!["#1a1a1a".to_string(), "#e0e0e0".to_string()],
            progress_ms: 0,
            duration_ms: 0,
            fft_bands: vec![],
            voice_log: vec![],
        }
    }
}
