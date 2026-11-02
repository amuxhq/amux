//! Serde adapters for checkpoint state that holds protobuf values and ids.
//!
//! A checkpoint is JSON so a dump reader can see it; protobuf messages and
//! byte ids inside it are written as lowercase hex of their encoding.

use std::collections::{BTreeMap, BTreeSet};

use prost::{Message, Name};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0xf)] as char);
    }
    out
}

pub fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("odd-length hex: {text:?}"));
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(format!("not lowercase hex: {text:?}")),
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

fn encode_msg<T: Message + Name>(msg: &T) -> String {
    to_hex(&msg.encode_to_vec())
}

fn decode_msg<T: Message + Default, E: serde::de::Error>(text: &str) -> Result<T, E> {
    let bytes = from_hex(text).map_err(E::custom)?;
    T::decode(bytes.as_slice()).map_err(E::custom)
}

/// `Vec<u8>` as hex.
pub mod bytes {
    use super::*;

    pub fn serialize<S: Serializer>(value: &[u8], s: S) -> Result<S::Ok, S::Error> {
        to_hex(value).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        from_hex(&String::deserialize(d)?).map_err(D::Error::custom)
    }
}

/// An encoded item body of the interpreter's own kind, as hex.
pub mod item_body {
    use super::*;

    pub fn serialize<S: Serializer>(value: &[u8], s: S) -> Result<S::Ok, S::Error> {
        to_hex(value).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        bytes::deserialize(d)
    }
}

/// A set of byte ids as a list of hex strings.
pub mod bytes_set {
    use super::*;

    pub fn serialize<S: Serializer>(value: &BTreeSet<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|id| to_hex(id))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeSet<Vec<u8>>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|text| from_hex(text).map_err(D::Error::custom))
            .collect()
    }
}

/// A list of byte ids as a list of hex strings.
pub mod bytes_vec {
    use super::*;

    pub fn serialize<S: Serializer>(value: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|id| to_hex(id))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|text| from_hex(text).map_err(D::Error::custom))
            .collect()
    }
}

/// A list of protobuf messages.
pub mod msgs {
    use super::*;

    pub fn serialize<T: Message + Name, S: Serializer>(
        value: &[T],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|msg| encode_msg(msg))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, T: Message + Default, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|text| decode_msg(text))
            .collect()
    }
}

/// Protobuf messages by string key.
pub mod msg_map {
    use super::*;

    pub fn serialize<T: Message + Name, S: Serializer>(
        value: &BTreeMap<String, T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|(key, msg)| (key.clone(), encode_msg(msg)))
            .collect::<BTreeMap<_, _>>()
            .serialize(s)
    }

    pub fn deserialize<'de, T: Message + Default, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, T>, D::Error> {
        BTreeMap::<String, String>::deserialize(d)?
            .into_iter()
            .map(|(key, text)| Ok((key, decode_msg(&text)?)))
            .collect()
    }
}

/// An optional protobuf message.
pub mod opt_msg {
    use super::*;

    pub fn serialize<T: Message + Name, S: Serializer>(
        value: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(|msg| encode_msg(msg)).serialize(s)
    }

    pub fn deserialize<'de, T: Message + Default, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| decode_msg(&text))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_every_byte() {
        let bytes = (0..=255).collect::<Vec<u8>>();
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
        assert!(from_hex("abc").is_err());
        assert!(from_hex("zz").is_err());
    }
}
