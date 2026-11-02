//! What leaves a process for a dump, by kind of value: free text such as
//! our own logs through the shared capture rules, and a protocol message
//! the daemon's core was fed as its structure alone, walked by descriptor.

use std::collections::HashMap;
use std::sync::OnceLock;

use prost::Message;
use prost_types::field_descriptor_proto::Type;
use prost_types::{DescriptorProto, FileDescriptorSet};
use redaction::{Redaction, RedactionSummary};

/// The rules free text is redacted under: this machine's home, user and
/// host names on top of the shared ones, read once.
fn rules() -> &'static Redaction {
    static RULES: OnceLock<Redaction> = OnceLock::new();
    RULES.get_or_init(Redaction::for_this_machine)
}

/// Redacts free text no kind can decode, such as the daemon's log: the
/// shared capture rules, credential prefixes and secret assignments.
pub fn redact_text(text: &str) -> String {
    let mut summary = RedactionSummary::default();
    redaction::redact_text(text, rules(), &mut summary)
}

/// An encoded protocol message of the named type (`amux.v1.Step`) as its
/// structure alone, for a stage that never reads content: walked by
/// descriptor, each string and bytes field treated by its name as
/// [`redaction::structure`] says, with aliases drawn from `aliases`. A
/// per-kind body, which no kind is there to decode, is bytes like any
/// other, and so zero bytes of its length.
pub fn structure_message(
    name: &str,
    bytes: &[u8],
    aliases: &mut redaction::structure::Aliases,
) -> Vec<u8> {
    structure(&format!(".{name}"), bytes, aliases)
}

fn structure(name: &str, bytes: &[u8], aliases: &mut redaction::structure::Aliases) -> Vec<u8> {
    use redaction::structure::Bytes;

    let Some(fields) = schema().get(name) else {
        return Vec::new();
    };
    let holder = name.rsplit('.').next().unwrap_or_default();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        let before = rest;
        let Some(key) = read_varint(&mut rest) else {
            break;
        };
        let field = fields.get(&((key >> 3) as i32));
        let fixed = match key & 7 {
            0 => {
                if read_varint(&mut rest).is_none() {
                    break;
                }
                None
            }
            1 => Some(8),
            5 => Some(4),
            2 => {
                let Some(len) = read_varint(&mut rest) else {
                    break;
                };
                let Some(payload) = take(&mut rest, len as usize) else {
                    break;
                };
                let Some(field) = field else {
                    continue;
                };
                let kept = match field.ty {
                    Type::String => aliases
                        .text(holder, &field.name, &String::from_utf8_lossy(payload))
                        .into_bytes(),
                    Type::Message => structure(&field.type_name, payload, aliases),
                    Type::Bytes => match redaction::structure::bytes(&field.name) {
                        Bytes::Kept => payload.to_vec(),
                        Bytes::Blank => vec![0; payload.len()],
                    },
                    // Packed repeated numbers carry no text.
                    _ => payload.to_vec(),
                };
                write_varint(&mut out, key);
                write_varint(&mut out, kept.len() as u64);
                out.extend_from_slice(&kept);
                continue;
            }
            // Groups are not in this schema; stop rather than guess.
            _ => break,
        };
        if let Some(width) = fixed
            && take(&mut rest, width).is_none()
        {
            break;
        }
        if field.is_some() {
            out.extend_from_slice(&before[..before.len() - rest.len()]);
        }
    }
    out
}

fn read_varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if bytes.len() < len {
        return None;
    }
    let (head, rest) = bytes.split_at(len);
    *bytes = rest;
    Some(head)
}

struct Field {
    name: String,
    ty: Type,
    type_name: String,
}

/// Every message in the wire schema by its dotted full name, with its
/// fields by number.
fn schema() -> &'static HashMap<String, HashMap<i32, Field>> {
    static SCHEMA: OnceLock<HashMap<String, HashMap<i32, Field>>> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let set = FileDescriptorSet::decode(wire::DESCRIPTOR_SET).expect("descriptor set decodes");
        let mut schema = HashMap::new();
        for file in &set.file {
            let package = file.package.clone().unwrap_or_default();
            for message in &file.message_type {
                add_message(&mut schema, &format!(".{package}"), message);
            }
        }
        schema
    })
}

fn add_message(
    schema: &mut HashMap<String, HashMap<i32, Field>>,
    scope: &str,
    message: &DescriptorProto,
) {
    let name = format!("{scope}.{}", message.name());
    let fields = message
        .field
        .iter()
        .map(|field| {
            (
                field.number(),
                Field {
                    name: field.name().to_owned(),
                    ty: field.r#type(),
                    type_name: field.type_name().to_owned(),
                },
            )
        })
        .collect();
    for nested in &message.nested_type {
        add_message(schema, &name, nested);
    }
    schema.insert(name, fields);
}
