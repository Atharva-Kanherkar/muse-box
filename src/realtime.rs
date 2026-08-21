//! Persistent GPT Realtime session management and audio preparation.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{error::AppError, render::PlaybackState};

const REALTIME_SAMPLE_RATE: u32 = 24_000;

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
    SearchAndPlay { query: String },
    QueueSearch { query: String },
    SetVolume { percent: u8 },
    NowPlaying,
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
        search(
            "search_and_play",
            "Search Spotify and immediately play the best matching track."
        ),
        search(
            "queue_search",
            "Search Spotify and add the best matching track to the queue."
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
    use std::f64::consts::TAU;

    use super::*;

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
}
