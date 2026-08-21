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
    // Armed before `thinking` goes out: if the caller disconnects mid-flight the
    // handler future is dropped and no `?` or match arm below ever runs, so this
    // guard is the only thing that can put the display back.
    let mut thinking = ThinkingGuard::new(state.hub.clone());
    state.hub.publish_thinking().await;

    let outcome = run_voice_command(&state, &audio).await;
    if outcome.is_err() {
        // Recovered inline rather than left to the guard, so the follow-up
        // broadcast is ordered before the response instead of racing it.
        recover_document(&state.hub).await;
    }
    thinking.settle();
    outcome.map(Json)
}

async fn run_voice_command(state: &VoiceState, audio: &AudioInput) -> Result<RenderDoc, AppError> {
    let observation = state.hub.published_observation().await;
    let context = PlaybackContext {
        track: observation.track.clone(),
        artist: observation.artist.clone(),
        state: playback_state(&observation),
    };
    let intent = state
        .model
        .command(&audio.samples, audio.rate, context)
        .await?;
    let action = dispatch_tool(&state.spotify, &intent.tool).await?;
    state
        .hub
        .append_voice_log(VoiceLogEntry {
            transcript: intent.transcript,
            action,
            timestamp: Utc::now(),
        })
        .await;
    let fresh = state
        .spotify
        .currently_playing()
        .await
        .map_err(playback_error)?;
    state.hub.force_publish(fresh).await?;
    let document = state.hub.current_document(RenderParams::default()).await?;
    Ok(document.as_ref().clone())
}

/// Restores a real document if a voice request never finishes.
///
/// `publish_thinking` only rewrites the cached documents; `published` is left
/// alone, so the poll loop sees no meaningful change and never republishes. A
/// playing box (steady progress is not a change) or a paused one would sit on
/// `thinking` indefinitely.
struct ThinkingGuard {
    hub: Arc<StateHub>,
    settled: bool,
}

impl ThinkingGuard {
    fn new(hub: Arc<StateHub>) -> Self {
        Self {
            hub,
            settled: false,
        }
    }

    fn settle(&mut self) {
        self.settled = true;
    }
}

