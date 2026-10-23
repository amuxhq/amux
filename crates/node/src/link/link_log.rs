//! The link log: one INFO line for each link a host's links go through,
//! coming up, going down or moving to another path, naming the peer, the
//! path, whether the link resumed a session and how its early data fared,
//! and one line each time the host's network changes under all of them.
//! Daemons and the phone write it to their logs; stories and journeys read
//! it back with [`LinkEvent::parse`] to say which path carried a link.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use crate::HostId;

/// The target every line is written under, which a log filter names to
/// keep only these.
pub const TARGET: &str = "amux::link";
/// The message every link's line carries.
const MESSAGE: &str = "link";
/// The message a network change's line carries.
const REBIND: &str = "rebind";

/// What happened to the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkTransition {
    Up,
    Down,
    PathChanged,
    /// The peer's catch-up arrived: on a launch's link, the inventory's
    /// answer to the call its first flight carried.
    CaughtUp,
}

/// The path a link's packets take, as the log names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathKind {
    /// An address on a network both hosts are on: private, unique-local,
    /// link-local or loopback.
    Lan,
    /// A public address of the peer's.
    Direct,
    /// An address the relay saw the peer's packets come from, reached by
    /// punching through both routers.
    Punched,
    /// Through the relay over UDP.
    RelayUdp,
    /// Through the relay over TLS on TCP.
    RelayTcp,
    /// Over an SSH session into the peer's link socket.
    Ssh,
    /// Through a paired daemon forwarding the link's packets.
    ViaDaemon,
}

impl PathKind {
    /// The kind of a direct path to `addr`: on the same network, or not.
    pub fn of_addr(addr: SocketAddr) -> Self {
        let local = match addr.ip().to_canonical() {
            IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
            IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.segments()[0] & 0xfe00 == 0xfc00
                    || ip.segments()[0] & 0xffc0 == 0xfe80
            }
        };
        if local { Self::Lan } else { Self::Direct }
    }
}

/// What became of the early data a resumed link sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EarlyData {
    /// None was sent: a full handshake.
    None,
    Accepted,
    Rejected,
}

/// One line of the link log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkEvent {
    pub peer: HostId,
    pub event: LinkTransition,
    pub path: PathKind,
    pub resumed: bool,
    pub early_data: EarlyData,
    /// From the link's first packet to the arrival of the peer's catch-up,
    /// where the link carried one.
    pub first_packet_to_catch_up_ms: Option<u32>,
}

impl LinkEvent {
    /// Writes the line for a link of `host`'s.
    pub fn write(&self, host: HostId) {
        tracing::info!(
            target: TARGET,
            %host,
            peer = %self.peer,
            event = %self.event,
            path = %self.path,
            resumed = self.resumed,
            early_data = %self.early_data,
            first_packet_to_catch_up_ms = %CatchUp(self.first_packet_to_catch_up_ms),
            "{MESSAGE}"
        );
    }

    /// Reads a line back as a log holds it: the host whose link it is, and
    /// the event. Anything else in the log reads as None.
    pub fn parse(line: &str) -> Option<(HostId, Self)> {
        let (_, fields) = line.split_once(&format!("{TARGET}: {MESSAGE} "))?;
        let mut host = None;
        let mut event = Self {
            peer: HostId::nil(),
            event: LinkTransition::Up,
            path: PathKind::Lan,
            resumed: false,
            early_data: EarlyData::None,
            first_packet_to_catch_up_ms: None,
        };
        let mut seen = 0;
        for field in fields.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.trim_matches('"');
            match key {
                "host" => host = Some(value.parse().ok()?),
                "peer" => event.peer = value.parse().ok()?,
                "event" => event.event = value.parse().ok()?,
                "path" => event.path = value.parse().ok()?,
                "resumed" => event.resumed = value.parse().ok()?,
                "early_data" => event.early_data = value.parse().ok()?,
                "first_packet_to_catch_up_ms" => {
                    event.first_packet_to_catch_up_ms = value.parse::<CatchUp>().ok()?.0;
                }
                _ => continue,
            }
            seen += 1;
        }
        (seen == 7).then_some((host?, event))
    }
}

/// The line written when the host's network changed: whether its link
/// socket was bound afresh, and how many path machines were told to probe
/// their paths again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rebind {
    pub rebound: bool,
    pub machines_told: usize,
}

impl Rebind {
    /// Writes the line for `host`'s network.
    pub fn write(&self, host: HostId) {
        tracing::info!(
            target: TARGET,
            %host,
            rebound = self.rebound,
            machines_told = self.machines_told,
            "{REBIND}"
        );
    }

