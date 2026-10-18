//! How an input's protocol messages are written where inputs are recorded:
//! as their protobuf encoding, in hex, so a recorded input is one line of
//! text and reads back as the same message.

use std::sync::Arc;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use store::AgentKey;

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0xf)] as char);
    }
    text
}

fn unhex<E: serde::de::Error>(text: &str) -> Result<Vec<u8>, E> {
    fn digit<E: serde::de::Error>(byte: u8) -> Result<u8, E> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(E::custom("not a hex digit")),
        }
    }
    if !text.len().is_multiple_of(2) {
        return Err(E::custom("an odd number of hex digits"));
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| Ok(digit::<E>(pair[0])? << 4 | digit::<E>(pair[1])?))
        .collect()
}

fn encode<T: prost::Message>(message: &T) -> String {
    hex(&message.encode_to_vec())
}

fn decode<'de, T: prost::Message + Default, D: Deserializer<'de>>(
    text: &str,
) -> Result<T, D::Error> {
    T::decode(unhex::<D::Error>(text)?.as_slice()).map_err(D::Error::custom)
}

/// One message.
pub(crate) mod message {
    use super::*;

    pub(crate) fn serialize<T: prost::Message, S: Serializer>(
        message: &T,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        encode(message).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<T, D::Error> {
        decode::<T, D>(&String::deserialize(from)?)
    }
}

/// One message shared between the subscribers it goes to.
pub(crate) mod shared {
    use super::*;

    pub(crate) fn serialize<T: prost::Message, S: Serializer>(
        message: &Arc<T>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        encode(message.as_ref()).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Arc<T>, D::Error> {
        decode::<T, D>(&String::deserialize(from)?).map(Arc::new)
    }
}

/// Messages in order.
pub(crate) mod messages {
    use super::*;

    pub(crate) fn serialize<T: prost::Message, S: Serializer>(
        messages: &[T],
        to: S,
    ) -> Result<S::Ok, S::Error> {
        to.collect_seq(messages.iter().map(encode))
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Vec<T>, D::Error> {
        Vec::<String>::deserialize(from)?
            .iter()
            .map(|text| decode::<T, D>(text))
            .collect()
    }
}

/// A message that may be absent.
pub(crate) mod optional {
    use super::*;

    pub(crate) fn serialize<T: prost::Message, S: Serializer>(
        message: &Option<T>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        message.as_ref().map(encode).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<String>::deserialize(from)?
            .map(|text| decode::<T, D>(&text))
            .transpose()
    }
}

/// A protocol enumeration, by its number.
pub(crate) mod numbered {
    use super::*;

    pub(crate) fn serialize<T: Copy + Into<i32>, S: Serializer>(
        value: &T,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        (*value).into().serialize(to)
    }

    pub(crate) fn deserialize<'de, T: TryFrom<i32>, D: Deserializer<'de>>(
        from: D,
    ) -> Result<T, D::Error> {
        let number = i32::deserialize(from)?;
        T::try_from(number).map_err(|_| D::Error::custom(format!("no value numbered {number}")))
    }
}

/// An agent by its host and id.
pub(crate) mod agent_key {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(key: &AgentKey, to: S) -> Result<S::Ok, S::Error> {
        (hex(&key.host), hex(&key.agent)).serialize(to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<AgentKey, D::Error> {
        let (host, agent) = <(String, String)>::deserialize(from)?;
        Ok(AgentKey::new(
            unhex::<D::Error>(&host)?,
            unhex::<D::Error>(&agent)?,
        ))
    }
}
