//! Persistent GPT Realtime session management and audio preparation.

use std::{fmt, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use crate::{error::AppError, render::PlaybackState};

const REALTIME_SAMPLE_RATE: u32 = 24_000;
const AUDIO_CHUNK_SAMPLES: usize = 4_800;
const COMMAND_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_REALTIME_ENDPOINT: &str = "wss://api.openai.com/v1/realtime";
const TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";
const MUSE_VOICE: &str = "marin";

/// Session instructions for Muse.
///
/// The client listens continuously and forwards every utterance, so Muse is
/// also the gate: speech that was not aimed at it must resolve to the
/// no-op `now_playing` tool rather than changing playback.
const MUSE_PERSONA: &str = "\
You are Muse, the voice of a small music box sitting on a shelf. You control \
one Spotify account through the tools you are given.

You hear every utterance in the room, not just commands aimed at you. Only act \
when someone is plainly talking to you or plainly asking for music. People \
usually address you as Muse, as in \"hey Muse, play something else\". If the \
speech is background conversation, is not about music, is empty, or you cannot \
tell what was wanted, call now_playing, which changes nothing.

When a request is vague, decide for it rather than asking. \"Play something \
calm\", \"put on something for focus\", \"I want the sad version\" are all \
answerable: invent a concrete search query that fits the mood and use \
search_and_play. Taste is yours to exercise. Never ask a clarifying question, \
because there is no way to hear your answer.

Resolve references against the current playback context, so \"this track\" or \
\"its acoustic version\" mean the track playing now. Choose exactly one tool.";

type RealtimeSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct RealtimeManager {
    api_key: Arc<str>,
    model: Arc<str>,
    endpoint: Arc<str>,
    session: Mutex<Option<RealtimeSocket>>,
    command_deadline: Duration,
}

impl fmt::Debug for RealtimeManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealtimeManager")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl RealtimeManager {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_endpoint(api_key, model, DEFAULT_REALTIME_ENDPOINT)
    }

    fn with_endpoint(
        api_key: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            api_key: Arc::from(api_key.into()),
            model: Arc::from(model.into()),
            endpoint: Arc::from(endpoint.into()),
            session: Mutex::new(None),
            command_deadline: COMMAND_DEADLINE,
        }
    }

    /// Shorten the command deadline. Tests use this instead of pausing Tokio
    /// time, because auto-advancing virtual time next to real socket I/O fires
    /// the deadline while the runtime is merely waiting on the network.
    #[cfg(test)]
    fn with_deadline(mut self, deadline: Duration) -> Self {
        self.command_deadline = deadline;
        self
    }

    #[cfg(test)]
    async fn session_is_open(&self) -> bool {
        self.session.lock().await.is_some()
    }

    pub async fn command(
        &self,
        pcm_mono: &[i16],
        input_rate: u32,
        context: PlaybackContext,
    ) -> Result<VoiceIntent, AppError> {
        if pcm_mono.is_empty() {
            return Err(AppError::Voice("audio input must not be empty".to_string()));
        }
        let audio = resample_to_24khz(pcm_mono, input_rate)?;
        let mut session = self.session.lock().await;
        // The deadline covers connect and the audio sends too, not just the
        // wait for a reply: a peer that accepts and never answers would
        // otherwise hang here forever holding the session lock, wedging every
        // later voice command.
        let exchange = async {
            if session.is_none() {
                *session = Some(self.connect().await?);
            }
            match session.as_mut() {
                Some(socket) => Self::run_command(socket, &audio, &context).await,
                None => Err(AppError::Voice(
                    "realtime session was not available".to_string(),
                )),
            }
        };
        let result = match tokio::time::timeout(self.command_deadline, exchange).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Voice("model timeout".to_string())),
        };
        if let Err(error) = &result {
            // Connect failures reach this too. They previously returned through
            // `?` above, so a bad key or an unreachable endpoint produced no log
            // line at all.
            tracing::warn!(%error, "realtime command failed; resetting session");
            *session = None;
        }
        result
    }

    /// Ask Muse to say one line about what it just did.
    ///
    /// Deliberately a second exchange rather than part of the command: the
    /// command response is forced to a tool call, and this runs only after the
    /// action has really succeeded, so Muse never narrates something that then
    /// failed.
    pub async fn say(&self, situation: &str) -> Result<Speech, AppError> {
        let mut session = self.session.lock().await;
        let exchange = async {
            if session.is_none() {
                *session = Some(self.connect().await?);
            }
            let Some(socket) = session.as_mut() else {
                return Err(AppError::Voice(
                    "realtime session was not available".to_string(),
                ));
            };
            send_json(
                socket,
                &json!({
                    "type": "response.create",
                    "response": {
                        "output_modalities": ["audio"],
                        "tool_choice": "none",
                        "instructions": format!(
                            "Say one short spoken line, at most twelve words, confirming this \
                             with personality and no emoji or markup. Do not ask a question. \
                             What happened: {situation}"
                        )
                    }
                }),
            )
            .await?;
            Self::receive_speech(socket).await
        };
        let result = match tokio::time::timeout(self.command_deadline, exchange).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Voice("model timeout".to_string())),
        };
        if let Err(error) = &result {
            tracing::warn!(%error, "realtime speech failed; resetting session");
            *session = None;
        }
        result
    }

    async fn receive_speech(socket: &mut RealtimeSocket) -> Result<Speech, AppError> {
        let mut audio = Vec::<u8>::new();
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| AppError::Voice("realtime connection dropped".to_string()))?;
            match message {
                Ok(Message::Text(text)) => {
                    let event: Value = serde_json::from_str(&text).map_err(|_| {
                        AppError::Voice("realtime service returned invalid JSON".to_string())
                    })?;
                    match event.get("type").and_then(Value::as_str) {
                        Some("response.output_audio.delta") => {
                            let delta = required_string(&event, "delta")?;
                            let mut chunk =
                                general_purpose::STANDARD.decode(delta).map_err(|_| {
                                    AppError::Voice(
                                        "realtime audio delta was not base64".to_string(),
                                    )
                                })?;
                            audio.append(&mut chunk);
                        }
                        Some("response.done") => {
                            if audio.is_empty() {
                                return Err(AppError::Voice(
                                    "realtime response carried no audio".to_string(),
                                ));
                            }
                            return Ok(Speech {
                                format: "pcm16".to_string(),
                                rate: REALTIME_SAMPLE_RATE,
                                audio: general_purpose::STANDARD.encode(&audio),
                            });
                        }
                        Some("error") => {
                            let message = event
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown realtime service error");
                            return Err(AppError::Voice(format!(
                                "realtime service error: {message}"
                            )));
                        }
                        _ => {}
                    }
                }
                Ok(Message::Ping(payload)) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|_| AppError::Voice("realtime connection dropped".to_string()))?;
                }
                Ok(Message::Close(_)) | Err(_) => {
                    return Err(AppError::Voice("realtime connection dropped".to_string()));
                }
                Ok(Message::Binary(_)) => {
                    return Err(AppError::Voice(
                        "realtime service returned an unexpected binary event".to_string(),
                    ));
                }
                Ok(Message::Pong(_) | Message::Frame(_)) => {}
            }
        }
    }

    /// Interpret an already-transcribed command.
    ///
    /// A browser has speech recognition of its own, so shipping PCM only to have
    /// it transcribed again costs latency, money, and a whole class of audio
    /// bugs. Hardware still uses [`Self::command`], which takes audio.
    pub async fn command_text(
        &self,
        transcript: &str,
        context: PlaybackContext,
    ) -> Result<VoiceIntent, AppError> {
        if transcript.trim().is_empty() {
            return Err(AppError::Voice("transcript must not be empty".to_string()));
        }
        let mut session = self.session.lock().await;
        let exchange = async {
            if session.is_none() {
                *session = Some(self.connect().await?);
            }
            let Some(socket) = session.as_mut() else {
                return Err(AppError::Voice(
                    "realtime session was not available".to_string(),
                ));
            };
            send_json(
                socket,
                &json!({
                    "type": "conversation.item.create",
                    "item": {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": transcript }]
                    }
                }),
            )
            .await?;
            let context_json = serde_json::to_string(&context)
                .map_err(|_| AppError::Voice("failed to serialize playback context".to_string()))?;
            send_json(
                socket,
                &json!({
                    "type": "response.create",
                    "response": {
                        "output_modalities": ["text"],
                        "tool_choice": "required",
                        "instructions": format!(
                            "Choose exactly one tool for this request. If it was not aimed at \
                             Muse and is not a music request, choose now_playing so nothing \
                             changes. Current playback context: {context_json}"
                        )
                    }
                }),
            )
            .await?;
            // The transcript came from the caller, so there is no transcription
            // event to wait for; the tool call is the whole answer.
            Self::receive_tool(socket, transcript).await
        };
        let result = match tokio::time::timeout(self.command_deadline, exchange).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Voice("model timeout".to_string())),
        };
        if let Err(error) = &result {
            tracing::warn!(%error, "realtime text command failed; resetting session");
            *session = None;
        }
        result
    }

    async fn receive_tool(
        socket: &mut RealtimeSocket,
        transcript: &str,
    ) -> Result<VoiceIntent, AppError> {
        let mut progress = CommandProgress::default();
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| AppError::Voice("realtime connection dropped".to_string()))?;
            match message {
                Ok(Message::Text(text)) => {
                    progress.consume(&text)?;
                    if let Some(tool) = progress.tool.as_ref() {
                        return Ok(VoiceIntent {
                            transcript: transcript.to_string(),
                            tool: tool.clone(),
                        });
                    }
                    if progress.response_done {
                        return Err(AppError::Voice(
                            "model returned no Spotify tool call".to_string(),
                        ));
                    }
                }
                Ok(Message::Ping(payload)) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|_| AppError::Voice("realtime connection dropped".to_string()))?;
                }
                Ok(Message::Close(_)) | Err(_) => {
                    return Err(AppError::Voice("realtime connection dropped".to_string()));
                }
                Ok(Message::Binary(_)) => {
                    return Err(AppError::Voice(
                        "realtime service returned an unexpected binary event".to_string(),
                    ));
                }
                Ok(Message::Pong(_) | Message::Frame(_)) => {}
            }
        }
    }

    async fn connect(&self) -> Result<RealtimeSocket, AppError> {
        let url = format!(
            "{}?model={}",
            self.endpoint,
            urlencoding::encode(&self.model)
        );
        let mut request = url
            .into_client_request()
            .map_err(|_| AppError::Voice("invalid realtime endpoint configuration".to_string()))?;
        let authorization = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|_| AppError::Voice("invalid OpenAI API key format".to_string()))?;
        request.headers_mut().insert(AUTHORIZATION, authorization);
        let (mut socket, _) = connect_async(request).await.map_err(|error| {
            AppError::Voice(format!("failed to connect to realtime service: {error}"))
        })?;
        send_json(&mut socket, &session_update()).await?;
        Ok(socket)
    }

    async fn run_command(
        socket: &mut RealtimeSocket,
        audio: &[i16],
        context: &PlaybackContext,
    ) -> Result<VoiceIntent, AppError> {
        for chunk in audio.chunks(AUDIO_CHUNK_SAMPLES) {
            let mut bytes = Vec::with_capacity(chunk.len() * 2);
            for sample in chunk {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            send_json(
                socket,
                &json!({
                    "type": "input_audio_buffer.append",
                    "audio": general_purpose::STANDARD.encode(bytes)
                }),
            )
            .await?;
        }
        send_json(socket, &json!({ "type": "input_audio_buffer.commit" })).await?;
        let context_json = serde_json::to_string(context)
            .map_err(|_| AppError::Voice("failed to serialize playback context".to_string()))?;
        send_json(
            socket,
            &json!({
                "type": "response.create",
                "response": {
                    "output_modalities": ["text"],
                    "tool_choice": "required",
                    "instructions": format!(
                        "Choose exactly one tool. If this speech was not aimed at Muse and is \
                         not a music request, choose now_playing so nothing changes. \
                         Current playback context: {context_json}"
                    )
                }
            }),
        )
        .await?;

        Self::receive_intent(socket).await
    }

    async fn receive_intent(socket: &mut RealtimeSocket) -> Result<VoiceIntent, AppError> {
        let mut progress = CommandProgress::default();
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| AppError::Voice("realtime connection dropped".to_string()))?;
            match message {
                Ok(Message::Text(text)) => {
                    progress.consume(&text)?;
                    if let (Some(transcript), Some(tool)) =
                        (progress.transcript.as_ref(), progress.tool.as_ref())
                    {
                        return Ok(VoiceIntent {
                            transcript: transcript.clone(),
                            tool: tool.clone(),
                        });
                    }
                    if (progress.response_done || progress.text_done) && progress.tool.is_none() {
                        return Err(AppError::Voice(
                            "model returned no Spotify tool call".to_string(),
                        ));
                    }
                }
                Ok(Message::Ping(payload)) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|_| AppError::Voice("realtime connection dropped".to_string()))?;
                }
                Ok(Message::Close(_)) | Err(_) => {
                    return Err(AppError::Voice("realtime connection dropped".to_string()));
                }
                Ok(Message::Binary(_)) => {
                    return Err(AppError::Voice(
                        "realtime service returned an unexpected binary event".to_string(),
                    ));
                }
                Ok(Message::Pong(_) | Message::Frame(_)) => {}
            }
        }
    }
}

