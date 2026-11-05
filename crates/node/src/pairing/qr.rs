//! The invitation a pairing QR code carries and the link it travels in:
//! `https://<cloud>/p#<invitation>`, the invitation a few bytes in
//! URL-safe base64 without padding.
//!
//! The bytes are, in order: a version (1), the host id (16), the one-shot
//! secret (32), the listener's port (two, big-endian) and then any number
//! of IPv4 addresses (four each), all at that port. Binary rather than JSON
//! because the code is drawn in a terminal too, where every module costs a
//! cell: a realistic invitation fits QR version 5. There is no cloud in it:
//! the link's own origin is the cloud's page.

use std::net::{Ipv4Addr, SocketAddr};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::HostId;

/// The path of the cloud's pairing page. The invitation follows it after
/// `#`, so it never reaches the server: a phone with the app opens the link
/// in the app, and one without is sent to the App Store.
const PAIR_PAGE: &str = "/p#";

/// The only invitation layout there is.
pub const INVITATION_VERSION: u8 = 1;

pub const QR_SECRET_LEN: usize = 32;

/// Version, host id, secret and port: what every invitation holds before
/// its addresses.
const HEAD_LEN: usize = 1 + 16 + QR_SECRET_LEN + 2;

/// What a one-shot QR pairing code carries: the responder, its one-shot
/// secret, and the IPv4 addresses it can be dialled at directly, all at one
/// port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QrPairingPayload {
    pub host_id: HostId,
    pub secret: [u8; QR_SECRET_LEN],
    pub port: u16,
    pub addrs: Vec<Ipv4Addr>,
}

#[derive(Debug, thiserror::Error)]
pub enum QrPairingError {
    #[error("a pairing link is https://<cloud>/p#<invitation>")]
    NotALink,
    #[error("the pairing link's invitation is not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("the pairing link's invitation is empty")]
    Empty,
    #[error("the pairing link's invitation is version {0}; this build reads version 1")]
    Version(u8),
    #[error("the pairing link's invitation is {0} bytes, too short for a host id, secret and port")]
    Truncated(usize),
    #[error(
        "the pairing link's addresses take {0} bytes, not a whole number of four-byte IPv4 \
         addresses"
    )]
    PartialAddress(usize),
    #[error("the pairing link names addresses at port 0")]
    NoPort,
}

impl QrPairingPayload {
    /// Where the invitation says its host can be dialled.
    pub fn dial_addrs(&self) -> Vec<SocketAddr> {
        self.addrs
            .iter()
            .map(|ip| SocketAddr::from((*ip, self.port)))
            .collect()
    }

    /// The invitation's bytes, before the link's base64.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEAD_LEN + 4 * self.addrs.len());
        bytes.push(INVITATION_VERSION);
        bytes.extend_from_slice(self.host_id.as_bytes());
        bytes.extend_from_slice(&self.secret);
        bytes.extend_from_slice(&self.port.to_be_bytes());
        for ip in &self.addrs {
            bytes.extend_from_slice(&ip.octets());
        }
        bytes
    }

    /// The invitation the bytes hold, or why they hold none.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, QrPairingError> {
        let (&version, rest) = bytes.split_first().ok_or(QrPairingError::Empty)?;
        if version != INVITATION_VERSION {
            return Err(QrPairingError::Version(version));
        }
        if bytes.len() < HEAD_LEN {
            return Err(QrPairingError::Truncated(bytes.len()));
        }
        let (host_id, rest) = rest.split_at(16);
        let (secret, rest) = rest.split_at(QR_SECRET_LEN);
        let (port, addrs) = rest.split_at(2);
        if addrs.len() % 4 != 0 {
            return Err(QrPairingError::PartialAddress(addrs.len()));
        }
        let port = u16::from_be_bytes([port[0], port[1]]);
        if port == 0 && !addrs.is_empty() {
            return Err(QrPairingError::NoPort);
        }
        Ok(Self {
            host_id: HostId::from_bytes(host_id.try_into().expect("sixteen bytes")),
            secret: secret.try_into().expect("the secret's length"),
            port,
            addrs: addrs
                .as_chunks::<4>()
                .0
                .iter()
                .map(|ip| Ipv4Addr::from(*ip))
                .collect(),
        })
    }
}

