use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use axum::{
    Json,
    body::to_bytes,
    extract::{Query, Request, State},
    http::header,
};
use chrono::Utc;
use tokio::sync::Mutex;

use crate::{
    error::AppError,
    realtime::{PlaybackContext, RealtimeManager, ToolCall, VoiceIntent},
    render::{PlaybackState, RenderDoc, VoiceLogEntry},
    spotify::{PlaybackFetchError, PlaybackObservation, SpotifyClient},
    state::{RenderParams, StateHub},
};

const MAX_AUDIO_SECONDS: u64 = 30;
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub type VoiceFuture<'a> = Pin<Box<dyn Future<Output = Result<VoiceIntent, AppError>> + Send + 'a>>;

pub trait VoiceModel: Send + Sync {
    fn command<'a>(
        &'a self,
        samples: &'a [i16],
        rate: u32,
        context: PlaybackContext,
    ) -> VoiceFuture<'a>;
}

#[cfg(test)]
pub(crate) struct FailingVoiceModel;

#[cfg(test)]
impl VoiceModel for FailingVoiceModel {
    fn command<'a>(
        &'a self,
        _samples: &'a [i16],
        _rate: u32,
        _context: PlaybackContext,
    ) -> VoiceFuture<'a> {
        Box::pin(async {
            Err(AppError::Voice(
                "voice model is not configured for this test".to_string(),
            ))
        })
    }
}

impl VoiceModel for RealtimeManager {
    fn command<'a>(
        &'a self,
        samples: &'a [i16],
        rate: u32,
        context: PlaybackContext,
    ) -> VoiceFuture<'a> {
        Box::pin(async move { RealtimeManager::command(self, samples, rate, context).await })
    }
}

#[derive(Clone)]
pub(crate) struct VoiceState {
    pub(crate) spotify: SpotifyClient,
    pub(crate) model: Arc<dyn VoiceModel>,
    pub(crate) hub: Arc<StateHub>,
    pub(crate) guard: Arc<Mutex<()>>,
}

pub(crate) async fn post_voice(
    State(state): State<VoiceState>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Result<Json<RenderDoc>, AppError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            AppError::PayloadTooLarge("voice upload exceeds the 10 MiB limit".to_string())
        })?;
    let audio = decode_audio(&content_type, &query, &body)?;

    let _guard = state.guard.lock().await;
    state.hub.publish_thinking().await;
    let observation = state.hub.published_observation().await;
    let context = PlaybackContext {
        track: observation.track.clone(),
        artist: observation.artist.clone(),
        state: playback_state(&observation),
    };
    let intent = match state
        .model
        .command(&audio.samples, audio.rate, context)
        .await
    {
        Ok(intent) => intent,
        Err(error) => {
            recover_document(&state.hub).await;
            return Err(error);
        }
    };
    let action = match dispatch_tool(&state.spotify, &intent.tool).await {
        Ok(action) => action,
        Err(error) => {
            recover_document(&state.hub).await;
            return Err(error);
        }
    };
    state
        .hub
        .append_voice_log(VoiceLogEntry {
            transcript: intent.transcript,
            action,
            timestamp: Utc::now(),
        })
        .await;
    let fresh = match state.spotify.currently_playing().await {
        Ok(observation) => observation,
        Err(error) => {
            recover_document(&state.hub).await;
            return Err(playback_error(error));
        }
    };
    if let Err(error) = state.hub.force_publish(fresh).await {
        recover_document(&state.hub).await;
        return Err(error);
    }
    let document = state.hub.current_document(RenderParams::default()).await?;
    Ok(Json(document.as_ref().clone()))
}

