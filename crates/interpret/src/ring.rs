//! One line of an agent's facts ring: every event the interpreter was fed,
//! as the agent process writes it and a dump carries it. Provider payloads
//! are text when they are UTF-8, which every provider's are, and hex
//! otherwise; inputs and agent messages are their encoded protobuf in hex. A segment of the
//! ring is these lines, one per event; the checkpoint beside it is the
//! interpreter's state before its first line.

use prost::Message as _;
use serde::{Deserialize, Serialize};

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
    /// The encoded `wire::ChildInput`: a person's input to one of the
    /// provider subagents the agent runs. A ring from before subagents were
    /// agents of their own has none.
    ChildInput {
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
    Stop,
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
            Event::ChildInput { agent_id, input } => Entry::ChildInput {
                hex: crate::to_hex(
                    &wire::ChildInput {
                        agent_id: agent_id.clone(),
                        input: Some(input.clone()),
                    }
                    .encode_to_vec(),
                ),
            },
            Event::AgentMessage(envelope) => Entry::AgentMessage {
                hex: crate::to_hex(&envelope.encode_to_vec()),
            },
            Event::Rename { name } => Entry::Rename { name: name.clone() },
            Event::Tick { at_ms } => Entry::Tick { at_ms: *at_ms },
            Event::ProviderExit { code } => Entry::ProviderExit { code: *code },
            Event::DaemonLost => Entry::DaemonLost,
            Event::StopRequested => Entry::Stop,
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
            Entry::ChildInput { hex } => {
                let child =
                    wire::ChildInput::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?;
                Event::ChildInput {
                    agent_id: child.agent_id,
                    input: child.input?,
                }
            }
            Entry::AgentMessage { hex } => Event::AgentMessage(
                wire::Envelope::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?,
            ),
            Entry::Rename { name } => Event::Rename { name },
            Entry::Tick { at_ms } => Event::Tick { at_ms },
            Entry::ProviderExit { code } => Event::ProviderExit { code },
            Entry::DaemonLost => Event::DaemonLost,
            Entry::Stop => Event::StopRequested,
            Entry::Exiting { cause, detail } => {
                Event::Exiting(wire::IncarnationEnd { cause, detail })
            }
            Entry::Git { hex: None } => Event::Git(None),
            Entry::Git { hex: Some(hex) } => Event::Git(Some(
                wire::Git::decode(crate::from_hex(&hex).ok()?.as_slice()).ok()?,
            )),
        })
    }
}

/// What stepping the interpreter through recorded entries produced.
#[derive(Debug, Default)]
pub struct Rerun {
    /// Every non-empty step, in order: what the agent journaled.
    pub steps: Vec<wire::Step>,
    /// The entries this build could not read and skipped, counted from one.
    pub unreadable: Vec<usize>,
}

