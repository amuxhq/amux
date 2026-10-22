//! The pairing exchange's arithmetic: SPAKE2 over Ed25519 (RFC 9382),
//! the keys it derives, key confirmation and the sealed identities. Every
//! function is pure; the random scalars come from the core's entropy, and
//! what a session keeps between messages is bytes.

use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity as _;
use prost::Message as _;
use ring::{aead, digest, hkdf, hmac};
use serde::{Deserialize, Serialize};

const PAIRING_HKDF_SALT: &[u8] = b"amux-pair-spake2-v1";
pub(crate) const PAIR_CONFIRM_A: &[u8] = b"amux-pair-confirm-A";
pub(crate) const PAIR_CONFIRM_B: &[u8] = b"amux-pair-confirm-B";
const PAIR_ID_AAD: &[u8] = b"amux-pair-id";
const AEAD_A_TO_B_INFO: &[u8] = "aead/A→B".as_bytes();
const AEAD_B_TO_A_INFO: &[u8] = "aead/B→A".as_bytes();
/// The responder (A) seals to the initiator (B) under this direction.
pub(crate) const A_TO_B: u8 = 0x01;
pub(crate) const B_TO_A: u8 = 0x02;
pub(crate) const SPAKE2_ED25519_M: [u8; 32] = [
    0xd0, 0x48, 0x03, 0x2c, 0x6e, 0xa0, 0xb6, 0xd6, 0x97, 0xdd, 0xc2, 0xe8, 0x6b, 0xda, 0x85, 0xa3,
    0x3a, 0xda, 0xc9, 0x20, 0xf1, 0xbf, 0x18, 0xe1, 0xb0, 0xc6, 0xd1, 0x66, 0xa5, 0xce, 0xcd, 0xaf,
];
pub(crate) const SPAKE2_ED25519_N: [u8; 32] = [
    0xd3, 0xbf, 0xb5, 0x18, 0xf4, 0x4f, 0x34, 0x30, 0xf2, 0x9d, 0x0c, 0x92, 0xaf, 0x50, 0x38, 0x65,
    0xa1, 0xed, 0x32, 0x81, 0xdc, 0x69, 0xb3, 0x5d, 0xd8, 0x68, 0xba, 0x85, 0xf8, 0x86, 0xc4, 0xab,
];

/// Which side of the exchange: the host in pairing mode answers as A, the
/// host that typed or scanned the secret starts as B.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Side {
    A,
    B,
}

/// One side's SPAKE2 secret as it waits for the other's message: its
/// random scalar and the password's.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Spake2 {
    side: Side,
    scalar: [u8; 32],
    password: [u8; 32],
}

impl Spake2 {
    /// Starts `side`'s half from `secret` and 64 random bytes: the state
    /// kept and the message sent.
    pub(crate) fn start(side: Side, secret: &[u8], random: [u8; 64]) -> (Spake2, Vec<u8>) {
        let mut scalar = Scalar::from_bytes_mod_order_wide(&random);
        if scalar == Scalar::ZERO {
            scalar = Scalar::ONE;
        }
        let password = password_scalar(secret);
        let blind = point(match side {
            Side::A => &SPAKE2_ED25519_M,
            Side::B => &SPAKE2_ED25519_N,
        })
        .expect("the SPAKE2 constants are points");
        let message = (ED25519_BASEPOINT_POINT * scalar + blind * password)
            .compress()
            .to_bytes()
            .to_vec();
        (
            Spake2 {
                side,
                scalar: scalar.to_bytes(),
                password: password.to_bytes(),
            },
            message,
        )
    }

    /// The shared secret, from the other side's message; none when that
    /// message is no point or the exchange ends at the identity.
    pub(crate) fn finish(&self, peer_message: &[u8]) -> Option<[u8; 32]> {
        let peer = point(peer_message)?;
        let unblind = point(match self.side {
            Side::A => &SPAKE2_ED25519_N,
            Side::B => &SPAKE2_ED25519_M,
        })?;
        let scalar = Option::<Scalar>::from(Scalar::from_canonical_bytes(self.scalar))?;
        let password = Option::<Scalar>::from(Scalar::from_canonical_bytes(self.password))?;
        let k = (peer - unblind * password) * scalar * Scalar::from(8_u8);
        if k.is_identity() {
            return None;
        }
        Some(k.compress().to_bytes())
    }
}

