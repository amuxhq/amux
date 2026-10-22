//! How an input's protocol messages are written where inputs are recorded:
//! as their protobuf encoding, in hex, so a recorded input is one line of
//! text and reads back as the same message. Each hex string is marked as
//! what it is on its way out, so a dump's redaction redacts a message as
//! that message and leaves an id alone.

use std::sync::Arc;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use store::AgentKey;

use crate::redact::{Opaque, write_opaque};

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

/// A message as its marked hex.
struct Encoded<'a, T>(&'a T);

impl<T: prost::Message + prost::Name> Serialize for Encoded<'_, T> {
    fn serialize<S: Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        write_opaque(
            Opaque::Message(T::PACKAGE, T::NAME),
            &hex(&self.0.encode_to_vec()),
            to,
        )
    }
}

/// An already encoded message of type `T`, as its marked hex.
struct EncodedAs<'a, T>(&'a [u8], std::marker::PhantomData<T>);

impl<T: prost::Name> Serialize for EncodedAs<'_, T> {
    fn serialize<S: Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        write_opaque(Opaque::Message(T::PACKAGE, T::NAME), &hex(self.0), to)
    }
}

/// An id's hex, marked to be kept as it is.
struct Id(String);

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        write_opaque(Opaque::Verbatim, &self.0, to)
    }
}

/// Content bytes, such as a blob or a patch, as their marked hex.
pub(crate) mod content {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], to: S) -> Result<S::Ok, S::Error> {
        write_opaque(Opaque::Content, &hex(bytes), to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<u8>, D::Error> {
        unhex::<D::Error>(&String::deserialize(from)?)
    }
}

fn decode<'de, T: prost::Message + Default, D: Deserializer<'de>>(
    text: &str,
) -> Result<T, D::Error> {
    T::decode(unhex::<D::Error>(text)?.as_slice()).map_err(D::Error::custom)
}

/// One message.
pub(crate) mod message {
    use super::*;

    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        message: &T,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        Encoded(message).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<T, D::Error> {
        decode::<T, D>(&String::deserialize(from)?)
    }
}

/// One message, boxed.
pub(crate) mod boxed {
    use super::*;

    #[expect(clippy::borrowed_box, reason = "serde hands the field over as it is")]
    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        message: &Box<T>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        Encoded(message.as_ref()).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Box<T>, D::Error> {
        decode::<T, D>(&String::deserialize(from)?).map(Box::new)
    }
}

/// A boxed message, or none.
pub(crate) mod optional_boxed {
    use super::*;

    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        message: &Option<Box<T>>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        message.as_deref().map(Encoded).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Option<Box<T>>, D::Error> {
        Option::<String>::deserialize(from)?
            .map(|text| decode::<T, D>(&text).map(Box::new))
            .transpose()
    }
}

/// One message shared between the subscribers it goes to.
pub(crate) mod shared {
    use super::*;

    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        message: &Arc<T>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        Encoded(message.as_ref()).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Arc<T>, D::Error> {
        decode::<T, D>(&String::deserialize(from)?).map(Arc::new)
    }
}

/// A message, or none.
pub(crate) mod optional {
    use super::*;

    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        message: &Option<T>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        message.as_ref().map(Encoded).serialize(to)
    }

    pub(crate) fn deserialize<'de, T: prost::Message + Default, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<String>::deserialize(from)?
            .map(|text| decode::<T, D>(&text))
            .transpose()
    }
}

/// Messages in order.
pub(crate) mod messages {
    use super::*;

    pub(crate) fn serialize<T: prost::Message + prost::Name, S: Serializer>(
        messages: &[T],
        to: S,
    ) -> Result<S::Ok, S::Error> {
        to.collect_seq(messages.iter().map(Encoded))
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
        (Id(hex(&key.host)), Id(hex(&key.agent))).serialize(to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<AgentKey, D::Error> {
        let (host, agent) = <(String, String)>::deserialize(from)?;
        Ok(AgentKey::new(
            unhex::<D::Error>(&host)?,
            unhex::<D::Error>(&agent)?,
        ))
    }
}

/// When clients last used each replica, by agent, in key order so the same
/// map always reads back as the same line.
pub(crate) mod last_used {
    use std::collections::HashMap;

    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        used: &HashMap<AgentKey, i64>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        let mut rows: Vec<(String, String, i64)> = used
            .iter()
            .map(|(key, at)| (hex(&key.host), hex(&key.agent), *at))
            .collect();
        rows.sort();
        to.collect_seq(
            rows.into_iter()
                .map(|(host, agent, at)| (Id(host), Id(agent), at)),
        )
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        from: D,
    ) -> Result<HashMap<AgentKey, i64>, D::Error> {
        <Vec<(String, String, i64)>>::deserialize(from)?
            .into_iter()
            .map(|(host, agent, at)| {
                Ok((
                    AgentKey::new(unhex::<D::Error>(&host)?, unhex::<D::Error>(&agent)?),
                    at,
                ))
            })
            .collect()
    }
}

/// Each agent's composer draft, encoded, by agent id: written as the id's
/// hex and the draft's, so a dump redacts the draft as the message it is.
pub(crate) mod drafts {
    use std::collections::BTreeMap;
    use std::marker::PhantomData;

    use super::*;

    type Line = (String, String);

    fn line<'a>(
        (agent, draft): (&'a Vec<u8>, &'a Vec<u8>),
    ) -> (Id, EncodedAs<'a, wire::ComposerDraft>) {
        (Id(hex(agent)), EncodedAs(draft, PhantomData))
    }

    fn read<'de, D: Deserializer<'de>>(from: D) -> Result<crate::drafts::DraftList, D::Error> {
        Vec::<Line>::deserialize(from)?
            .iter()
            .map(|(agent, draft)| Ok((unhex::<D::Error>(agent)?, unhex::<D::Error>(draft)?)))
            .collect()
    }

    /// The core's map of drafts.
    pub(crate) mod map {
        use super::*;

        pub(crate) fn serialize<S: Serializer>(
            drafts: &BTreeMap<Vec<u8>, Vec<u8>>,
            to: S,
        ) -> Result<S::Ok, S::Error> {
            to.collect_seq(drafts.iter().map(line))
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            from: D,
        ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, D::Error> {
            Ok(read(from)?.into_iter().collect())
        }
    }

    /// The drafts as the file holds them, in its order.
    pub(crate) mod list {
        use super::*;

        pub(crate) fn serialize<S: Serializer>(
            drafts: &[(Vec<u8>, Vec<u8>)],
            to: S,
        ) -> Result<S::Ok, S::Error> {
            to.collect_seq(drafts.iter().map(|(agent, draft)| line((agent, draft))))
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            from: D,
        ) -> Result<crate::drafts::DraftList, D::Error> {
            read(from)
        }
    }
}
