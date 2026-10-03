//! Streaming JSON envelopes. A record is bounded before serde allocates it;
//! the file can be arbitrarily larger than the working set.
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};

/// Maximum serialized record (including a collection definition).
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
/// Maximum nesting; serde's own recursion limit is stricter too.
const MAX_DEPTH: usize = 128;
/// Maximum posts committed together.
pub const BATCH_ITEMS: usize = 500;
/// Bound serialized input held in a batch, even with large records.
pub const BATCH_BYTES: usize = 4 * 1024 * 1024;

/// A malformed/unsupported envelope, or a record outside the bounded contract.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem failure, retryable by the job.
    #[error("reading import: {0}")]
    Io(#[from] std::io::Error),
    /// Permanent input failure.
    #[error("import_format_unknown")]
    Format,
}
/// One record, never the entire export.
#[derive(Debug)]
pub enum Record {
    /// Post with its zero-based index and serialized size.
    Post {
        index: u64,
        value: Value,
        bytes: usize,
    },
    /// Top-level collection definition.
    Collection(Value),
}

/// Visits a bare array or `{posts: [...], collections: [...]}`. Unknown
/// envelope fields are ignored but still syntax checked and bounded. Duplicate
/// posts/collections fields and trailing bytes are rejected, never ambiguous.
/// Callback errors (including cancellation) stop reading immediately.
pub fn read<R: Read, E: From<Error>>(
    reader: R,
    mut visit: impl FnMut(Record) -> Result<(), E>,
) -> Result<(), E> {
    let mut input = Input(BufReader::with_capacity(64 * 1024, reader));
    if input.peek_raw()? == Some(0xef) {
        input.expect(0xef)?;
        input.expect(0xbb)?;
        input.expect(0xbf)?;
    }
    let first = input.peek()?.ok_or(Error::Format)?;
    let mut index = 0;
    match first {
        b'[' => input.array(|value, bytes| {
            let at = index;
            index += 1;
            visit(Record::Post {
                index: at,
                value,
                bytes,
            })
        })?,
        b'{' => {
            input.take()?;
            let mut posts = false;
            let mut collections = false;
            if input.peek()? == Some(b'}') {
                return Err(Error::Format.into());
            }
            loop {
                let (key, _) = input.value()?;
                let key = key.as_str().ok_or(Error::Format)?;
                input.expect(b':')?;
                match key {
                    "posts" if !posts => {
                        posts = true;
                        input.array(|value, bytes| {
                            let at = index;
                            index += 1;
                            visit(Record::Post {
                                index: at,
                                value,
                                bytes,
                            })
                        })?;
                    }
                    "collections" if !collections => {
                        collections = true;
                        input.array(|value, _| visit(Record::Collection(value)))?;
                    }
                    "posts" | "collections" => return Err(Error::Format.into()),
                    _ => {
                        input.value()?;
                    }
                }
                match input.peek()? {
                    Some(b',') => {
                        input.take()?;
                    }
                    Some(b'}') => {
                        input.take()?;
                        break;
                    }
                    _ => return Err(Error::Format.into()),
                }
            }
            if !posts {
                return Err(Error::Format.into());
            }
        }
        _ => return Err(Error::Format.into()),
    }
    if input.peek()?.is_some() {
        return Err(Error::Format.into());
    }
    Ok(())
}

struct Input<R>(BufReader<R>);
impl<R: Read> Input<R> {
    fn peek_raw(&mut self) -> Result<Option<u8>, Error> {
        Ok(self.0.fill_buf()?.first().copied())
    }
    fn take(&mut self) -> Result<u8, Error> {
        let c = self.peek_raw()?.ok_or(Error::Format)?;
        self.0.consume(1);
        Ok(c)
    }
    fn peek(&mut self) -> Result<Option<u8>, Error> {
        loop {
            match self.peek_raw()? {
                Some(b' ' | b'\n' | b'\r' | b'\t') => {
                    self.0.consume(1);
                }
                other => return Ok(other),
            }
        }
    }
    fn expect(&mut self, c: u8) -> Result<(), Error> {
        if self.peek()? != Some(c) {
            return Err(Error::Format);
        }
        self.take()?;
        Ok(())
    }
    fn array<E: From<Error>>(
        &mut self,
        mut visit: impl FnMut(Value, usize) -> Result<(), E>,
    ) -> Result<(), E> {
        self.expect(b'[')?;
        if self.peek()? == Some(b']') {
            self.take()?;
            return Ok(());
        }
        loop {
            let (value, bytes) = self.value()?;
            visit(value, bytes)?;
            match self.peek()? {
                Some(b',') => {
                    self.take()?;
                }
                Some(b']') => {
                    self.take()?;
                    return Ok(());
                }
                _ => return Err(Error::Format.into()),
            }
        }
    }
    fn value(&mut self) -> Result<(Value, usize), Error> {
        let first = self.peek()?.ok_or(Error::Format)?;
        let compound = matches!(first, b'{' | b'[');
        let quoted = first == b'"';
        let mut buf = Vec::with_capacity(1024);
        let mut stack = Vec::new();
        let mut string = false;
        let mut escape = false;
        let mut nodes = 0;
        loop {
            if !compound
                && !quoted
                && !buf.is_empty()
                && self
                    .peek_raw()?
                    .is_none_or(|c| matches!(c, b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'))
            {
                break;
            }
            let c = self.take()?;
            if buf.len() == MAX_RECORD_BYTES {
                return Err(Error::Format);
            }
            buf.push(c);
            if string {
                if escape {
                    escape = false;
                } else if c == b'\\' {
                    escape = true;
                } else if c == b'"' {
                    string = false;
                }
            } else {
                match c {
                    b'"' => {
                        string = true;
                        nodes += 1;
                    }
                    b'{' | b'[' => {
                        stack.push(c);
                        nodes += 1;
                        if stack.len() > MAX_DEPTH {
                            return Err(Error::Format);
                        }
                    }
                    b'}' | b']' => {
                        if stack.pop() != Some(if c == b'}' { b'{' } else { b'[' }) {
                            return Err(Error::Format);
                        }
                    }
                    b',' => nodes += 1,
                    _ => {}
                }
                if nodes > 16_384 {
                    return Err(Error::Format);
                }
            }
            if compound && stack.is_empty() && !string || quoted && !string {
                break;
            }
        }
        let value = serde_json::from_slice(&buf).map_err(|_| Error::Format)?;
        Ok((value, buf.len()))
    }
}

/// Conservative heap charge for batching serde values, including map/tree
/// allocation overhead, rather than merely their compact serialized bytes.
pub fn weight(v: &Value) -> usize {
    128 + match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(weight).sum(),
        Value::Object(o) => o.iter().map(|(k, v)| k.len() + weight(v)).sum(),
        _ => 0,
    }
}
