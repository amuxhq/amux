//! Authored records and a tape that applies messages and prints what the
//! session looks like after each one.

#![allow(dead_code)]

use prost::Message;
use ui_state::{
    Connection, InputOutcome, ItemClass, Msg, Outcome, RunIndex, SessionState, Transcript,
};
use wire::{
    Agent, Ask, CodexAsk, Input, Item, Kind, Lifecycle, Phase, QueuedInput, SendInputResponse,
    SessionEvent, Snapshot, ToolClass, ToolState, session_event,
};

pub const KINDS: [Kind; 3] = [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex];

/// The window's cap in cases about something else: more rows than any of
/// them delivers.
pub const CAP: usize = 200;

pub fn agent(kind: Kind) -> Agent {
    Agent {
        agent_id: b"agent-1".to_vec(),
        host_id: b"host-a".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        cwd: "/src".into(),
        lifecycle: Lifecycle::Live as i32,
        phase: Phase::Idle as i32,
        incarnation: 1,
        ..Agent::default()
    }
}

pub fn with_phase(mut agent: Agent, phase: Phase) -> Agent {
    agent.phase = phase as i32;
    agent
}

pub fn exited(mut agent: Agent, cause: wire::ExitCause) -> Agent {
    agent.lifecycle = Lifecycle::Exited as i32;
    agent.exit_cause = cause as i32;
    agent
}

/// An item body, authored once and encoded for any kind.
#[derive(Clone, Debug)]
pub enum Body {
    Prompt,
    Text {
        complete: bool,
    },
    Thinking {
        complete: bool,
    },
    Tool {
        name: &'static str,
        state: ToolState,
        explore: bool,
        subagent: bool,
    },
    Turn,
    Retry {
        attempt: u32,
        max: u32,
    },
    Compacting,
    Boundary,
}

pub fn encode(kind: Kind, body: &Body) -> Vec<u8> {
    use wire::{claude_pty_item as pty, claude_sdk_item as sdk, codex_item as codex};
    let class = |name: &str, explore: bool| {
        (match (explore, name) {
            (false, _) => ToolClass::Consequential,
            (true, "Read") => ToolClass::Read,
            (true, _) => ToolClass::Search,
        }) as i32
    };
    let tool = |name: &str, state: ToolState, explore: bool, subagent: bool| wire::ToolCall {
        name: name.into(),
        state: state as i32,
        class: class(name, explore),
        subagent: subagent.then(|| wire::SubagentProgress {
            tool_count: 1,
            last_tool: "Read".into(),
            finished: false,
        }),
        ..wire::ToolCall::default()
    };
    let retry = |attempt: u32, max: u32| wire::ApiError {
        error_kind: "overloaded".into(),
        will_retry: true,
        attempt,
        max_attempts: max,
        ..wire::ApiError::default()
    };
    match kind {
        Kind::ClaudePty => {
            let kind = match body.clone() {
                Body::Prompt => pty::Kind::Prompt(wire::Prompt {}),
                Body::Text { complete } => pty::Kind::Message(wire::Text { complete }),
                Body::Thinking { complete } => pty::Kind::Thinking(wire::Thinking { complete }),
                Body::Tool {
                    name,
                    state,
                    explore,
                    subagent,
                } => pty::Kind::Tool(tool(name, state, explore, subagent)),
                Body::Turn => pty::Kind::Turn(wire::Turn::default()),
                Body::Retry { attempt, max } => pty::Kind::ApiError(retry(attempt, max)),
                Body::Compacting => pty::Kind::Compaction(wire::Compaction::default()),
                Body::Boundary => pty::Kind::Boundary(wire::Boundary::default()),
            };
            wire::ClaudePtyItem { kind: Some(kind) }.encode_to_vec()
        }
        Kind::ClaudeSdk => {
            let kind = match body.clone() {
                Body::Prompt => sdk::Kind::Prompt(wire::Prompt {}),
                Body::Text { complete } => sdk::Kind::Message(wire::Text { complete }),
                Body::Thinking { complete } => sdk::Kind::Thinking(wire::Thinking { complete }),
                Body::Tool {
                    name,
                    state,
                    explore,
                    subagent,
                } => sdk::Kind::Tool(tool(name, state, explore, subagent)),
                Body::Turn => sdk::Kind::Turn(wire::Turn::default()),
                Body::Retry { attempt, max } => sdk::Kind::ApiError(retry(attempt, max)),
                Body::Compacting => sdk::Kind::Status(wire::Status {
                    status: wire::SdkStatus::Compacting as i32,
                    word: "compacting".into(),
                }),
                Body::Boundary => sdk::Kind::Boundary(wire::Boundary::default()),
            };
            wire::ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()
        }
        Kind::Codex => {
            let kind = match body.clone() {
                Body::Prompt => codex::Kind::Prompt(wire::Prompt {}),
                Body::Text { complete } => codex::Kind::Message(wire::Text { complete }),
                Body::Thinking { complete } => codex::Kind::Reasoning(wire::Reasoning {
                    complete,
                    summary: vec![],
                }),
                Body::Tool {
                    name,
                    state,
                    explore,
                    subagent,
                } => {
                    let of = if subagent {
                        wire::work::Of::Collab(wire::CollabWork::default())
                    } else {
                        wire::work::Of::Command(wire::CommandWork {
                            command: name.into(),
                            ..wire::CommandWork::default()
                        })
                    };
                    codex::Kind::Work(wire::Work {
                        of: Some(of),
                        state: state as i32,
                        class: class(name, explore),
                        ..wire::Work::default()
                    })
                }
                Body::Turn => codex::Kind::Turn(wire::Turn::default()),
                Body::Retry { attempt, max } => codex::Kind::Error(retry(attempt, max)),
                // Codex has no compacting status item; a boundary stands in.
                Body::Compacting => codex::Kind::Boundary(wire::Boundary::default()),
                Body::Boundary => codex::Kind::Boundary(wire::Boundary::default()),
            };
            wire::CodexItem { kind: Some(kind) }.encode_to_vec()
        }
        Kind::Unspecified => Vec::new(),
    }
}