    /// Reads a line back as a log holds it: the host whose network
    /// changed, and what the change did. Anything else reads as None.
    pub fn parse(line: &str) -> Option<(HostId, Self)> {
        let (_, fields) = line.split_once(&format!("{TARGET}: {REBIND} "))?;
        let (mut host, mut rebound, mut machines_told) = (None, None, None);
        for field in fields.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.trim_matches('"');
            match key {
                "host" => host = Some(value.parse().ok()?),
                "rebound" => rebound = Some(value.parse().ok()?),
                "machines_told" => machines_told = Some(value.parse().ok()?),
                _ => {}
            }
        }
        Some((
            host?,
            Self {
                rebound: rebound?,
                machines_told: machines_told?,
            },
        ))
    }
}

/// The catch-up's milliseconds as the line spells them: a number, or
/// `none`.
struct CatchUp(Option<u32>);

impl fmt::Display for CatchUp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("none"),
        }
    }
}

impl FromStr for CatchUp {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        match text {
            "none" => Ok(Self(None)),
            ms => ms.parse().map(|ms| Self(Some(ms))).map_err(|_| ()),
        }
    }
}

/// Each enum spelled as the line writes it, both ways.
macro_rules! spelled {
    ($kind:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl fmt::Display for $kind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => $text),+ })
            }
        }

        impl FromStr for $kind {
            type Err = ();
            fn from_str(text: &str) -> Result<Self, ()> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(()),
                }
            }
        }
    };
}

spelled!(LinkTransition {
    Up => "up",
    Down => "down",
    PathChanged => "path_changed",
    CaughtUp => "caught_up",
});
spelled!(PathKind {
    Lan => "lan",
    Direct => "direct",
    Punched => "punched",
    RelayUdp => "relay_udp",
    RelayTcp => "relay_tcp",
    Ssh => "ssh",
    ViaDaemon => "via_daemon",
});
spelled!(EarlyData { None => "none", Accepted => "accepted", Rejected => "rejected" });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_log_reads_back_every_spelling_of_a_line() {
        let host = HostId::from_u128(1);
        let event = LinkEvent {
            peer: HostId::from_u128(2),
            event: LinkTransition::PathChanged,
            path: PathKind::RelayTcp,
            resumed: true,
            early_data: EarlyData::Rejected,
            first_packet_to_catch_up_ms: Some(41),
        };
        // As the daemon's log writes it, inside a span whose fields come
        // before the target and are not the line's.
        let line = format!(
            "2026-10-21T09:00:00.000000Z  INFO dial{{peer=x}}: amux::link: link host={host} \
             peer={} event=path_changed path=relay_tcp resumed=true early_data=rejected \
             first_packet_to_catch_up_ms=41",
            event.peer
        );
        assert_eq!(LinkEvent::parse(&line), Some((host, event)));
        let none = line.replace("=41", "=none");
        assert_eq!(
            LinkEvent::parse(&none).map(|(_, event)| event.first_packet_to_catch_up_ms),
            Some(None)
        );
        assert_eq!(LinkEvent::parse(&line.replace(" path=relay_tcp", "")), None);
        assert_eq!(
            LinkEvent::parse("INFO node::edge: listening for direct links"),
            None
        );
    }

    #[test]
    fn a_network_change_reads_back_as_a_rebind_and_never_as_a_link() {
        let host = HostId::from_u128(1);
        let line = format!(
            "2026-10-21T09:00:00.000000Z  INFO amux::link: rebind host={host} rebound=true \
             machines_told=3"
        );
        let rebind = Rebind {
            rebound: true,
            machines_told: 3,
        };
        assert_eq!(Rebind::parse(&line), Some((host, rebind)));
        assert_eq!(LinkEvent::parse(&line), None);
        assert_eq!(Rebind::parse(&line.replace(" rebound=true", "")), None);
    }

    #[test]
    fn link_log_names_a_direct_address_on_the_same_network_lan() {
        let kind = |addr: &str| PathKind::of_addr(addr.parse().unwrap());
        for lan in [
            "192.168.1.4:5000",
            "10.0.0.2:5000",
            "127.0.0.1:5000",
            "[::1]:5000",
            "[fd00::1]:5000",
            "[fe80::1]:5000",
            "[::ffff:192.168.1.4]:5000",
        ] {
            assert_eq!(kind(lan), PathKind::Lan, "{lan}");
        }
        for direct in ["203.0.113.9:5000", "[2001:db8::1]:5000"] {
            assert_eq!(kind(direct), PathKind::Direct, "{direct}");
        }
    }
}