/// The link a QR code shows for an invitation: the cloud's pairing page,
/// `https://amux.sh/p#…`, so a phone's camera opens it whether or not the
/// app is installed yet.
pub fn qr_link(cloud_url: &str, invitation: &QrPairingPayload) -> String {
    format!(
        "{}{PAIR_PAGE}{}",
        cloud_url.trim_end_matches('/'),
        URL_SAFE_NO_PAD.encode(invitation.to_bytes())
    )
}

/// The invitation a pairing link carries, whatever cloud's page it is.
pub fn parse_pair_link(link: &str) -> Result<QrPairingPayload, QrPairingError> {
    let link = link.trim();
    let encoded = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(host, path)| (!host.is_empty()).then_some(path))
        .and_then(|path| path.strip_prefix(&PAIR_PAGE[1..]))
        .ok_or(QrPairingError::NotALink)?;
    QrPairingPayload::from_bytes(&URL_SAFE_NO_PAD.decode(encoded)?)
}

/// The addresses an invitation names, of those the listener answers at: one
/// private home-network IPv4 address (RFC 1918), the one the default route
/// leaves by when it is among them, and one Tailscale address
/// (100.64.0.0/10). Nothing else: on the home network the phone finds the
/// host by its id through discovery, and the private address is for a
/// network that drops discovery's multicast; Tailscale carries no multicast
/// at all, so its address has to travel in the code. Loopback, link-local
/// and IPv6 addresses would only make the code bigger.
pub fn invitation_addrs(listed: &[SocketAddr], routed: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let ipv4: Vec<Ipv4Addr> = listed
        .iter()
        .filter_map(|addr| match addr.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        })
        .collect();
    let home = routed
        .filter(|ip| ip.is_private() && ipv4.contains(ip))
        .or_else(|| ipv4.iter().copied().find(Ipv4Addr::is_private));
    let tailscale = ipv4.iter().copied().find(is_tailscale);
    home.into_iter().chain(tailscale).collect()
}

