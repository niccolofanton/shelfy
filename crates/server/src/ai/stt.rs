//! Strict PCM WAV validation before any audio leaves this server.
use crate::error::{ApiError, ErrorCode};
pub const MAX_SECONDS: usize = 120;
const BYTES_PER_SECOND: usize = 32_000;
fn invalid() -> ApiError {
    ApiError::invalid_field("audio", "requires one complete 16 kHz mono 16-bit PCM WAV")
}
fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
/// Validates RIFF boundaries, PCM format and the actual data chunk length.
/// Unknown chunks are allowed, but incomplete/duplicate chunks are refused.
pub fn validate_wav(bytes: &[u8]) -> Result<(), ApiError> {
    if bytes.get(..4) != Some(b"RIFF")
        || bytes.get(8..12) != Some(b"WAVE")
        || u32_at(bytes, 4)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| n.checked_add(8))
            != Some(bytes.len())
    {
        return Err(invalid());
    }
    let mut at = 12_usize;
    let mut format = false;
    let mut data = None;
    while at < bytes.len() {
        let tag = bytes.get(at..at + 4).ok_or_else(invalid)?;
        let length =
            usize::try_from(u32_at(bytes, at + 4).ok_or_else(invalid)?).map_err(|_| invalid())?;
        let start = at.checked_add(8).ok_or_else(invalid)?;
        let end = start.checked_add(length).ok_or_else(invalid)?;
        let chunk = bytes.get(start..end).ok_or_else(invalid)?;
        if tag == b"fmt " {
            if format
                || length < 16
                || u16_at(chunk, 0) != Some(1)
                || u16_at(chunk, 2) != Some(1)
                || u32_at(chunk, 4) != Some(16_000)
                || u32_at(chunk, 8) != Some(32_000)
                || u16_at(chunk, 12) != Some(2)
                || u16_at(chunk, 14) != Some(16)
            {
                return Err(invalid());
            }
            format = true;
        } else if tag == b"data" {
            if data.is_some() || length == 0 || !length.is_multiple_of(2) {
                return Err(invalid());
            }
            data = Some(length);
        }
        at = end.checked_add(length % 2).ok_or_else(invalid)?;
        if at > bytes.len() {
            return Err(invalid());
        }
    }
    if !format {
        return Err(invalid());
    }
    let length = data.ok_or_else(invalid)?;
    if length > MAX_SECONDS * BYTES_PER_SECOND {
        return Err(ApiError::new(ErrorCode::SttTooLong));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn wav(samples: usize) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36_u32 + u32::try_from(samples * 2).unwrap()).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&16_000_u32.to_le_bytes());
        out.extend_from_slice(&32_000_u32.to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(samples * 2).unwrap().to_le_bytes());
        out.resize(44 + samples * 2, 0);
        out
    }
    #[test]
    fn duration_is_the_data_size_not_a_claimed_rate() {
        assert!(validate_wav(&wav(120 * 16_000)).is_ok());
        assert_eq!(
            validate_wav(&wav(120 * 16_000 + 1)).unwrap_err().code(),
            ErrorCode::SttTooLong
        );
        let mut forged = wav(120 * 16_000 + 1);
        forged[28..32].copy_from_slice(&64_000_u32.to_le_bytes());
        assert_eq!(
            validate_wav(&forged).unwrap_err().code(),
            ErrorCode::ValidationFailed
        );
    }
    #[test]
    fn malformed_wrong_format_and_trailing_audio_are_refused() {
        for offset in [0, 4, 8, 16, 20, 22, 24, 28, 32, 34, 36, 40] {
            let mut bytes = wav(16);
            bytes[offset] ^= 64;
            assert!(validate_wav(&bytes).is_err(), "offset {offset}");
        }
        let mut bytes = wav(16);
        bytes.push(0);
        assert!(validate_wav(&bytes).is_err());
        assert!(validate_wav(&wav(0)).is_err());
        for size in 0..44 {
            assert!(validate_wav(&wav(16)[..size]).is_err());
        }
    }
}
