use std::collections::HashMap;

use crate::error::AppError;

const MAX_AUDIO_SECONDS: u64 = 30;

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
    use super::*;

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
