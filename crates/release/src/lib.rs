//! A release channel's manifest and what is done with it: signing by the
//! release tool, verification by the daemon's supervisor, and the choice of
//! build a host installs.
//!
//! A channel is one manifest, keyed by target triple, naming a version, an
//! artifact URL, the artifact's SHA-256 and an Ed25519 signature over
//! [`signed_message`]. The signature binds the version to the hash, so a
//! tampered manifest cannot relabel an old signed artifact as a newer
//! release. The stable manifest may carry a rollout percentage; a host is
//! inside it when hash(host id) mod 100 is under the number, so a host's
//! place in every rollout is fixed and nothing is stored.
//!
//! Shared by the daemon and the release tool, so the tool signs what the
//! daemon verifies without building the daemon.

use std::collections::BTreeMap;

/// The private half of the key debug builds of the daemon trust when the
/// build names no release key, for tests and test harnesses that sign
/// their own releases.
#[cfg(feature = "test-key")]
pub const TEST_SEED: [u8; 32] = [
    0xfe, 0x91, 0xb0, 0xb9, 0x1e, 0xa7, 0x94, 0x55, 0xea, 0x7c, 0xb1, 0xa7, 0x83, 0xec, 0x33, 0x47,
    0x28, 0x61, 0x70, 0x17, 0x17, 0xc9, 0x9b, 0x2a, 0xaa, 0x45, 0xe9, 0x43, 0xbb, 0xd6, 0x48, 0x12,
];
use std::fmt::Write as _;

use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A channel's manifest, signed whole: the channel it is for, who takes it
/// and every build in it are under the one signature, so the server that
/// hands it out can neither move a build to another channel nor widen a
/// rollout.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// The channel this manifest is for; a supervisor on another refuses it.
    pub channel: String,
    /// The share of hosts, out of 100, that take this release; absent is
    /// every host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<u8>,
    pub targets: BTreeMap<String, Release>,
    /// Ed25519 over [`signed_message`], base64.
    #[serde(default)]
    pub signature: String,
}

/// One target's build in a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// The artifact's SHA-256, hex.
    pub sha256: String,
    /// The artifact's length in bytes: what a download may be, known
    /// before it is read.
    pub size: u64,
}

/// What a manifest's signature covers: the channel, the rollout (100 when
/// absent) and, per target in name order, the version, the url, the
/// artifact's hash and its size.
pub fn signed_message(manifest: &Manifest) -> Vec<u8> {
    let mut message = format!(
        "amux manifest\n{}\n{}\n",
        manifest.channel,
        manifest.rollout.unwrap_or(100)
    );
    for (target, release) in &manifest.targets {
        let _ = write!(
            message,
            "{target}\n{}\n{}\n{}\n{}\n",
            release.version,
            release.url,
            release.sha256.to_ascii_lowercase(),
            release.size
        );
    }
    message.into_bytes()
}

/// Signs a manifest with the private key's 32-byte seed; the publishing
/// side of [`verify_manifest`]. The manifest's own `signature` field is not
/// part of what is signed.
pub fn sign(seed: &[u8; 32], manifest: &Manifest) -> String {
    use base64::Engine as _;
    let pair = Ed25519KeyPair::from_seed_unchecked(seed).expect("an Ed25519 seed is any 32 bytes");
    let signature = pair.sign(&signed_message(manifest));
    base64::engine::general_purpose::STANDARD.encode(signature.as_ref())
}

/// A fresh seed from the system's randomness, for a new release key.
pub fn new_seed() -> [u8; 32] {
    use ring::rand::SecureRandom as _;
    let mut seed = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut seed)
        .expect("the system's randomness is available");
    seed
}

/// The public half of the key whose seed this is; what a build compiles in
/// as `AMUX_RELEASE_PUBLIC_KEY` to trust releases signed with the seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    use ring::signature::KeyPair as _;
    let pair = Ed25519KeyPair::from_seed_unchecked(seed).expect("an Ed25519 seed is any 32 bytes");
    let mut key = [0u8; 32];
    key.copy_from_slice(pair.public_key().as_ref());
    key
}

