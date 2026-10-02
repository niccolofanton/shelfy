//! A scripted HTTP server for the client's unit tests: it answers each
//! connection with the next scripted response and records the requests.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::thread;

/// One request the scripted server saw: its request line and headers
/// (names lowercased).
#[derive(Debug)]
pub(crate) struct Seen {
    pub(crate) line: String,
    pub(crate) headers: Vec<(String, String)>,
}

impl Seen {
    /// Whether the request had the header `name` (lowercase).
    pub(crate) fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(n, _)| n == name)
    }
}

/// One scripted response: the status, extra header lines (each ending in
/// `\r\n`) and the JSON body.
pub(crate) type Reply = (u16, &'static str, String);

/// Starts a server on a local port that answers one connection per entry of
/// `script`, in order, and closes each; returns its origin and the requests
/// it saw once the script is done.
pub(crate) fn scripted(script: Vec<Reply>) -> (String, thread::JoinHandle<Vec<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, extra, body) in script {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut headers = Vec::new();
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_owned());
                if name == "content-length" {
                    length = value.parse().unwrap();
                }
                headers.push((name, value));
            }
            let mut request_body = vec![0u8; length];
            reader.read_exact(&mut request_body).unwrap();
            seen.push(Seen {
                line: line.trim_end().to_owned(),
                headers,
            });
            let content_type = if status >= 400 {
                "application/problem+json"
            } else {
                "application/json"
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n{extra}\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        seen
    });
    (origin, handle)
}
