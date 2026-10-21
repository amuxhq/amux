//! An imitated home router: what stands between an in-process host's link
//! socket and loopback, so a story can put a host behind the address
//! translation and filtering that decide whether punching works.
//!
//! The router is the host's socket layer's [`Datagrams`] substrate. The
//! host's own address is a private one nobody can reach; what it sends
//! leaves from a public loopback port the router's table hands out, and
//! what arrives passes only through a mapping its filter allows. A mapping
//! nothing has used for `idle` expires, and the next packet out gets a new
//! public port.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use node::socket::Datagrams;
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::time::Instant;

/// How the router maps and filters: the two axes that decide whether a
/// punched packet gets through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouterKind {
    /// No translation: the host's own address is reachable by anyone.
    Open,
    /// One public port for everything the host sends; inbound allowed
    /// from any address and port the host has sent to through it. (On
    /// loopback every host shares one IP, so the port is what tells them
    /// apart.)
    Ordinary,
    /// A public port per destination; inbound only from the exact address
    /// and port that port was opened toward.
    Strict,
}

/// One public port and when it was last used.
struct Mapping {
    socket: Arc<UdpSocket>,
    /// The same port for sending: tokio's half says a fresh socket would
    /// block until it has been polled once.
    send: Arc<std::net::UdpSocket>,
    used: Instant,
}

#[derive(Default)]
struct Table {
    /// Open and Ordinary: the one public port.
    shared: Option<Mapping>,
    /// Ordinary: the addresses the host sent to, and when it last did.
    sent_to: HashMap<SocketAddr, Instant>,
    /// Strict: a public port per destination.
    per_destination: HashMap<SocketAddr, Mapping>,
    recv_waker: Option<Waker>,
}

pub struct Router {
    kind: RouterKind,
    idle: Duration,
    /// The host's own address behind the router.
    inner: SocketAddr,
    table: Mutex<Table>,
}

/// Each router's private network is its own, so two hosts behind routers
/// never share an inner address.
static NEXT_NETWORK: AtomicU8 = AtomicU8::new(1);

impl Router {
    pub fn new(kind: RouterKind, idle: Duration) -> io::Result<Arc<Self>> {
        let mut table = Table::default();
        let inner = match kind {
            // Open has no translation: the host is its public port.
            RouterKind::Open => {
                let mapping = bind_public(Instant::now())?;
                let addr = mapping.socket.local_addr()?;
                table.shared = Some(mapping);
                addr
            }
            RouterKind::Ordinary | RouterKind::Strict => {
                let network = NEXT_NETWORK.fetch_add(1, Ordering::Relaxed);
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, network, 0, 2)), 4433)
            }
        };
        Ok(Arc::new(Self {
            kind,
            idle,
            inner,
            table: Mutex::new(table),
        }))
    }

    pub fn kind(&self) -> RouterKind {
        self.kind
    }

    /// The public address the host's packets from `inner` to `dst` leave
    /// from now: what the far end sees them come from. None when no live
    /// mapping would carry them.
    pub fn public_addr_for(&self, inner: SocketAddr, dst: SocketAddr) -> Option<SocketAddr> {
        if inner != self.inner {
            return None;
        }
        let now = Instant::now();
        let table = self.lock();
        let mapping = match self.kind {
            RouterKind::Open => table.shared.as_ref(),
            RouterKind::Ordinary => table.shared.as_ref().filter(|m| self.live(m.used, now)),
            RouterKind::Strict => table
                .per_destination
                .get(&dst)
                .filter(|m| self.live(m.used, now)),
        }?;
        mapping.socket.local_addr().ok()
    }

    fn live(&self, used: Instant, now: Instant) -> bool {
        now.saturating_duration_since(used) < self.idle
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The public port a packet to `dst` leaves from, opening a fresh one
    /// where none is live.
    fn outbound(&self, dst: SocketAddr) -> io::Result<Arc<std::net::UdpSocket>> {
        let now = Instant::now();
        let mut table = self.lock();
        let (socket, fresh) = match self.kind {
            RouterKind::Open => (
                table
                    .shared
                    .as_ref()
                    .expect("open has its port")
                    .send
                    .clone(),
                false,
            ),
            RouterKind::Ordinary => {
                let fresh = !table
                    .shared
                    .as_ref()
                    .is_some_and(|m| self.live(m.used, now));
                if fresh {
                    // An expired mapping takes its filter with it.
                    table.sent_to.clear();
                    table.shared = Some(bind_public(now)?);
                }
                let mapping = table.shared.as_mut().expect("just made");
                mapping.used = now;
                let socket = mapping.send.clone();
                table.sent_to.insert(dst, now);
                (socket, fresh)
            }
            RouterKind::Strict => {
                let fresh = !table
                    .per_destination
                    .get(&dst)
                    .is_some_and(|m| self.live(m.used, now));
                if fresh {
                    table.per_destination.insert(dst, bind_public(now)?);
                }
                let mapping = table.per_destination.get_mut(&dst).expect("just made");
                mapping.used = now;
                (mapping.send.clone(), fresh)
            }
        };
        // A new port is one the receiver is not yet polling.
        if fresh && let Some(waker) = table.recv_waker.take() {
            waker.wake();
        }
        Ok(socket)
    }

    /// Whether the filter lets a packet from `from` in through `socket`,
    /// refreshing the mapping it passes.
    fn admits(&self, table: &mut Table, socket: &Arc<UdpSocket>, from: SocketAddr) -> bool {
        let now = Instant::now();
        match self.kind {
            RouterKind::Open => true,
            RouterKind::Ordinary => {
                let Some(mapping) = table.shared.as_mut() else {
                    return false;
                };
                let allowed = Arc::ptr_eq(&mapping.socket, socket)
                    && self.live(mapping.used, now)
                    && table
                        .sent_to
                        .get(&from)
                        .is_some_and(|sent| self.live(*sent, now));
                if allowed {
                    mapping.used = now;
                }
                allowed
            }
            RouterKind::Strict => match table.per_destination.get_mut(&from) {
                Some(mapping)
                    if Arc::ptr_eq(&mapping.socket, socket) && self.live(mapping.used, now) =>
                {
                    mapping.used = now;
                    true
                }
                _ => false,
            },
        }
    }
}