#[derive(Default)]
struct CommandProgress {
    committed_item_id: Option<String>,
    pending_transcript: Option<(String, String)>,
    transcript: Option<String>,
    tool: Option<ToolCall>,
    response_done: bool,
    text_done: bool,
}

impl CommandProgress {
    fn consume(&mut self, text: &str) -> Result<(), AppError> {
        let event: Value = serde_json::from_str(text)
            .map_err(|_| AppError::Voice("realtime service returned invalid JSON".to_string()))?;
        match event.get("type").and_then(Value::as_str) {
            Some("input_audio_buffer.committed") => {
                if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
                    self.committed_item_id = Some(item_id.to_string());
                    if self
                        .pending_transcript
                        .as_ref()
                        .is_some_and(|(pending_id, _)| pending_id == item_id)
                    {
                        self.transcript = self
                            .pending_transcript
                            .take()
                            .map(|(_, transcript)| transcript);
                    }
                }
            }
            Some("conversation.item.input_audio_transcription.completed") => {
                let item_id = required_string(&event, "item_id")?;
                let transcript = required_string(&event, "transcript")?;
                if self.committed_item_id.as_deref() == Some(item_id) {
                    self.transcript = Some(transcript.to_string());
                } else {
                    self.pending_transcript = Some((item_id.to_string(), transcript.to_string()));
                }
            }
            Some("response.function_call_arguments.done") => {
                let name = required_string(&event, "name")?;
                let arguments = required_string(&event, "arguments")?;
                self.tool = Some(parse_realtime_tool_call(name, arguments)?);
            }
            Some("response.done") => {
                self.response_done = true;
                if self.tool.is_none() {
                    self.tool = tool_from_response_done(&event)?;
                }
            }
            Some("response.output_text.done") => {
                self.text_done = true;
            }
            Some("error") => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown realtime service error");
                return Err(AppError::Voice(format!(
                    "realtime service error: {message}"
                )));
            }
            _ => {}
        }
        Ok(())
    }
}

