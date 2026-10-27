//! A UDP forwarder standing in for the network between a host and one
//! address: it can eat every datagram, as a network that blocks UDP does,
//! or treat them the way a poor one does: lose some, delay the rest by a
//! varying amount, let some overtake others, and pass no more than a
//! link's bandwidth.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// What the gate does to each datagram it lets through, each way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// Drops this many datagrams in a hundred.
    pub loss_percent: u32,
    /// Holds each datagram this long before passing it on.
    pub delay: Duration,
    /// Adds up to this much more to each datagram's delay, drawn afresh
    /// for each. Datagrams still leave in the order they came, as on a
    /// wire whose queue varies.
    pub jitter: Duration,
    /// Holds this many datagrams in a hundred back by the delay and jitter
    /// once more, out of the queue, so the ones behind overtake them.
    pub reorder_percent: u32,
    /// Passes at most this many bytes a second, queueing the rest up to a
    /// second's worth and dropping beyond it, as a slow link's buffer
    /// does; zero is no cap.
    pub rate_bytes_per_sec: u64,
}

impl Faults {
    /// The faults of a path that crosses two bad networks, one after the
    /// other: losses compound, delays add, the slower link caps the rate.
    pub fn then(self, next: Faults) -> Faults {
        let kept = (100 - self.loss_percent.min(100)) * (100 - next.loss_percent.min(100));
        let rate = match (self.rate_bytes_per_sec, next.rate_bytes_per_sec) {
            (0, rate) | (rate, 0) => rate,
            (a, b) => a.min(b),
        };
        Faults {
            loss_percent: 100 - kept / 100,
            delay: self.delay + next.delay,
            jitter: self.jitter + next.jitter,
            reorder_percent: self.reorder_percent.max(next.reorder_percent),
            rate_bytes_per_sec: rate,
        }
    }

    /// The faults in words, for an acknowledgement.
    pub fn describe(&self) -> String {
        if *self == Faults::default() {
            return "a clean network".to_owned();
        }
        let mut parts = Vec::new();
        if self.loss_percent > 0 {
            parts.push(format!("{}% loss", self.loss_percent));
        }
        if !self.delay.is_zero() {
            parts.push(format!("{} ms delay", self.delay.as_millis()));
        }
        if !self.jitter.is_zero() {
            parts.push(format!("{} ms jitter", self.jitter.as_millis()));
        }
        if self.reorder_percent > 0 {
            parts.push(format!("{}% reordered", self.reorder_percent));
        }
        if self.rate_bytes_per_sec > 0 {
            parts.push(format!("{} bytes/s", self.rate_bytes_per_sec));
        }
        parts.join(", ")
    }
}

/// How a datagram goes through: dropped, or after a delay, in its turn or
/// let out of the queue to be overtaken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Drop,
    InTurn(Duration),
    Overtaken(Duration),
}

struct Shared {
    blocked: AtomicBool,
    faults: Mutex<Faults>,
    /// A small generator for which datagrams are lost, held back and how
    /// long each waits, seeded so a run can be told from another only by
    /// timing.
    draw: AtomicU64,
    /// Where datagrams go, and how many times that has changed.
    upstream: Mutex<(u64, Option<SocketAddr>)>,
}

impl Shared {
    /// The next number from the generator.
    fn draw(&self) -> u64 {
        let mut x = self.draw.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.draw.store(x, Ordering::Relaxed);
        x
    }

    /// Whether this datagram goes through, and how.
    fn pass(&self) -> Pass {
        if self.blocked.load(Ordering::SeqCst) {
            return Pass::Drop;
        }
        let faults = *self.faults.lock().unwrap();
        if faults.loss_percent > 0 && (self.draw() % 100) < u64::from(faults.loss_percent) {
            return Pass::Drop;
        }
        let mut delay = faults.delay;
        let jitter = u64::try_from(faults.jitter.as_micros()).unwrap_or(u64::MAX);
        if jitter > 0 {
            delay += Duration::from_micros(self.draw() % (jitter + 1));
        }
        if faults.reorder_percent > 0 && (self.draw() % 100) < u64::from(faults.reorder_percent) {
            // Held back by at least a millisecond, so a datagram right
            // behind it overtakes it even on an otherwise perfect wire.
            let back = (faults.delay + faults.jitter).max(Duration::from_millis(1));
            return Pass::Overtaken(delay + back);
        }
        Pass::InTurn(delay)
    }