pub fn item(kind: Kind, order: u64, revision: u64, text: &str, body: Body) -> Item {
    Item {
        agent: b"agent-1".to_vec(),
        key: format!("k{order}"),
        order,
        revision,
        producer_version: "test".into(),
        text: text.into(),
        kind: kind as i32,
        body: encode(kind, &body),
        at_ms: 1_000 * order as i64,
        ..Item::default()
    }
}

pub fn text(kind: Kind, order: u64, revision: u64, text: &str) -> Item {
    item(kind, order, revision, text, Body::Text { complete: true })
}

pub fn streaming(kind: Kind, order: u64, revision: u64, text: &str) -> Item {
    item(kind, order, revision, text, Body::Text { complete: false })
}

pub fn prompt(kind: Kind, order: u64, revision: u64, text: &str, input_id: &[u8]) -> Item {
    let mut item = item(kind, order, revision, text, Body::Prompt);
    item.input_id = input_id.to_vec();
    item
}

pub fn read(kind: Kind, order: u64, revision: u64) -> Item {
    item(
        kind,
        order,
        revision,
        "",
        Body::Tool {
            name: "Read",
            state: ToolState::Succeeded,
            explore: true,
            subagent: false,
        },
    )
}

pub fn grep(kind: Kind, order: u64, revision: u64) -> Item {
    item(
        kind,
        order,
        revision,
        "",
        Body::Tool {
            name: "Grep",
            state: ToolState::Succeeded,
            explore: true,
            subagent: false,
        },
    )
}

pub fn thinking(kind: Kind, order: u64, revision: u64) -> Item {
    item(kind, order, revision, "", Body::Thinking { complete: true })
}

pub fn turn(kind: Kind, order: u64, revision: u64) -> Item {
    item(kind, order, revision, "", Body::Turn)
}

pub fn command(kind: Kind, order: u64, revision: u64, state: ToolState) -> Item {
    item(
        kind,
        order,
        revision,
        "",
        Body::Tool {
            name: "Bash",
            state,
            explore: false,
            subagent: false,
        },
    )
}

/// An open ask in the kind's own shape.
pub fn ask(kind: Kind, key: &str) -> Vec<u8> {
    match kind {
        Kind::Codex => CodexAsk {
            key: key.into(),
            body: Some(wire::codex_ask::Body::Command(wire::CommandApproval {
                command: "cargo test".into(),
                ..wire::CommandApproval::default()
            })),
            ..CodexAsk::default()
        }
        .encode_to_vec(),
        _ => Ask {
            key: key.into(),
            body: Some(wire::ask::Body::Permission(wire::PermissionAsk {
                tool_name: "Bash".into(),
                ..wire::PermissionAsk::default()
            })),
            ..Ask::default()
        }
        .encode_to_vec(),
    }
}

