//! whisper.cpp's server (G3-26): `POST {url}`, the `/inference` endpoint
//! itself, with a multipart `file` (a 16 kHz mono WAV), `response_format=json`,
//! `temperature=0` and the `language`, as `electron/stt.ts` sends it. A key,
//! when the server has one, travels as a Bearer token.

use bytes::Bytes;

use crate::multipart::Form;
use crate::request::TranscribeRequest;

/// The form of an `/inference` call.
pub(crate) fn inference_form(request: &TranscribeRequest) -> (String, Bytes) {
    let mut form = Form::new();
    form.file("file", "audio.wav", "audio/wav", &request.wav);
    form.text("response_format", "json");
    form.text("temperature", "0");
    if let Some(language) = request
        .language
        .as_deref()
        .filter(|language| !language.is_empty())
    {
        form.text("language", language);
    }
    form.finish()
}

/// Whether `wav` starts like a RIFF WAVE file.
pub(crate) fn looks_like_wav(wav: &[u8]) -> bool {
    wav.len() >= 12 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE"
}
