//! A sleeping AI node (F15): a loopback port where connecting hangs, as it
//! does when the owner's PC sleeps behind Tailscale. Its listener never
//! accepts and its accept queue is full, so the kernel drops every new SYN:
//! a client sees neither an answer nor a refusal, only its connect timeout.

use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpSocket};

/// A loopback port where connecting hangs. Drop it to free the port.
pub struct SleepingNode {
    addr: SocketAddr,
    _listener: TcpListener,
    _queued: Vec<TcpStream>,
}

impl SleepingNode {
    /// Binds a port with the smallest accept queue and fills the queue.
    ///
    /// # Panics
    ///
    /// When the port cannot be bound or the queue never fills.
    pub fn start() -> Self {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(0).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut queued = Vec::new();
        // macOS maps backlog 0 to its default queue rather than Linux's
        // minimal queue. Bound the fill above both defaults.
        let deadline = Instant::now() + Duration::from_secs(3);
        for _ in 0..4096 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match TcpStream::connect_timeout(&addr, remaining.min(Duration::from_millis(200))) {
                Ok(stream) => queued.push(stream),
                Err(_) => {
                    return Self {
                        addr,
                        _listener: listener,
                        _queued: queued,
                    };
                }
            }
        }
        panic!("the accept queue of {addr} never filled");
    }

    /// The address where connecting hangs.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }
}
