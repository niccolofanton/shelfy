//! `shelfy-migrate login`: the device sign-in (RFC 8628; plan §4.1 step 2,
//! §2.11; P1-17's `/auth/device/*`).
//!
//! 1. `POST /auth/device/start`, without a cookie or CSRF headers: a device
//!    code (kept secret) and a user code to show with the page that approves
//!    it.
//! 2. The user opens the page, signed in to the web app, and approves the
//!    code. The CLI polls `POST /auth/device/poll` every `interval` seconds
//!    meanwhile: `slow_down` sets a longer interval, a 429 waits its
//!    `Retry-After`, and `invalid_device_code` (expired, or the server
//!    restarted) starts over with a new code.
//! 3. Once approved, the poll answers a `migrate` token, valid 7 days, which
//!    is written to the token file with mode 0600 (its directory 0700) and
//!    never printed.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;

use crate::client::{Client, DeviceAuthorization, DevicePoll, Header};

/// Directory of the default token file, under the user's config directory.
pub const CONFIG_DIR: &str = "shelfy-migrate";
/// File name of the default token file.
pub const TOKEN_FILE: &str = "token";

/// Failed polls (network errors, 5xx) tolerated in a row.
const POLL_FAILURES: u32 = 5;

/// What `login` does.
#[derive(Debug, Clone)]
pub struct LoginOptions {
    /// The server's public origin.
    pub server: String,
    /// Extra headers (the Access service token).
    pub headers: Vec<Header>,
    /// Where the token goes.
    pub token_file: PathBuf,
    /// The shortest wait between polls.
    pub min_interval: Duration,
    /// New codes after an expired or refused one, before giving up.
    pub max_restarts: u32,
}

/// What `login` tells the user, in order.
#[derive(Debug)]
pub enum LoginEvent<'a> {
    /// A code to approve.
    Code(&'a DeviceAuthorization),
    /// The server asked for slower polls: the new interval, seconds.
    SlowDown(u64),
    /// The server limited the polls: waiting this long.
    Waiting(Duration),
    /// The code expired or was refused: a new one follows.
    Restart,
}

/// A signed-in CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The token file.
    pub path: PathBuf,
    /// When the token stops working, unix ms.
    pub expires_at: i64,
}

/// The default token file: `$XDG_CONFIG_HOME/shelfy-migrate/token`, else
/// `~/.config/shelfy-migrate/token` (`%APPDATA%\shelfy-migrate\token` on
/// Windows).
#[must_use]
pub fn default_token_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join(CONFIG_DIR).join(TOKEN_FILE))
}

/// Signs in with the device flow and saves the token; `on` hears what to
/// show the user.
///
/// # Errors
///
/// The server cannot be reached or refuses, the codes keep expiring, or the
/// token file cannot be written.
pub fn login(opts: &LoginOptions, on: &mut dyn FnMut(LoginEvent<'_>)) -> anyhow::Result<Saved> {
    let client = Client::with_headers(&opts.server, None, &opts.headers)?;
    for restart in 0..=opts.max_restarts {
        if restart > 0 {
            on(LoginEvent::Restart);
        }
        let started = client.device_start().context("cannot start the sign-in")?;
        on(LoginEvent::Code(&started));
        let deadline = Instant::now() + Duration::from_secs(started.expires_in);
        let mut interval = wait(started.interval, opts.min_interval);
        let mut failures = 0;
        loop {
            if Instant::now() + interval >= deadline {
                break;
            }
            thread::sleep(interval);
            match client.device_poll(&started.device_code) {
                Ok(DevicePoll::Pending { interval: next }) => {
                    failures = 0;
                    interval = wait(next, opts.min_interval);
                }
                Ok(DevicePoll::SlowDown { interval: next }) => {
                    failures = 0;
                    interval = wait(next, opts.min_interval);
                    on(LoginEvent::SlowDown(next));
                }
                Ok(DevicePoll::Limited { retry_after }) => {
                    on(LoginEvent::Waiting(retry_after));
                    thread::sleep(retry_after);
                }
                Ok(DevicePoll::Invalid) => break,
                Ok(DevicePoll::Approved { token, expires_at }) => {
                    save_token(&opts.token_file, &token).with_context(|| {
                        format!("cannot write the token file {}", opts.token_file.display())
                    })?;
                    return Ok(Saved {
                        path: opts.token_file.clone(),
                        expires_at,
                    });
                }
                Err(err) => {
                    failures += 1;
                    if failures >= POLL_FAILURES {
                        return Err(err.context("the sign-in polls keep failing"));
                    }
                }
            }
        }
    }
    anyhow::bail!(
        "no code was approved in time ({} tries): run `shelfy-migrate login` again",
        opts.max_restarts + 1
    )
}

/// The wait for an interval of `seconds`, at least `floor`.
fn wait(seconds: u64, floor: Duration) -> Duration {
    Duration::from_secs(seconds).max(floor)
}

/// Writes `token` to `path` with mode 0600, atomically; its directory is
/// created with mode 0700.
///
/// # Errors
///
/// The file system refused.
pub fn save_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    match fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&partial)?;
    file.write_all(token.trim().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, path)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Reads a token file. On Unix, a file others may read is refused: it holds
