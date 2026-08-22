//! Turning a transcript into one Spotify action, over plain HTTP.
//!
//! This used to go through the Realtime WebSocket, which was the wrong tool for
//! it: a long-lived socket that has to be reconnected, re-configured and
//! babysat, in exchange for nothing, because the request is text in and a
//! function call out. Chat completions is stateless, so a failure is one request
//! to retry rather than a session to rebuild, and retries are safe because
//! nothing has happened yet when it fails.
//!
//! Speech is the same story: one request, PCM back. The Realtime path stays for
//! hardware, which sends audio and has no speech recognition of its own.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    realtime::{PlaybackContext, Speech, ToolCall, VoiceIntent, parse_realtime_tool_call},
};

const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const SPEECH_URL: &str = "https://api.openai.com/v1/audio/speech";
/// Muse's replies come back as raw PCM16 at this rate, matching what the
/// hardware and the browser already play.
const SPEECH_SAMPLE_RATE: u32 = 24_000;
/// Attempts per request. Transient upstream failures are common enough to be
/// worth absorbing, and rare enough that three is plenty.
const ATTEMPTS: u32 = 3;

pub struct IntentModel {
    http: reqwest::Client,
    api_key: String,
    model: String,
    speech_model: String,
    voice: String,
}

impl IntentModel {
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        speech_model: impl Into<String>,
        voice: impl Into<String>,
    ) -> Self {
        Self {
            http: crate::spotify::http_client(),
            api_key: api_key.into(),
            model: model.into(),
            speech_model: speech_model.into(),
            voice: voice.into(),
        }
    }

    /// Choose exactly one tool for a transcript.
    pub async fn intent(
        &self,
        transcript: &str,
        context: &PlaybackContext,
        instructions: &str,
        tools: &Value,
    ) -> Result<VoiceIntent, AppError> {
        let transcript = transcript.trim();
        if transcript.is_empty() {
            return Err(AppError::Voice("transcript must not be empty".to_string()));
        }
        let context_json = serde_json::to_string(context)
            .map_err(|_| AppError::Voice("failed to serialize playback context".to_string()))?;

        // The Responses API takes tools in the same flat shape the Realtime
        // schema already uses, so there is nothing to translate.
        let body = json!({
            "model": self.model,
            "tool_choice": "required",
            "parallel_tool_calls": false,
            "tools": tools,
            "instructions": format!("{instructions}\n\nCurrent playback context: {context_json}"),
            "input": [
                { "role": "user", "content": transcript }
            ]
        });

        let response = self.post_with_retries(RESPONSES_URL, &body).await?;
        let call = first_tool_call(&response)?;
        let tool = parse_realtime_tool_call(&call.name, &call.arguments)?;
        Ok(VoiceIntent {
            transcript: transcript.to_string(),
            tool,
        })
    }

    /// One short spoken line about what just happened.
    pub async fn speak(&self, situation: &str) -> Result<Speech, AppError> {
        let body = json!({
            "model": self.speech_model,
            "voice": self.voice,
            "input": situation,
            // Raw PCM16 at 24 kHz, so no decoder is needed at either client.
            "response_format": "pcm",
            "instructions": "Warm, dry, unhurried. One line, as if said in passing."
        });

        let mut last = None;
        for attempt in 0..ATTEMPTS {
            match self.speech_attempt(&body).await {
                Ok(speech) => return Ok(speech),
                Err(error) => {
                    last = Some(error);
                    if attempt + 1 < ATTEMPTS {
                        tokio::time::sleep(backoff(attempt)).await;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| AppError::Voice("speech failed".to_string())))
    }

    async fn speech_attempt(&self, body: &Value) -> Result<Speech, AppError> {
        let response = self
            .http
            .post(SPEECH_URL)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| AppError::Voice(format!("speech request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(AppError::Voice(format!(
                "speech request failed with {}",
                response.status()
            )));
        }
        let audio = response
            .bytes()
            .await
            .map_err(|error| AppError::Voice(format!("speech response unreadable: {error}")))?;
        if audio.is_empty() {
            return Err(AppError::Voice("speech response was empty".to_string()));
        }
        Ok(Speech {
            format: "pcm16".to_string(),
            rate: SPEECH_SAMPLE_RATE,
            audio: general_purpose::STANDARD.encode(&audio),
        })
    }

    /// Retry transient failures. Nothing has happened upstream when a request
    /// fails here, so retrying cannot double an action.
    async fn post_with_retries(&self, url: &str, body: &Value) -> Result<Value, AppError> {
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            match self.post_once(url, body).await {
                Ok(value) => return Ok(value),
                Err(RequestError::Permanent(error)) => return Err(error),
                Err(RequestError::Transient(error)) => {
                    tracing::warn!(%error, attempt = attempt + 1, "retrying model request");
                    last = Some(error);
                    if attempt + 1 < ATTEMPTS {
                        tokio::time::sleep(backoff(attempt)).await;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| AppError::Voice("model request failed".to_string())))
    }

    async fn post_once(&self, url: &str, body: &Value) -> Result<Value, RequestError> {
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                // A timeout or a dropped connection is worth another go.
                RequestError::Transient(AppError::Voice(format!("model request failed: {error}")))
            })?;

        let status = response.status();
        if status.is_success() {
            return response.json::<Value>().await.map_err(|error| {
                RequestError::Transient(AppError::Voice(format!(
                    "model response unreadable: {error}"
                )))
            });
        }

        let detail = response.text().await.unwrap_or_default();
        let detail = detail.chars().take(200).collect::<String>();
        // 429 and 5xx are worth retrying; a 400 means the request itself is
        // wrong and will be wrong again.
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            Err(RequestError::Transient(AppError::Voice(format!(
                "model returned {status}: {detail}"
            ))))
        } else {
            Err(RequestError::Permanent(AppError::Voice(format!(
                "model returned {status}: {detail}"
            ))))
        }
    }
}