impl Datagrams for Router {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
        self.outbound(to)?.send_to(buf, to).map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        loop {
            let sockets: Vec<Arc<UdpSocket>> = {
                let mut table = self.lock();
                table.recv_waker = Some(cx.waker().clone());
                table
                    .shared
                    .iter()
                    .chain(table.per_destination.values())
                    .map(|mapping| mapping.socket.clone())
                    .collect()
            };
            let mut arrived = None;
            for socket in &sockets {
                let mut read = ReadBuf::new(buf);
                match socket.poll_recv_from(cx, &mut read) {
                    Poll::Ready(Ok(from)) => {
                        arrived = Some((socket.clone(), read.filled().len(), from));
                        break;
                    }
                    // A port's error (an ICMP unreachable echoed back) is
                    // that port's; the host hears of none, as behind a NAT.
                    Poll::Ready(Err(_)) | Poll::Pending => {}
                }
            }
            let Some((socket, len, from)) = arrived else {
                return Poll::Pending;
            };
            if self.admits(&mut self.lock(), &socket, from) {
                return Poll::Ready(Ok((len, from)));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.inner)
    }
}

fn bind_public(now: Instant) -> io::Result<Mapping> {
    let send = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    send.set_nonblocking(true)?;
    let socket = UdpSocket::from_std(send.try_clone()?)?;
    Ok(Mapping {
        socket: Arc::new(socket),
        send: Arc::new(send),
        used: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(30);

    async fn recv(router: &Router, within: Duration) -> Option<(Vec<u8>, SocketAddr)> {
        let mut buf = [0u8; 1500];
        tokio::time::timeout(
            within,
            std::future::poll_fn(|cx| router.poll_recv(cx, &mut buf)),
        )
        .await
        .ok()
        .map(|result| {
            let (len, from) = result.unwrap();
            (buf[..len].to_vec(), from)
        })
    }

    async fn socket() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.unwrap()
    }

    /// The host behind `router` reaches the relay, which reads the address
    /// it came from, then punches toward the peer, which answers that
    /// observed address.
    async fn punch(kind: RouterKind) -> Option<Vec<u8>> {
        let router = Router::new(kind, IDLE).unwrap();
        let (relay, peer) = (socket().await, socket().await);
        router
            .send_to(b"hello relay", relay.local_addr().unwrap())
            .unwrap();
        let mut buf = [0u8; 64];
        let (_, observed) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(
            router.public_addr_for(router.local_addr().unwrap(), relay.local_addr().unwrap()),
            Some(observed)
        );
        router
            .send_to(b"punch", peer.local_addr().unwrap())
            .unwrap();
        peer.send_to(b"punched reply", observed).await.unwrap();
        recv(&router, Duration::from_millis(300))
            .await
            .map(|(bytes, _)| bytes)
    }

    #[tokio::test]
    async fn ordinary_router_lets_a_punched_reply_through() {
        assert_eq!(
            punch(RouterKind::Ordinary).await.as_deref(),
            Some(&b"punched reply"[..])
        );
    }

    #[tokio::test]
    async fn strict_router_drops_a_punched_reply() {
        assert_eq!(punch(RouterKind::Strict).await, None);
    }

    #[tokio::test]
    async fn strict_router_opens_a_port_per_destination() {
        let router = Router::new(RouterKind::Strict, IDLE).unwrap();
        let inner = router.local_addr().unwrap();
        let (a, b) = (socket().await, socket().await);
        router.send_to(b"a", a.local_addr().unwrap()).unwrap();
        router.send_to(b"b", b.local_addr().unwrap()).unwrap();
        let to_a = router
            .public_addr_for(inner, a.local_addr().unwrap())
            .unwrap();
        let to_b = router
            .public_addr_for(inner, b.local_addr().unwrap())
            .unwrap();
        assert_ne!(to_a, to_b);
        // The exact address answering on its own port gets through.
        a.send_to(b"from a", to_a).await.unwrap();
        assert_eq!(
            recv(&router, Duration::from_secs(5)).await,
            Some((b"from a".to_vec(), a.local_addr().unwrap()))
        );
    }

    #[tokio::test]
    async fn an_unsolicited_packet_is_dropped_and_an_open_host_is_reachable() {
        let stranger = socket().await;
        for kind in [RouterKind::Ordinary, RouterKind::Strict] {
            let router = Router::new(kind, IDLE).unwrap();
            let elsewhere = socket().await;
            router
                .send_to(b"out", elsewhere.local_addr().unwrap())
                .unwrap();
            let public = router
                .public_addr_for(
                    router.local_addr().unwrap(),
                    elsewhere.local_addr().unwrap(),
                )
                .unwrap();
            stranger.send_to(b"unasked", public).await.unwrap();
            assert_eq!(
                recv(&router, Duration::from_millis(300)).await,
                None,
                "{kind:?}"
            );
            // Behind translation the host's own address reaches nobody.
            assert!(
                router
                    .local_addr()
                    .unwrap()
                    .ip()
                    .to_string()
                    .starts_with("10.")
            );
        }
        let open = Router::new(RouterKind::Open, IDLE).unwrap();
        stranger
            .send_to(b"anyone", open.local_addr().unwrap())
            .await
            .unwrap();
        assert_eq!(
            recv(&open, Duration::from_secs(5)).await,
            Some((b"anyone".to_vec(), stranger.local_addr().unwrap()))
        );
    }

    #[tokio::test]
    async fn an_idle_mapping_expires_and_the_next_packet_gets_a_new_port() {
        let idle = Duration::from_millis(200);
        let router = Router::new(RouterKind::Ordinary, idle).unwrap();
        let inner = router.local_addr().unwrap();
        let peer = socket().await;
        let peer_addr = peer.local_addr().unwrap();
        router.send_to(b"out", peer_addr).unwrap();
        let before = router.public_addr_for(inner, peer_addr).unwrap();

        tokio::time::sleep(idle + Duration::from_millis(100)).await;
        assert_eq!(router.public_addr_for(inner, peer_addr), None);
        peer.send_to(b"too late", before).await.unwrap();
        assert_eq!(recv(&router, Duration::from_millis(300)).await, None);

        router.send_to(b"again", peer_addr).unwrap();
        let after = router.public_addr_for(inner, peer_addr).unwrap();
        assert_ne!(before, after);
    }
}