fn password_scalar(secret: &[u8]) -> Scalar {
    // The out-of-band secret (PIN digits or QR secret bytes) is the
    // RFC 9382 password input; this is the ciphersuite-required
    // hash-to-scalar step, not application stretching.
    let digest = digest::digest(&digest::SHA512, secret);
    let mut wide = [0_u8; 64];
    wide.copy_from_slice(digest.as_ref());
    Scalar::from_bytes_mod_order_wide(&wide)
}

pub(crate) fn point(bytes: &[u8]) -> Option<EdwardsPoint> {
    let encoded: [u8; 32] = bytes.try_into().ok()?;
    let point = CompressedEdwardsY(encoded).decompress()?;
    (!point.is_identity() && point.is_torsion_free()).then_some(point)
}

/// What both sides derive from the shared secret and the transcript.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Keys {
    pub(crate) transcript_hash: [u8; 32],
    pub(crate) kc_a: [u8; 32],
    pub(crate) kc_b: [u8; 32],
    pub(crate) aead_a_to_b: [u8; 32],
    pub(crate) aead_b_to_a: [u8; 32],
}

impl Keys {
    /// The keys of an exchange whose messages were `msg_b`, then `msg_a`.
    pub(crate) fn derive(shared_secret: &[u8], msg_b: &[u8], msg_a: &[u8]) -> Keys {
        let transcript_hash = transcript_hash(msg_b, msg_a);
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, PAIRING_HKDF_SALT);
        let prk = salt.extract(shared_secret);
        Keys {
            transcript_hash,
            kc_a: expand_32(&prk, b"kc/A", &transcript_hash),
            kc_b: expand_32(&prk, b"kc/B", &transcript_hash),
            aead_a_to_b: expand_32(&prk, AEAD_A_TO_B_INFO, &transcript_hash),
            aead_b_to_a: expand_32(&prk, AEAD_B_TO_A_INFO, &transcript_hash),
        }
    }

    /// `side`'s key confirmation.
    pub(crate) fn confirmation(&self, side: Side) -> Vec<u8> {
        match side {
            Side::A => hmac_confirm(&self.kc_a, PAIR_CONFIRM_A, &self.transcript_hash),
            Side::B => hmac_confirm(&self.kc_b, PAIR_CONFIRM_B, &self.transcript_hash),
        }
    }

    /// Whether `tag` is `side`'s key confirmation.
    pub(crate) fn confirms(&self, side: Side, tag: &[u8]) -> bool {
        let (key, label) = match side {
            Side::A => (&self.kc_a, PAIR_CONFIRM_A),
            Side::B => (&self.kc_b, PAIR_CONFIRM_B),
        };
        let key = hmac::Key::new(hmac::HMAC_SHA256, key);
        let mut message = Vec::with_capacity(label.len() + self.transcript_hash.len());
        message.extend_from_slice(label);
        message.extend_from_slice(&self.transcript_hash);
        hmac::verify(&key, &message, tag).is_ok()
    }

    /// `identity`, sealed by `from` to the other side.
    pub(crate) fn seal(&self, from: Side, identity: &wire::PairingIdentity) -> Vec<u8> {
        let (key, direction) = self.direction(from);
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).expect("a 32-byte key"),
        );
        let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
        let aad = identity_aad(direction, &self.transcript_hash);
        let mut ciphertext = identity.encode_to_vec();
        key.seal_in_place_append_tag(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
            .expect("sealing an identity this short succeeds");
        ciphertext
    }

    /// The identity `from` sealed; none when it does not open under these
    /// keys.
    pub(crate) fn open(
        &self,
        from: Side,
        mut ciphertext: Vec<u8>,
    ) -> Option<wire::PairingIdentity> {
        let (key, direction) = self.direction(from);
        let key =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).ok()?);
        let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
        let aad = identity_aad(direction, &self.transcript_hash);
        let plaintext = key
            .open_in_place(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
            .ok()?;
        wire::PairingIdentity::decode(&*plaintext).ok()
    }

    fn direction(&self, from: Side) -> (&[u8; 32], u8) {
        match from {
            Side::A => (&self.aead_a_to_b, A_TO_B),
            Side::B => (&self.aead_b_to_a, B_TO_A),
        }
    }
}

fn transcript_hash(msg_b: &[u8], msg_a: &[u8]) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(&wire::PROTOCOL_VERSION.to_be_bytes());
    for message in [msg_b, msg_a] {
        ctx.update(&(message.len() as u32).to_be_bytes());
        ctx.update(message);
    }
    let digest = ctx.finish();
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(digest.as_ref());
    hash
}

