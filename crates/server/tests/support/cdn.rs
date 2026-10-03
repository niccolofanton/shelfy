//! A fixture CDN with scripted answers (P2-04), for the outbound tests and
//! the archive and hydration tests after them (P2-10, P2-11, P4-16). P2-11
//! added Pinterest's short-link hosts to the certificate.
//!
//! - Two loopback listeners: https, with a certificate for the platform
//!   hosts ([`CERT_NAMES`]) signed by a test CA made on the fly, and plain
//!   http. HTTP/1.1, one request per connection.
//! - Answers are scripted per host and request target ([`FixtureCdn::route`]);
//!   anything else answers 404. Every request is logged ([`Hit`]), with the
//!   peak of requests in flight.
//! - [`FixtureCdn::config`] gives an outbound configuration that sends the
//!   chosen host names to the listeners (`SHELFY_DEV_EGRESS_HOSTS`), trusts
//!   the CA (`SHELFY_DEV_EGRESS_CA`) and answers no other name lookup, so no
//!   test reaches the network.
//! - [`ProxyStub`] stands in for the egress proxy: it logs every request,
//!   tunnels `CONNECT` to the https listener, forwards absolute-form
//!   requests to the plain one, and refuses the hosts it is told to, as
//!   Smokescreen does.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::extension::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName,
};
use openssl::x509::{X509, X509Builder, X509NameBuilder};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use shelfy_server::outbound::{Lookup, OutboundConfig};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;

/// The names the fixture's certificate is valid for.
pub const CERT_NAMES: &[&str] = &[
    "*.cdninstagram.com",
    "*.fbcdn.net",
    "pbs.twimg.com",
    "video.twimg.com",
    "*.pinimg.com",
    "www.instagram.com",
    "cdn.syndication.twimg.com",
    "publish.x.com",
    "www.pinterest.com",
    "widgets.pinterest.com",
    "pin.it",
    "api.pinterest.com",
    "*.example.test",
];

/// The largest request head the fixture reads.
const MAX_HEAD: usize = 64 * 1024;

/// A minimal JPEG for the store: its magic bytes, then filler.
#[must_use]
pub fn jpeg(len: usize) -> Vec<u8> {
    let mut bytes = vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0,
    ];
    bytes.resize(len.max(bytes.len()), 0x42);
    bytes
}

/// One logged request.
#[derive(Clone, Debug)]
pub struct Hit {
    /// When it arrived.
    pub at: Instant,
    /// Whether it came over TLS.
    pub tls: bool,
    /// The method.
    pub method: String,
    /// The `Host` header, without a port.
    pub host: String,
    /// The request target: path and query.
    pub target: String,
    /// The headers, names in lowercase.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Hit {
    /// The value of the header `name` (lowercase).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A body.
#[derive(Clone, Debug)]
pub enum Body {
    /// These bytes.
    Bytes(Vec<u8>),
    /// `head`, then zero bytes up to `len`, written in chunks; without a
    /// `Content-Length` when `chunked`.
    Large {
        /// The first bytes.
        head: Vec<u8>,
        /// The total length.
        len: u64,
        /// `Transfer-Encoding: chunked` instead of a length.
        chunked: bool,
    },
}

/// What the fixture does with a request.
#[derive(Clone, Debug)]
pub enum Answer {
    /// Answers after `delay`.
    Respond {
        /// The status.
        status: u16,
        /// Extra headers.
        headers: Vec<(String, String)>,
        /// The body.
        body: Body,
        /// The wait before the answer.
        delay: Duration,
    },
    /// Never answers; holds the connection until the client leaves.
    Hang,
    /// Closes the connection without an answer.
    Close,
}

impl Answer {
    /// `status` with a body and its type.
    #[must_use]
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::Respond {
            status,
            headers: vec![("content-type".to_owned(), content_type.to_owned())],
            body: Body::Bytes(body.into()),
            delay: Duration::ZERO,
        }
    }