async fn dispatch_tool(spotify: &SpotifyClient, tool: &ToolCall) -> Result<String, AppError> {
    match tool {
        ToolCall::Play => {
            spotify.resume_playback().await?;
            Ok("spotify:play".to_string())
        }
        ToolCall::Pause => {
            spotify.pause_playback().await?;
            Ok("spotify:pause".to_string())
        }
        ToolCall::Next => {
            spotify.skip_next().await?;
            Ok("spotify:next".to_string())
        }
        ToolCall::Previous => {
            spotify.skip_previous().await?;
            Ok("spotify:previous".to_string())
        }
        ToolCall::SearchAndPlay { query } => {
            let track_id = spotify.search_top_track(query).await?;
            spotify.play_track(&track_id).await?;
            Ok(format!("spotify:play:track:{track_id}"))
        }
        ToolCall::QueueSearch { query } => {
            let track_id = spotify.search_top_track(query).await?;
            spotify.queue_track(&track_id).await?;
            Ok(format!("queue:search:{query}"))
        }
        ToolCall::SetVolume { percent } => {
            spotify.set_volume(*percent).await?;
            Ok(format!("spotify:volume:{percent}"))
        }
        ToolCall::NowPlaying => Ok("query:now_playing".to_string()),
    }
}

async fn recover_document(hub: &StateHub) {
    let observation = hub.published_observation().await;
    if let Err(error) = hub.force_publish(observation).await {
        tracing::error!(%error, "failed to publish voice recovery document");
    }
}

fn playback_state(observation: &PlaybackObservation) -> PlaybackState {
    match (&observation.track_id, observation.is_playing) {
        (None, _) => PlaybackState::Idle,
        (Some(_), true) => PlaybackState::Playing,
        (Some(_), false) => PlaybackState::Paused,
    }
}

