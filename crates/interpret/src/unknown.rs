//! Snapshot totality: every field of every per-kind snapshot body has an
//! explicit unknown, and the first frame carries exactly these values. What
//! every kind says the same way (sign-in, background jobs, the model's
//! name, the conversation) is the envelope's, which the shared snapshot
//! fills at its unknowns from these too.
//!
//! Each constructor is an exhaustive struct literal with no `..Default`, so
//! a field added to a snapshot body fails to compile here until someone
//! decides what its unknown is. The schema test below checks the other
//! half: every scalar is `optional` (absent is unknown) and every message
//! is present with its own unknown state.

use wire::{
    AcpSnapshot, AcpUsage, BackgroundJobs, ClaudeSnapshot, ClaudeUsage, CodexSnapshot, CodexUsage,
    ContextMeter, SignIn, SignInState, TaskList, UsageState,
};

pub fn task_list() -> TaskList {
    TaskList {
        known: false,
        entries: Vec::new(),
    }
}

pub fn context_meter() -> ContextMeter {
    ContextMeter {
        known: false,
        used_tokens: 0,
        window_tokens: None,
    }
}

pub fn claude_usage() -> ClaudeUsage {
    ClaudeUsage {
        state: UsageState::Unspecified as i32,
        windows: Vec::new(),
    }
}

pub fn codex_usage() -> CodexUsage {
    CodexUsage {
        state: UsageState::Unspecified as i32,
        windows: Vec::new(),
        credits: None,
    }
}

pub fn sign_in() -> SignIn {
    SignIn {
        state: SignInState::Unspecified as i32,
        account: String::new(),
        message: String::new(),
    }
}

pub fn background_jobs() -> BackgroundJobs {
    BackgroundJobs {
        known: false,
        jobs: Vec::new(),
    }
}

/// No ask is open until a fact opens one, so an empty ask list is known,
/// not unknown.
pub fn claude() -> ClaudeSnapshot {
    ClaudeSnapshot {
        asks: Vec::new(),
        tasks: Some(task_list()),
        context: Some(context_meter()),
        switched_away: None,
        usage: Some(claude_usage()),
        servers: Vec::new(),
        running_calls: Vec::new(),
        active_tasks: Vec::new(),
    }
}

pub fn codex() -> CodexSnapshot {
    CodexSnapshot {
        asks: Vec::new(),
        context: Some(context_meter()),
        approval_policy: None,
        servers: Vec::new(),
        usage: Some(codex_usage()),
        plan: Some(task_list()),
    }
}

pub fn acp_usage() -> AcpUsage {
    AcpUsage {
        cost: None,
        currency: String::new(),
        input_tokens: None,
        output_tokens: None,
        thought_tokens: None,
        cached_read_tokens: None,
        cached_write_tokens: None,
        total_tokens: None,
    }
}

pub fn acp() -> AcpSnapshot {
    AcpSnapshot {
        asks: Vec::new(),
        context: Some(context_meter()),
        usage: Some(acp_usage()),
        plan: Some(task_list()),
        servers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use prost::Message as _;
    use prost_types::field_descriptor_proto::{Label, Type};
    use prost_types::{DescriptorProto, FileDescriptorSet};

    use super::*;

    fn message(name: &str) -> DescriptorProto {
        FileDescriptorSet::decode(wire::DESCRIPTOR_SET)
            .expect("descriptor set")
            .file
            .into_iter()
            .flat_map(|file| file.message_type)
            .find(|message| message.name() == name)
            .unwrap_or_else(|| panic!("{name} in the descriptor set"))
    }

    /// Field numbers present in an encoding: a present empty message still
    /// writes its tag, so this shows which message fields are Some.
    fn present_fields(mut bytes: &[u8]) -> BTreeSet<i32> {
        let mut present = BTreeSet::new();
        while !bytes.is_empty() {
            let key = prost::encoding::decode_varint(&mut bytes).expect("key");
            let wire_type = key & 7;
            present.insert((key >> 3) as i32);
            match wire_type {
                0 => {
                    prost::encoding::decode_varint(&mut bytes).expect("varint");
                }
                1 => bytes = &bytes[8..],
                2 => {
                    let len = prost::encoding::decode_varint(&mut bytes).expect("len") as usize;
                    bytes = &bytes[len..];
                }
                5 => bytes = &bytes[4..],
                other => panic!("unexpected wire type {other}"),
            }
        }
        present
    }

    /// Every scalar is optional and every singular message is present, so
    /// no field of the body says "known" by accident.
    fn assert_total(name: &str, encoded: Vec<u8>) {
        let descriptor = message(name);
        let present = present_fields(&encoded);
        for field in &descriptor.field {
            if field.label() == Label::Repeated {
                continue;
            }
            match field.r#type() {
                Type::Message => assert!(
                    present.contains(&field.number()),
                    "{name}.{} has no explicit unknown",
                    field.name()
                ),
                _ => assert!(
                    field.proto3_optional(),
                    "{name}.{} is a plain scalar: its zero value would read as known",
                    field.name()
                ),
            }
        }
    }

    /// The envelope says what every kind says the same way at its own
    /// unknowns until the kind reports it.
    #[test]
    fn the_first_envelope_says_every_shared_fact_at_its_unknown() {
        let spec = wire::AgentSpec {
            agent_id: b"a".to_vec(),
            ..Default::default()
        };
        let mut shared: crate::Shared<wire::ClaudeAsk> =
            crate::Shared::new(&spec, wire::Kind::ClaudeSdk);
        let snapshot = shared
            .initial_step(claude().encode_to_vec())
            .snapshot
            .expect("the first frame is a snapshot");
        assert_eq!(snapshot.sign_in, Some(sign_in()));
        assert_eq!(snapshot.jobs, Some(background_jobs()));
        assert_eq!(snapshot.model_name, None);
        assert_eq!(snapshot.conversation, None);
        assert!(snapshot.settings().is_none());
    }

    #[test]
    fn every_snapshot_field_has_an_explicit_unknown() {
        assert_total("ClaudeSnapshot", claude().encode_to_vec());
        assert_total("CodexSnapshot", codex().encode_to_vec());
        assert_total("AcpSnapshot", acp().encode_to_vec());
    }
}
