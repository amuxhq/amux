//! A client session's record is enough to replay it: every recorded session
//! the repository has, each interpreter fixture of every kind committed the
//! way the daemon commits it and driven through the session model as a
//! driver would, replays from its record to the same structure after every
//! message and breaks the same check at the same step as from its whole
//! history. So does each with a row the stream never delivered, which
//! breaks the window, and so does a real daemon's dump with that break
//! planted. And nothing anybody wrote reaches the record's bytes.

use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Committer, Interpreter, Replayed, replay};
use prost::Message as _;
use replay_support::bundle::Bundle;
use replay_support::record::replays_alike;
use ui_state::{Connection, InputOutcome, Msg, SessionState};
use wire::dump::{ClientSessionEvents, ClientSessionHistory, ClientTraced, client_traced};
use wire::{Kind, SessionEvent, session_event};

use crate::support;

/// Small enough that the longer fixtures trim their window, page room and
/// all.
const CAP: usize = 12;

fn fixtures(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind)
}

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        cwd: "/Users/someone/project".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::Working as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

/// A fixture's emission as a session's driver would have applied it, each
/// message with the driver's clock: the connection, then each frame's
/// input, verdicts, committed rows and catalogue, caught up after the
/// first. The reader leaves the newest row a third of the way in and
/// returns two thirds of the way, so rows are held above the window too.
fn driven(frames: &[Replayed]) -> Vec<(i64, Msg)> {
    let mut committer = Committer::default();
    let mut msgs = vec![(0, Msg::Connection(Connection::Live))];
    let (leave, back) = (frames.len() / 3, frames.len() * 2 / 3);
    for (index, frame) in frames.iter().enumerate() {
        let at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(0);
        if index == leave && leave > 0 {
            msgs.push((at, Msg::Following(false)));
        }
        if index == back && back > leave {
            msgs.push((at, Msg::Following(true)));
        }
        if let Some(input) = &frame.input {
            msgs.push((at, Msg::Send(input.clone())));
        }
        for (id, verdict) in &frame.replies {
            msgs.push((
                at,
                Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())),
            ));
        }
        for committed in committer.commit(&frame.step) {
            msgs.push((at, Msg::Event(committed)));
        }
        if index == 0 {
            msgs.push((
                at,
                event(session_event::Of::CaughtUp(wire::CaughtUp {
                    revision: committer.revision,
                })),
            ));
        }
        if let Some((hash, catalogue)) = &frame.catalogue {
            msgs.push((
                at,
                Msg::Catalogue {
                    hash: hash.clone(),
                    catalogue: catalogue.clone(),
                },
            ));
        }
    }
    msgs
}

/// The history a driver would hand a dump: the session as it stood after
/// the first `from` messages, then every message after, with its size.
fn history(kind: Kind, msgs: &[(i64, Msg)], from: usize) -> ClientSessionHistory {
    let mut start = SessionState::new(agent(kind), CAP);
    for (_, msg) in &msgs[..from] {
        start.update(msg.clone());
    }
    let traced = msgs[from..]
        .iter()
        .enumerate()
        .map(|(seq, (at_ms, msg))| {
            let msg = msg.to_wire();
            ClientTraced {
                seq: (from + seq) as u64,
                at_ms: *at_ms,
                size: msg.encoded_len() as u32,
                event: Some(client_traced::Event::Msg(msg)),
            }
        })
        .collect();
    ClientSessionHistory {
        agent_id: b"agent".to_vec(),
        start: Some(start.to_wire()),
        events: Some(ClientSessionEvents { traced }),
        structure_only: false,
    }
}

/// The same messages without the first row that has a row before it and
/// a new row after it in order: a stream that skipped it, which leaves a
/// gap in the window.
fn skipping_a_row(msgs: &[(i64, Msg)]) -> Option<Vec<(i64, Msg)>> {
    let item = |msg: &Msg| match msg {
        Msg::Event(SessionEvent {
            of: Some(session_event::Of::Item(item)),
        }) => Some((item.key.clone(), item.order)),
        _ => None,
    };
    let rows: Vec<(usize, String, u64)> = msgs
        .iter()
        .enumerate()
        .filter_map(|(at, (_, msg))| item(msg).map(|(key, order)| (at, key, order)))
        .collect();
    let (skip, key, _) = rows.iter().find(|(at, _, order)| {
        let before = rows
            .iter()
            .any(|(earlier, _, held)| earlier < at && held + 1 == *order);
        let after = rows
            .iter()
            .any(|(later, _, next)| later > at && *next == order + 1);
        before && after
    })?;
    Some(
        msgs.iter()
            .enumerate()
            .filter(|(at, (_, msg))| {
                *at != *skip && item(msg).is_none_or(|(other, _)| other != *key)
            })
            .map(|(_, held)| held.clone())
            .collect(),
    )
}

