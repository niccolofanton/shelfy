//! A small `multipart/form-data` reader for the stub's transcription
//! endpoints.

/// One part of a form.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FormPart {
    pub(crate) name: String,
    pub(crate) filename: Option<String>,
    pub(crate) content_type: Option<String>,
    pub(crate) data: Vec<u8>,
}

/// The boundary of a `multipart/form-data` content type.
pub(crate) fn boundary(content_type: &str) -> Option<String> {
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    params.split(';').find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("boundary")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

/// The parts of `body`, or `None` when it is not a well-formed form.
pub(crate) fn parse(body: &[u8], boundary: &str) -> Option<Vec<FormPart>> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();
    let mut rest = body.strip_prefix(delimiter)?;
    let mut parts = Vec::new();
    loop {
        if rest.starts_with(b"--") {
            return Some(parts);
        }
        rest = rest.strip_prefix(b"\r\n")?;
        let header_end = find(rest, b"\r\n\r\n")?;
        let headers = std::str::from_utf8(&rest[..header_end]).ok()?;
        rest = &rest[header_end + 4..];
        let mut closing = Vec::with_capacity(delimiter.len() + 2);
        closing.extend_from_slice(b"\r\n");
        closing.extend_from_slice(delimiter);
        let data_end = find(rest, &closing)?;
        let data = rest[..data_end].to_vec();
        rest = &rest[data_end + closing.len()..];
        let mut part = FormPart {
            name: String::new(),
            filename: None,
            content_type: None,
            data,
        };
        for line in headers.split("\r\n") {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-disposition") {
                for param in value.split(';').skip(1) {
                    let Some((key, value)) = param.split_once('=') else {
                        continue;
                    };
                    let value = value.trim().trim_matches('"').to_owned();
                    match key.trim() {
                        "name" => part.name = value,
                        "filename" => part.filename = Some(value),
                        _ => {}
                    }
                }
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                part.content_type = Some(value.trim().to_owned());
            }
        }
        parts.push(part);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_adapter_form() {
        let body = b"--b0\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n\r\nWAVE\r\n--b0\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nit\r\n--b0--\r\n";
        assert_eq!(
            boundary("multipart/form-data; boundary=b0").as_deref(),
            Some("b0")
        );
        assert_eq!(boundary("application/json"), None);
        let parts = parse(body, "b0").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].filename.as_deref(), Some("audio.wav"));
        assert_eq!(parts[0].content_type.as_deref(), Some("audio/wav"));
        assert_eq!(parts[0].data, b"RIFF\r\n\r\nWAVE");
        assert_eq!(parts[1].name, "language");
        assert_eq!(parts[1].data, b"it");
        assert!(parse(b"garbage", "b0").is_none());
    }
}
