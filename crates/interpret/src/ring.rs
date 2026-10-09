//! One line of an agent's facts ring: every event the interpreter was fed,
//! as the agent process writes it and a dump carries it. Provider payloads
//! are text when they are UTF-8, which every provider's are, and hex
//! otherwise; inputs and agent messages are their encoded protobuf in hex. A segment of the
//! ring is these lines, one per event; the checkpoint beside it is the
//! interpreter's state before its first line.

use prost::Message as _;
use serde::{Deserialize, Serialize};
use wire::StopMode;

use crate::{Channel, Event, Fact};

/// One event as the ring records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Entry {
    Fact {
        channel: Channel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hex: Option<String>,
    },
    Input {
        hex: String,
    },
    /// The encoded `wire::Envelope`.
    AgentMessage {
        hex: String,
    },
    Rename {
        name: String,
    },
    Tick {
        at_ms: i64,
    },
    ProviderExit {
        code: Option<i32>,
    },
    DaemonLost,
    Stop {
        mode: i32,
    },
    /// The cause's raw value, so one this build does not know is kept.
    Exiting {
        cause: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// The encoded `wire::Git`, absent outside a repository.
    Git {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hex: Option<String>,
    },
}

impl Entry {
    pub fn of(event: &Event) -> Self {
        match event {
            Event::Fact(fact) => match std::str::from_utf8(&fact.payload) {
                Ok(text) => Entry::Fact {
                    channel: fact.channel,
                    text: Some(text.to_owned()),
                    hex: None,
                },
                Err(_) => Entry::Fact {
                    channel: fact.channel,
                    text: None,
                    hex: Some(crate::to_hex(&fact.payload)),
                },
            },
            Event::Input(input) => Entry::Input {
                hex: crate::to_hex(&input.encode_to_vec()),
            },
            Event::AgentMessage(envelope) => Entry::AgentMessage {
                hex: crate::to_hex(&envelope.encode_to_vec()),
            },
            Event::Rename { name } => Entry::Rename { name: name.clone() },
            Event::Tick { at_ms } => Entry::Tick { at_ms: *at_ms },
            Event::ProviderExit { code } => Entry::ProviderExit { code: *code },
            Event::DaemonLost => Entry::DaemonLost,
            Event::StopRequested(mode) => Entry::Stop { mode: *mode as i32 },
            Event::Exiting(exiting) => Entry::Exiting {
                cause: exiting.cause,
                detail: exiting.detail.clone(),
            },
            Event::Git(git) => Entry::Git {
                hex: git.as_ref().map(|git| crate::to_hex(&git.encode_to_vec())),
            },
        }
    }

    /// The event again; None for an entry this build cannot read.
    pub fn event(self) -> Option<Event> {
        Some(match self {
            Entry::Fact { channel, text, hex } => Event::Fact(Fact {
                channel,
                payload: match (text, hex) {
                    (Some(text), _) => text.into_bytes(),
                    (None, Some(hex)) => crate::from_hex(&hex).ok()?,
                    (None, None) => Vec::new(),
                },
            }),
            Entry::Input { hex } => {
                Event::Input(wire::Input::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?)
            }
            Entry::AgentMessage { hex } => Event::AgentMessage(
                wire::Envelope::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?,
            ),
            Entry::Rename { name } => Event::Rename { name },
            Entry::Tick { at_ms } => Event::Tick { at_ms },
            Entry::ProviderExit { code } => Event::ProviderExit { code },
            Entry::DaemonLost => Event::DaemonLost,
            Entry::Stop { mode } => Event::StopRequested(StopMode::try_from(mode).ok()?),
            Entry::Exiting { cause, detail } => Event::Exiting(wire::Exiting { cause, detail }),
            Entry::Git { hex: None } => Event::Git(None),
            Entry::Git { hex: Some(hex) } => Event::Git(Some(
                wire::Git::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?,
            )),
        })
    }
}

/// The entries of one segment's bytes, in order. A torn last line, from a
/// process that died mid-write, is left out, and so is everything after a
/// line this build cannot read.
pub fn entries(segment: &[u8]) -> Vec<Entry> {
    segment
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map_while(|line| serde_json::from_slice(line).ok())
        .collect()
}