enum RequestError {
    Transient(AppError),
    Permanent(AppError),
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2_u64.pow(attempt))
}

struct NamedCall {
    name: String,
    arguments: String,
}

/// Pull the tool call out of a Responses `output` array.
///
/// The array can also carry reasoning items, so the call is searched for by
/// type rather than assumed to be first.
fn first_tool_call(response: &Value) -> Result<NamedCall, AppError> {
    #[derive(Deserialize)]
    struct Call {
        name: String,
        arguments: String,
    }

    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Voice("model response had no output".to_string()))?;
    let call = output
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .ok_or_else(|| AppError::Voice("model chose no tool".to_string()))?;
    let call: Call = serde_json::from_value(call.clone())
        .map_err(|error| AppError::Voice(format!("model tool call was malformed: {error}")))?;
    Ok(NamedCall {
        name: call.name,
        arguments: call.arguments,
    })
}

/// Convenience so callers do not have to know the tool shape.
pub fn spotify_tools() -> Value {
    crate::realtime::spotify_tool_schema()
}

/// The tool a request maps to when nothing should change.
pub const NO_OP: ToolCall = ToolCall::NowPlaying;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_schema_is_already_in_the_shape_responses_wants() {
        // Responses takes tools flat, which is what the Realtime schema emits,
        // so there is one source of truth and no translation to drift.
        let tools = spotify_tools();
        let list = tools.as_array().expect("an array");
        assert_eq!(list.len(), 9);
        for tool in list {
            assert_eq!(tool["type"], "function");
            assert!(tool["name"].is_string(), "{tool}");
            assert!(tool["parameters"].is_object(), "{tool}");
        }
    }

    #[test]
    fn a_tool_call_is_found_past_any_reasoning_items() {
        // Reasoning models put a reasoning item in the output first, so the call
        // cannot be assumed to be at index zero.
        let response = json!({
            "output": [
                { "type": "reasoning", "summary": [] },
                { "type": "function_call", "call_id": "call_1",
                  "name": "pause", "arguments": "{}" }
            ]
        });
        let call = first_tool_call(&response).expect("a call");
        assert_eq!(call.name, "pause");
        assert_eq!(call.arguments, "{}");
    }

    #[test]
    fn a_response_without_a_tool_call_is_an_error_not_a_silent_no_op() {
        let spoke_instead = json!({
            "output": [{ "type": "message", "content": [{ "type": "output_text",
                         "text": "sure" }] }]
        });
        assert!(first_tool_call(&spoke_instead).is_err());
        assert!(first_tool_call(&json!({ "output": [] })).is_err());
        assert!(first_tool_call(&json!({})).is_err());
    }

    #[test]
    fn backoff_grows_and_stays_short() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1_000));
    }
}