impl Drop for ThinkingGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let hub = Arc::clone(&self.hub);
        // Drop cannot await, so the recovery has to outlive this frame.
        // `try_current` keeps this panic-free if ever dropped off-runtime.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tracing::warn!("voice request ended before completing; restoring playback state");
                recover_document(&hub).await;
            });
        }
    }
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
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        body::{Body, BodyDataStream, to_bytes},
        extract::Request as AxumRequest,
        http::{Request, StatusCode, header},
        response::IntoResponse,
        routing::any,
    };
    use futures::StreamExt;
    use serde_json::Value;
    use serde_json::json;
    use tokio::sync::{Mutex as TokioMutex, Notify};
    use tower::ServiceExt;

    use crate::{
        routes,
        spotify::{SpotifyClient, SpotifyConfig},
        state::StateHub,
    };

    use super::*;

    enum ModelResult {
        Pause,
        Fail,
    }

    struct GatedModel {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        result: ModelResult,
    }

    impl VoiceModel for GatedModel {
        fn command<'a>(
            &'a self,
            samples: &'a [i16],
            rate: u32,
            _context: PlaybackContext,
        ) -> VoiceFuture<'a> {
            Box::pin(async move {
                assert_eq!(rate, 16_000);
                assert_eq!(samples, [120, -120]);
                self.entered.notify_one();
                self.release.notified().await;
                match self.result {
                    ModelResult::Pause => Ok(VoiceIntent {
                        transcript: "pause".to_string(),
                        tool: ToolCall::Pause,
                    }),
                    ModelResult::Fail => Err(AppError::Voice("model timeout".to_string())),
                }
            })
        }
    }

    #[tokio::test]
    async fn cancelled_request_still_restores_a_non_thinking_document() {
        let hub = Arc::new(StateHub::new());
        // A playing track: steady progress is not a meaningful change, so the
        // poll loop would never republish and rescue this on its own.
        hub.force_publish(PlaybackObservation {
            observed_at: Utc::now(),
            track_id: Some("track-a".to_string()),
            track: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            art_url: None,
            is_playing: true,
            progress_ms: 1_000,
            duration_ms: 300_000,
        })
        .await
        .expect("seed playback");

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let app = routes::router(
            SpotifyClient::new(SpotifyConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                redirect_uri: "http://localhost/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            }),
            "device-token".to_string(),
            hub.clone(),
            Arc::new(GatedModel {
                entered: entered.clone(),
                release,
                result: ModelResult::Pause,
            }),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/voice?rate=16000&bits=16&ch=1")
            .header(header::CONTENT_TYPE, "audio/pcm")
            .header(header::AUTHORIZATION, "Bearer device-token")
            .body(Body::from(vec![120, 0, 136, 255]))
            .expect("request");
        let call = tokio::spawn(async move { app.oneshot(request).await });

        // `thinking` is published and the model is in flight.
        entered.notified().await;
        assert!(matches!(
            hub.current_document(RenderParams::default())
                .await
                .expect("thinking document")
                .state,
            PlaybackState::Thinking
        ));

        // The caller goes away: flaky wifi, or a client timeout shorter than the
        // model deadline.
        call.abort();
        let _ = call.await;

        // The guard's recovery runs on its own task, so give it a bounded chance.
        let mut state = PlaybackState::Thinking;
        for _ in 0..200 {
            state = hub
                .current_document(RenderParams::default())
                .await
                .expect("document")
                .state
                .clone();
            if !matches!(state, PlaybackState::Thinking) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            matches!(state, PlaybackState::Playing),
            "a dropped request must not leave the box stuck: {state:?}"
        );
    }

    #[test]
    fn wav_and_pcm_parsers_never_panic_on_arbitrary_input() {
        // Deterministic xorshift. A hand-rolled binary parser is exactly where a
        // slice or arithmetic panic hides, and this endpoint is reachable by any
        // holder of the device token.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let valid = wav_fixture(16_000, 1, 1, 16, &[120, -120, 32_767, -32_768]);

        for iteration in 0..20_000 {
            let mut bytes = if iteration % 3 == 0 {
                valid.clone()
            } else {
                let length = (next() % 96) as usize;
                (0..length).map(|_| (next() & 0xff) as u8).collect()
            };
            if !bytes.is_empty() {
                for _ in 0..1 + (next() % 4) {
                    let index = (next() as usize) % bytes.len();
                    bytes[index] = (next() & 0xff) as u8;
                }
            }
            let _ = decode_wav(&bytes);

            let mut query = HashMap::new();
            query.insert("rate".to_string(), (next() % 200_000).to_string());
            query.insert("bits".to_string(), (next() % 64).to_string());
            query.insert("ch".to_string(), (next() % 8).to_string());
            let _ = decode_raw_pcm(&query, &bytes);
        }
    }

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

    #[tokio::test]
    async fn every_tool_dispatches_once_and_uses_a_stable_action_string() {
        let requests = Arc::new(TokioMutex::new(Vec::new()));
        let observed = requests.clone();
        let app = Router::new().fallback(any(move |request: AxumRequest| {
            let observed = observed.clone();
            async move {
                let method = request.method().to_string();
                let uri = request.uri().to_string();
                observed.lock().await.push((method, uri.clone()));
                if uri.starts_with("/v1/search?") {
                    Json(json!({ "tracks": { "items": [{ "id": "resolved-id" }] } }))
                        .into_response()
                } else {
                    StatusCode::NO_CONTENT.into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind dispatch mock");
        let address = listener.local_addr().expect("dispatch mock address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("dispatch mock");
        });
        let spotify = SpotifyClient::with_test_api_url(
            SpotifyConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                redirect_uri: "http://localhost/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            },
            format!("http://{address}/v1"),
        );
        spotify.authorize_for_test().await;

        let tools = [
            ToolCall::Play,
            ToolCall::Pause,
            ToolCall::Next,
            ToolCall::Previous,
            ToolCall::SearchAndPlay {
                query: "mellow".to_string(),
            },
            ToolCall::QueueSearch {
                query: "focus".to_string(),
            },
            ToolCall::SetVolume { percent: 55 },
            ToolCall::NowPlaying,
        ];
        let mut actions = Vec::new();
        for tool in &tools {
            actions.push(dispatch_tool(&spotify, tool).await.expect("dispatch"));
        }
        assert_eq!(
            actions,
            [
                "spotify:play",
                "spotify:pause",
                "spotify:next",
                "spotify:previous",
                "spotify:play:track:resolved-id",
                "queue:search:focus",
                "spotify:volume:55",
                "query:now_playing",
            ]
        );
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 9);
        assert_eq!(
            requests
                .iter()
                .filter(|(_, uri)| uri == "/v1/me/player/play")
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(_, uri)| uri == "/v1/me/player/pause")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(_, uri)| uri.starts_with("/v1/search?"))
                .count(),
            2
        );
        assert!(requests.iter().any(|(_, uri)| {
            uri.starts_with("/v1/me/player/queue?uri=spotify%3Atrack%3Aresolved-id")
        }));
    }

    #[tokio::test]
    async fn wav_pause_e2e_emits_thinking_then_matching_paused_doc() {
        let pause_calls = Arc::new(AtomicUsize::new(0));
        let spotify = mock_spotify(pause_calls.clone(), false).await;
        let hub = Arc::new(StateHub::new());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let app = routes::router(
            spotify,
            "device-token".to_string(),
            hub,
            Arc::new(GatedModel {
                entered: entered.clone(),
                release: release.clone(),
                result: ModelResult::Pause,
            }),
        );
        let mut events = open_state_stream(&app).await;
        assert_eq!(next_state(&mut events).await["state"], "idle");

        let request = voice_request(wav_fixture(16_000, 1, 1, 16, &[120, -120]));
        let voice_app = app.clone();
        let request_task =
            tokio::spawn(async move { voice_app.oneshot(request).await.expect("voice response") });
        entered.notified().await;
        let thinking = next_state(&mut events).await;
        assert_eq!(thinking["state"], "thinking");

        release.notify_one();
        let response = request_task.await.expect("voice task");
        assert_eq!(response.status(), StatusCode::OK);
        let returned: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("voice body"),
        )
        .expect("voice document");
        let published = next_state(&mut events).await;

        assert_eq!(pause_calls.load(Ordering::SeqCst), 1);
        assert_eq!(returned["state"], "paused");
        assert_eq!(returned, published);
        assert_eq!(returned["voice_log"][0]["transcript"], "pause");
        assert_eq!(returned["voice_log"][0]["action"], "spotify:pause");
    }

    #[tokio::test]
    async fn model_failure_after_thinking_emits_corrective_document() {
        let spotify = mock_spotify(Arc::new(AtomicUsize::new(0)), false).await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let app = routes::router(
            spotify,
            "device-token".to_string(),
            Arc::new(StateHub::new()),
            Arc::new(GatedModel {
                entered: entered.clone(),
                release: release.clone(),
                result: ModelResult::Fail,
            }),
        );
        let mut events = open_state_stream(&app).await;
        assert_eq!(next_state(&mut events).await["state"], "idle");

        let voice_app = app.clone();
        let task = tokio::spawn(async move {
            voice_app
                .oneshot(voice_request(wav_fixture(16_000, 1, 1, 16, &[120, -120])))
                .await
                .expect("voice response")
        });
        entered.notified().await;
        assert_eq!(next_state(&mut events).await["state"], "thinking");
        release.notify_one();

        let response = task.await.expect("voice task");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), 1024)
                .await
                .expect("error body"),
        )
        .expect("JSON error");
        assert_eq!(body, json!({ "error": "model timeout" }));
        assert_ne!(next_state(&mut events).await["state"], "thinking");
    }

    #[tokio::test]
    async fn spotify_failure_after_thinking_emits_corrective_document() {
        let spotify = mock_spotify(Arc::new(AtomicUsize::new(0)), true).await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let app = routes::router(
            spotify,
            "device-token".to_string(),
            Arc::new(StateHub::new()),
            Arc::new(GatedModel {
                entered: entered.clone(),
                release: release.clone(),
                result: ModelResult::Pause,
            }),
        );
        let mut events = open_state_stream(&app).await;
        let _initial = next_state(&mut events).await;

        let voice_app = app.clone();
        let task = tokio::spawn(async move {
            voice_app
                .oneshot(voice_request(wav_fixture(16_000, 1, 1, 16, &[120, -120])))
                .await
                .expect("voice response")
        });
        entered.notified().await;
        assert_eq!(next_state(&mut events).await["state"], "thinking");
        release.notify_one();

        let response = task.await.expect("voice task");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_ne!(next_state(&mut events).await["state"], "thinking");
    }

    async fn mock_spotify(pause_calls: Arc<AtomicUsize>, fail_pause: bool) -> SpotifyClient {
        let app = Router::new().fallback(any(move |request: AxumRequest| {
            let pause_calls = pause_calls.clone();
            async move {
                match (request.method().as_str(), request.uri().path()) {
                    ("PUT", "/v1/me/player/pause") => {
                        pause_calls.fetch_add(1, Ordering::SeqCst);
                        if fail_pause {
                            StatusCode::SERVICE_UNAVAILABLE.into_response()
                        } else {
                            StatusCode::NO_CONTENT.into_response()
                        }
                    }
                    ("GET", "/v1/me/player/currently-playing") => Json(json!({
                        "is_playing": false,
                        "progress_ms": 42,
                        "item": {
                            "id": "known-track",
                            "name": "Known Track",
                            "duration_ms": 120000,
                            "artists": [{ "name": "Known Artist" }],
                            "album": { "name": "Known Album", "images": [] }
                        }
                    }))
                    .into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Spotify mock");
        let address = listener.local_addr().expect("Spotify mock address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("Spotify mock");
        });
        let spotify = SpotifyClient::with_test_api_url(
            SpotifyConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                redirect_uri: "http://localhost/callback".to_string(),
                token_store_path: PathBuf::from("unused"),
            },
            format!("http://{address}/v1"),
        );
        spotify.authorize_for_test().await;
        spotify
    }

    async fn open_state_stream(app: &Router) -> BodyDataStream {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/state")
                    .header(header::AUTHORIZATION, "Bearer device-token")
                    .body(Body::empty())
                    .expect("state request"),
            )
            .await
            .expect("state response");
        assert_eq!(response.status(), StatusCode::OK);
        response.into_body().into_data_stream()
    }

    async fn next_state(events: &mut BodyDataStream) -> Value {
        let chunk = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .expect("SSE event timeout")
            .expect("SSE stream ended")
            .expect("SSE chunk");
        let event = std::str::from_utf8(&chunk).expect("UTF-8 SSE");
        let data = event
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("SSE data line");
        serde_json::from_str(data).expect("SSE JSON")
    }

    fn voice_request(wav: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/voice")
            .header(header::AUTHORIZATION, "Bearer device-token")
            .header(header::CONTENT_TYPE, "audio/wav")
            .body(Body::from(wav))
            .expect("voice request")
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
