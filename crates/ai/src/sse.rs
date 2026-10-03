//! An incremental parser of server-sent events, the framing of every streamed
//! answer (OpenAI-compatible chunks, Anthropic events).
//!
//! It follows the WHATWG event-stream rules: lines end in LF, CRLF or CR (a
//! CRLF may be split across chunks), `data` lines of one event join with LF,
//! a blank line dispatches the event, `:` lines are comments (OpenRouter sends
//! them while it waits), one space after the colon is dropped, a leading BOM
//! is ignored, and `id` and `retry` are ignored. One leniency: an event still
//! pending when the stream ends is dispatched by [`SseParser::finish`], since
//! some servers omit the last blank line.

/// One event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field (Anthropic's event types); `None` for unnamed events.
    pub event: Option<String>,
    /// The `data` lines, joined with LF.
    pub data: String,
}

/// Why a stream cannot be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    /// A line is not UTF-8.
    #[error("a stream line is not UTF-8")]
    Utf8,
    /// A line or an event is longer than the parser's cap.
    #[error("a stream event is longer than {0} bytes")]
    TooLong(usize),
}

/// The largest line or event the parser holds (1 MiB): a whole answer fits
/// many times over, and a hostile endpoint cannot grow it without bound.
pub const MAX_EVENT_BYTES: usize = 1 << 20;

