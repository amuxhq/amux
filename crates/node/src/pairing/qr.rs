use std::net::SocketAddr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::HostId;

/// What the app's own pairing link starts with; the rest is the
/// invitation, base64.
pub const PAIR_LINK: &str = "amux://pair?payload=";

/// The path of the cloud's pairing page. The invitation follows it after
/// `#`, so it never reaches the server: a phone with the app opens the link
/// in the app, and one without is sent to the App Store.
const PAIR_PAGE: &str = "/p#";

const QR_SECRET_LEN: usize = 32;

/// What a one-shot QR pairing code carries: the responder, its one-shot
/// secret, the addresses it can be dialled at directly and the cloud it
/// names if it has one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QrPairingPayload {
    pub host_id: HostId,
    pub secret: Vec<u8>,
    pub addrs: Vec<SocketAddr>,
    pub cloud_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum QrPairingError {
    #[error("a pairing link is https://<cloud>/p#<invitation> or starts with {PAIR_LINK}")]
    NotALink,
    #[error("the pairing link's payload is not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("the pairing link's payload is not text")]
    NotText,
    #[error("failed to decode QR pairing payload: {0}")]
    Json(#[from] serde_json::Error),
    #[error("QR pairing host_id {value} is invalid: {source}")]
    InvalidHostId { value: String, source: uuid::Error },
    #[error("QR pairing {field} must be {expected} bytes, got {actual}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("QR pairing address {value:?} is invalid: {source}")]
    InvalidAddress {
        value: String,
        source: std::net::AddrParseError,
    },
}

#[derive(Deserialize, Serialize)]
struct WireQrPairingPayload {
    host_id: String,
    secret: Vec<u8>,
    addrs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_url: Option<String>,
}

/// The invitation a responder's QR code carries, from its parts: the
/// responder, the addresses it can be dialled at directly, the cloud it names
/// if it has one, and the one-shot secret. A responder with no account still
/// issues an invitation: the addresses alone are enough on a shared network.
pub fn encode_qr_pairing_invitation(
    host_id: HostId,
    addrs: &[SocketAddr],
    cloud_url: Option<&str>,
    secret: &[u8],
) -> Result<String, QrPairingError> {
    let payload = WireQrPairingPayload {
        host_id: host_id.to_string(),
        secret: secret.to_vec(),
        addrs: addrs.iter().map(ToString::to_string).collect(),
        cloud_url: cloud_url.map(ToOwned::to_owned),
    };
    Ok(serde_json::to_string(&payload)?)
}

/// The app's own link for an invitation: `amux://pair?payload=…`.
pub fn pair_link(invitation_json: &str) -> String {
    format!(
        "{PAIR_LINK}{}",
        URL_SAFE_NO_PAD.encode(invitation_json.as_bytes())
    )
}

/// The link a QR code shows for an invitation: the cloud's pairing page,
/// `https://amux.sh/p#…`, so a phone's camera opens it whether or not the
/// app is installed yet.
pub fn qr_link(cloud_url: &str, invitation_json: &str) -> String {
    format!(
        "{}{PAIR_PAGE}{}",
        cloud_url.trim_end_matches('/'),
        URL_SAFE_NO_PAD.encode(invitation_json.as_bytes())
    )
}

/// The invitation a pairing link carries: the cloud page's link, whatever
/// cloud it names, or the app's own.
pub fn parse_pair_link(link: &str) -> Result<QrPairingPayload, QrPairingError> {
    let link = link.trim();
    let encoded = match link.strip_prefix(PAIR_LINK) {
        Some(encoded) => encoded,
        None => link
            .strip_prefix("https://")
            .or_else(|| link.strip_prefix("http://"))
            .and_then(|rest| rest.split_once('/'))
            .and_then(|(host, path)| (!host.is_empty()).then_some(path))
            .and_then(|path| path.strip_prefix(&PAIR_PAGE[1..]))
            .ok_or(QrPairingError::NotALink)?,
    };
    let json = URL_SAFE_NO_PAD.decode(encoded)?;
    let json = std::str::from_utf8(&json).map_err(|_| QrPairingError::NotText)?;
    parse_qr_pairing_payload(json)
}

pub fn parse_qr_pairing_payload(payload: &str) -> Result<QrPairingPayload, QrPairingError> {
    let payload: WireQrPairingPayload = serde_json::from_str(payload)?;
    let host_id =
        HostId::parse_str(&payload.host_id).map_err(|source| QrPairingError::InvalidHostId {
            value: payload.host_id.clone(),
            source,
        })?;
    validate_qr_payload_bytes("secret", &payload.secret)?;
    let addrs = payload
        .addrs
        .into_iter()
        .map(|value| {
            value
                .parse()
                .map_err(|source| QrPairingError::InvalidAddress { value, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(QrPairingPayload {
        host_id,
        secret: payload.secret,
        addrs,
        cloud_url: payload.cloud_url,
    })
}

fn validate_qr_payload_bytes(field: &'static str, bytes: &[u8]) -> Result<(), QrPairingError> {
    if bytes.len() == QR_SECRET_LEN {
        Ok(())
    } else {
        Err(QrPairingError::InvalidLength {
            field,
            expected: QR_SECRET_LEN,
            actual: bytes.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_pairing_payload_round_trips() {
        let payload = encode_qr_pairing_invitation(
            HostId::from_u128(1),
            &["192.0.2.4:9001".parse().unwrap()],
            Some("https://relay.example"),
            &[9; 32],
        )
        .unwrap();
        let parsed = parse_qr_pairing_payload(&payload).unwrap();

        assert_eq!(parsed.host_id, HostId::from_u128(1));
        assert_eq!(parsed.cloud_url.as_deref(), Some("https://relay.example"));
        assert_eq!(parsed.addrs, vec!["192.0.2.4:9001".parse().unwrap()]);
        assert_eq!(parsed.secret, vec![9; 32]);
    }

    #[test]
    fn both_forms_of_a_pairing_link_carry_the_invitation() {
        let invitation =
            encode_qr_pairing_invitation(HostId::from_u128(1), &[], None, &[9; 32]).unwrap();
        let web = qr_link("https://amux.sh/", &invitation);
        assert!(web.starts_with("https://amux.sh/p#"), "{web}");
        assert!(!web.contains('='), "no padding: {web}");
        for link in [
            web,
            pair_link(&invitation),
            qr_link("http://127.0.0.1:9", &invitation),
        ] {
            let parsed = parse_pair_link(&link).unwrap();
            assert_eq!(parsed.host_id, HostId::from_u128(1));
            assert_eq!(parsed.secret, vec![9; 32]);
        }
        for link in [
            "https://amux.sh/get#abc",
            "https:///p#abc",
            "amux://pairing?payload=abc",
        ] {
            assert!(matches!(
                parse_pair_link(link),
                Err(QrPairingError::NotALink)
            ));
        }
    }

    #[test]
    fn qr_pairing_payload_carries_no_pubkey() {
        let payload =
            encode_qr_pairing_invitation(HostId::from_u128(1), &[], None, &[9; 32]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(value.get("pubkey").is_none());
        assert!(value.get("name").is_none());
    }

    #[test]
    fn qr_pairing_payload_validates_secret_length() {
        let payload = serde_json::json!({
            "host_id": "00000000-0000-0000-0000-000000000001",
            "cloud_url": "https://amux.sh",
            "secret": [9],
            "addrs": [],
        })
        .to_string();

        assert!(matches!(
            parse_qr_pairing_payload(&payload),
            Err(QrPairingError::InvalidLength {
                field: "secret",
                ..
            })
        ));
    }
}