    fn rate(&self) -> u64 {
        self.faults.lock().unwrap().rate_bytes_per_sec
    }
}

/// Each sender gets its own upstream socket, so the far end sees one peer
/// per sender.
pub struct UdpGate {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl UdpGate {
    pub async fn start(upstream: SocketAddr) -> std::io::Result<Self> {
        Self::bind(Some(upstream))
    }

    /// A gate in front of `upstream`, or of nothing yet: datagrams that
    /// arrive before [`UdpGate::set_upstream`] names a listener are
    /// dropped. Binds at once, so it needs no await, only a runtime.
    /// The gate binds loopback in the upstream's family, so a host bound
    /// IPv6 only is gated over IPv6.
    pub fn bind(upstream: Option<SocketAddr>) -> std::io::Result<Self> {
        let loopback: std::net::IpAddr = match upstream {
            Some(SocketAddr::V6(_)) => std::net::Ipv6Addr::LOCALHOST.into(),
            _ => std::net::Ipv4Addr::LOCALHOST.into(),
        };
        Self::bind_on(loopback, upstream)
    }

    /// The same, its front socket on `front` rather than loopback: where a
    /// client outside this Mac, a phone over its USB link, can dial it. The
    /// socket toward the upstream is still this Mac's own.
    pub fn bind_on(front: std::net::IpAddr, upstream: Option<SocketAddr>) -> std::io::Result<Self> {
        let front = std::net::UdpSocket::bind(SocketAddr::new(front, 0))?;
        front.set_nonblocking(true)?;
        let front = Arc::new(UdpSocket::from_std(front)?);
        let addr = front.local_addr()?;
        let shared = Arc::new(Shared {
            blocked: AtomicBool::new(false),
            faults: Mutex::new(Faults::default()),
            draw: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
            upstream: Mutex::new((0, upstream)),
        });
        let task = tokio::spawn({
            let shared = shared.clone();
            async move {
                let mut senders: HashMap<SocketAddr, Sender> = HashMap::new();
                let mut generation = 0;
                let mut buf = vec![0; 65_536];
                loop {
                    let Ok((n, from)) = front.recv_from(&mut buf).await else {
                        return;
                    };
                    let (now, upstream) = *shared.upstream.lock().unwrap();
                    if now != generation {
                        // The listener moved: every sender reaches it
                        // afresh, as after a host restarts on a new port.
                        senders.clear();
                        generation = now;
                    }
                    let Some(upstream) = upstream else {
                        continue;
                    };
                    let sender = match senders.get(&from) {
                        Some(sender) => sender,
                        None => {
                            // Toward the upstream from this Mac's own loopback,
                            // in the upstream's family, wherever the front is.
                            let loopback: std::net::IpAddr = if upstream.is_ipv6() {
                                std::net::Ipv6Addr::LOCALHOST.into()
                            } else {
                                std::net::Ipv4Addr::LOCALHOST.into()
                            };
                            let Ok(back) = UdpSocket::bind(SocketAddr::new(loopback, 0)).await
                            else {
                                continue;
                            };
                            if back.connect(upstream).await.is_err() {
                                continue;
                            }
                            let back = Arc::new(back);
                            let returning = tokio::spawn({
                                let (back, front, shared) =
                                    (back.clone(), front.clone(), shared.clone());
                                async move {
                                    let replies = Held::start(move |datagram| {
                                        let front = front.clone();
                                        async move {
                                            let _ = front.send_to(&datagram, from).await;
                                        }
                                    });
                                    let mut buf = vec![0; 65_536];
                                    loop {
                                        match back.recv(&mut buf).await {
                                            Ok(n) => replies.hold(
                                                buf[..n].to_vec(),
                                                shared.pass(),
                                                shared.rate(),
                                            ),
                                            // Sending while the listener
                                            // is down is refused; once it
                                            // is back, what it sends,
                                            // dialling out as much as
                                            // answering, must still pass.
                                            Err(error)
                                                if matches!(
                                                    error.kind(),
                                                    std::io::ErrorKind::ConnectionRefused
                                                        | std::io::ErrorKind::ConnectionReset
                                                ) => {}
                                            Err(_) => break,
                                        }
                                    }
                                }
                            });
                            let onward = Held::start({
                                let back = back.clone();
                                move |datagram| {
                                    let back = back.clone();
                                    async move {
                                        let _ = back.send(&datagram).await;
                                    }
                                }
                            });
                            senders.entry(from).or_insert(Sender {
                                onward,
                                _returning: Returning(returning),
                            })
                        }
                    };
                    sender
                        .onward
                        .hold(buf[..n].to_vec(), shared.pass(), shared.rate());
                }
            }
        });
        Ok(Self { addr, shared, task })
    }