struct Hkdf32;

impl hkdf::KeyType for Hkdf32 {
    fn len(&self) -> usize {
        32
    }
}

fn expand_32(prk: &hkdf::Prk, label: &[u8], transcript_hash: &[u8; 32]) -> [u8; 32] {
    let info = [label, transcript_hash.as_slice()];
    let mut out = [0_u8; 32];
    prk.expand(&info, Hkdf32)
        .and_then(|okm| okm.fill(&mut out))
        .expect("HKDF-SHA256 expands 32 bytes");
    out
}

fn hmac_confirm(key: &[u8; 32], label: &[u8], transcript_hash: &[u8; 32]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(label);
    ctx.update(transcript_hash);
    ctx.sign().as_ref().to_vec()
}

fn identity_aad(direction: u8, transcript_hash: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(PAIR_ID_AAD.len() + 1 + transcript_hash.len());
    aad.extend_from_slice(PAIR_ID_AAD);
    aad.push(direction);
    aad.extend_from_slice(transcript_hash);
    aad
}

/// The core's randomness: a seed the driver hands over as
/// [`Input::Entropy`](crate::Input::Entropy), stretched by counting. Each
/// draw is SHA-512 of the seed and the draw's number, so a run is a
/// function of its inputs and no two draws repeat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Entropy {
    seed: Option<[u8; 32]>,
    drawn: u64,
}

impl Entropy {
    /// Mixes `fresh` into the seed.
    pub(crate) fn add(&mut self, fresh: [u8; 32]) {
        let mut ctx = digest::Context::new(&digest::SHA256);
        ctx.update(&self.seed.unwrap_or_default());
        ctx.update(&fresh);
        let mut seed = [0_u8; 32];
        seed.copy_from_slice(ctx.finish().as_ref());
        self.seed = Some(seed);
    }

    /// 64 random bytes; none before any entropy arrived.
    pub(crate) fn draw(&mut self) -> Option<[u8; 64]> {
        let seed = self.seed?;
        self.drawn += 1;
        let mut ctx = digest::Context::new(&digest::SHA512);
        ctx.update(&seed);
        ctx.update(&self.drawn.to_be_bytes());
        let mut out = [0_u8; 64];
        out.copy_from_slice(ctx.finish().as_ref());
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spake2_ed25519_constants_are_valid_rfc9382_points() {
        assert!(point(&SPAKE2_ED25519_M).is_some());
        assert!(point(&SPAKE2_ED25519_N).is_some());
    }

    #[test]
    fn both_sides_of_one_secret_derive_the_same_keys_and_confirm_each_other() {
        let (a, msg_a) = Spake2::start(Side::A, b"123456", [1; 64]);
        let (b, msg_b) = Spake2::start(Side::B, b"123456", [2; 64]);
        let at_a = Keys::derive(&a.finish(&msg_b).unwrap(), &msg_b, &msg_a);
        let at_b = Keys::derive(&b.finish(&msg_a).unwrap(), &msg_b, &msg_a);
        assert_eq!(at_a, at_b);
        assert!(at_a.confirms(Side::B, &at_b.confirmation(Side::B)));
        assert!(at_b.confirms(Side::A, &at_a.confirmation(Side::A)));
        let identity = wire::PairingIdentity {
            name: "laptop".into(),
            ..Default::default()
        };
        let sealed = at_a.seal(Side::A, &identity);
        assert_eq!(at_b.open(Side::A, sealed.clone()), Some(identity));
        assert_eq!(
            at_b.open(Side::B, sealed),
            None,
            "a direction opens only its own"
        );
    }

    #[test]
    fn a_wrong_secret_fails_key_confirmation() {
        let (a, msg_a) = Spake2::start(Side::A, b"123456", [1; 64]);
        let (b, msg_b) = Spake2::start(Side::B, b"654321", [2; 64]);
        let at_a = Keys::derive(&a.finish(&msg_b).unwrap(), &msg_b, &msg_a);
        let at_b = Keys::derive(&b.finish(&msg_a).unwrap(), &msg_b, &msg_a);
        assert!(!at_a.confirms(Side::B, &at_b.confirmation(Side::B)));
    }

    #[test]
    fn entropy_draws_nothing_before_a_seed_and_never_the_same_twice() {
        let mut entropy = Entropy::default();
        assert_eq!(entropy.draw(), None);
        entropy.add([7; 32]);
        let first = entropy.draw().unwrap();
        assert_ne!(entropy.draw().unwrap(), first);
    }
}
