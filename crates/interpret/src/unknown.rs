//! Snapshot totality: every field of every per-kind snapshot body has an
//! explicit unknown, and the first frame carries exactly these values.
//!
//! Each constructor is an exhaustive struct literal with no `..Default`, so
//! a field added to a snapshot body fails to compile here until someone
//! decides what its unknown is. The schema test below checks the other
//! half: every scalar is `optional` (absent is unknown) and every message
//! is present with its own unknown state.

use wire::{
    BackgroundJobs, ClaudePtySnapshot, ClaudeSdkSnapshot, ClaudeUsage, CodexSnapshot, CodexUsage,
    ContextMeter, HealthState, SignIn, SignInState, TaskList, ToolServerHealth, UsageState,
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
        breakdown: Vec::new(),
    }
}

pub fn claude_usage() -> ClaudeUsage {
    ClaudeUsage {
        state: UsageState::Unknown as i32,
        windows: Vec::new(),
    }
}

pub fn codex_usage() -> CodexUsage {
    CodexUsage {
        state: UsageState::Unknown as i32,
        windows: Vec::new(),
        credits: None,
    }
}

pub fn tool_server_health() -> ToolServerHealth {
    ToolServerHealth {
        state: HealthState::Unknown as i32,
        servers: Vec::new(),
    }
}

pub fn sign_in() -> SignIn {
    SignIn {
        state: SignInState::Unknown as i32,
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
pub fn claude_pty() -> ClaudePtySnapshot {
    ClaudePtySnapshot {
        asks: Vec::new(),
        tasks: Some(task_list()),
        context: Some(context_meter()),
        model: None,
        model_name: None,
        permission: None,
        provider_session: None,
        effort: None,
        switched_away: None,
        usage: Some(claude_usage()),
        servers: Some(tool_server_health()),
        sign_in: Some(sign_in()),
        background_jobs: Some(background_jobs()),
        running_calls: Vec::new(),
    }
}

pub fn claude_sdk() -> ClaudeSdkSnapshot {
    ClaudeSdkSnapshot {
        asks: Vec::new(),
        tasks: Some(task_list()),
        context: Some(context_meter()),
        model: None,
        model_name: None,
        effort: None,
        permission: None,
        active_tasks: Vec::new(),
        usage: Some(claude_usage()),
        servers: Some(tool_server_health()),
        sign_in: Some(sign_in()),
        background_jobs: Some(background_jobs()),
        provider_session: None,
    }
}

pub fn codex() -> CodexSnapshot {
    CodexSnapshot {
        asks: Vec::new(),
        context: Some(context_meter()),
        model: None,
        model_name: None,
        permission: None,
        mode: None,
        approval_policy: None,
        active_turn: None,
        servers: Some(tool_server_health()),
        usage: Some(codex_usage()),
        sign_in: Some(sign_in()),
        background_jobs: Some(background_jobs()),
        plan: Some(task_list()),
        effort: None,
        thread_id: None,
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

    #[test]
    fn every_snapshot_field_has_an_explicit_unknown() {
        assert_total("ClaudePtySnapshot", claude_pty().encode_to_vec());
        assert_total("ClaudeSdkSnapshot", claude_sdk().encode_to_vec());
        assert_total("CodexSnapshot", codex().encode_to_vec());
    }
}