    /// Points the gate at a listener, when it differs from the one it
    /// points at now.
    pub fn set_upstream(&self, upstream: SocketAddr) {
        let mut current = self.shared.upstream.lock().unwrap();
        if current.1 != Some(upstream) {
            *current = (current.0 + 1, Some(upstream));
        }
    }

    pub fn upstream(&self) -> Option<SocketAddr> {
        self.shared.upstream.lock().unwrap().1
    }

    /// Where a host sends to go through the gate.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn block(&self, blocked: bool) {
        self.shared.blocked.store(blocked, Ordering::SeqCst);
    }

    pub fn set_faults(&self, faults: Faults) {
        *self.shared.faults.lock().unwrap() = faults;
    }

    pub fn faults(&self) -> Faults {
        *self.shared.faults.lock().unwrap()
    }

    pub fn is_blocked(&self) -> bool {
        self.shared.blocked.load(Ordering::SeqCst)
    }
}

/// One sender through the gate: the queue carrying its datagrams on, and
/// the task carrying its replies back.
struct Sender {
    onward: Held,
    _returning: Returning,
}

/// Datagrams held for their delay and passed on in the order they came,
/// as a slow wire passes them. Holding each on its own timer would let
/// two sent in the same millisecond swap places, and a QUIC packet that
/// overtakes the handshake it follows is dropped by the receiver and
/// counted lost by the sender, which no household network does; only a
/// datagram the faults pick to be overtaken leaves the queue.
struct Held {
    queue: mpsc::UnboundedSender<(Instant, Vec<u8>)>,
    task: JoinHandle<()>,
    /// Delivers one datagram on its own timer, outside the queue.
    aside: Arc<dyn Fn(Vec<u8>, Instant) + Send + Sync>,
    /// When the rate-capped wire is next free to start sending.
    wire_free: Mutex<Instant>,
}

impl Held {
    fn start<F, Fut>(deliver: F) -> Self
    where
        F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let deliver = Arc::new(deliver);
        let (queue, mut held) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
        let task = tokio::spawn({
            let deliver = deliver.clone();
            async move {
                while let Some((due, datagram)) = held.recv().await {
                    tokio::time::sleep_until(due).await;
                    deliver(datagram).await;
                }
            }
        });
        let aside = Arc::new(move |datagram: Vec<u8>, due: Instant| {
            let deliver = deliver.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(due).await;
                deliver(datagram).await;
            });
        });
        Self {
            queue,
            task,
            aside,
            wire_free: Mutex::new(Instant::now()),
        }
    }

    fn hold(&self, datagram: Vec<u8>, pass: Pass, rate_bytes_per_sec: u64) {
        let delay = match pass {
            Pass::Drop => return,
            Pass::InTurn(delay) | Pass::Overtaken(delay) => delay,
        };
        let now = Instant::now();
        let mut sent = now;
        if let Some(micros) = u64::try_from(datagram.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000)
            .checked_div(rate_bytes_per_sec)
        {
            let mut free = self.wire_free.lock().unwrap();
            let start = (*free).max(now);
            // A second's worth of queue, as a household router buffers;
            // past it the datagram is dropped, as the router would.
            if start.duration_since(now) > Duration::from_secs(1) {
                return;
            }
            *free = start + Duration::from_micros(micros);
            sent = *free;
        }
        match pass {
            Pass::Overtaken(_) => (self.aside)(datagram, sent + delay),
            _ => {
                let _ = self.queue.send((sent + delay, datagram));
            }
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The task carrying one sender's replies back, stopped with the gate.
struct Returning(JoinHandle<()>);

impl Drop for Returning {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for UdpGate {
    fn drop(&mut self) {
        self.task.abort();
    }
}