/// A snapshot body with these asks open and every other field at its
/// explicit unknown.
pub fn snapshot_body(kind: Kind, asks: &[&str]) -> Vec<u8> {
    match kind {
        Kind::ClaudePty => wire::ClaudePtySnapshot {
            asks: asks
                .iter()
                .map(|key| Ask::decode(&ask(kind, key)[..]).unwrap())
                .collect(),
            ..Default::default()
        }
        .encode_to_vec(),
        Kind::ClaudeSdk => wire::ClaudeSdkSnapshot {
            asks: asks
                .iter()
                .map(|key| Ask::decode(&ask(kind, key)[..]).unwrap())
                .collect(),
            ..Default::default()
        }
        .encode_to_vec(),
        Kind::Codex => wire::CodexSnapshot {
            asks: asks
                .iter()
                .map(|key| CodexAsk::decode(&ask(kind, key)[..]).unwrap())
                .collect(),
            ..Default::default()
        }
        .encode_to_vec(),
        Kind::Unspecified => Vec::new(),
    }
}

pub fn snapshot(
    kind: Kind,
    revision: u64,
    phase: Phase,
    asks: &[&str],
    queue: &[(&[u8], bool)],
) -> Snapshot {
    Snapshot {
        agent: b"agent-1".to_vec(),
        revision,
        queue: queue
            .iter()
            .map(|(id, steer)| QueuedInput {
                input_id: id.to_vec(),
                text: format!("queued {}", String::from_utf8_lossy(id)),
                steer: *steer,
                ..QueuedInput::default()
            })
            .collect(),
        kind: kind as i32,
        body: snapshot_body(kind, asks),
        phase: phase as i32,
        working_on: None,
        at_ms: 1_000 * revision as i64,
        ..Snapshot::default()
    }
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

pub fn ev_item(item: Item) -> Msg {
    event(session_event::Of::Item(item))
}

pub fn ev_snapshot(snapshot: Snapshot) -> Msg {
    event(session_event::Of::Snapshot(snapshot))
}

pub fn ev_append(key: &str, base: u64, revision: u64, text: &str) -> Msg {
    event(session_event::Of::Append(wire::Append {
        agent: b"agent-1".to_vec(),
        key: key.into(),
        base_revision: base,
        revision,
        text: text.into(),
    }))
}

pub fn caught_up(revision: u64) -> Msg {
    event(session_event::Of::CaughtUp(wire::CaughtUp { revision }))
}

pub fn reset() -> Msg {
    event(session_event::Of::Reset(wire::Reset {}))
}

pub fn detached() -> Msg {
    event(session_event::Of::Detached(wire::Detached {}))
}

pub fn lagged() -> Msg {
    event(session_event::Of::Lagged(wire::Lagged {}))
}

pub fn reconnecting() -> Msg {
    Msg::Connection(Connection::Reconnecting)
}

/// A page requested now, under the state's current epoch.
pub fn apply_page(state: &mut SessionState, items: Vec<Item>, exhausted: bool) -> Outcome {
    let epoch = state.epoch();
    apply_checked(
        state,
        Msg::Page {
            items,
            exhausted,
            epoch,
        },
    )
}

pub fn prompt_input(kind: Kind, id: &[u8], text: &str) -> Input {
    use wire::KindInput as Of;
    let prompt = wire::PromptInput {
        text: text.into(),
        attachments: vec![],
    };
    let of = match kind {
        Kind::ClaudePty => Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Prompt(prompt)),
        }),
        Kind::ClaudeSdk => Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(prompt)),
        }),
        _ => Of::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(prompt)),
        }),
    };
    Input::new(id.to_vec(), of)
}

pub fn answer_input(kind: Kind, id: &[u8], ask_key: &str) -> Input {
    use wire::KindInput as Of;
    let answer = wire::ClaudeAnswer {
        ask_key: ask_key.into(),
        of: None,
    };
    let of = match kind {
        Kind::ClaudePty => Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Answer(answer)),
        }),
        Kind::ClaudeSdk => Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Answer(answer)),
        }),
        _ => Of::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Answer(wire::CodexAnswer {
                ask_key: ask_key.into(),
                of: Some(wire::codex_answer::Of::Approve(
                    wire::Decision::Approve as i32,
                )),
            })),
        }),
    };
    Input::new(id.to_vec(), of)
}

pub fn send_now_input(kind: Kind, id: &[u8], target: &[u8]) -> Input {
    use wire::KindInput as Of;
    let now = wire::SendQueuedNow {
        queued_input_id: target.to_vec(),
    };
    let of = match kind {
        Kind::ClaudePty => Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::SendNow(now)),
        }),
        Kind::ClaudeSdk => Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::SendNow(now)),
        }),
        _ => Of::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::SendNow(now)),
        }),
    };
    Input::new(id.to_vec(), of)
}

pub fn accepted(queued: bool) -> InputOutcome {
    InputOutcome::Reply(SendInputResponse {
        of: Some(wire::send_input_response::Of::Accepted(wire::Accepted {
            queued,
        })),
    })
}