    /// `status` with no body.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self::Respond {
            status,
            headers: Vec::new(),
            body: Body::Bytes(Vec::new()),
            delay: Duration::ZERO,
        }
    }

    /// `status` with a text body.
    #[must_use]
    pub fn text(status: u16, text: &str) -> Self {
        Self::new(status, "text/plain", text)
    }

    /// 200 with a JPEG of `len` bytes.
    #[must_use]
    pub fn jpeg(len: usize) -> Self {
        Self::new(200, "image/jpeg", jpeg(len))
    }

    /// A redirect to `location`.
    #[must_use]
    pub fn redirect(status: u16, location: &str) -> Self {
        Self::status(status).with_header("location", location)
    }

    /// 200 with a large body of `content_type`.
    #[must_use]
    pub fn large(content_type: &str, head: Vec<u8>, len: u64, chunked: bool) -> Self {
        Self::Respond {
            status: 200,
            headers: vec![("content-type".to_owned(), content_type.to_owned())],
            body: Body::Large { head, len, chunked },
            delay: Duration::ZERO,
        }
    }

    /// The same answer with one more header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        if let Self::Respond { headers, .. } = &mut self {
            headers.push((name.to_owned(), value.to_owned()));
        }
        self
    }

    /// The same answer after `delay`.
    #[must_use]
    pub fn delayed(mut self, wait: Duration) -> Self {
        if let Self::Respond { delay, .. } = &mut self {
            *delay = wait;
        }
        self
    }
}

#[derive(Default)]
struct Script {
    /// (host, target) → answers; the last one repeats.
    routes: HashMap<(String, String), VecDeque<Answer>>,
    hits: Vec<Hit>,
    in_flight: usize,
    peak: usize,
}

impl Script {
    fn answer(&mut self, host: &str, target: &str) -> Answer {
        match self.routes.get_mut(&(host.to_owned(), target.to_owned())) {
            Some(answers) if answers.len() > 1 => answers.pop_front().expect("not empty"),
            Some(answers) if !answers.is_empty() => answers[0].clone(),
            _ => Answer::text(404, "not routed"),
        }
    }
}

type Shared = Arc<Mutex<Script>>;

fn lock(script: &Shared) -> std::sync::MutexGuard<'_, Script> {
    script.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fixture CDN. Its listeners stop when it is dropped.
pub struct FixtureCdn {
    https: SocketAddr,
    http: SocketAddr,
    ca: X509,
    script: Shared,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for FixtureCdn {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FixtureCdn {
    /// Starts both listeners on loopback.
    pub async fn start() -> Self {
        let (ca, ca_key) = make_ca();
        let (leaf, leaf_key) = make_leaf(&ca, &ca_key, CERT_NAMES);
        let chain = vec![
            CertificateDer::from(leaf.to_der().unwrap()),
            CertificateDer::from(ca.to_der().unwrap()),
        ];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            leaf_key.private_key_to_pkcs8().unwrap(),
        ));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(tls));

        let script = Shared::default();
        let https_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let https = https_listener.local_addr().unwrap();
        let http = http_listener.local_addr().unwrap();
        let tls_script = Arc::clone(&script);
        let tls_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = https_listener.accept().await else {
                    return;
                };
                let (acceptor, script) = (acceptor.clone(), Arc::clone(&tls_script));
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(stream).await {
                        serve(stream, true, script).await;
                    }
                });
            }
        });
        let plain_script = Arc::clone(&script);
        let plain_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = http_listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(stream, false, Arc::clone(&plain_script)));
            }
        });
        Self {
            https,
            http,
            ca,
            script,
            tasks: vec![tls_task, plain_task],
        }
    }

    /// The https listener.
    #[must_use]
    pub fn https_addr(&self) -> SocketAddr {
        self.https
    }

    /// The plain http listener.
    #[must_use]
    pub fn http_addr(&self) -> SocketAddr {
        self.http
    }

    /// The test CA, to trust.
    #[must_use]
    pub fn ca(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.ca.to_der().unwrap())
    }

    /// The test CA as PEM, for `SHELFY_DEV_EGRESS_CA`.
    #[must_use]
    pub fn ca_pem(&self) -> Vec<u8> {
        self.ca.to_pem().unwrap()
    }

    /// Scripts the answers to `host` and `target` (path and query), in
    /// order; the last one repeats.
    pub fn route(&self, host: &str, target: &str, answers: impl IntoIterator<Item = Answer>) {
        let answers: VecDeque<Answer> = answers.into_iter().collect();
        assert!(!answers.is_empty(), "a route needs an answer");
        lock(&self.script)
            .routes
            .insert((host.to_owned(), target.to_owned()), answers);
    }

    /// Every request so far.
    #[must_use]
    pub fn hits(&self) -> Vec<Hit> {
        lock(&self.script).hits.clone()
    }

    /// The requests to `host` and `target`.
    #[must_use]
    pub fn hits_of(&self, host: &str, target: &str) -> Vec<Hit> {
        self.hits()
            .into_iter()
            .filter(|hit| hit.host == host && hit.target == target)
            .collect()
    }

    /// The most requests in flight at once so far.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        lock(&self.script).peak
    }

    /// An outbound configuration (direct mode) that sends `https_hosts` to
    /// the https listener and `http_hosts` to the plain one, trusts the CA,
    /// and finds no other name: only the `lookup` answers.
    #[must_use]
    pub fn config(
        &self,
        https_hosts: &[&str],
        http_hosts: &[&str],
        lookup: &[(&str, &[IpAddr])],
    ) -> OutboundConfig {
        let mut dev_hosts = BTreeMap::new();
        for host in https_hosts {
            dev_hosts.insert((*host).to_owned(), self.https);
        }
        for host in http_hosts {
            dev_hosts.insert((*host).to_owned(), self.http);
        }
        OutboundConfig {
            dev_hosts,
            extra_roots: vec![self.ca()],
            lookup: Lookup::fixed(lookup.iter().map(|(name, ips)| (*name, ips.to_vec()))),
            ..OutboundConfig::default()
        }
    }
}

