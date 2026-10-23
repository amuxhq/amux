//! The core's checkpoint and recorded inputs as a dump carries them:
//! written again with every protocol message and every text inside them
//! redacted, and every type, number and id kept, so the copy restores and
//! replays as the original does.
//!
//! The core decodes no agent's bodies and knows no redaction rules, so the
//! caller brings the store's [`Redactor`]. The values are walked by serialization: a
//! protocol message is written by `codec` as its encoding in hex, and says
//! so on its way through, so the redactor sees it whole and by name;
//! content bytes, such as a kept blob or a diff's patch, are written as hex
//! and redacted as text when they are text; every other string is free
//! text.

use std::cell::Cell;

use serde::ser::{
    self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
};

pub use store::redact::Redactor;

use crate::io::{Input, Recorded};
use crate::{Checkpoint, RestoreError};

/// What the next string written is, when it is not free text.
#[derive(Clone, Copy)]
pub(crate) enum Opaque {
    /// A protocol message's encoding in hex, with its package and name.
    Message(&'static str, &'static str),
    /// Hex of an id: kept as it is.
    Verbatim,
    /// Hex of content bytes, such as a blob or a patch: redacted as text
    /// when they are UTF-8, kept when they are not.
    Content,
}

thread_local! {
    static NEXT: Cell<Option<Opaque>> = const { Cell::new(None) };
}

/// Writes `text` through `to`, marked as `what` for a redacting writer.
pub(crate) fn write_opaque<S: Serializer>(
    what: Opaque,
    text: &str,
    to: S,
) -> Result<S::Ok, S::Error> {
    NEXT.with(|next| next.set(Some(what)));
    let written = to.serialize_str(text);
    NEXT.with(|next| next.set(None));
    written
}

impl Checkpoint {
    /// This checkpoint with its state redacted; one under another layout,
    /// or one that does not decode, is refused.
    pub fn redacted(&self, redactor: &dyn Redactor) -> Result<Checkpoint, RestoreError> {
        let parts = crate::checkpoint_parts(self)?;
        Ok(Checkpoint {
            schema: self.schema,
            state: postcard::to_allocvec(&Redacting::value(&parts, redactor))
                .map_err(|error| RestoreError::Corrupt(error.to_string()))?,
        })
    }
}

impl Input {
    /// This input with its messages and text redacted.
    pub fn redacted(&self, redactor: &dyn Redactor) -> Input {
        redacted(self, redactor)
    }
}

impl Recorded {
    /// This line of the input ring with its messages and text redacted.
    pub fn redacted(&self, redactor: &dyn Redactor) -> Recorded {
        redacted(self, redactor)
    }
}

/// A value written as JSON through the redactor and read back.
fn redacted<T: Serialize + serde::de::DeserializeOwned>(value: &T, redactor: &dyn Redactor) -> T {
    let json = serde_json::to_vec(&Redacting::value(value, redactor))
        .expect("a core value writes as JSON");
    serde_json::from_slice(&json).expect("a redacted core value reads back as its type")
}

/// A serializer that hands each string to the redactor on its way to
/// `inner`.
struct Redacting<'r, S> {
    inner: S,
    redactor: &'r dyn Redactor,
}

/// A value whose strings go through the redactor when it is serialized.
struct Value<'r, T: ?Sized> {
    value: &'r T,
    redactor: &'r dyn Redactor,
}

impl<'r> Redacting<'r, ()> {
    fn value<T: ?Sized>(value: &'r T, redactor: &'r dyn Redactor) -> Value<'r, T> {
        Value { value, redactor }
    }
}