pub fn rejected(refusal: wire::Refusal) -> InputOutcome {
    InputOutcome::Reply(SendInputResponse {
        of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
            refusal: refusal as i32,
            detail: None,
        })),
    })
}

/// Every held row's projection: what a view reads for it, including the
/// item drawn on its row.
pub fn projection(transcript: &Transcript) -> Vec<(String, String)> {
    transcript
        .iter()
        .map(|held| {
            // A step reads differently once its turn has ended.
            let ended = matches!(held.class, ItemClass::Tool(_))
                && transcript.turn_ended_after(held.item.order);
            let row = format!(
                "{:?}|{:?}|{ended}",
                held,
                transcript.run_at(held.item.order)
            );
            (held.item.key.clone(), row)
        })
        .collect()
}

/// Checks the invariants every update must keep: the run index equals a
/// rebuild; the changed set names exactly the rows that differ.
pub fn apply_checked(state: &mut SessionState, msg: Msg) -> Outcome {
    let before = projection(state.transcript());
    let outcome = state.update(msg.clone());
    let after = projection(state.transcript());
    assert_eq!(
        state.transcript().runs(),
        &RunIndex::rebuild(state.transcript()),
        "run index drifted from a rebuild after {msg:?}"
    );

    let mut differs: Vec<String> = Vec::new();
    for (key, row) in &after {
        if before.iter().find(|(k, _)| k == key).map(|(_, r)| r) != Some(row) {
            differs.push(key.clone());
        }
    }
    for (key, _) in &before {
        if !after.iter().any(|(k, _)| k == key) {
            differs.push(key.clone());
        }
    }
    let mut changed = outcome.changed.clone();
    changed.sort();
    differs.sort();
    assert_eq!(changed, differs, "changed keys wrong after {msg:?}");
    outcome
}

/// [`apply_checked`], and the block invariant after it: orders contiguous
/// and every index naming only held rows.
pub fn apply_block(state: &mut SessionState, msg: Msg) -> Outcome {
    let outcome = apply_checked(state, msg.clone());
    if let Err(broken) = state.transcript().check() {
        panic!("block invariant broken after {msg:?}: {broken}");
    }
    outcome
}

/// One line per held row, then the session's state.
pub fn describe(state: &SessionState) -> String {
    let mut out = String::new();
    let transcript = state.transcript();
    for held in transcript.iter() {
        let run = transcript
            .run_at(held.item.order)
            .map(|run| format!(" run[{}..{} x{}]", run.oldest, run.newest, run.steps))
            .unwrap_or_default();
        out.push_str(&format!(
            "    {:>3} {} r{} {:?} {:?}{run}\n",
            held.item.order, held.item.key, held.item.revision, held.class, held.item.text
        ));
    }
    let asks: Vec<&str> = state.open_asks().iter().map(|ask| ask.key()).collect();
    let queue: Vec<String> = state
        .queue()
        .iter()
        .map(|row| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&row.entry.input_id),
                if row.steered { "(steered)" } else { "" }
            )
        })
        .collect();
    let inputs: Vec<String> = state
        .inputs()
        .iter()
        .map(|sent| format!("{}={:?}", String::from_utf8_lossy(&sent.id), sent.state))
        .collect();
    out.push_str(&format!(
        "    caught_up={} reset_pending={} composer={:?} phase={:?} asks={asks:?} queue={queue:?} inputs={inputs:?}\n",
        state.caught_up(),
        state.reset_pending(),
        state.composer(),
        state.phase(),
    ));
    out
}

/// Applies messages with the invariants checked, recording a readable
/// transcript of what the session showed after each.
pub struct Tape {
    pub state: SessionState,
    pub lines: String,
}

impl Tape {
    pub fn new(agent: Agent) -> Tape {
        let state = SessionState::new(agent, CAP);
        let mut tape = Tape {
            lines: String::new(),
            state,
        };
        tape.lines.push_str("open\n");
        tape.lines.push_str(&describe(&tape.state));
        tape
    }

    pub fn apply(&mut self, label: &str, msg: Msg) -> Outcome {
        let outcome = apply_checked(&mut self.state, msg);
        self.lines.push_str(&format!(
            "{label}  (changed {:?}{}{})\n",
            outcome.changed,
            if outcome.reloaded { ", reloaded" } else { "" },
            outcome
                .need_get
                .as_ref()
                .map(|key| format!(", get {key}"))
                .unwrap_or_default()
        ));
        self.lines.push_str(&describe(&self.state));
        outcome
    }

    pub fn print(&self, title: &str) {
        println!("== {title}\n{}", self.lines);
    }
}

/// A small deterministic generator for property cases.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}