/// Reads 64 hex digits as a key or seed.
pub fn parse_key(text: &str) -> Option<[u8; 32]> {
    parse_hex::<32>(text)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn sha256_hex(digest: &[u8]) -> String {
    hex(digest)
}

pub fn sha256_of(bytes: &[u8]) -> String {
    sha256_hex(&Sha256::digest(bytes))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("the manifest is for the {actual} channel, not {expected}")]
    Channel { expected: String, actual: String },
    #[error("the manifest's signature does not verify against this build's key")]
    Signature,
    #[error("the artifact's hash {actual} is not the manifest's {expected}")]
    Hash { expected: String, actual: String },
    #[error("the artifact is {actual} bytes, not the manifest's {expected}")]
    Size { expected: u64, actual: u64 },
}

/// Checks that a manifest is for `channel` and that its signature verifies
/// against `key`; done before anything in it is believed.
pub fn verify_manifest(
    manifest: &Manifest,
    channel: &str,
    key: &[u8; 32],
) -> Result<(), VerifyError> {
    use base64::Engine as _;
    if manifest.channel != channel {
        return Err(VerifyError::Channel {
            expected: channel.to_owned(),
            actual: manifest.channel.clone(),
        });
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(manifest.signature.trim())
        .map_err(|_| VerifyError::Signature)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&signed_message(manifest), &signature)
        .map_err(|_| VerifyError::Signature)
}

/// Checks a downloaded artifact's hash and size against its entry in a
/// verified manifest.
pub fn verify_artifact(
    release: &Release,
    actual_sha256_hex: &str,
    actual_size: u64,
) -> Result<(), VerifyError> {
    if release.size != actual_size {
        return Err(VerifyError::Size {
            expected: release.size,
            actual: actual_size,
        });
    }
    if !release.sha256.eq_ignore_ascii_case(actual_sha256_hex) {
        return Err(VerifyError::Hash {
            expected: release.sha256.clone(),
            actual: actual_sha256_hex.to_owned(),
        });
    }
    Ok(())
}

/// Whether a host takes a release published to `percent` of hosts.
pub fn inside_rollout(host: Option<&Uuid>, percent: Option<u8>) -> bool {
    let Some(percent) = percent.filter(|percent| *percent < 100) else {
        return true;
    };
    // A host with no id yet has no fixed place, so it waits for a full
    // rollout rather than take a place at random.
    let Some(host) = host else {
        return false;
    };
    rollout_slot(host) < u64::from(percent)
}

/// hash(host id) mod 100: the host's fixed place in every rollout.
pub fn rollout_slot(host: &Uuid) -> u64 {
    let digest = Sha256::digest(host.hyphenated().to_string().as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head) % 100
}

/// What a manifest means for this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Install {
        release: Release,
        version: Version,
    },
    /// Nothing to do, and why.
    Skip(Skip),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    NoBuildForTarget,
    UnreadableVersion(String),
    NotNewer(Version),
    Rejected(Version),
    OutsideRollout,
}

/// Picks the build to install: only one newer than the running version, not
/// the build that was rolled back, and only inside the rollout.
pub fn choose(
    manifest: &Manifest,
    target: &str,
    running: &Version,
    rejected: Option<&Version>,
    host: Option<&Uuid>,
) -> Choice {
    let Some(release) = manifest.targets.get(target) else {
        return Choice::Skip(Skip::NoBuildForTarget);
    };
    let Ok(version) = Version::parse(&release.version) else {
        return Choice::Skip(Skip::UnreadableVersion(release.version.clone()));
    };
    if version <= *running {
        return Choice::Skip(Skip::NotNewer(version));
    }
    if rejected == Some(&version) {
        return Choice::Skip(Skip::Rejected(version));
    }
    if !inside_rollout(host, manifest.rollout) {
        return Choice::Skip(Skip::OutsideRollout);
    }
    Choice::Install {
        release: release.clone(),
        version,
    }
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.trim().as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in bytes.chunks(2).enumerate() {
        out[i] = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Some(out)
}
