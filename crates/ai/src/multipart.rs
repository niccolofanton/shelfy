//! `multipart/form-data` bodies for the transcription calls: the WAV file
//! and a few text fields, as `electron/stt.ts` sends them.

use bytes::{BufMut, Bytes, BytesMut};

/// A form being written.
pub(crate) struct Form {
    boundary: String,
    body: BytesMut,
}

impl Form {
    /// An empty form with a random boundary.
    pub(crate) fn new() -> Self {
        let mut random = [0_u8; 12];
        // A boundary only has to be absent from the content; on the off
        // chance the system generator fails, a fixed one still works.
        let _ = getrandom::fill(&mut random);
        let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            boundary: format!("shelfy-{hex}"),
            body: BytesMut::new(),
        }
    }

    /// A text field.
    pub(crate) fn text(&mut self, name: &str, value: &str) {
        self.start(name, None, None);
        self.body.put_slice(value.as_bytes());
        self.body.put_slice(b"\r\n");
    }

    /// A file field.
    pub(crate) fn file(&mut self, name: &str, filename: &str, content_type: &str, data: &[u8]) {
        self.start(name, Some(filename), Some(content_type));
        self.body.put_slice(data);
        self.body.put_slice(b"\r\n");
    }

    fn start(&mut self, name: &str, filename: Option<&str>, content_type: Option<&str>) {
        self.body.put_slice(b"--");
        self.body.put_slice(self.boundary.as_bytes());
        self.body
            .put_slice(b"\r\nContent-Disposition: form-data; name=\"");
        self.body.put_slice(name.as_bytes());
        self.body.put_slice(b"\"");
        if let Some(filename) = filename {
            self.body.put_slice(b"; filename=\"");
            self.body.put_slice(filename.as_bytes());
            self.body.put_slice(b"\"");
        }
        self.body.put_slice(b"\r\n");
        if let Some(content_type) = content_type {
            self.body.put_slice(b"Content-Type: ");
            self.body.put_slice(content_type.as_bytes());
            self.body.put_slice(b"\r\n");
        }
        self.body.put_slice(b"\r\n");
    }

    /// The `Content-Type` header value and the body.
    pub(crate) fn finish(mut self) -> (String, Bytes) {
        self.body.put_slice(b"--");
        self.body.put_slice(self.boundary.as_bytes());
        self.body.put_slice(b"--\r\n");
        (
            format!("multipart/form-data; boundary={}", self.boundary),
            self.body.freeze(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_fields_and_a_file() {
        let mut form = Form::new();
        let boundary = form.boundary.clone();
        form.file("file", "audio.wav", "audio/wav", b"RIFF");
        form.text("response_format", "json");
        let (content_type, body) = form.finish();
        assert_eq!(
            content_type,
            format!("multipart/form-data; boundary={boundary}")
        );
        let expected = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\nRIFF\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n\
             --{boundary}--\r\n"
        );
        assert_eq!(body, expected.as_bytes());
    }
}