fn playback_error(error: PlaybackFetchError) -> AppError {
    match error {
        PlaybackFetchError::Spotify(error) => error,
        other => AppError::Spotify(other.to_string()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AudioInput {
    pub samples: Vec<i16>,
    pub rate: u32,
}

pub fn decode_audio(
    content_type: &str,
    query: &HashMap<String, String>,
    body: &[u8],
) -> Result<AudioInput, AppError> {
    match content_type.split(';').next().map(str::trim) {
        Some("audio/wav") => decode_wav(body),
        Some("audio/pcm") => decode_raw_pcm(query, body),
        _ => Err(AppError::UnsupportedMediaType(
            "content type must be audio/wav or audio/pcm".to_string(),
        )),
    }
}

fn decode_raw_pcm(query: &HashMap<String, String>, body: &[u8]) -> Result<AudioInput, AppError> {
    let rate = required_number(query, "rate")?;
    let bits = required_number(query, "bits")?;
    let channels = required_number(query, "ch")?;
    if rate == 0 {
        return Err(AppError::BadRequest(
            "rate must be greater than zero".to_string(),
        ));
    }
    if bits != 16 {
        return Err(AppError::BadRequest(
            "only 16-bit PCM is supported".to_string(),
        ));
    }
    if !matches!(channels, 1 | 2) {
        return Err(AppError::BadRequest("ch must be 1 or 2".to_string()));
    }
    decode_pcm16(body, rate, channels)
}

fn decode_wav(body: &[u8]) -> Result<AudioInput, AppError> {
    if body.len() < 12 || &body[0..4] != b"RIFF" || &body[8..12] != b"WAVE" {
        return Err(AppError::BadRequest("invalid WAV header".to_string()));
    }
    let declared_size = read_u32(body, 4)? as usize;
    if declared_size.checked_add(8) != Some(body.len()) {
        return Err(AppError::BadRequest(
            "WAV size does not match its RIFF header".to_string(),
        ));
    }

    let mut format = None;
    let mut data = None;
    let mut offset = 12_usize;
    while offset < body.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| AppError::BadRequest("invalid WAV chunk size".to_string()))?;
        if header_end > body.len() {
            return Err(AppError::BadRequest(
                "truncated WAV chunk header".to_string(),
            ));
        }
        let size = read_u32(body, offset + 4)? as usize;
        let start = header_end;
        let end = start
            .checked_add(size)
            .ok_or_else(|| AppError::BadRequest("invalid WAV chunk size".to_string()))?;
        if end > body.len() {
            return Err(AppError::BadRequest("truncated WAV chunk".to_string()));
        }
        match &body[offset..offset + 4] {
            b"fmt " => format = Some(parse_wav_format(&body[start..end])?),
            b"data" => data = Some(&body[start..end]),
            _ => {}
        }
        offset = end
            .checked_add(size % 2)
            .ok_or_else(|| AppError::BadRequest("invalid WAV padding".to_string()))?;
        if offset > body.len() {
            return Err(AppError::BadRequest("truncated WAV padding".to_string()));
        }
    }
    let (rate, channels) =
        format.ok_or_else(|| AppError::BadRequest("WAV is missing its fmt chunk".to_string()))?;
    let data = data.ok_or_else(|| AppError::BadRequest("WAV is missing audio data".to_string()))?;
    decode_pcm16(data, rate, channels)
}

fn parse_wav_format(chunk: &[u8]) -> Result<(u32, u32), AppError> {
    if chunk.len() < 16 {
        return Err(AppError::BadRequest("truncated WAV format".to_string()));
    }
    if read_u16(chunk, 0)? != 1 {
        return Err(AppError::BadRequest(
            "only PCM WAV audio is supported".to_string(),
        ));
    }
    let channels = u32::from(read_u16(chunk, 2)?);
    let rate = read_u32(chunk, 4)?;
    let block_align = u32::from(read_u16(chunk, 12)?);
    let bits = u32::from(read_u16(chunk, 14)?);
    if rate == 0 || bits != 16 || !matches!(channels, 1 | 2) {
        return Err(AppError::BadRequest(
            "WAV must be PCM16 mono or stereo with a positive sample rate".to_string(),
        ));
    }
    if block_align != channels * 2 {
        return Err(AppError::BadRequest(
            "WAV block alignment does not match its format".to_string(),
        ));
    }
    Ok((rate, channels))
}

fn decode_pcm16(body: &[u8], rate: u32, channels: u32) -> Result<AudioInput, AppError> {
    let frame_bytes = usize::try_from(channels * 2)
        .map_err(|_| AppError::BadRequest("invalid channel count".to_string()))?;
    if body.is_empty() {
        return Err(AppError::BadRequest(
            "audio input must not be empty".to_string(),
        ));
    }
    if !body.len().is_multiple_of(frame_bytes) {
        return Err(AppError::BadRequest(
            "PCM data ends in a partial sample frame".to_string(),
        ));
    }
    let frames = body.len() / frame_bytes;
    if frames as u64 > u64::from(rate) * MAX_AUDIO_SECONDS {
        return Err(AppError::PayloadTooLarge(
            "audio duration exceeds 30 seconds".to_string(),
        ));
    }

    let mut samples = Vec::with_capacity(frames);
    for frame in body.chunks_exact(frame_bytes) {
        let left = i16::from_le_bytes([frame[0], frame[1]]);
        let mono = if channels == 1 {
            left
        } else {
            let right = i16::from_le_bytes([frame[2], frame[3]]);
            ((i32::from(left) + i32::from(right)) / 2) as i16
        };
        samples.push(mono);
    }
    Ok(AudioInput { samples, rate })
}

fn required_number(query: &HashMap<String, String>, name: &str) -> Result<u32, AppError> {
    let value = query
        .get(name)
        .ok_or_else(|| AppError::BadRequest(format!("missing {name} query parameter")))?;
    value
        .parse::<u32>()
        .map_err(|_| AppError::BadRequest(format!("invalid {name}: {value}")))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, AppError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| AppError::BadRequest("truncated WAV field".to_string()))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AppError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| AppError::BadRequest("truncated WAV field".to_string()))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::{
        routes,
        spotify::{SpotifyClient, SpotifyConfig},
        state::StateHub,
    };

    use super::*;

    #[tokio::test]
    async fn route_rejects_auth_content_type_params_and_body_size_matrix() {
        let app = routes::router(
            SpotifyClient::new(SpotifyConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                redirect_uri: "http://localhost/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            }),
            "device-token".to_string(),
            Arc::new(StateHub::new()),
            Arc::new(FailingVoiceModel),
        );
        let cases = [
            (
                Request::builder()
                    .method("POST")
                    .uri("/voice?rate=16000&bits=16&ch=1")
                    .header(header::CONTENT_TYPE, "audio/pcm")
                    .body(Body::from(vec![0, 0]))
                    .expect("missing-auth request"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Request::builder()
                    .method("POST")
                    .uri("/voice")
                    .header(header::AUTHORIZATION, "Bearer device-token")
                    .header(header::CONTENT_TYPE, "audio/pcm")
                    .body(Body::from(vec![0, 0]))
                    .expect("missing-params request"),
                StatusCode::BAD_REQUEST,
            ),
            (
                Request::builder()
                    .method("POST")
                    .uri("/voice")
                    .header(header::AUTHORIZATION, "Bearer device-token")
                    .header(header::CONTENT_TYPE, "audio/webm")
                    .body(Body::from(vec![0, 0]))
                    .expect("unsupported request"),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (
                Request::builder()
                    .method("POST")
                    .uri("/voice?rate=16000&bits=16&ch=1")
                    .header(header::AUTHORIZATION, "Bearer device-token")
                    .header(header::CONTENT_TYPE, "audio/pcm")
                    .body(Body::from(vec![0; MAX_BODY_BYTES + 1]))
                    .expect("oversized request"),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ];

        for (request, expected) in cases {
            let response = app.clone().oneshot(request).await.expect("response");
            assert_eq!(response.status(), expected);
            let body = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            let json: Value = serde_json::from_slice(&body).expect("JSON error");
            assert!(json["error"].is_string());
            assert_eq!(json.as_object().map(serde_json::Map::len), Some(1));
        }
    }

    #[tokio::test]
    async fn now_playing_is_a_non_mutating_query_action() {
        let spotify = SpotifyClient::new(SpotifyConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            redirect_uri: "http://localhost/callback".to_string(),
            token_store_path: PathBuf::from("unused"),
        });
        assert_eq!(
            dispatch_tool(&spotify, &ToolCall::NowPlaying)
                .await
                .expect("query action"),
            "query:now_playing"
        );
    }

    #[test]
    fn wav_pcm16_mono_preserves_samples() {
        let wav = wav_fixture(16_000, 1, 1, 16, &[-32_768, -1, 0, 1, 32_767]);
        assert_eq!(
            decode_wav(&wav).expect("mono WAV"),
            AudioInput {
                samples: vec![-32_768, -1, 0, 1, 32_767],
                rate: 16_000
            }
        );
    }

    #[test]
    fn wav_pcm16_stereo_downmixes_without_overflow() {
        let wav = wav_fixture(
            48_000,
            2,
            1,
            16,
            &[32_767, 32_767, -32_768, -32_768, 100, -50],
        );
        assert_eq!(
            decode_wav(&wav).expect("stereo WAV").samples,
            vec![32_767, -32_768, 25]
        );
    }

    #[test]
    fn wav_rejects_compression_bit_depth_truncation_and_duration() {
        for wav in [
            wav_fixture(16_000, 1, 3, 16, &[0]),
            wav_fixture(16_000, 1, 1, 24, &[0]),
        ] {
            assert!(matches!(decode_wav(&wav), Err(AppError::BadRequest(_))));
        }
        let mut truncated = wav_fixture(16_000, 1, 1, 16, &[0]);
        truncated.pop();
        assert!(matches!(
            decode_wav(&truncated),
            Err(AppError::BadRequest(_))
        ));
        let too_long = vec![0; (8_000 * 30 + 1) * 2];
        assert!(matches!(
            decode_raw_pcm(&raw_query(8_000, 16, 1), &too_long),
            Err(AppError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn raw_pcm_requires_format_and_complete_supported_frames() {
        for query in [
            HashMap::new(),
            raw_query(16_000, 24, 1),
            raw_query(16_000, 16, 3),
            raw_query(0, 16, 1),
        ] {
            assert!(matches!(
                decode_raw_pcm(&query, &[0, 0]),
                Err(AppError::BadRequest(_))
            ));
        }
        assert!(matches!(
            decode_raw_pcm(&raw_query(16_000, 16, 2), &[0, 0]),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            decode_audio("audio/x-wav", &HashMap::new(), &[0, 0]),
            Err(AppError::UnsupportedMediaType(_))
        ));
    }

    fn raw_query(rate: u32, bits: u32, channels: u32) -> HashMap<String, String> {
        HashMap::from([
            ("rate".to_string(), rate.to_string()),
            ("bits".to_string(), bits.to_string()),
            ("ch".to_string(), channels.to_string()),
        ])
    }

    fn wav_fixture(rate: u32, channels: u16, format: u16, bits: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<_> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36_u32 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&format.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        wav.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }
}