/// a credential.
///
/// # Errors
///
/// The file cannot be read, is empty, or is readable by others.
pub fn read_token(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path)
            .with_context(|| format!("cannot read the token file {}", path.display()))?
            .permissions()
            .mode();
        anyhow::ensure!(
            mode & 0o077 == 0,
            "the token file {} is readable by others (mode {:o}): chmod 600 it",
            path.display(),
            mode & 0o777
        );
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read the token file {}", path.display()))?;
    let token = text.trim().to_owned();
    anyhow::ensure!(
        !token.is_empty(),
        "the token file {} is empty",
        path.display()
    );
    Ok(token)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::net::TcpListener;

    use super::*;

    /// One request the scripted server saw: its request line and headers.
    #[derive(Debug)]
    struct Seen {
        line: String,
        headers: Vec<(String, String)>,
    }

    /// A server that answers each connection with the next scripted
    /// response (`status`, extra headers, JSON body) and closes it.
    fn scripted(
        script: Vec<(u16, &'static str, String)>,
    ) -> (String, thread::JoinHandle<Vec<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, extra, body) in script {
                let (stream, _) = listener.accept().unwrap();
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
                let mut stream = stream;
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            seen
        });
        (origin, handle)
    }

    fn started(code: &str) -> String {
        format!(
            r#"{{"deviceCode":"dc-{code}","userCode":"{code}","verificationUri":"http://x/device",
               "verificationUriComplete":"http://x/device#{code}","expiresIn":600,"interval":0}}"#
        )
    }

    #[test]
    fn login_follows_slow_downs_limits_and_new_codes_until_approved() {
        let (origin, server) = scripted(vec![
            (200, "", started("BCDF-GHJK")),
            (200, "", r#"{"status":"slow_down","interval":0}"#.to_owned()),
            (
                429,
                "Retry-After: 1\r\n",
                r#"{"code":"rate_limited","status":429}"#.to_owned(),
            ),
            (
                400,
                "",
                r#"{"code":"invalid_device_code","status":400}"#.to_owned(),
            ),
            (200, "", started("LMNP-QRST")),
            (200, "", r#"{"status":"pending","interval":0}"#.to_owned()),
            (
                200,
                "",
                r#"{"status":"approved","token":"shx_secret","tokenId":"T1","scopes":["migrate"],
                   "expiresAt":1790899200000}"#
                    .to_owned(),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let options = LoginOptions {
            server: origin,
            headers: vec![Header::parse("CF-Access-Client-Id: id.access").unwrap()],
            token_file: dir.path().join("token"),
            min_interval: Duration::from_millis(10),
            max_restarts: 2,
        };
        let mut events = Vec::new();
        let saved = login(&options, &mut |event| {
            events.push(match event {
                LoginEvent::Code(code) => format!("code {}", code.user_code),
                LoginEvent::SlowDown(seconds) => format!("slow down {seconds}"),
                LoginEvent::Waiting(wait) => format!("wait {}", wait.as_secs()),
                LoginEvent::Restart => "restart".to_owned(),
            });
        })
        .unwrap();
        assert_eq!(
            events,
            [
                "code BCDF-GHJK",
                "slow down 0",
                "wait 1",
                "restart",
                "code LMNP-QRST"
            ]
        );
        assert_eq!(saved.expires_at, 1_790_899_200_000);
        assert_eq!(read_token(&saved.path).unwrap(), "shx_secret");

        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 7);
        for request in &seen {
            let has = |name: &str| request.headers.iter().any(|(n, _)| n == name);
            // The device routes take no cookie, no CSRF headers, no token.
            for absent in ["cookie", "origin", "x-shelfy-client", "authorization"] {
                assert!(!has(absent), "{absent} in {request:?}");
            }
            assert!(has("cf-access-client-id"), "{request:?}");
        }
        assert_eq!(seen[0].line, "POST /api/v1/auth/device/start HTTP/1.1");
        assert_eq!(seen[1].line, "POST /api/v1/auth/device/poll HTTP/1.1");
        assert_eq!(seen[4].line, "POST /api/v1/auth/device/start HTTP/1.1");
    }

    #[test]
    fn login_gives_up_when_codes_keep_expiring() {
        let (origin, server) = scripted(vec![
            (200, "", started("BCDF-GHJK")),
            (
                400,
                "",
                r#"{"code":"invalid_device_code","status":400}"#.to_owned(),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let options = LoginOptions {
            server: origin,
            headers: Vec::new(),
            token_file: dir.path().join("token"),
            min_interval: Duration::from_millis(10),
            max_restarts: 0,
        };
        let err = login(&options, &mut |_| {}).unwrap_err().to_string();
        assert!(err.contains("login` again"), "{err}");
        assert!(!dir.path().join("token").exists());
        server.join().unwrap();
    }

    #[test]
    fn the_token_file_is_private_and_replaced_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("config")
            .join("shelfy-migrate")
            .join("token");
        save_token(&path, "shx_first\n").unwrap();
        assert_eq!(read_token(&path).unwrap(), "shx_first");
        save_token(&path, "shx_second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "shx_second\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let refused = read_token(&path).unwrap_err().to_string();
            assert!(refused.contains("chmod 600"), "{refused}");
        }
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no partial file is left"
        );
    }

    #[test]
    fn the_wait_follows_the_server_but_never_drops_below_the_floor() {
        assert_eq!(wait(5, Duration::from_secs(1)), Duration::from_secs(5));
        assert_eq!(
            wait(0, Duration::from_millis(20)),
            Duration::from_millis(20)
        );
    }
}