impl<T: Serialize + ?Sized> Serialize for Value<'_, T> {
    fn serialize<S: Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(Redacting {
            inner: to,
            redactor: self.redactor,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The compound serializers, each element wrapped the same way.
struct Compound<'r, C> {
    inner: C,
    redactor: &'r dyn Redactor,
}

impl<'r, S: Serializer> Serializer for Redacting<'r, S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = Compound<'r, S::SerializeSeq>;
    type SerializeTuple = Compound<'r, S::SerializeTuple>;
    type SerializeTupleStruct = Compound<'r, S::SerializeTupleStruct>;
    type SerializeTupleVariant = Compound<'r, S::SerializeTupleVariant>;
    type SerializeMap = Compound<'r, S::SerializeMap>;
    type SerializeStruct = Compound<'r, S::SerializeStruct>;
    type SerializeStructVariant = Compound<'r, S::SerializeStructVariant>;

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }

    fn serialize_str(self, text: &str) -> Result<S::Ok, S::Error> {
        match NEXT.with(|next| next.take()) {
            Some(Opaque::Message(package, name)) => {
                let Some(bytes) = unhex(text) else {
                    return Err(ser::Error::custom("a message that is not hex"));
                };
                let redacted = self.redactor.message(&format!("{package}.{name}"), &bytes);
                self.inner.serialize_str(&hex(&redacted))
            }
            Some(Opaque::Verbatim) => self.inner.serialize_str(text),
            Some(Opaque::Content) => {
                let Some(bytes) = unhex(text) else {
                    return Err(ser::Error::custom("content that is not hex"));
                };
                match std::str::from_utf8(&bytes) {
                    Ok(content) => self
                        .inner
                        .serialize_str(&hex(self.redactor.text(content).as_bytes())),
                    Err(_) => self.inner.serialize_str(text),
                }
            }
            None => self.inner.serialize_str(&self.redactor.text(text)),
        }
    }

    fn serialize_bool(self, v: bool) -> Result<S::Ok, S::Error> {
        self.inner.serialize_bool(v)
    }
    fn serialize_i8(self, v: i8) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i8(v)
    }
    fn serialize_i16(self, v: i16) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i16(v)
    }
    fn serialize_i32(self, v: i32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i32(v)
    }
    fn serialize_i64(self, v: i64) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i64(v)
    }
    fn serialize_i128(self, v: i128) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i128(v)
    }
    fn serialize_u8(self, v: u8) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u8(v)
    }
    fn serialize_u16(self, v: u16) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u16(v)
    }
    fn serialize_u32(self, v: u32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u32(v)
    }
    fn serialize_u64(self, v: u64) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u64(v)
    }
    fn serialize_u128(self, v: u128) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u128(v)
    }
    fn serialize_f32(self, v: f32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_f32(v)
    }
    fn serialize_f64(self, v: f64) -> Result<S::Ok, S::Error> {
        self.inner.serialize_f64(v)
    }
    fn serialize_char(self, v: char) -> Result<S::Ok, S::Error> {
        self.inner.serialize_char(v)
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<S::Ok, S::Error> {
        self.inner.serialize_bytes(v)
    }
    fn serialize_none(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_none()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<S::Ok, S::Error> {
        self.inner
            .serialize_some(&Redacting::value(value, self.redactor))
    }
    fn serialize_unit(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit()
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit_struct(name)
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
    ) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit_variant(name, index, variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error> {
        self.inner
            .serialize_newtype_struct(name, &Redacting::value(value, self.redactor))
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error> {
        self.inner.serialize_newtype_variant(
            name,
            index,
            variant,
            &Redacting::value(value, self.redactor),
        )
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, S::Error> {
        Ok(Compound {
            inner: self.inner.serialize_seq(len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, S::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple(len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, S::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple_struct(name, len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, S::Error> {
        Ok(Compound {
            inner: self
                .inner
                .serialize_tuple_variant(name, index, variant, len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
        Ok(Compound {
            inner: self.inner.serialize_map(len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, S::Error> {
        Ok(Compound {
            inner: self.inner.serialize_struct(name, len)?,
            redactor: self.redactor,
        })
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, S::Error> {
        Ok(Compound {
            inner: self
                .inner
                .serialize_struct_variant(name, index, variant, len)?,
            redactor: self.redactor,
        })
    }
}

impl<C: SerializeSeq> SerializeSeq for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_element(&value)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTuple> SerializeTuple for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_element(&value)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTupleStruct> SerializeTupleStruct for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_field(&value)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTupleVariant> SerializeTupleVariant for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_field(&value)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeMap> SerializeMap for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), C::Error> {
        let key = Value {
            value: key,
            redactor: self.redactor,
        };
        self.inner.serialize_key(&key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_value(&value)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeStruct> SerializeStruct for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_field(key, &value)
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.inner.skip_field(key)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeStructVariant> SerializeStructVariant for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), C::Error> {
        let value = Value {
            value,
            redactor: self.redactor,
        };
        self.inner.serialize_field(key, &value)
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.inner.skip_field(key)
    }
    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use prost::Message as _;
    use store::Sqlite;

    use super::*;
    use crate::{AgentId, Daemon, HostId, StartOptions};

    const PLANTED: &[u8] = b"PLANTED";

    /// Replaces the planted word in place, so every message stays one, and
    /// remembers the message names it was handed.
    #[derive(Default)]
    struct Swap {
        names: RefCell<BTreeSet<String>>,
    }

    fn swap(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        for at in 0..out.len().saturating_sub(PLANTED.len() - 1) {
            if out[at..].starts_with(PLANTED) {
                out[at..at + PLANTED.len()].copy_from_slice(b"REMOVED");
            }
        }
        out
    }

    impl Redactor for Swap {
        fn message(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
            self.names.borrow_mut().insert(name.to_owned());
            swap(bytes)
        }
        fn text(&self, text: &str) -> String {
            String::from_utf8(swap(text.as_bytes())).expect("still text")
        }
        fn agent(&self, _: i32, agent: wire::Agent) -> wire::Agent {
            agent
        }
        fn step(&self, _: i32, step: store::Step) -> store::Step {
            step
        }
    }

    fn holds(bytes: &[u8]) -> bool {
        bytes.windows(PLANTED.len()).any(|window| window == PLANTED)
    }

    fn draft(text: &str) -> Vec<u8> {
        wire::ComposerDraft {
            text: text.to_owned(),
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn a_redacted_checkpoint_restores_with_its_messages_and_text_redacted() {
        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        let mut daemon = Daemon::new(
            store,
            StartOptions {
                agents: PathBuf::from("/home/PLANTED/agents"),
                ..StartOptions::default()
            },
        );
        let agent = vec![7; 16];
        daemon.step(
            Input::Drafts {
                drafts: vec![(agent.clone(), draft("the key is PLANTED"))],
            },
            &mut Vec::new(),
        );
        let checkpoint = daemon.checkpoint();
        assert!(holds(&checkpoint.state));

        let redactor = Swap::default();
        let redacted = checkpoint.redacted(&redactor).expect("it redacts");
        assert!(!holds(&redacted.state), "the planted word is gone");
        assert!(
            redactor.names.borrow().contains("amux.v1.ComposerDraft"),
            "the draft went through as its message: {:?}",
            redactor.names.borrow()
        );
        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        let restored = Daemon::restore(redacted, store).expect("the redacted checkpoint restores");
        let again = restored.checkpoint().state;
        let kept = hex(&draft("the key is REMOVED"));
        assert!(
            again
                .windows(kept.len())
                .any(|window| window == kept.as_bytes()),
            "the restored core holds the redacted draft"
        );
    }

    #[test]
    fn a_redacted_input_keeps_its_ids_and_reads_back() {
        let redactor = Swap::default();
        let host = HostId([0x50; 16]);
        let input = Input::DialError {
            host,
            error: Some("refused by PLANTED".into()),
        };
        assert_eq!(
            input.redacted(&redactor),
            Input::DialError {
                host,
                error: Some("refused by REMOVED".into()),
            }
        );
        let read = Recorded::Read {
            agent: AgentId([1; 16]),
            seen: 42,
            failed: Some("disk PLANTED".into()),
        };
        let line = serde_json::to_string(&Recorded::input(&input)).unwrap();
        assert_eq!(
            serde_json::from_str::<Recorded>(&line).unwrap(),
            Recorded::Input(Box::new(input)),
            "a borrowed input writes the line an owned one reads back as"
        );
        assert_eq!(
            read.redacted(&redactor),
            Recorded::Read {
                agent: AgentId([1; 16]),
                seen: 42,
                failed: Some("disk REMOVED".into()),
            }
        );
    }

    #[test]
    fn a_kept_blob_and_a_patch_are_redacted_as_text_and_binary_content_is_kept() {
        use crate::JobId;
        use crate::payload::{JobResult, Patch};

        let redactor = Swap::default();
        let compared = |bytes: &[u8]| {
            Recorded::Input(Box::new(Input::JobDone {
                job: JobId(4),
                result: JobResult::Compared {
                    diff: wire::Diff::default(),
                    patch: Some(Patch {
                        hash: vec![8; 32],
                        bytes: bytes.to_vec(),
                    }),
                },
            }))
        };
        for (planted, removed) in [(
            compared(b"+token = PLANTED\n"),
            compared(b"+token = REMOVED\n"),
        )] {
            let in_hex = hex(PLANTED);
            let carries = |line: &Recorded| serde_json::to_string(line).unwrap().contains(&in_hex);
            assert!(carries(&planted), "the line carries the content in hex");
            let redacted = planted.redacted(&redactor);
            assert!(!carries(&redacted), "the planted word is gone");
            assert_eq!(redacted, removed, "only the content's text changed");
        }

        let binary = [0xff, 0xfe, b'P', b'L', b'A', b'N', b'T', b'E', b'D'];
        assert_eq!(
            compared(&binary).redacted(&redactor),
            compared(&binary),
            "content that is not text is kept as it is"
        );
    }
}