/// A parsed request head, and what of the body was read with it.
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads a request: its head, and its body when it has a length.
async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > MAX_HEAD {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let mut body = buffer[end + 4..].to_vec();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

/// Serves one request on `stream`.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, tls: bool, script: Shared) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let host = request
        .header("host")
        .map(|host| strip_port(host).to_ascii_lowercase())
        .unwrap_or_default();
    let answer = {
        let mut script = lock(&script);
        script.hits.push(Hit {
            at: Instant::now(),
            tls,
            method: request.method.clone(),
            host: host.clone(),
            target: request.target.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        });
        script.in_flight += 1;
        script.peak = script.peak.max(script.in_flight);
        script.answer(&host, &request.target)
    };
    match answer {
        Answer::Hang => {
            // Until the client gives up.
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte).await;
        }
        Answer::Close => {}
        Answer::Respond {
            status,
            headers,
            body,
            delay,
        } => {
            tokio::time::sleep(delay).await;
            let _ = respond(&mut stream, status, &headers, &body).await;
        }
    }
    lock(&script).in_flight -= 1;
}

async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    headers: &[(String, String)],
    body: &Body,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nconnection: close\r\n",
        reason(status)
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    match body {
        Body::Bytes(bytes) => {
            head.push_str(&format!("content-length: {}\r\n\r\n", bytes.len()));
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(bytes).await?;
        }
        Body::Large {
            head: first,
            len,
            chunked,
        } => {
            if *chunked {
                head.push_str("transfer-encoding: chunked\r\n\r\n");
            } else {
                head.push_str(&format!("content-length: {len}\r\n\r\n"));
            }
            stream.write_all(head.as_bytes()).await?;
            let first = &first[..first.len().min(usize::try_from(*len).unwrap_or(usize::MAX))];
            write_chunk(stream, first, *chunked).await?;
            let mut sent = first.len() as u64;
            let zeros = vec![0_u8; 64 * 1024];
            while sent < *len {
                let size =
                    usize::try_from(*len - sent).map_or(zeros.len(), |left| left.min(zeros.len()));
                write_chunk(stream, &zeros[..size], *chunked).await?;
                sent += size as u64;
            }
            if *chunked {
                stream.write_all(b"0\r\n\r\n").await?;
            }
        }
    }
    stream.flush().await?;
    stream.shutdown().await
}

async fn write_chunk<S: AsyncWrite + Unpin>(
    stream: &mut S,
    chunk: &[u8],
    chunked: bool,
) -> std::io::Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    if chunked {
        stream
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await?;
        stream.write_all(chunk).await?;
        stream.write_all(b"\r\n").await
    } else {
        stream.write_all(chunk).await
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        return host.split_once(']').map_or(host, |(h, _)| &h[1..]);
    }
    host.rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(name, _)| name)
}

/// A stand-in for the egress proxy.
pub struct ProxyStub {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    denied: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for ProxyStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ProxyStub {
    /// Starts the stub in front of `fixture`.
    pub async fn start(fixture: &FixtureCdn) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let denied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (https, http) = (fixture.https, fixture.http);
        let (task_log, task_denied) = (Arc::clone(&log), Arc::clone(&denied));
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (log, denied) = (Arc::clone(&task_log), Arc::clone(&task_denied));
                tokio::spawn(proxy(stream, https, http, log, denied));
            }
        });
        Self {
            addr,
            log,
            denied,
            task,
        }
    }

    /// The proxy's URL, for `SHELFY_EGRESS_PROXY`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request the proxy saw: `CONNECT host:port` or `METHOD url`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Refuses `host` from now on: 403 to a `CONNECT`, 503 with
    /// `X-Smokescreen-Error` to a plain request.
    pub fn deny(&self, host: &str) {
        self.denied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(host.to_owned());
    }
}