fn required_string<'a>(event: &'a Value, field: &str) -> Result<&'a str, AppError> {
    event
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Voice(format!("realtime event is missing string field: {field}")))
}

fn tool_from_response_done(event: &Value) -> Result<Option<ToolCall>, AppError> {
    let Some(output) = event.pointer("/response/output").and_then(Value::as_array) else {
        return Ok(None);
    };
    for item in output {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let name = required_string(item, "name")?;
            let arguments = required_string(item, "arguments")?;
            return parse_realtime_tool_call(name, arguments).map(Some);
        }
    }
    Ok(None)
}

fn session_update() -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "output_modalities": ["text"],
            "audio": {
                "output": {
                    "format": {
                        "type": "audio/pcm",
                        "rate": REALTIME_SAMPLE_RATE
                    },
                    "voice": MUSE_VOICE
                },
                "input": {
                    "format": {
                        "type": "audio/pcm",
                        "rate": REALTIME_SAMPLE_RATE
                    },
                    "transcription": {
                        "model": TRANSCRIPTION_MODEL
                    },
                    "turn_detection": null
                }
            },
            "tools": spotify_tool_schema(),
            "tool_choice": "auto",
            "instructions": MUSE_PERSONA
        }
    })
}

async fn send_json(socket: &mut RealtimeSocket, value: &Value) -> Result<(), AppError> {
    let text = serde_json::to_string(value)
        .map_err(|_| AppError::Voice("failed to serialize realtime event".to_string()))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| AppError::Voice("realtime connection dropped".to_string()))
}