/// Tailscale's addresses: the shared address space of RFC 6598.
fn is_tailscale(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (b & 0xc0) == 64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation(addrs: &[[u8; 4]]) -> QrPairingPayload {
        QrPairingPayload {
            host_id: HostId::from_u128(0x8c3e_1a2b_4d5f_4a6b_8c7d_9e0f_1a2b_3c4d),
            secret: [9; 32],
            port: 7420,
            addrs: addrs.iter().map(|ip| Ipv4Addr::from(*ip)).collect(),
        }
    }

    #[test]
    fn an_invitation_round_trips_through_its_link() {
        let sent = invitation(&[[192, 168, 1, 24], [100, 101, 102, 103]]);
        let link = qr_link("https://amux.sh/", &sent);
        assert!(link.starts_with("https://amux.sh/p#"), "{link}");
        assert!(!link.contains('='), "no padding: {link}");
        let read = parse_pair_link(&link).unwrap();
        assert_eq!(read, sent);
        assert_eq!(
            read.dial_addrs(),
            [
                "192.168.1.24:7420".parse::<SocketAddr>().unwrap(),
                "100.101.102.103:7420".parse().unwrap()
            ]
        );
        // Any cloud's page, and an invitation that names no address.
        let bare = invitation(&[]);
        let read = parse_pair_link(&qr_link("http://127.0.0.1:9", &bare)).unwrap();
        assert_eq!(read, bare);
        assert!(read.dial_addrs().is_empty());
    }

    /// A first run's invitation, naming a home and a Tailscale address, is
    /// 59 bytes and a 97-character link: QR version 5, which the terminal's
    /// own test pins.
    #[test]
    fn a_realistic_link_is_97_characters() {
        let sent = invitation(&[[192, 168, 1, 24], [100, 101, 102, 103]]);
        assert_eq!(sent.to_bytes().len(), 59);
        let link = qr_link("https://amux.sh", &sent);
        assert_eq!(link.len(), 97, "{link}");
    }

    /// The iPhone app reads invitations itself, to refuse a malformed link
    /// before it becomes a screen; its tests pin these same links.
    #[test]
    fn the_links_the_phone_app_reads_are_these() {
        let sent = |addrs: Vec<Ipv4Addr>| QrPairingPayload {
            host_id: "3a1f9d2c-0000-4000-8000-000000000001".parse().unwrap(),
            secret: std::array::from_fn(|at| at as u8 + 1),
            port: 9001,
            addrs,
        };
        assert_eq!(
            qr_link("https://amux.sh", &sent(Vec::new())),
            "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMp"
        );
        assert_eq!(
            qr_link("https://amux.sh", &sent(vec![Ipv4Addr::new(192, 0, 2, 4)])),
            "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMpwAACBA"
        );
    }

    #[test]
    fn a_malformed_link_says_what_is_wrong() {
        let good = invitation(&[[10, 0, 0, 2]]).to_bytes();
        let link = |bytes: &[u8]| format!("https://amux.sh/p#{}", URL_SAFE_NO_PAD.encode(bytes));
        for not in [
            "https://amux.sh/get#abc",
            "https:///p#abc",
            "amux://pair?payload=abc",
            "ftp://amux.sh/p#abc",
        ] {
            assert!(
                matches!(parse_pair_link(not), Err(QrPairingError::NotALink)),
                "{not}"
            );
        }
        assert!(matches!(
            parse_pair_link("https://amux.sh/p#%%%"),
            Err(QrPairingError::Base64(_))
        ));
        assert!(matches!(
            parse_pair_link(&link(&[])),
            Err(QrPairingError::Empty)
        ));
        let mut later = good.clone();
        later[0] = 2;
        assert!(matches!(
            parse_pair_link(&link(&later)),
            Err(QrPairingError::Version(2))
        ));
        assert!(matches!(
            parse_pair_link(&link(&good[..HEAD_LEN - 1])),
            Err(QrPairingError::Truncated(50))
        ));
        assert!(matches!(
            parse_pair_link(&link(&good[..good.len() - 1])),
            Err(QrPairingError::PartialAddress(3))
        ));
        let mut portless = good;
        portless[HEAD_LEN - 2..HEAD_LEN].copy_from_slice(&[0, 0]);
        assert!(matches!(
            parse_pair_link(&link(&portless)),
            Err(QrPairingError::NoPort)
        ));
    }

    #[test]
    fn an_invitation_names_one_home_address_and_one_tailscale_address() {
        let listed: Vec<SocketAddr> = [
            "10.0.0.5:7420",
            "100.64.0.1:7420",
            "100.100.1.2:7420",
            "127.0.0.1:7420",
            "169.254.3.4:7420",
            "172.16.0.9:7420",
            "192.168.1.24:7420",
            "203.0.113.7:7420",
            "[::1]:7420",
            "[2001:db8::1]:7420",
            "[fd7a:115c:a1e0::1]:7420",
        ]
        .iter()
        .map(|addr| addr.parse().unwrap())
        .collect();
        let ips = |ips: &[[u8; 4]]| ips.iter().map(|ip| Ipv4Addr::from(*ip)).collect::<Vec<_>>();
        // The default route's address, when it is a home one listed here.
        assert_eq!(
            invitation_addrs(&listed, Some(Ipv4Addr::new(192, 168, 1, 24))),
            ips(&[[192, 168, 1, 24], [100, 64, 0, 1]])
        );
        // Otherwise the first home address listed.
        for routed in [
            None,
            Some(Ipv4Addr::new(100, 64, 0, 1)),
            Some(Ipv4Addr::new(192, 168, 9, 9)),
        ] {
            assert_eq!(
                invitation_addrs(&listed, routed),
                ips(&[[10, 0, 0, 5], [100, 64, 0, 1]])
            );
        }
        // Loopback, link-local, public and IPv6 addresses name none.
        assert!(invitation_addrs(&listed[3..5], None).is_empty());
        assert!(invitation_addrs(&listed[7..], None).is_empty());
        // Just past Tailscale's range is not Tailscale.
        assert!(invitation_addrs(&["100.128.0.1:1".parse().unwrap()], None).is_empty());
    }
}