/// Steps the interpreter through entries an incarnation recorded, as it
/// first stepped through them. Both an agent resuming and a dump's replay
/// go through here, so what a dump replays is what a resume does.
pub fn rerun<I: crate::Interpreter>(
    state: &mut I::State,
    entries: impl IntoIterator<Item = Entry>,
) -> Rerun {
    let mut rerun = Rerun::default();
    for (at, entry) in entries.into_iter().enumerate() {
        let Some(event) = entry.event() else {
            rerun.unreadable.push(at + 1);
            continue;
        };
        let stepped = I::step(state, event);
        if stepped.step != wire::Step::default() {
            rerun.steps.push(stepped.step);
        }
    }
    rerun
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Event, Fact};

    #[test]
    fn entries_round_trip_every_event() {
        let events = [
            Event::Fact(Fact {
                channel: Channel::Hook,
                payload: br#"{"a":1}"#.to_vec(),
            }),
            Event::Fact(Fact {
                channel: Channel::Stream,
                payload: vec![0xff, 0x00],
            }),
            Event::Fact(Fact {
                channel: Channel::SubagentTranscript,
                payload: br#"{"agentId":"a1","type":"user"}"#.to_vec(),
            }),
            Event::Input(wire::Input {
                input_id: b"i1".to_vec(),
                ..Default::default()
            }),
            Event::ChildInput {
                agent_id: vec![7; 16],
                input: wire::Input {
                    input_id: b"i2".to_vec(),
                    body: vec![1, 2, 3],
                    ..Default::default()
                },
            },
            Event::Tick { at_ms: 5 },
            Event::ProviderExit { code: Some(3) },
            Event::DaemonLost,
            Event::StopRequested,
            Event::Exiting(wire::IncarnationEnd {
                cause: wire::ExitCause::Stopped as i32,
                detail: None,
            }),
            Event::Exiting(wire::IncarnationEnd {
                cause: wire::ExitCause::ProviderExited as i32,
                detail: Some("exit code 3".into()),
            }),
            // A cause a newer build wrote is kept as its value.
            Event::Exiting(wire::IncarnationEnd {
                cause: 99,
                detail: None,
            }),
        ];
        for event in events {
            let line = serde_json::to_vec(&Entry::of(&event)).unwrap();
            let entry: Entry = serde_json::from_slice(&line).unwrap();
            assert_eq!(entry.event(), Some(event));
        }
    }

    /// The channel names a ring line carries are part of the facts file
    /// on disk: a line an earlier build wrote reads as it did, and a
    /// subagent's transcript row is written under a name of its own.
    #[test]
    fn channel_names_on_disk_hold() {
        let line = br#"{"event":"fact","channel":"transcript","text":"{}"}"#;
        let entry: Entry = serde_json::from_slice(line).unwrap();
        assert_eq!(
            entry.event(),
            Some(Event::Fact(Fact {
                channel: Channel::Transcript,
                payload: b"{}".to_vec(),
            }))
        );
        let written = serde_json::to_string(&Entry::of(&Event::Fact(Fact {
            channel: Channel::SubagentTranscript,
            payload: b"{}".to_vec(),
        })))
        .unwrap();
        assert_eq!(
            written,
            r#"{"event":"fact","channel":"subagent_transcript","text":"{}"}"#
        );
    }

    /// A person's input to a subagent is a ring line of its own, under a
    /// name an older ring simply lacks; its body is the encoded
    /// `ChildInput`, so it reads back whole.
    #[test]
    fn a_child_input_line_on_disk_holds() {
        let line = br#"{"event":"child_input","hex":"0a02070712040a026932"}"#;
        let entry: Entry = serde_json::from_slice(line).unwrap();
        let event = entry.event().unwrap();
        let Event::ChildInput { agent_id, input } = &event else {
            panic!("{event:?}");
        };
        assert_eq!(agent_id, &[7, 7]);
        assert_eq!(input.input_id, b"i2");
        let written = serde_json::to_string(&Entry::of(&event)).unwrap();
        assert!(
            written.starts_with(r#"{"event":"child_input","hex":""#),
            "{written}"
        );
        // A child input that carries no input is a line this build cannot
        // read, skipped as any other.
        let empty = Entry::ChildInput {
            hex: crate::to_hex(&wire::ChildInput::default().encode_to_vec()),
        };
        assert_eq!(empty.event(), None);
    }

    /// Entries this build cannot read are skipped and named; every other
    /// is stepped, and each step it made comes back in order.
    #[test]
    fn a_rerun_steps_every_readable_entry_and_names_the_rest() {
        use crate::Interpreter as _;
        use crate::testkind::TestKind;
        let text = |id: &str| Entry::Fact {
            channel: Channel::Stream,
            text: Some(format!(r#"{{"type":"text","id":"{id}","delta":"hi"}}"#)),
            hex: None,
        };
        let (mut state, _) = TestKind::<false>::initial(&wire::AgentSpec::default());
        let rerun = rerun::<TestKind>(
            &mut state,
            [
                text("a"),
                Entry::Input {
                    hex: "not hex".into(),
                },
                text("b"),
            ],
        );
        assert_eq!(rerun.unreadable, [2]);
        let keys: Vec<_> = rerun
            .steps
            .iter()
            .flat_map(|step| step.items.iter().map(|item| item.key.as_str()))
            .collect();
        assert_eq!(keys, ["a", "b"]);
    }
}