#[derive(Clone, Debug, Serialize)]
pub struct PlaybackContext {
    pub track: Option<String>,
    pub artist: Option<String>,
    pub state: PlaybackState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCall {
    Play,
    Pause,
    Next,
    Previous,
    /// Play from the listener's own library, matched semantically.
    PlayFromTaste {
        description: String,
    },
    SearchAndPlay {
        query: String,
    },
    QueueSearch {
        query: String,
    },
    SetVolume {
        percent: u8,
    },
    NowPlaying,
}

/// Spoken reply from Muse: PCM16 mono, little-endian, at the rate in `rate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speech {
    pub format: String,
    pub rate: u32,
    /// Base64 of the raw samples.
    pub audio: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceIntent {
    pub transcript: String,
    pub tool: ToolCall,
}

/// Resample mono PCM16 to the 24 kHz format required by Realtime.
pub fn resample_to_24khz(input: &[i16], input_rate: u32) -> Result<Vec<i16>, AppError> {
    if input_rate == 0 {
        return Err(AppError::Voice(
            "input sample rate must be greater than zero".to_string(),
        ));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if input_rate == REALTIME_SAMPLE_RATE {
        return Ok(input.to_vec());
    }

    let input_len = u64::try_from(input.len())
        .map_err(|_| AppError::Voice("input audio is too large".to_string()))?;
    let scaled_len = input_len
        .checked_mul(u64::from(REALTIME_SAMPLE_RATE))
        .ok_or_else(|| AppError::Voice("resampled audio is too large".to_string()))?;
    let output_len = scaled_len.div_ceil(u64::from(input_rate));
    let output_capacity = usize::try_from(output_len)
        .map_err(|_| AppError::Voice("resampled audio is too large".to_string()))?;
    let mut output = Vec::with_capacity(output_capacity);
    for output_index in 0..output_capacity {
        let source_position =
            output_index as f64 * f64::from(input_rate) / f64::from(REALTIME_SAMPLE_RATE);
        let left_index = source_position.floor() as usize;
        let right_index = (left_index + 1).min(input.len() - 1);
        let fraction = source_position - left_index as f64;
        let interpolated = f64::from(input[left_index]) * (1.0 - fraction)
            + f64::from(input[right_index]) * fraction;
        output.push(interpolated.round().clamp(-32_768.0, 32_767.0) as i16);
    }
    Ok(output)
}

/// Return the fixed Spotify function schema sent to every Realtime session.
pub fn spotify_tool_schema() -> Value {
    let empty_parameters = || {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    };
    let simple = |name: &str, description: &str| {
        json!({
            "type": "function",
            "name": name,
            "description": description,
            "parameters": empty_parameters()
        })
    };
    let search = |name: &str, description: &str| {
        json!({
            "type": "function",
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Spotify search query including useful track and artist details.",
                        "minLength": 1
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        })
    };

    json!([
        simple("play", "Resume the current Spotify playback."),
        simple("pause", "Pause the current Spotify playback."),
        simple("next", "Skip to the next Spotify track."),
        simple("previous", "Return to the previous Spotify track."),
        json!({
            "type": "function",
            "name": "play_from_taste",
            "description": "Play something from this listener's own music: their \
                            playlists, saved tracks, top tracks and recent plays, \
                            matched semantically. Prefer this for any request about \
                            mood, vibe, an artist or language they listen to, or \
                            anything phrased as what they like.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What the listener wants, in their own words: \
                                        a mood, an artist, a language, an occasion. \
                                        Not a Spotify search string.",
                        "minLength": 1
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }),
        search(
            "search_and_play",
            "Search Spotify and start playing the best match right now, replacing \
             whatever is playing. This is the default for any request to hear \
             something, including corrections such as wanting a different version \
             of the current track. Use this only when a specific track or artist \
             is named that play_from_taste did not find, or that is plainly not \
             theirs."
        ),
        search(
            "queue_search",
            "Search Spotify and add the best match to the end of the queue, \
             without interrupting the current track. Only for requests that \
             explicitly ask to queue something or play it later or next."
        ),
        json!({
            "type": "function",
            "name": "set_volume",
            "description": "Set Spotify playback volume to an exact percentage.",
            "parameters": {
                "type": "object",
                "properties": {
                    "percent": {
                        "type": "integer",
                        "description": "Volume percentage from 0 through 100.",
                        "minimum": 0,
                        "maximum": 100
                    }
                },
                "required": ["percent"],
                "additionalProperties": false
            }
        }),
        simple(
            "now_playing",
            "Report the current Spotify track, artist, and playback state."
        )
    ])
}

/// Validate and convert Realtime function-call output into a Spotify action.
pub fn parse_realtime_tool_call(name: &str, arguments: &str) -> Result<ToolCall, AppError> {
    match name {
        "play" => parse_empty(arguments, ToolCall::Play),
        "pause" => parse_empty(arguments, ToolCall::Pause),
        "next" => parse_empty(arguments, ToolCall::Next),
        "previous" => parse_empty(arguments, ToolCall::Previous),
        "now_playing" => parse_empty(arguments, ToolCall::NowPlaying),
        "play_from_taste" => {
            parse_query(arguments).map(|description| ToolCall::PlayFromTaste { description })
        }
        "search_and_play" => parse_query(arguments).map(|query| ToolCall::SearchAndPlay { query }),
        "queue_search" => parse_query(arguments).map(|query| ToolCall::QueueSearch { query }),
        "set_volume" => {
            let args: VolumeArguments = parse_arguments(name, arguments)?;
            if args.percent > 100 {
                return Err(AppError::Voice(
                    "set_volume percent must be between 0 and 100".to_string(),
                ));
            }
            Ok(ToolCall::SetVolume {
                percent: args.percent as u8,
            })
        }
        _ => Err(AppError::Voice(format!(
            "model requested unknown tool: {name}"
        ))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArguments {
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeArguments {
    percent: u16,
}

fn parse_empty(arguments: &str, call: ToolCall) -> Result<ToolCall, AppError> {
    let _: EmptyArguments = parse_arguments("parameterless tool", arguments)?;
    Ok(call)
}

fn parse_query(arguments: &str) -> Result<String, AppError> {
    let args: QueryArguments = parse_arguments("search tool", arguments)?;
    let query = args.query.trim();
    if query.is_empty() {
        return Err(AppError::Voice(
            "search tool query must not be empty".to_string(),
        ));
    }
    Ok(query.to_string())
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(
    name: &str,
    arguments: &str,
) -> Result<T, AppError> {
    serde_json::from_str(arguments)
        .map_err(|error| AppError::Voice(format!("invalid arguments for {name}: {error}")))
}

#[cfg(test)]
mod tests {
    use std::{
        f64::consts::TAU,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;
    use base64::engine::general_purpose;
    use futures::{SinkExt, StreamExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{
        WebSocketStream, accept_async, accept_hdr_async,
        tungstenite::{
            Message,
            handshake::server::{ErrorResponse, Request, Response},
        },
    };

    #[test]
    fn resampler_has_exact_lengths_for_supported_rates() {
        for input_rate in [16_000, 44_100, 48_000] {
            let input = vec![0; input_rate];
            assert_eq!(
                resample_to_24khz(&input, input_rate as u32).unwrap().len(),
                24_000
            );
        }
    }

    #[test]
    fn resampler_preserves_test_tone_frequency() {
        const TONE_HZ: f64 = 440.0;
        for input_rate in [16_000, 44_100, 48_000] {
            let input: Vec<_> = (0..input_rate)
                .map(|index| {
                    let phase = TAU * TONE_HZ * index as f64 / input_rate as f64;
                    (phase.sin() * 20_000.0).round() as i16
                })
                .collect();
            let output = resample_to_24khz(&input, input_rate as u32).unwrap();
            let crossings = output
                .windows(2)
                .filter(|samples| samples[0] <= 0 && samples[1] > 0)
                .count();
            let error = (crossings as f64 - TONE_HZ).abs() / TONE_HZ;
            assert!(error <= 0.02, "{input_rate} Hz input error was {error}");
        }
    }

    #[test]
    /// Changing a tool name, description or parameter changes what the model is
    /// told it can do, so the schema is pinned. To update deliberately, print
    /// `serde_json::to_string_pretty(&spotify_tool_schema())` into the snapshot
    /// file and say why in the commit.
    fn tool_schema_matches_committed_snapshot() {
        let expected: Value = serde_json::from_str(include_str!(
            "../testing/snapshots/issue-6-tool-schema.json"
        ))
        .unwrap();
        assert_eq!(spotify_tool_schema(), expected);
    }

    #[test]
    fn tool_call_validation_rejects_bad_names_and_arguments() {
        assert_eq!(
            parse_realtime_tool_call("play", "{}").unwrap(),
            ToolCall::Play
        );
        assert_eq!(
            parse_realtime_tool_call("search_and_play", r#"{"query":"  Halo Beyoncé  "}"#).unwrap(),
            ToolCall::SearchAndPlay {
                query: "Halo Beyoncé".to_string()
            }
        );
        assert_eq!(
            parse_realtime_tool_call("set_volume", r#"{"percent":100}"#).unwrap(),
            ToolCall::SetVolume { percent: 100 }
        );
        for (name, args) in [
            ("delete_playlist", "{}"),
            ("play", r#"{"extra":true}"#),
            ("queue_search", r#"{"query":"  "}"#),
            ("set_volume", r#"{"percent":101}"#),
            ("set_volume", r#"{"percent":"loud"}"#),
        ] {
            assert!(
                parse_realtime_tool_call(name, args).is_err(),
                "{name} accepted {args}"
            );
        }
    }

    #[tokio::test]
    async fn mock_server_reuses_connection_and_returns_transcript_and_tool() {
        let (listener, endpoint) = mock_listener().await;
        let connections = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let connections = connections.clone();
            async move {
                let mut socket = accept_mock(&listener, &connections).await;
                let update = receive_json(&mut socket).await;
                assert_session_update(&update);

                let first_response = receive_command(&mut socket).await;
                assert!(
                    first_response
                        .pointer("/response/instructions")
                        .and_then(Value::as_str)
                        .unwrap()
                        .contains("Massive Attack")
                );
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "response.function_call_arguments.done",
                        "name": "pause",
                        "arguments": "{}"
                    }),
                )
                .await;
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "conversation.item.input_audio_transcription.completed",
                        "item_id": "input-1",
                        "transcript": "pause this"
                    }),
                )
                .await;
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "input_audio_buffer.committed",
                        "item_id": "input-1"
                    }),
                )
                .await;

                let second_response = receive_command(&mut socket).await;
                assert!(
                    second_response
                        .pointer("/response/instructions")
                        .and_then(Value::as_str)
                        .unwrap()
                        .contains("paused")
                );
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "input_audio_buffer.committed",
                        "item_id": "input-2"
                    }),
                )
                .await;
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "conversation.item.input_audio_transcription.completed",
                        "item_id": "input-2",
                        "transcript": "play halo"
                    }),
                )
                .await;
                send_server_json(
                    &mut socket,
                    json!({
                        "type": "response.done",
                        "response": {
                            "output": [{
                                "type": "function_call",
                                "name": "search_and_play",
                                "arguments": "{\"query\":\"Halo Beyoncé\"}"
                            }]
                        }
                    }),
                )
                .await;
            }
        });
        let manager = RealtimeManager::with_endpoint("test-key", "gpt-realtime-mini", endpoint);
        let audio = vec![123_i16; 9_600];

        let first = manager
            .command(
                &audio,
                48_000,
                context("Teardrop", "Massive Attack", PlaybackState::Playing),
            )
            .await
            .unwrap();
        assert_eq!(
            first,
            VoiceIntent {
                transcript: "pause this".to_string(),
                tool: ToolCall::Pause
            }
        );
        let second = manager
            .command(
                &audio,
                48_000,
                context("Teardrop", "Massive Attack", PlaybackState::Paused),
            )
            .await
            .unwrap();
        assert_eq!(
            second,
            VoiceIntent {
                transcript: "play halo".to_string(),
                tool: ToolCall::SearchAndPlay {
                    query: "Halo Beyoncé".to_string()
                }
            }
        );
        server.await.unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_socket_fails_current_command_and_next_reconnects() {
        let (listener, endpoint) = mock_listener().await;
        let connections = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let connections = connections.clone();
            async move {
                let mut first = accept_mock(&listener, &connections).await;
                receive_json(&mut first).await;
                receive_command(&mut first).await;
                first.close(None).await.unwrap();

                let mut second = accept_mock(&listener, &connections).await;
                assert_session_update(&receive_json(&mut second).await);
                receive_command(&mut second).await;
                send_success(&mut second, "input-2", "resume", "play", "{}").await;
            }
        });
        let manager = RealtimeManager::with_endpoint("test-key", "gpt-realtime-mini", endpoint);
        let audio = vec![123_i16; 1_600];
        let first = manager
            .command(
                &audio,
                16_000,
                context("Song", "Artist", PlaybackState::Paused),
            )
            .await;
        assert!(
            matches!(first, Err(AppError::Voice(message)) if message == "realtime connection dropped")
        );

        let second = manager
            .command(
                &audio,
                16_000,
                context("Song", "Artist", PlaybackState::Paused),
            )
            .await
            .unwrap();
        assert_eq!(second.tool, ToolCall::Play);
        assert_eq!(second.transcript, "resume");
        server.await.unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn timeout_resets_session_and_manager_reconnects() {
        // A short real deadline instead of `start_paused`: paused time
        // auto-advances whenever the runtime goes idle, so waiting on a real
        // socket would fire the deadline spuriously.
        let (listener, endpoint) = mock_listener().await;
        let connections = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let connections = connections.clone();
            async move {
                // Accept, read the command, then never answer.
                let mut first = accept_mock(&listener, &connections).await;
                receive_json(&mut first).await;
                receive_command(&mut first).await;
                let stalled = first;

                let mut second = accept_mock(&listener, &connections).await;
                receive_json(&mut second).await;
                receive_command(&mut second).await;
                send_success(&mut second, "input-2", "resume", "play", "{}").await;
                drop(stalled);
            }
        });
        let manager = RealtimeManager::with_endpoint("test-key", "gpt-realtime-mini", endpoint)
            .with_deadline(Duration::from_millis(250));
        let audio = vec![123_i16; 1_600];

        let timeout = manager
            .command(
                &audio,
                16_000,
                context("Song", "Artist", PlaybackState::Paused),
            )
            .await;
        assert!(matches!(timeout, Err(AppError::Voice(message)) if message == "model timeout"));
        assert!(
            !manager.session_is_open().await,
            "a timed-out session must be discarded"
        );

        let recovered = manager
            .command(
                &audio,
                16_000,
                context("Song", "Artist", PlaybackState::Paused),
            )
            .await
            .unwrap();
        assert_eq!(recovered.tool, ToolCall::Play);
        server.await.unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn diagnostics_never_contain_api_key() {
        // Asserted on the values themselves rather than on captured log output:
        // tracing's per-callsite interest cache is process-global, so a sibling
        // test touching the same `warn!` can silence it here and the assertion
        // flakes with test parallelism.
        let secret = "sk-proj-do-not-log-this-value";
        let (listener, endpoint) = mock_listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            receive_json(&mut socket).await;
            socket.close(None).await.unwrap();
        });
        let manager = RealtimeManager::with_endpoint(secret, "gpt-realtime-mini", endpoint)
            .with_deadline(Duration::from_secs(5));

        let error = manager
            .command(
                &[1; 1_600],
                16_000,
                context("Song", "Artist", PlaybackState::Playing),
            )
            .await
            .expect_err("the server closes without answering");
        server.await.unwrap();

        assert!(!error.to_string().contains(secret), "{error}");
        assert!(!format!("{error:?}").contains(secret));
        assert!(!format!("{manager:?}").contains(secret));
        assert!(
            !manager.session_is_open().await,
            "a failed command must reset the session"
        );
    }

    #[tokio::test]
    async fn stalled_connect_times_out_instead_of_hanging() {
        // Accept the TCP connection and never complete the WebSocket upgrade.
        // The deadline has to cover `connect`, or this wedges the session lock
        // forever and every later voice command blocks behind it.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        let manager = RealtimeManager::with_endpoint("test-key", "gpt-realtime-mini", endpoint)
            .with_deadline(Duration::from_millis(250));

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            manager.command(
                &[1; 1_600],
                16_000,
                context("Song", "Artist", PlaybackState::Playing),
            ),
        )
        .await
        .expect("the command deadline must fire instead of hanging");
        assert!(matches!(outcome, Err(AppError::Voice(message)) if message == "model timeout"));
        assert!(!manager.session_is_open().await);
    }

    #[tokio::test]
    async fn connect_failure_is_reported_without_leaking_the_key() {
        // Nothing is listening: the failure happens inside `connect`, which used
        // to return early and therefore never log or reset anything.
        let secret = "sk-proj-do-not-log-this-value";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        drop(listener);
        let manager = RealtimeManager::with_endpoint(secret, "gpt-realtime-mini", endpoint)
            .with_deadline(Duration::from_secs(5));

        let error = manager
            .command(
                &[1; 1_600],
                16_000,
                context("Song", "Artist", PlaybackState::Playing),
            )
            .await
            .expect_err("connect must fail");
        let message = error.to_string();
        assert!(
            message.contains("failed to connect to realtime service:"),
            "the cause must be preserved for operators: {message}"
        );
        // The cause is preserved for operators, but never the credential.
        assert!(!message.contains(secret), "{message}");
        assert!(!manager.session_is_open().await);
    }

    async fn mock_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/v1/realtime", listener.local_addr().unwrap());
        (listener, endpoint)
    }

    async fn accept_mock(
        listener: &TcpListener,
        connections: &AtomicUsize,
    ) -> WebSocketStream<TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        connections.fetch_add(1, Ordering::SeqCst);
        accept_hdr_async(stream, inspect_handshake).await.unwrap()
    }

    #[allow(clippy::result_large_err)]
    fn inspect_handshake(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-key")
        );
        assert_eq!(request.uri().path(), "/v1/realtime");
        assert_eq!(request.uri().query(), Some("model=gpt-realtime-mini"));
        Ok(response)
    }

    async fn receive_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
        let message = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = message else {
            panic!("expected a text event");
        };
        serde_json::from_str(&text).unwrap()
    }

    async fn receive_command(socket: &mut WebSocketStream<TcpStream>) -> Value {
        let mut event_types = Vec::new();
        loop {
            let event = receive_json(socket).await;
            let event_type = event.get("type").and_then(Value::as_str).unwrap();
            event_types.push(event_type.to_string());
            if event_type == "input_audio_buffer.append" {
                let bytes = general_purpose::STANDARD
                    .decode(event.get("audio").and_then(Value::as_str).unwrap())
                    .unwrap();
                assert!(!bytes.is_empty());
                assert_eq!(bytes.len() % 2, 0);
                assert!(bytes.len() <= AUDIO_CHUNK_SAMPLES * 2);
            }
            if event_type == "response.create" {
                assert!(event_types.len() >= 3);
                assert_eq!(
                    event_types[event_types.len() - 2],
                    "input_audio_buffer.commit"
                );
                assert!(
                    event_types[..event_types.len() - 2]
                        .iter()
                        .all(|kind| kind == "input_audio_buffer.append")
                );
                return event;
            }
        }
    }

    async fn send_server_json(socket: &mut WebSocketStream<TcpStream>, event: Value) {
        socket
            .send(Message::Text(serde_json::to_string(&event).unwrap().into()))
            .await
            .unwrap();
    }

    async fn send_success(
        socket: &mut WebSocketStream<TcpStream>,
        item_id: &str,
        transcript: &str,
        name: &str,
        arguments: &str,
    ) {
        send_server_json(
            socket,
            json!({
                "type": "input_audio_buffer.committed",
                "item_id": item_id
            }),
        )
        .await;
        send_server_json(
            socket,
            json!({
                "type": "conversation.item.input_audio_transcription.completed",
                "item_id": item_id,
                "transcript": transcript
            }),
        )
        .await;
        send_server_json(
            socket,
            json!({
                "type": "response.function_call_arguments.done",
                "name": name,
                "arguments": arguments
            }),
        )
        .await;
    }

    fn assert_session_update(update: &Value) {
        assert_eq!(
            update.get("type").and_then(Value::as_str),
            Some("session.update")
        );
        assert_eq!(
            update.pointer("/session/type").and_then(Value::as_str),
            Some("realtime")
        );
        assert_eq!(
            update.pointer("/session/output_modalities"),
            Some(&json!(["text"]))
        );
        assert_eq!(
            update
                .pointer("/session/audio/input/format/type")
                .and_then(Value::as_str),
            Some("audio/pcm")
        );
        assert_eq!(
            update.pointer("/session/audio/input/format/rate"),
            Some(&json!(24_000))
        );
        assert_eq!(
            update
                .pointer("/session/audio/input/transcription/model")
                .and_then(Value::as_str),
            Some(TRANSCRIPTION_MODEL)
        );
        assert_eq!(
            update.pointer("/session/audio/input/turn_detection"),
            Some(&Value::Null)
        );
        assert_eq!(
            update
                .pointer("/session/tool_choice")
                .and_then(Value::as_str),
            Some("auto")
        );
        assert_eq!(
            update.pointer("/session/tools"),
            Some(&spotify_tool_schema())
        );
    }

    fn context(track: &str, artist: &str, state: PlaybackState) -> PlaybackContext {
        PlaybackContext {
            track: Some(track.to_string()),
            artist: Some(artist.to_string()),
            state,
        }
    }
}