/// Parses events from chunks of bytes.
#[derive(Debug)]
pub struct SseParser {
    line: Vec<u8>,
    after_cr: bool,
    first_line: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    max: usize,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    /// A parser capped at [`MAX_EVENT_BYTES`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_max(MAX_EVENT_BYTES)
    }

    /// A parser capped at `max` bytes per line and per event.
    #[must_use]
    pub fn with_max(max: usize) -> Self {
        Self {
            line: Vec::new(),
            after_cr: false,
            first_line: true,
            event: None,
            data: String::new(),
            has_data: false,
            max,
        }
    }

    /// Feeds `chunk`; returns the events it completed.
    ///
    /// # Errors
    ///
    /// [`SseError`]; the parser is then unusable.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.after_cr = true;
                    self.end_line(&mut events)?;
                }
                b'\n' => self.end_line(&mut events)?,
                _ => {
                    if self.line.len() >= self.max {
                        return Err(SseError::TooLong(self.max));
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// Ends the stream: a last line without its line ending is read, and an
    /// event still pending is dispatched.
    ///
    /// # Errors
    ///
    /// [`SseError`] for the last line.
    pub fn finish(&mut self) -> Result<Option<SseEvent>, SseError> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut events)?;
        }
        if let Some(event) = self.dispatch() {
            events.push(event);
        }
        Ok(events.pop())
    }

    fn end_line(&mut self, events: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&bytes).map_err(|_| SseError::Utf8)?;
        if self.first_line {
            self.first_line = false;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            if let Some(event) = self.dispatch() {
                events.push(event);
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if self.data.len() + value.len() + 1 > self.max {
                    return Err(SseError::TooLong(self.max));
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = Some(value.to_owned()),
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(SseEvent {
            event: event.filter(|name| !name.is_empty()),
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Result<Vec<SseEvent>, SseError> {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.push(chunk)?);
        }
        events.extend(parser.finish()?);
        Ok(events)
    }

    fn event(name: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: name.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn reads_openai_chunks_and_done() {
        let body = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        assert_eq!(
            parse_all(&[body]).unwrap(),
            [
                event(None, "{\"a\":1}"),
                event(None, "{\"b\":2}"),
                event(None, "[DONE]")
            ]
        );
    }

    #[test]
    fn reads_named_events_comments_and_multiline_data() {
        let body = b": OPENROUTER PROCESSING\n\nevent: message_start\ndata: one\ndata: two\nid: 7\nretry: 100\n\nevent: ping\ndata:\n\n";
        assert_eq!(
            parse_all(&[body]).unwrap(),
            [
                event(Some("message_start"), "one\ntwo"),
                event(Some("ping"), "")
            ]
        );
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_ending() {
        assert_eq!(
            parse_all(&[b"data: x\r", b"\n\r", b"\ndata: y\r\r"]).unwrap(),
            [event(None, "x"), event(None, "y")]
        );
    }

    #[test]
    fn a_pending_event_is_dispatched_at_the_end() {
        assert_eq!(parse_all(&[b"data: last"]).unwrap(), [event(None, "last")]);
        assert_eq!(parse_all(&[b"event: x\n"]).unwrap(), Vec::<SseEvent>::new());
    }

    #[test]
    fn a_bom_and_one_space_are_dropped() {
        assert_eq!(
            parse_all(&["\u{feff}data:  two spaces\n\n".as_bytes()]).unwrap(),
            [event(None, " two spaces")]
        );
    }

    #[test]
    fn caps_and_bad_utf8_are_errors() {
        let mut parser = SseParser::with_max(8);
        assert_eq!(parser.push(b"data: 0123456789"), Err(SseError::TooLong(8)));
        assert_eq!(parse_all(&[b"data: \xff\xfe\n\n"]), Err(SseError::Utf8));
    }

    /// Events with names and data lines that SSE can carry: no CR or LF
    /// inside a line, and a value that does not start with a space (the
    /// parser drops one).
    fn events() -> impl Strategy<Value = Vec<(Option<String>, Vec<String>)>> {
        let line = "[^\r\n ][^\r\n]{0,30}|";
        let name = proptest::option::of("[a-z_]{1,12}");
        proptest::collection::vec((name, proptest::collection::vec(line, 1..4)), 0..8)
    }

    /// Serializes `events`, each with its own line ending (taken in turn from
    /// `endings`). Within an event the ending stays the same: a CR line ending
    /// followed by a blank line ending in LF would read as one CRLF.
    fn encode(
        events: &[(Option<String>, Vec<String>)],
        endings: &[&str],
        comments: bool,
    ) -> Vec<u8> {
        let mut out = String::new();
        for (n, (name, lines)) in events.iter().enumerate() {
            let eol = endings[n % endings.len()];
            if comments {
                out.push_str(": keep-alive");
                out.push_str(eol);
            }
            if let Some(name) = name {
                out.push_str("event: ");
                out.push_str(name);
                out.push_str(eol);
            }
            for line in lines {
                out.push_str("data: ");
                out.push_str(line);
                out.push_str(eol);
            }
            out.push_str(eol);
        }
        out.into_bytes()
    }

    fn expected(events: &[(Option<String>, Vec<String>)]) -> Vec<SseEvent> {
        events
            .iter()
            .map(|(name, lines)| SseEvent {
                event: name.clone(),
                data: lines.join("\n"),
            })
            .collect()
    }

    proptest! {
        #[test]
        fn any_chunking_and_line_endings_give_the_same_events(
            events in events(),
            endings in proptest::sample::subsequence(vec!["\n", "\r\n", "\r"], 1..=3),
            comments in any::<bool>(),
            cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..12),
        ) {
            let bytes = encode(&events, &endings, comments);
            let mut points: Vec<usize> = cuts.iter().map(|cut| cut.index(bytes.len() + 1)).collect();
            points.sort_unstable();
            let mut chunks = Vec::new();
            let mut start = 0;
            for point in points {
                chunks.push(&bytes[start..point]);
                start = point;
            }
            chunks.push(&bytes[start..]);
            prop_assert_eq!(parse_all(&chunks).unwrap(), expected(&events));
        }

        #[test]
        fn arbitrary_bytes_never_panic(chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), 0..8)) {
            let mut parser = SseParser::with_max(256);
            for chunk in &chunks {
                if parser.push(chunk).is_err() {
                    return Ok(());
                }
            }
            let _ = parser.finish();
        }
    }
}