async fn proxy(
    mut client: TcpStream,
    https: SocketAddr,
    http: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    denied: Arc<Mutex<Vec<String>>>,
) {
    let Some(request) = read_request(&mut client).await else {
        return;
    };
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(format!("{} {}", request.method, request.target));
    let is_denied = |host: &str| {
        denied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|d| d == host)
    };
    if request.method == "CONNECT" {
        if is_denied(strip_port(&request.target)) {
            let _ = client
                .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                .await;
            return;
        }
        let Ok(mut upstream) = TcpStream::connect(https).await else {
            return;
        };
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return;
    }
    let Ok(url) = url::Url::parse(&request.target) else {
        return;
    };
    if is_denied(url.host_str().unwrap_or_default()) {
        let _ = client
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nx-smokescreen-error: denied\r\n\
                  content-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
        return;
    }
    let Ok(mut upstream) = TcpStream::connect(http).await else {
        return;
    };
    let mut origin_form = url.path().to_owned();
    if let Some(query) = url.query() {
        origin_form.push('?');
        origin_form.push_str(query);
    }
    let mut head = format!("{} {origin_form} HTTP/1.1\r\n", request.method);
    for (name, value) in &request.headers {
        if name != "proxy-connection" {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str("\r\n");
    if upstream.write_all(head.as_bytes()).await.is_err()
        || upstream.write_all(&request.body).await.is_err()
    {
        return;
    }
    let _ = tokio::io::copy(&mut upstream, &mut client).await;
    let _ = client.shutdown().await;
}

fn x509_name(common: &str) -> openssl::x509::X509Name {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", common).unwrap();
    name.build()
}

fn validity(cert: &mut X509Builder) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let now = i64::try_from(now).unwrap();
    cert.set_not_before(&Asn1Time::from_unix(now - 3600).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::from_unix(now + 7 * 86_400).unwrap())
        .unwrap();
}

fn ec_key() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}

/// A self-signed CA.
fn make_ca() -> (X509, PKey<Private>) {
    let key = ec_key();
    let mut cert = X509Builder::new().unwrap();
    cert.set_version(2).unwrap();
    cert.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
        .unwrap();
    let subject = x509_name("Shelfy fixture CA");
    cert.set_subject_name(&subject).unwrap();
    cert.set_issuer_name(&subject).unwrap();
    cert.set_pubkey(&key).unwrap();
    validity(&mut cert);
    cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    cert.append_extension(
        KeyUsage::new()
            .critical()
            .key_cert_sign()
            .crl_sign()
            .build()
            .unwrap(),
    )
    .unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    (cert.build(), key)
}

/// A server certificate for `names`, signed by the CA.
fn make_leaf(ca: &X509, ca_key: &PKey<Private>, names: &[&str]) -> (X509, PKey<Private>) {
    let key = ec_key();
    let mut cert = X509Builder::new().unwrap();
    cert.set_version(2).unwrap();
    cert.set_serial_number(&BigNum::from_u32(2).unwrap().to_asn1_integer().unwrap())
        .unwrap();
    cert.set_subject_name(&x509_name("Shelfy fixture CDN"))
        .unwrap();
    cert.set_issuer_name(ca.subject_name()).unwrap();
    cert.set_pubkey(&key).unwrap();
    validity(&mut cert);
    cert.append_extension(BasicConstraints::new().build().unwrap())
        .unwrap();
    cert.append_extension(
        KeyUsage::new()
            .critical()
            .digital_signature()
            .build()
            .unwrap(),
    )
    .unwrap();
    cert.append_extension(ExtendedKeyUsage::new().server_auth().build().unwrap())
        .unwrap();
    let mut alt = SubjectAlternativeName::new();
    for dns in names {
        alt.dns(dns);
    }
    let alt = alt.build(&cert.x509v3_context(Some(ca), None)).unwrap();
    cert.append_extension(alt).unwrap();
    cert.sign(ca_key, MessageDigest::sha256()).unwrap();
    (cert.build(), key)
}