/// Every fixture of a kind: the record replays alike from the opening and
/// from a checkpoint halfway, healthy and with a row skipped. Returns how
/// many skipped rows broke the window, so the gate is seen to cover breaks.
fn run_all<I: Interpreter>(kind: Kind, dir: &str) -> usize {
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json").then_some(path)
        })
        .collect();
    names.sort();
    assert!(!names.is_empty());
    let mut failures = Vec::new();
    let mut broken = 0;
    for path in &names {
        let name = path.file_stem().unwrap().to_string_lossy();
        let frames = match replay::<I>(path) {
            Ok(frames) => frames,
            Err(error) => {
                failures.push(format!("{dir}/{name}: {error}"));
                continue;
            }
        };
        let msgs = driven(&frames);
        let mut cases = vec![("whole", msgs.clone())];
        if let Some(skipped) = skipping_a_row(&msgs) {
            cases.push(("a row skipped", skipped));
        }
        for (case, msgs) in cases {
            for from in [0, msgs.len() / 2] {
                match replays_alike(&history(kind, &msgs, from)) {
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) if case == "whole" => failures.push(format!(
                        "{dir}/{name} ({case} from {from}): a healthy session broke its window"
                    )),
                    Ok(Err(_)) => broken += 1,
                    Err(why) => failures.push(format!("{dir}/{name} ({case} from {from}): {why}")),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    broken
}

#[test]
fn every_claude_pty_session_replays_alike_from_its_record() {
    assert!(run_all::<ClaudePty>(Kind::ClaudePty, "claude_pty") > 0);
}

#[test]
fn every_claude_sdk_session_replays_alike_from_its_record() {
    assert!(run_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk") > 0);
}

#[test]
fn every_codex_session_replays_alike_from_its_record() {
    assert!(run_all::<Codex>(Kind::Codex, "codex") > 0);
}

/// A real daemon's dump with a session that dropped a row: its record
/// fails at the same message, naming the same rows.
#[test]
fn a_dumped_session_that_broke_its_window_breaks_at_the_same_step_from_its_record() {
    let dumped = support::client_violation();
    let bundle = Bundle::open(dumped.path()).unwrap();
    let [client] = &bundle.clients[..] else {
        panic!("{} client sessions", bundle.clients.len());
    };
    let held = client.history.as_ref().unwrap();
    let history = ClientSessionHistory {
        agent_id: Vec::new(),
        start: Some(held.start.clone()),
        events: Some(ClientSessionEvents {
            traced: held.events.clone(),
        }),
        structure_only: false,
    };
    let failing = replays_alike(&history)
        .expect("the record replays alike")
        .expect_err("the window breaks");
    assert_eq!(failing.step, 5, "{failing:?}");
}

const MARKER: &str = "zebra-quartz-marker";

fn marked() -> String {
    format!("the {MARKER} said")
}

/// The hash of a file somebody attached: theirs as much as its name.
fn hash(file: &str) -> Vec<u8> {
    format!("{MARKER}:{file}").into_bytes()
}

fn blob(hash: &[u8]) -> wire::BlobRef {
    wire::BlobRef {
        hash: hash.to_vec(),
        name: marked(),
        mime: marked(),
        size: 3,
    }
}

/// Every place a session holds words somebody wrote, a name or a path,
/// with the marker in it.
fn marked_history() -> ClientSessionHistory {
    use wire::claude_item::Kind as Claude;
    let kind = Kind::ClaudeSdk;
    let text = |words: String| wire::ToolInput {
        subject: words,
        of: Some(wire::tool_input::Of::OtherJson(marked().into_bytes())),
    };
    let item = |key: &str, order: u64, body: Claude| wire::Item {
        key: key.to_owned(),
        order,
        revision: order,
        input_id: Vec::new(),
        text: marked(),
        attachments: vec![
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(&hash("image")))),
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::Text(wire::InlineText {
                    name: marked(),
                    text: marked(),
                })),
            },
        ],
        kind: kind as i32,
        body: wire::ClaudeItem { kind: Some(body) }.encode_to_vec(),
        at_ms: order as i64,
    };
    let items = vec![
        item("prompt", 1, Claude::Prompt(Default::default())),
        item(
            "tool",
            2,
            Claude::Tool(wire::ToolCall {
                name: marked(),
                state: wire::ToolState::Running as i32,
                outcome_text: marked(),
                server: marked(),
                input: Some(text(marked())),
                subagent: Some(wire::SubagentProgress {
                    last_tool: marked(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
        item(
            "error",
            3,
            Claude::ApiError(wire::ApiError {
                error_kind: marked(),
                message: marked(),
                will_retry: true,
                attempt: 2,
                max_attempts: 5,
                retry_at_ms: Some(9),
            }),
        ),
        item(
            "status",
            4,
            Claude::Status(wire::Status {
                status: wire::SdkStatus::Compacting as i32,
                word: marked(),
            }),
        ),
    ];
    let snapshot_body = wire::ClaudeSnapshot {
        asks: vec![wire::Ask {
            key: "ask".into(),
            item_key: "tool".into(),
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                ..Default::default()
            })),
            ..Default::default()
        }],
        model: Some(marked()),
        model_name: Some(marked()),
        permission: Some(marked()),
        provider_session: Some(marked()),
        running_calls: vec![wire::RunningCall {
            tool_use_id: "call".into(),
            tool_name: marked(),
            since_ms: 1,
        }],
        active_tasks: vec![wire::Task {
            task_id: "task".into(),
            description: marked(),
            state: wire::TaskState::Running as i32,
            last_tool: marked(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let queued = wire::QueuedInput {
        input_id: b"queued".to_vec(),
        text: marked(),
        attachments: vec![wire::Attachment {
            of: Some(wire::attachment::Of::File(blob(&hash("file")))),
        }],
        steer: false,
        sender: None,
    };
    let snapshot = wire::Snapshot {
        revision: 5,
        queue: vec![queued],
        kind: kind as i32,
        body: snapshot_body.encode_to_vec(),
        phase: wire::Phase::Working as i32,
        working_on: Some(marked()),
        at_ms: 5,
        phase_since_ms: 1,
        git: Some(wire::Git {
            branch: Some(marked()),
            ..Default::default()
        }),
        catalogue: Some(b"catalogue".to_vec()),
        background_jobs: 0,
    };
    let prompt = wire::Input::new(
        b"sent".to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: marked(),
                attachments: vec![wire::Attachment {
                    of: Some(wire::attachment::Of::Image(blob(&hash("sent-image")))),
                }],
            })),
        },
    );
    let mut named = agent(kind);
    named.name = marked();
    named.cwd = marked();
    named.working_on = Some(marked());
    named.exit_detail = Some(marked());
    let host = wire::HostEntry {
        host: Some(wire::Host {
            host_id: b"host".to_vec(),
            name: marked(),
            version: marked(),
            ..Default::default()
        }),
        generation: 1,
        ..Default::default()
    };
    let catalogue = wire::Catalogue {
        commands: vec![wire::OfferedCommand {
            name: marked(),
            description: marked(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut msgs = vec![
        Msg::Entry(named.clone()),
        Msg::Host(host),
        Msg::Connection(Connection::Live),
        event(session_event::Of::Snapshot(snapshot)),
    ];
    msgs.extend(
        items
            .iter()
            .cloned()
            .map(|item| event(session_event::Of::Item(item))),
    );
    msgs.extend([
        event(session_event::Of::CaughtUp(wire::CaughtUp { revision: 5 })),
        event(session_event::Of::Append(wire::Append {
            key: "tool".into(),
            base_revision: 2,
            revision: 6,
            text: marked(),
        })),
        Msg::Catalogue {
            hash: b"catalogue".to_vec(),
            catalogue,
        },
        Msg::Send(prompt.clone()),
        Msg::Sent(
            b"sent".to_vec(),
            InputOutcome::Reply(wire::SendInputResponse {
                of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                    refusal: wire::Refusal::Unspecified as i32,
                    detail: Some(marked()),
                })),
            }),
        ),
        Msg::Blob {
            hash: hash("image"),
            status: ui_state::BlobStatus::Failed(marked()),
        },
        Msg::Page {
            items: items.clone(),
            exhausted: true,
            epoch: 0,
        },
    ]);
    let msgs: Vec<(i64, Msg)> = msgs.into_iter().map(|msg| (7, msg)).collect();
    // From halfway, so the checkpoint holds marked rows, inputs and blobs
    // too.
    let mut history = history(kind, &msgs, msgs.len() / 2);
    history
        .start
        .as_mut()
        .unwrap()
        .inputs
        .push(wire::dump::ClientInput {
            input: Some(prompt),
            state: wire::dump::ClientInputState::Rejected as i32,
            rejected: Some(wire::Rejected {
                refusal: 0,
                detail: Some(marked()),
            }),
            ..Default::default()
        });
    history
}

#[test]
fn nothing_anybody_wrote_reaches_a_sessions_record() {
    let whole = marked_history();
    let marker = MARKER.as_bytes();
    let holds = |bytes: &[u8]| bytes.windows(marker.len()).any(|window| window == marker);
    assert!(
        holds(&whole.encode_to_vec()),
        "the history holds the marker"
    );
    let record = ui_state::record(whole.clone());
    assert!(record.structure_only);
    assert!(
        !holds(&record.encode_to_vec()),
        "the record holds the marker: {record:?}"
    );
    // A row's attachment and the blob that arrives for it still match:
    // their hash is one alias of its length.
    let fetched: Vec<Vec<u8>> = record
        .events
        .iter()
        .flat_map(|events| &events.traced)
        .filter_map(|traced| match &traced.event {
            Some(wire::dump::client_traced::Event::Msg(wire::dump::ClientSessionMsg {
                of: Some(wire::dump::client_session_msg::Of::Blob(blob)),
            })) => Some(blob.hash.clone()),
            _ => None,
        })
        .collect();
    let attached: Vec<Vec<u8>> = record
        .start
        .iter()
        .flat_map(|start| start.transcript.iter().flat_map(|window| &window.items))
        .flat_map(|item| &item.attachments)
        .filter_map(|attachment| match &attachment.of {
            Some(wire::attachment::Of::Image(blob)) => Some(blob.hash.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    assert_eq!(fetched[0].len(), hash("image").len());
    assert!(attached.contains(&fetched[0]), "{attached:?} {fetched:?}");
    // The same conclusions: the tool in flight, the retry, the compaction,
    // the ask and the announced call all survive.
    replays_alike(&whole)
        .expect("the record replays alike")
        .expect("the window holds");
    let sizes: Vec<u32> = record
        .events
        .unwrap()
        .traced
        .iter()
        .map(|traced| traced.size)
        .collect();
    assert!(
        sizes.iter().all(|size| *size > 0),
        "each event keeps its size: {sizes:?}"
    );
}

/// Rows of `kind` with these bodies, one after another, each also the
/// newest for a message, so the activity line reads each in turn.
fn rows_of(kind: Kind, bodies: Vec<Vec<u8>>) -> ClientSessionHistory {
    let mut msgs = vec![(0, Msg::Connection(Connection::Live))];
    for (at, body) in bodies.into_iter().enumerate() {
        let order = at as u64 + 1;
        msgs.push((
            order as i64,
            event(session_event::Of::Item(wire::Item {
                key: format!("row{order}"),
                order,
                revision: order,
                kind: kind as i32,
                body,
                at_ms: order as i64,
                ..Default::default()
            })),
        ));
    }
    msgs.push((
        99,
        event(session_event::Of::CaughtUp(wire::CaughtUp { revision: 99 })),
    ));
    history(kind, &msgs, 0)
}

/// The conclusions no recorded session draws on yet, each in a row of its
/// own: a subagent's own step and Claude's task-list bookkeeping, which a
/// run passes through, a subagent still running, Codex's open reasoning
/// and its running safety check, which the activity line names.
#[test]
fn every_conclusion_the_model_draws_from_a_body_survives_the_record() {
    use wire::claude_item::Kind as Claude;
    use wire::codex_item::Kind as Codex;
    let claude = |kind: Claude| wire::ClaudeItem { kind: Some(kind) }.encode_to_vec();
    let running = wire::ToolState::Running as i32;
    let tool = |call: wire::ToolCall| claude(Claude::Tool(call));
    let claude_rows = vec![
        tool(wire::ToolCall {
            name: "Task".into(),
            state: running,
            subagent: Some(wire::SubagentProgress {
                finished: false,
                ..Default::default()
            }),
            ..Default::default()
        }),
        tool(wire::ToolCall {
            name: "Read".into(),
            state: running,
            parent_key: "row1".into(),
            ..Default::default()
        }),
        tool(wire::ToolCall {
            name: "TodoWrite".into(),
            state: wire::ToolState::Succeeded as i32,
            input: Some(wire::ToolInput {
                subject: "the list".into(),
                of: Some(wire::tool_input::Of::TaskList(wire::Empty {})),
            }),
            ..Default::default()
        }),
        tool(wire::ToolCall {
            name: "Bash".into(),
            state: wire::ToolState::Succeeded as i32,
            ..Default::default()
        }),
    ];
    let codex = |kind: Codex| wire::CodexItem { kind: Some(kind) }.encode_to_vec();
    let codex_rows = vec![
        codex(Codex::Reasoning(wire::Reasoning {
            complete: false,
            summary: vec!["weighing it".into()],
        })),
        codex(Codex::SafetyCheck(wire::SafetyCheck { running: true })),
        codex(Codex::Reasoning(wire::Reasoning {
            complete: true,
            summary: Vec::new(),
        })),
    ];
    for (kind, rows) in [(Kind::ClaudeSdk, claude_rows), (Kind::Codex, codex_rows)] {
        let history = rows_of(kind, rows.clone());
        // Each prefix, so each row is the newest once.
        for upto in 1..=rows.len() {
            let mut prefix = history.clone();
            let traced = &mut prefix.events.as_mut().unwrap().traced;
            traced.truncate(upto + 1);
            replays_alike(&prefix)
                .unwrap_or_else(|why| panic!("{kind:?} up to row {upto}: {why}"))
                .expect("the window holds");
        }
    }
}
