//! View invariants over small authored session states, and the authored
//! goldens for what no interpreter fixture reaches: an input not confirmed,
//! the exited composer, steering a queued prompt, an unanswerable or
//! dismissed ask, and the overview and composer facts.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use prost::Message;
use ui_state::{AgentKey, Attention, FleetMsg, FleetState, InputOutcome, Key, Msg, SessionState};
use ui_view::*;
use wire::{Item, Kind, Phase, SessionEvent, ToolClass, ToolState, session_event};

const KINDS: [Kind; 3] = [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex];

/// The window's cap: more rows than any case here delivers.
const CAP: usize = 200;

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: Phase::Working as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn body(kind: Kind, tool: Option<(&str, bool)>, text: bool) -> Vec<u8> {
    use wire::{claude_pty_item as pty, claude_sdk_item as sdk, codex_item as codex};
    let class = |explore: bool| if explore { ToolClass::Read } else { ToolClass::Consequential } as i32;
    match (kind, tool) {
        (Kind::Codex, Some((name, explore))) => wire::CodexItem {
            kind: Some(codex::Kind::Work(wire::Work {
                of: Some(wire::work::Of::Command(wire::CommandWork {
                    command: name.into(),
                    action: if name == "Read" {
                        "read".into()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                })),
                state: ToolState::Succeeded as i32,
                class: class(explore),
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        (Kind::Codex, None) if text => wire::CodexItem {
            kind: Some(codex::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (Kind::Codex, None) => wire::CodexItem {
            kind: Some(codex::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
        (_, Some((name, explore))) => {
            let tool = wire::ToolCall {
                name: name.into(),
                state: ToolState::Succeeded as i32,
                class: class(explore),
                input_json: br#"{"file_path":"src/lib.rs"}"#.to_vec(),
                ..Default::default()
            };
            if kind == Kind::ClaudePty {
                wire::ClaudePtyItem {
                    kind: Some(pty::Kind::Tool(tool)),
                }
                .encode_to_vec()
            } else {
                wire::ClaudeSdkItem {
                    kind: Some(sdk::Kind::Tool(tool)),
                }
                .encode_to_vec()
            }
        }
        (Kind::ClaudePty, None) if text => wire::ClaudePtyItem {
            kind: Some(pty::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (Kind::ClaudePty, None) => wire::ClaudePtyItem {
            kind: Some(pty::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
        (_, None) if text => wire::ClaudeSdkItem {
            kind: Some(sdk::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (_, None) => wire::ClaudeSdkItem {
            kind: Some(sdk::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
    }
}

fn item(kind: Kind, order: u64, revision: u64, tool: Option<(&str, bool)>) -> Item {
    Item {
        key: format!("k{order}"),
        order,
        revision,
        text: if tool.is_none() {
            format!("text {order}")
        } else {
            String::new()
        },
        kind: wire::kind_tag(kind).into(),
        body: body(kind, tool, true),
        at_ms: order as i64 * 1000,
        ..Item::default()
    }
}

fn read(kind: Kind, order: u64, revision: u64) -> Item {
    item(kind, order, revision, Some(("Read", true)))
}

fn snapshot(kind: Kind, phase: Phase, body: Vec<u8>, queue: Vec<wire::QueuedInput>) -> Msg {
    event(session_event::Of::Snapshot(wire::Snapshot {
        kind: wire::kind_tag(kind).into(),
        body,
        phase: phase as i32,
        queue,
        ..wire::Snapshot::default()
    }))
}

fn caught_up() -> Msg {
    event(session_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }))
}

fn page(state: &mut SessionState, items: Vec<Item>, exhausted: bool) {
    let epoch = state.epoch();
    state.update(Msg::Page {
        items,
        exhausted,
        epoch,
    });
}

fn all_rows(state: &SessionState, opts: &ChatOptions) -> Vec<Row> {
    chat_rows(state, 0..=u64::MAX, opts)
}

/// A random walk of live items, revisions and pages; after every step the
/// row invariants hold.
#[test]
fn row_ids_never_move_and_rows_by_keys_equal_rows_by_range() {
    for kind in KINDS {
        for seed in 1..25u64 {
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            let mut state = SessionState::new(agent(kind), CAP);
            state.update(snapshot(kind, Phase::Working, vec![], vec![]));
            let start = 40;
            let mut head = start;
            state.update(event(session_event::Of::Item(read(kind, head, 1))));
            state.update(caught_up());
            let open = HashSet::new();
            let opts = ChatOptions {
                tools: ToolRows::Collapse { open: &open },
            };
            for revision in 2..60u64 {
                let before: Vec<Key> = all_rows(&state, &opts)
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                let choice = next() % 4;
                if choice == 3 {
                    let oldest = state.oldest_order().unwrap();
                    if oldest > 1 {
                        let items = (oldest.saturating_sub(3).max(1)..oldest)
                            .rev()
                            .map(|o| read(kind, o, revision))
                            .collect();
                        page(&mut state, items, next() % 7 == 0);
                    }
                } else {
                    let order = if choice < 2 {
                        head += 1;
                        head
                    } else {
                        start + next() % (head - start + 1)
                    };
                    let tool = match next() % 3 {
                        0 => Some(("Read", true)),
                        1 => Some(("Bash", false)),
                        _ => None,
                    };
                    state.update(event(session_event::Of::Item(item(
                        kind, order, revision, tool,
                    ))));
                }
                let rows = all_rows(&state, &opts);
                let after: Vec<Key> = rows.iter().map(|row| row.id.clone()).collect();
                // Ids keep their relative order; new ids arrive only at the edges.
                let kept: Vec<&Key> = after.iter().filter(|id| before.contains(id)).collect();
                assert_eq!(
                    kept,
                    before.iter().collect::<Vec<_>>(),
                    "{kind:?} seed {seed}"
                );
                if let (Some(first), Some(last)) = (before.first(), before.last()) {
                    let lo = after.iter().position(|id| id == first).unwrap();
                    let hi = after.iter().position(|id| id == last).unwrap();
                    assert_eq!(
                        hi - lo + 1,
                        before.len(),
                        "nothing inserted between held rows"
                    );
                }
                // Rows for keys are the same rows as by range.
                let keys: Vec<Key> = after.iter().filter(|_| next() % 2 == 0).cloned().collect();
                let by_keys = chat_rows_for(&state, &keys, &opts);
                let by_range: Vec<Row> = rows
                    .iter()
                    .filter(|row| keys.contains(&row.id))
                    .cloned()
                    .collect();
                assert_eq!(by_keys, by_range);
                // open_below only for the oldest run, and only while older
                // history exists.
                let transcript = state.transcript();
                let first = rows
                    .iter()
                    .find_map(|row| row.run.as_ref())
                    .map(|run| &run.first);
                for row in &rows {
                    if let Some(run) = &row.run
                        && run.open_below
                    {
                        assert_eq!(Some(&run.first), first);
                        assert!(transcript.has_older());
                    }
                }
            }
        }
    }
}

#[test]
fn a_folded_run_draws_at_its_newest_step_and_opens_by_any_step() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Working, vec![], vec![]));
        for order in 1..=3 {
            state.update(event(session_event::Of::Item(item(
                kind, order, order, None,
            ))));
        }
        for order in 4..=8 {
            state.update(event(session_event::Of::Item(read(kind, order, order))));
        }
        state.update(event(session_event::Of::Item(item(kind, 9, 9, None))));
        let none = HashSet::new();
        let opts = ChatOptions {
            tools: ToolRows::Collapse { open: &none },
        };
        let rows = chat_rows(&state, 2..=5, &opts);
        let orders: Vec<u64> = rows.iter().map(|row| row.order).collect();
        assert_eq!(orders, vec![2, 3, 4, 5], "a range is its own rows");
        let rows = chat_rows(&state, 1..=9, &opts);
        let last = rows.iter().find(|row| row.order == 8).unwrap();
        let run = last.run.as_ref().unwrap();
        assert!(run.is_last() && !last.collapsed);
        assert_eq!((run.first.as_str(), run.steps), ("k4", 5));
        assert_eq!(run.counts.as_ref().unwrap().reads, 5);
        assert!(rows[3..7].iter().all(|row| row.collapsed));
        // Opened by one of its steps.
        let open: HashSet<Key> = ["k6".to_owned()].into();
        let opts = ChatOptions {
            tools: ToolRows::Collapse { open: &open },
        };
        assert!(
            chat_rows(&state, 4..=8, &opts)
                .iter()
                .all(|row| !row.collapsed)
        );
        let hidden = chat_rows(
            &state,
            1..=9,
            &ChatOptions {
                tools: ToolRows::Hide,
            },
        );
        assert!(hidden.iter().filter(|row| row.collapsed).count() == 5);
        let shown = chat_rows(
            &state,
            1..=9,
            &ChatOptions {
                tools: ToolRows::ShowAll,
            },
        );
        assert!(shown.iter().all(|row| !row.collapsed));
    }
}

#[test]
fn attachments_keep_their_positions() {
    let image = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![1; 32],
            name: "a.png".into(),
            mime: "image/png".into(),
            size: 10,
        })),
    };
    let text = wire::Attachment {
        of: Some(wire::attachment::Of::Text(wire::InlineText {
            name: "pasted".into(),
            text: "x\ny".into(),
        })),
    };
    let tokens = composer_tokens("see \u{FFFC} and \u{FFFC}.", &[image.clone(), text.clone()]);
    assert_eq!(
        tokens,
        vec![
            Segment::Text("see ".into()),
            Segment::Attachment(AttachmentView::of(&image)),
            Segment::Text(" and ".into()),
            Segment::Attachment(AttachmentView::of(&text)),
            Segment::Text(".".into()),
        ]
    );
    assert!(matches!(
        tokens[3],
        Segment::Attachment(AttachmentView::Text { lines: 2, .. })
    ));
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        let mut prompt = item(kind, 1, 1, None);
        prompt.body = body(kind, None, false);
        prompt.text = "\u{FFFC}first".into();
        prompt.attachments = vec![image.clone()];
        state.update(event(session_event::Of::Item(prompt)));
        let rows = all_rows(&state, &ChatOptions::default());
        assert_eq!(
            rows[0].kind,
            RowKind::Prompt {
                text: vec![
                    Segment::Attachment(AttachmentView::of(&image)),
                    Segment::Text("first".into())
                ],
                steered: false
            }
        );
    }
}

fn fleet_entry(
    host: &str,
    id: &str,
    phase: Phase,
    parent: Option<&str>,
    since: i64,
) -> wire::Agent {
    wire::Agent {
        agent_id: id.as_bytes().to_vec(),
        host_id: host.as_bytes().to_vec(),
        kind: Kind::ClaudeSdk as i32,
        name: id.into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: phase as i32,
        parent: parent.map(|parent| wire::AgentParent {
            host_id: host.as_bytes().to_vec(),
            agent_id: parent.as_bytes().to_vec(),
        }),
        phase_since_ms: since,
        ..wire::Agent::default()
    }
}

fn listed(agent: wire::Agent) -> FleetMsg {
    FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Agent(agent)),
    }))
}

fn fleet_row(host: &str, id: &str, phase: Phase, parent: Option<&str>, since: i64) -> FleetMsg {
    listed(fleet_entry(host, id, phase, parent, since))
}

/// Home as names, depths and sections, in order.
fn home(view: &FleetView) -> Vec<(SectionKind, String, u32)> {
    view.sections
        .iter()
        .flat_map(|section| {
            section
                .rows
                .iter()
                .map(|row| (section.kind, row.card.name.clone(), row.depth))
        })
        .collect()
}

fn no_sessions() -> HashMap<AgentKey, SessionLine> {
    HashMap::new()
}

#[test]
fn the_fleet_places_families_by_their_loudest_member_newest_first() {
    let mut fleet = FleetState::new();
    for msg in [
        fleet_row("a", "quiet", Phase::Idle, None, 900),
        fleet_row("a", "parent", Phase::Idle, None, 100),
        fleet_row("a", "child", Phase::NeedsYou, Some("parent"), 50),
        fleet_row("a", "asker", Phase::NeedsYou, None, 40),
        fleet_row("a", "busy", Phase::Working, None, 10),
    ] {
        fleet.update(msg);
    }
    let mut gone = fleet_entry("a", "gone", Phase::Idle, None, 2000);
    gone.lifecycle = wire::Lifecycle::Exited as i32;
    fleet.update(listed(gone));
    use SectionKind::*;
    let all = |_: &wire::Agent| true;
    let view = fleet_view(&fleet, b"a", &no_sessions(), &HashSet::new(), &all);
    // The parent's family needs you through its child, and is ordered by
    // when the child began to: after the asker, which began later.
    assert_eq!(
        home(&view),
        vec![
            (NeedsYou, "parent".into(), 0),
            (NeedsYou, "asker".into(), 0),
            (Live, "quiet".into(), 0),
            (Live, "busy".into(), 0),
            (Exited, "gone".into(), 0),
        ]
    );
    assert_eq!(
        view.sections
            .iter()
            .map(|section| section.families)
            .collect::<Vec<_>>(),
        vec![2, 2, 1]
    );
    // Only the exited start folded.
    assert_eq!(
        view.sections
            .iter()
            .map(|section| section.folded_by_default)
            .collect::<Vec<_>>(),
        vec![false, false, true]
    );
    // The counts are of agents, folded or not: the child and the asker
    // need the person, and one agent works.
    assert_eq!((view.need_you, view.working), (2, 1));
    // Folded, the parent speaks for the child that needs you.
    let parent = &view.sections[0].rows[0];
    let loud = parent.loud.as_ref().unwrap();
    assert_eq!(loud.name, "child");
    assert_eq!(
        (&parent.second_line, &loud.second_line),
        (&SecondLine::Blank, &SecondLine::Blank)
    );
    let expand: HashSet<Vec<u8>> = [b"parent".to_vec()].into();
    let view = fleet_view(&fleet, b"a", &no_sessions(), &expand, &all);
    assert_eq!(
        home(&view)[..3],
        [
            (NeedsYou, "parent".into(), 0),
            (NeedsYou, "child".into(), 1),
            (NeedsYou, "asker".into(), 0),
        ]
    );
    assert!(view.sections[0].rows[0].loud.is_none());
    // A filter keeps a family when any member matches, folded or not.
    let only_child = |agent: &wire::Agent| agent.name == "child";
    let view = fleet_view(&fleet, b"a", &no_sessions(), &HashSet::new(), &only_child);
    assert_eq!(home(&view), vec![(NeedsYou, "parent".into(), 0)]);
    // A filter leaves the counts whole.
    assert_eq!((view.need_you, view.working), (2, 1));
    let card = fleet_card(&fleet, b"a", b"parent").unwrap();
    assert_eq!(
        (card.attention, card.family_attention, card.children),
        (Attention::Idle, Attention::NeedsYou, 1)
    );
    // Folded, the family still counts its two members.
    assert_eq!(card.members, 2);
    let alone = fleet_card(&fleet, b"a", b"busy").unwrap();
    assert_eq!(alone.members, 1);
    // The child's ask is not hosted in the parent's chat: the family header
    // carries its attention and how many need the person, and nothing more.
    let header = family_header(&fleet, b"a", b"parent").unwrap();
    assert_eq!(
        (header.attention, header.need_you),
        (Attention::NeedsYou, 1)
    );
    assert_eq!(
        header.children[0].agent,
        AgentKey {
            host: b"a".to_vec(),
            agent: b"child".to_vec()
        }
    );
    assert!(family_header(&fleet, b"a", b"quiet").is_none());
    // The child's own ask is in its own chat, so its header counts nobody.
    assert_eq!(family_header(&fleet, b"a", b"child").unwrap().need_you, 0);
    assert_eq!(
        family_header(&fleet, b"a", b"child")
            .unwrap()
            .parent
            .unwrap()
            .name,
        "parent"
    );
}

#[test]
fn the_review_document_parses_files_hunks_and_places_comments() {
    let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +10,4 @@ fn a()\n keep\n-old\n+new\n+more\n keep2\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\ndiff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
    let comments = vec![
        wire::ReviewComment {
            path: "src/a.rs".into(),
            line: 11,
            old_line: 0,
            text: "why new?".into(),
        },
        wire::ReviewComment {
            path: "src/a.rs".into(),
            line: 0,
            old_line: 11,
            text: "keep old".into(),
        },
        wire::ReviewComment {
            path: "new.txt".into(),
            line: 0,
            old_line: 0,
            text: "whole file".into(),
        },
    ];
    let doc = review_doc(
        &wire::Diff {
            head: "abc".into(),
            ..Default::default()
        },
        patch,
        &comments,
    );
    assert_eq!(doc.files.len(), 3);
    assert_eq!((doc.added, doc.removed), (3, 1));
    let a = &doc.files[0];
    assert_eq!(
        (a.path.as_str(), a.status, a.added, a.removed),
        ("src/a.rs", FileStatus::Modified, 2, 1)
    );
    let lines = &a.hunks[0].lines;
    assert_eq!(lines[0].old_line, Some(10));
    assert_eq!(
        (lines[2].kind, lines[2].new_line, lines[2].comments.clone()),
        (LineKind::Added, Some(11), vec!["why new?".to_owned()])
    );
    assert_eq!(lines[1].comments, vec!["keep old".to_owned()]);
    assert_eq!(doc.files[1].status, FileStatus::Added);
    assert_eq!(doc.files[1].comments, vec!["whole file".to_owned()]);
    assert_eq!(
        (
            doc.files[2].status,
            doc.files[2].old_path.as_deref(),
            doc.files[2].path.as_str()
        ),
        (FileStatus::Renamed, Some("x"), "y")
    );
}

fn full_snapshot(kind: Kind, asks: Vec<wire::Ask>, codex_asks: Vec<wire::CodexAsk>) -> Vec<u8> {
    let meter = Some(wire::UsageMeter {
        used_percent: 91.0,
        resets_at_ms: Some(5),
        state: wire::UsageState::NearLimit as i32,
    });
    let claude_usage = wire::ClaudeUsage {
        state: wire::UsageState::NearLimit as i32,
        windows: vec![wire::ClaudeUsageWindow {
            limit: wire::ClaudeLimit::FiveHour as i32,
            model: None,
            provider_name: "five_hour".into(),
            meter,
        }],
    };
    let codex_usage = wire::CodexUsage {
        state: wire::UsageState::NearLimit as i32,
        windows: vec![wire::CodexUsageWindow {
            limit: wire::CodexLimit::FiveHour as i32,
            window_minutes: 300,
            meter,
        }],
        credits: None,
    };
    let servers = wire::ToolServerHealth {
        state: wire::HealthState::Degraded as i32,
        servers: vec![
            wire::ToolServer {
                name: "ok".into(),
                status: wire::ToolServerStatus::Ready as i32,
                error: String::new(),
            },
            wire::ToolServer {
                name: "github".into(),
                status: wire::ToolServerStatus::Failed as i32,
                error: "401".into(),
            },
        ],
    };
    let sign_in = wire::SignIn {
        state: wire::SignInState::Expired as i32,
        account: "me@example.com".into(),
        message: "Log in again".into(),
    };
    let background = wire::BackgroundJobs {
        known: true,
        jobs: vec![
            wire::BackgroundJob {
                step: "t1".into(),
                command: "npm run dev".into(),
                started_at_ms: 1_000,
            },
            wire::BackgroundJob {
                step: "t2".into(),
                command: "cargo watch".into(),
                started_at_ms: 2_000,
            },
        ],
    };
    let tasks = wire::TaskList {
        known: true,
        entries: vec![
            wire::TaskListEntry {
                id: "1".into(),
                subject: "Read".into(),
                status: wire::TaskListStatus::Completed as i32,
                active_form: String::new(),
            },
            wire::TaskListEntry {
                id: "2".into(),
                subject: "Update copy".into(),
                status: wire::TaskListStatus::InProgress as i32,
                active_form: "Updating the copy".into(),
            },
            wire::TaskListEntry {
                id: "3".into(),
                subject: "Test".into(),
                status: wire::TaskListStatus::Pending as i32,
                active_form: String::new(),
            },
        ],
    };
    let context = wire::ContextMeter {
        known: true,
        used_tokens: 171_000,
        window_tokens: Some(200_000),
        breakdown: vec![],
    };
    match kind {
        Kind::ClaudePty => wire::ClaudePtySnapshot {
            asks,
            tasks: Some(tasks),
            context: Some(context),
            model: Some("opus".into()),
            permission: Some("default".into()),
            usage: Some(claude_usage.clone()),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_jobs: Some(background.clone()),
            ..Default::default()
        }
        .encode_to_vec(),
        Kind::ClaudeSdk => wire::ClaudeSdkSnapshot {
            asks,
            tasks: Some(tasks),
            context: Some(context),
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission: Some("default".into()),
            usage: Some(claude_usage),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_jobs: Some(background.clone()),
            ..Default::default()
        }
        .encode_to_vec(),
        _ => wire::CodexSnapshot {
            asks: codex_asks,
            plan: Some(tasks),
            context: Some(context),
            model: Some("gpt".into()),
            effort: Some("high".into()),
            approval_policy: Some("on-request".into()),
            sandbox: Some("workspace-write".into()),
            usage: Some(codex_usage),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_jobs: Some(background.clone()),
            ..Default::default()
        }
        .encode_to_vec(),
    }
}

fn prompt_input(kind: Kind, id: &[u8], text: &str) -> wire::Input {
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
    wire::Input::new(id.to_vec(), of)
}

fn queued(id: &[u8], steer: bool, from_agent: Option<&str>) -> wire::QueuedInput {
    wire::QueuedInput {
        input_id: id.to_vec(),
        text: format!("queued {}", String::from_utf8_lossy(id)),
        steer,
        sender: from_agent.map(|name| wire::Sender {
            value: Some(wire::sender::Value::Agent(wire::AgentSender {
                name: name.into(),
                ..Default::default()
            })),
        }),
        ..Default::default()
    }
}

/// The authored story each kind's golden tells.
fn authored(kind: Kind) -> String {
    let mut out = String::new();
    let mut state = SessionState::new(agent(kind), CAP);
    let unanswerable = wire::Ask {
        key: "menu-1".into(),
        body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
            reason: "a menu this build can't read".into(),
        })),
        ..Default::default()
    };
    let codex_question = wire::CodexAsk {
        key: "q-1".into(),
        body: Some(wire::codex_ask::Body::Question(wire::QuestionAsk {
            questions: vec![wire::Question {
                header: "Env".into(),
                question: "Which environment?".into(),
                options: vec![
                    wire::QuestionOption {
                        label: "staging (Recommended)".into(),
                        ..Default::default()
                    },
                    wire::QuestionOption {
                        label: "production".into(),
                        ..Default::default()
                    },
                ],
                allow_other: true,
                ..Default::default()
            }],
            ..Default::default()
        })),
        ..Default::default()
    };
    let body = full_snapshot(kind, vec![unanswerable], vec![codex_question]);
    let mut show = |label: &str, state: &SessionState| {
        let _ = writeln!(out, "## {label}");
        let _ = writeln!(out, "  composer {:?}", composer(state, 10_000));
        let _ = writeln!(out, "  queue {:?}", queue_rows(state));
        let _ = writeln!(out, "  underway {:?}", prompts_underway(state, |_| false));
        let _ = writeln!(out, "  refused {:?}", refused_prompts(state));
        match ask_card(state) {
            Some(card) => {
                let _ = writeln!(
                    out,
                    "  ask {} {:?} body {:?}",
                    card.key, card.state, card.body
                );
                let choices: Vec<String> = card
                    .choices
                    .iter()
                    .map(|choice| {
                        format!(
                            "{:?}{}",
                            choice.outcome,
                            if choice.primary { "*" } else { "" }
                        )
                    })
                    .collect();
                let _ = writeln!(out, "    choices [{}]", choices.join(", "));
            }
            None => {
                let _ = writeln!(out, "  ask none");
            }
        }
        let _ = writeln!(out, "  overview {:?}", overview(state, None));
        let _ = writeln!(
            out,
            "  context {:?} sign-in {:?}",
            context(state),
            sign_in(state)
        );
    };
    state.update(snapshot(
        kind,
        Phase::NeedsYou,
        body.clone(),
        vec![
            queued(b"q1", false, None),
            queued(b"m1", false, Some("reviewer")),
        ],
    ));
    state.update(Msg::Entry(wire::Agent {
        phase: Phase::NeedsYou as i32,
        ..agent(kind)
    }));
    state.update(caught_up());
    show(
        "an ask that can't be answered here, two queued prompts, every overview fact",
        &state,
    );

    state.update(Msg::Send(prompt_input(kind, b"q2", "mine")));
    state.update(Msg::Sent(
        b"q2".to_vec(),
        ui_state::InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                queued: true,
            })),
        }),
    ));
    let queue = vec![
        queued(b"q1", true, None),
        queued(b"m1", false, Some("reviewer")),
        queued(b"q2", false, None),
    ];
    state.update(snapshot(kind, Phase::NeedsYou, body.clone(), queue));
    show(
        "q1 sent now: it reads steered; my own prompt queued behind",
        &state,
    );

    state.update(Msg::Send(prompt_input(kind, b"lost", "did this land?")));
    state.update(Msg::Sent(b"lost".to_vec(), InputOutcome::Lost));
    state.update(Msg::Send(prompt_input(kind, b"nope", "rejected one")));
    state.update(Msg::Sent(
        b"nope".to_vec(),
        ui_state::InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                reason: "draining".into(),
            })),
        }),
    ));
    state.update(Msg::Send(prompt_input(kind, b"flying", "in flight")));
    show(
        "not confirmed (resend or discard), rejected with its reason, and one in flight",
        &state,
    );

    state.update(Msg::Entry(wire::Agent {
        lifecycle: wire::Lifecycle::Exited as i32,
        exit_cause: Some("killed".into()),
        ..agent(kind)
    }));
    show(
        "exited: the ask is dismissed and the composer offers Resume",
        &state,
    );

    state.update(event(session_event::Of::Detached(wire::Detached {})));
    state.update(Msg::Entry(wire::Agent {
        incarnation: 2,
        ..agent(kind)
    }));
    show(
        "resumed but detached: drafting continues, sending waits",
        &state,
    );
    out
}

#[test]
fn authored_view_goldens() {
    let mut failures = Vec::new();
    for kind in KINDS {
        let rendered = authored(kind);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens/authored")
            .join(format!("{}.golden", wire::kind_tag(kind)));
        if std::env::var("UI_VIEW_UPDATE_GOLDENS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &rendered).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(golden) if golden == rendered => {}
            Ok(_) => failures.push(format!(
                "{} differs; rerun with UI_VIEW_UPDATE_GOLDENS=1 and review",
                path.display()
            )),
            Err(_) => failures.push(format!("{} missing", path.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A resume's first prompt is sent with the resume, so it reads sending;
/// when the new incarnation holds it in its queue, the queue row alone
/// draws it.
#[test]
fn a_sent_prompt_the_queue_lists_is_drawn_once() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Starting, vec![], vec![]));
        state.update(caught_up());
        state.update(Msg::Send(prompt_input(kind, b"r1", "resumed")));
        assert_eq!(
            prompts_underway(&state, |_| false).len(),
            1,
            "sending until it lands"
        );
        state.update(snapshot(
            kind,
            Phase::NeedsYou,
            vec![],
            vec![queued(b"r1", false, None)],
        ));
        assert!(prompts_underway(&state, |_| false).is_empty());
        assert_eq!(queue_rows(&state).len(), 1);
    }
}

/// A refused Send now (terminal Claude too old to steer) leaves the prompt
/// queued to run at turn end: its row reads queued again with Send now still
/// offered, and nothing is left on its way. The reason is the press's reply.
#[test]
fn a_refused_send_now_leaves_the_prompt_queued() {
    let kind = Kind::ClaudePty;
    let mut state = SessionState::new(agent(kind), CAP);
    state.update(snapshot(
        kind,
        Phase::Working,
        vec![],
        vec![queued(b"q1", false, None)],
    ));
    state.update(caught_up());
    state.update(Msg::Send(wire::Input::new(
        b"now".to_vec(),
        wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::SendNow(wire::SendQueuedNow {
                queued_input_id: b"q1".to_vec(),
            })),
        },
    )));
    assert!(
        queue_rows(&state)[0].steered,
        "steered while the press is in flight"
    );
    state.update(Msg::Sent(
        b"now".to_vec(),
        InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                reason: "Send now needs Claude 2.1.283 or later".into(),
            })),
        }),
    ));
    let rows = queue_rows(&state);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].steered);
    assert!(rows[0].can_send_now);
    assert!(rows[0].can_withdraw);
    assert!(prompts_underway(&state, |_| false).is_empty());
}

#[test]
fn unknown_renders_one_way() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Starting, vec![], vec![]));
        assert_eq!(overview(&state, None), Overview::default());
        assert_eq!((context(&state), sign_in(&state)), (None, None));
    }
}

fn host_entry(host: &str, signed_in: Option<bool>) -> FleetMsg {
    FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(wire::HostEntry {
            host_id: host.as_bytes().to_vec(),
            name: host.into(),
            signed_in,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            ..wire::HostEntry::default()
        })),
    }))
}

fn set_host(fleet: &mut FleetState, host: &str, change: impl FnOnce(&mut wire::HostEntry)) {
    let FleetMsg::Event(mut event) = host_entry(host, None) else {
        unreachable!()
    };
    if let Some(wire::inventory_event::Of::Host(entry)) = &mut event.of {
        if let Some(known) = fleet.host(host.as_bytes()) {
            *entry = known.clone();
        }
        change(entry);
    }
    fleet.update(FleetMsg::Event(event));
}

#[test]
fn a_host_is_reached_by_its_route_or_away_or_offline_for_what_this_machine_knows() {
    let mut fleet = FleetState::new();
    fleet.update(host_entry("desk", Some(true)));
    // A profile never bound to an account is not signed out.
    fleet.update(host_entry("laptop", None));
    set_host(&mut fleet, "desk", |entry| {
        entry.via = wire::HostVia::Relay as i32
    });
    assert_eq!(
        reach(&fleet, b"laptop", b"desk"),
        Reach::Online(wire::HostVia::Relay)
    );
    // This machine always reaches itself, and knows nothing of a host it
    // was never told about.
    assert!(reach(&fleet, b"laptop", b"laptop").online());
    assert_eq!(reach(&fleet, b"laptop", b"cabin"), Reach::Offline);

    set_host(&mut fleet, "desk", |entry| {
        entry.presence = wire::Presence::Offline as i32
    });
    assert_eq!(reach(&fleet, b"laptop", b"desk"), Reach::Offline);
    set_host(&mut fleet, "desk", |entry| {
        entry.presence = wire::Presence::Unspecified as i32
    });
    assert_eq!(reach(&fleet, b"laptop", b"desk"), Reach::Offline);
    set_host(&mut fleet, "desk", |entry| {
        entry.presence = wire::Presence::Away as i32
    });
    assert_eq!(reach(&fleet, b"laptop", b"desk"), Reach::Away(Away::Plain));

    // Signed out, a host this machine does not reach is away for that.
    set_host(&mut fleet, "laptop", |entry| entry.signed_in = Some(false));
    assert_eq!(
        reach(&fleet, b"laptop", b"desk"),
        Reach::Away(Away::SignedOut)
    );
    assert!(reach(&fleet, b"laptop", b"laptop").online());
    // The desk's own sign-in says nothing about this machine.
    assert!(reach(&fleet, b"desk", b"laptop").online());
    // A host reached directly needs no account.
    set_host(&mut fleet, "desk", |entry| {
        entry.presence = wire::Presence::Online as i32;
        entry.via = wire::HostVia::Direct as i32;
    });
    assert_eq!(
        reach(&fleet, b"laptop", b"desk"),
        Reach::Online(wire::HostVia::Direct)
    );

    // A host that no longer trusts this machine says so first, whatever
    // route still reaches it.
    set_host(&mut fleet, "desk", |entry| entry.revoked = Some(true));
    assert_eq!(
        reach(&fleet, b"laptop", b"desk"),
        Reach::Away(Away::Revoked)
    );
}

fn settings_of(kind: Kind, body: Vec<u8>) -> SettingsView {
    settings_offering(kind, body, Vec::new(), Vec::new())
}

/// Settings for an agent whose fetched catalogue offers `models` and
/// `commands`, and the permissions and modes its kind's interpreter offers.
fn settings_offering(
    kind: Kind,
    body: Vec<u8>,
    models: Vec<wire::OfferedModel>,
    commands: Vec<wire::OfferedCommand>,
) -> SettingsView {
    let (permissions, modes) = offered_controls(kind);
    settings_with(
        kind,
        body,
        wire::Catalogue {
            models,
            commands,
            permissions,
            modes,
            ..Default::default()
        },
    )
}

fn settings_with(kind: Kind, body: Vec<u8>, catalogue: wire::Catalogue) -> SettingsView {
    let mut state = SessionState::new(agent(kind), CAP);
    state.set_catalogue(wire::Catalogue {
        hash: b"offered".to_vec(),
        ..catalogue
    });
    state.update(event(session_event::Of::Snapshot(wire::Snapshot {
        kind: wire::kind_tag(kind).into(),
        body,
        phase: Phase::Idle as i32,
        catalogue: Some(b"offered".to_vec()),
        ..wire::Snapshot::default()
    })));
    settings(&state)
}

/// What each kind's interpreter offers to set: Claude's permissions (auto
/// for the `sonnet` model only; terminal Claude's not settable), Codex's
/// permissions and modes.
fn offered_controls(kind: Kind) -> (Vec<wire::OfferedPermission>, Vec<wire::OfferedMode>) {
    let permission = |value: &str, name: &str| wire::OfferedPermission {
        value: value.into(),
        display_name: name.into(),
        normal: value == "default",
        settable: kind != Kind::ClaudePty,
        ..Default::default()
    };
    match kind {
        Kind::Codex => (
            vec![
                permission("read-only", "Read only"),
                permission("default", "Default"),
                permission("auto", "Auto"),
                wire::OfferedPermission {
                    never_asks: true,
                    ..permission("full-access", "Full access")
                },
            ],
            vec![
                wire::OfferedMode {
                    value: "default".into(),
                    display_name: "Default".into(),
                    normal: true,
                    settable: true,
                },
                wire::OfferedMode {
                    value: "plan".into(),
                    display_name: "Plan".into(),
                    normal: false,
                    settable: true,
                },
            ],
        ),
        _ => (
            vec![
                permission("default", "Ask"),
                permission("acceptEdits", "Accept edits"),
                permission("plan", "Plan"),
                wire::OfferedPermission {
                    models: vec!["sonnet".into()],
                    ..permission("auto", "Auto")
                },
            ],
            Vec::new(),
        ),
    }
}

fn offered(value: &str, efforts: &[&str], default: Option<&str>) -> wire::OfferedModel {
    wire::OfferedModel {
        value: value.into(),
        display_name: value.to_uppercase(),
        description: String::new(),
        efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
        default_effort: default.map(Into::into),
        resolved_model: format!("id-{value}"),
    }
}

fn command(name: &str) -> wire::OfferedCommand {
    wire::OfferedCommand {
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn settings_mark_the_current_model_effort_and_permission() {
    let view = settings_offering(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            permission: Some("plan".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        vec![
            offered("default", &["low", "high"], None),
            offered("sonnet", &["low", "medium", "high"], None),
        ],
        vec![
            command("compact"),
            command("config"),
            command("stripe:test-cards"),
        ],
    );
    let current: Vec<&str> = view
        .models
        .iter()
        .filter(|model| model.current)
        .map(|model| model.value.as_str())
        .collect();
    assert_eq!(current, ["sonnet"]);
    assert_eq!(
        view.efforts
            .iter()
            .map(|effort| (effort.value.as_str(), effort.current))
            .collect::<Vec<_>>(),
        [("low", false), ("medium", false), ("high", true)],
        "the current model's efforts"
    );
    let permissions: Vec<(&str, bool, bool)> = view
        .permissions
        .iter()
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.current,
                permission.settable,
            )
        })
        .collect();
    assert_eq!(
        permissions,
        [
            ("default", false, true),
            ("acceptEdits", false, true),
            ("plan", true, true),
            ("auto", false, true),
        ],
        "auto is settable while a model it names runs"
    );
    assert!(view.modes.is_empty(), "Claude has no modes");
    assert_eq!(
        view.commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["compact", "stripe:test-cards"],
        "a terminal-only command is dropped"
    );
    assert_eq!(
        view.changeable,
        Changeability {
            model: Changeable::Pick,
            effort: Changeable::Pick,
            permission: Changeable::Pick,
            mode: Changeable::NotOffered,
        },
        "headless Claude takes a model, effort and permission pick"
    );
}

#[test]
fn a_reported_value_outside_the_offer_is_shown_as_current() {
    let view = settings_offering(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("claude-haiku-4-5-20251001".into()),
            effort: Some("max".into()),
            permission: Some("dontAsk".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        vec![offered("haiku", &[], None)],
        Vec::new(),
    );
    let last = view.models.last().unwrap();
    assert!(last.current && last.unlisted);
    assert_eq!(last.value, "claude-haiku-4-5-20251001");
    assert_eq!(
        last.display_name, "claude-haiku-4-5-20251001",
        "no name from the interpreter or the catalogue: the id"
    );
    assert!(!view.models[0].current);
    assert_eq!(view.efforts.len(), 1);
    assert!(view.efforts[0].current && view.efforts[0].unlisted);
    let permission = view.permissions.last().unwrap();
    assert_eq!(permission.value, "dontAsk");
    assert!(permission.current && permission.unlisted && !permission.settable);
    assert!(!permission.custom);
    assert_eq!(
        view.permissions
            .iter()
            .filter(|permission| permission.current)
            .count(),
        1
    );
    assert!(
        !view.permissions[3].settable,
        "auto names sonnet, and haiku runs"
    );

    let nothing = settings_of(Kind::ClaudeSdk, Vec::new());
    assert!(nothing.models.is_empty() && nothing.efforts.is_empty());
    assert!(nothing.permissions.iter().all(|mode| !mode.current));
}

#[test]
fn codex_offers_permissions_and_modes_and_settings_outside_them_read_custom() {
    let codex = |snapshot: wire::CodexSnapshot| {
        settings_offering(
            Kind::Codex,
            wire::CodexSnapshot {
                model: Some("gpt-a".into()),
                ..snapshot
            }
            .encode_to_vec(),
            vec![offered("gpt-a", &["low", "medium"], Some("medium"))],
            vec![command("config")],
        )
    };
    let view = codex(wire::CodexSnapshot {
        permission: Some("full-access".into()),
        mode: Some("plan".into()),
        approval_policy: Some("never".into()),
        sandbox: Some("danger-full-access".into()),
        ..Default::default()
    });
    let permissions: Vec<(&str, bool, bool)> = view
        .permissions
        .iter()
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.current,
                permission.never_asks,
            )
        })
        .collect();
    assert_eq!(
        permissions,
        [
            ("read-only", false, false),
            ("default", false, false),
            ("auto", false, false),
            ("full-access", true, true),
        ]
    );
    let modes: Vec<(&str, bool)> = view
        .modes
        .iter()
        .map(|mode| (mode.value.as_str(), mode.current))
        .collect();
    assert_eq!(modes, [("default", false), ("plan", true)]);
    assert_eq!(
        view.efforts
            .iter()
            .map(|effort| (effort.value.as_str(), effort.default, effort.current))
            .collect::<Vec<_>>(),
        [("low", false, false), ("medium", true, true)],
        "no effort reported: the model's default is the one in force"
    );
    assert_eq!(view.commands.len(), 1, "a Codex skill is never filtered");
    assert_eq!(
        view.changeable,
        Changeability {
            model: Changeable::Pick,
            effort: Changeable::Pick,
            permission: Changeable::Pick,
            mode: Changeable::Pick,
        }
    );

    let view = codex(wire::CodexSnapshot {
        approval_policy: Some("untrusted".into()),
        sandbox: Some("workspace-write".into()),
        ..Default::default()
    });
    let custom = view.permissions.last().unwrap();
    assert_eq!(custom.value, "", "settings that match no name");
    assert!(custom.custom && custom.current && custom.unlisted && !custom.settable);
    assert_eq!(view.permissions.len(), 5);
    assert_eq!(
        setting_input(Kind::Codex, &SettingChange::Permission(String::new())),
        None
    );

    let unsaid = codex(wire::CodexSnapshot::default());
    assert!(
        unsaid
            .permissions
            .iter()
            .all(|permission| !permission.current),
        "nothing reported yet is not custom"
    );
}

#[test]
fn an_offered_alias_is_marked_for_the_model_id_it_resolves_to() {
    let view = settings_offering(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("id-sonnet".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        vec![
            offered("default", &["low", "high"], None),
            offered("sonnet", &["low", "medium", "high"], None),
        ],
        Vec::new(),
    );
    let current: Vec<(&str, bool)> = view
        .models
        .iter()
        .filter(|model| model.current)
        .map(|model| (model.value.as_str(), model.unlisted))
        .collect();
    assert_eq!(
        current,
        [("sonnet", false)],
        "the reported id marks the alias that resolves to it"
    );
}

#[test]
fn terminal_claude_shows_what_it_reports_and_says_how_to_type_a_change() {
    let (permissions, _) = offered_controls(Kind::ClaudePty);
    let view = settings_with(
        Kind::ClaudePty,
        wire::ClaudePtySnapshot {
            model: Some("claude-sonnet-5".into()),
            permission: Some("acceptEdits".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        wire::Catalogue {
            permissions,
            ..Default::default()
        },
    );
    assert_eq!(
        view.models
            .iter()
            .map(|model| (model.value.as_str(), model.current, model.unlisted))
            .collect::<Vec<_>>(),
        [("claude-sonnet-5", true, true)],
        "the reported model alone: nothing is offered"
    );
    assert!(view.efforts.is_empty(), "nothing reports the effort");
    assert!(view.commands.is_empty());
    assert_eq!(
        view.permissions
            .iter()
            .map(|permission| (
                permission.value.as_str(),
                permission.current,
                permission.settable
            ))
            .collect::<Vec<_>>(),
        [
            ("default", false, false),
            ("acceptEdits", true, false),
            ("plan", false, false),
            ("auto", false, false),
        ],
        "listed, but reached only by cycling"
    );
    assert_eq!(
        view.changeable,
        Changeability {
            model: Changeable::ByTyping("/model".into()),
            effort: Changeable::ByTyping("/effort".into()),
            permission: Changeable::Cycle,
            mode: Changeable::NotOffered,
        }
    );

    let sdk = settings_of(Kind::ClaudeSdk, Vec::new());
    assert_eq!(
        sdk.changeable,
        Changeability {
            permission: Changeable::Pick,
            ..Changeability::default()
        },
        "headless Claude picks its permission, and its model once it lists models"
    );
}

#[test]
fn the_running_model_has_one_name_and_only_unusual_controls_are_summarised() {
    let claude = |permission: &str| {
        let catalogue = {
            let (permissions, modes) = offered_controls(Kind::ClaudeSdk);
            wire::Catalogue {
                models: vec![offered("sonnet", &["low", "high"], Some("high"))],
                permissions,
                modes,
                ..Default::default()
            }
        };
        let mut state = SessionState::new(agent(Kind::ClaudeSdk), CAP);
        state.set_catalogue(wire::Catalogue {
            hash: b"offered".to_vec(),
            ..catalogue
        });
        state.update(event(session_event::Of::Snapshot(wire::Snapshot {
            kind: wire::kind_tag(Kind::ClaudeSdk).into(),
            body: wire::ClaudeSdkSnapshot {
                model: Some("id-sonnet".into()),
                model_name: Some("Sonnet 5".into()),
                permission: Some(permission.into()),
                ..Default::default()
            }
            .encode_to_vec(),
            phase: Phase::Idle as i32,
            catalogue: Some(b"offered".to_vec()),
            ..wire::Snapshot::default()
        })));
        state
    };
    let normal = controls(&claude("default"));
    assert_eq!(
        normal.model.as_deref(),
        Some("Sonnet 5"),
        "the interpreter's name"
    );
    assert_eq!(
        normal.effort.as_deref(),
        Some("high"),
        "the model's default"
    );
    assert_eq!(normal.permission, None, "the normal permission goes unsaid");
    let plan = controls(&claude("plan"));
    assert_eq!(
        plan.permission.map(|permission| permission.display_name),
        Some("Plan".to_owned())
    );

    let custom = |snapshot: wire::CodexSnapshot| {
        let mut state = SessionState::new(agent(Kind::Codex), CAP);
        let (permissions, modes) = offered_controls(Kind::Codex);
        state.set_catalogue(wire::Catalogue {
            hash: b"offered".to_vec(),
            permissions,
            modes,
            ..Default::default()
        });
        state.update(event(session_event::Of::Snapshot(wire::Snapshot {
            kind: wire::kind_tag(Kind::Codex).into(),
            body: snapshot.encode_to_vec(),
            phase: Phase::Idle as i32,
            catalogue: Some(b"offered".to_vec()),
            ..wire::Snapshot::default()
        })));
        controls(&state)
    };
    let codex = custom(wire::CodexSnapshot {
        approval_policy: Some("untrusted".into()),
        sandbox: Some("workspace-write".into()),
        mode: Some("plan".into()),
        ..Default::default()
    });
    assert!(codex.permission.is_some_and(|permission| permission.custom));
    assert_eq!(codex.mode.map(|mode| mode.value), Some("plan".to_owned()));
    let unsaid = custom(wire::CodexSnapshot {
        permission: Some("default".into()),
        mode: Some("default".into()),
        ..Default::default()
    });
    assert_eq!((unsaid.permission, unsaid.mode), (None, None));

    // Terminal Claude offers no models: the interpreter's tidied name names
    // the reported one, in the list as beside the composer.
    let mut state = SessionState::new(agent(Kind::ClaudePty), CAP);
    state.update(event(session_event::Of::Snapshot(wire::Snapshot {
        kind: wire::kind_tag(Kind::ClaudePty).into(),
        body: wire::ClaudePtySnapshot {
            model: Some("claude-sonnet-5".into()),
            model_name: Some("Sonnet 5".into()),
            permission: Some("plan".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        phase: Phase::Idle as i32,
        ..wire::Snapshot::default()
    })));
    assert_eq!(settings(&state).models[0].display_name, "Sonnet 5");
    let pty = controls(&state);
    assert_eq!(pty.model.as_deref(), Some("Sonnet 5"));
    assert_eq!(
        pty.permission, None,
        "nothing says plan is unusual until the catalogue is held"
    );
}

/// A host's Codex catalogue: two models, a permission only the first
/// takes, one never settable, and two modes.
fn new_agent_catalogue() -> wire::Catalogue {
    let permission = |value: &str, normal: bool, models: &[&str]| wire::OfferedPermission {
        value: value.into(),
        display_name: value.to_uppercase(),
        normal,
        settable: true,
        models: models.iter().map(|model| (*model).into()).collect(),
        ..Default::default()
    };
    let (_, modes) = offered_controls(Kind::Codex);
    wire::Catalogue {
        models: vec![
            offered("astra", &["low", "high"], Some("low")),
            wire::OfferedModel {
                display_name: String::new(),
                ..offered("sol", &["low", "medium"], Some("medium"))
            },
        ],
        permissions: vec![
            permission("default", true, &[]),
            permission("auto", false, &["astra"]),
            wire::OfferedPermission {
                settable: false,
                ..permission("locked", false, &[])
            },
        ],
        modes,
        ..Default::default()
    }
}

#[test]
fn a_new_agent_lists_what_its_host_offers_for_the_chosen_model() {
    let catalogue = new_agent_catalogue();
    let nothing = new_agent_settings(&catalogue, &NewAgentChoices::default());
    assert_eq!(
        nothing
            .models
            .iter()
            .map(|model| (model.display_name.as_str(), model.current))
            .collect::<Vec<_>>(),
        [("ASTRA", false), ("sol", false)],
        "a model without a name reads its value"
    );
    assert!(nothing.efforts.is_empty(), "no model: no efforts");
    assert_eq!(
        nothing
            .permissions
            .iter()
            .map(|permission| permission.value.as_str())
            .collect::<Vec<_>>(),
        ["default"],
        "one naming models waits for one of them; one not settable never shows"
    );
    assert_eq!(
        nothing
            .modes
            .iter()
            .map(|mode| (mode.value.as_str(), mode.current))
            .collect::<Vec<_>>(),
        [("default", true), ("plan", false)],
        "none chosen: the normal mode is in force"
    );
    assert_eq!(
        nothing.changeable,
        Changeability {
            model: Changeable::Pick,
            effort: Changeable::NotOffered,
            permission: Changeable::Pick,
            mode: Changeable::Pick,
        }
    );

    let astra = new_agent_settings(
        &catalogue,
        &NewAgentChoices {
            model: Some("astra".into()),
            effort: Some("high".into()),
            permission: Some("auto".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        astra
            .efforts
            .iter()
            .map(|effort| (effort.value.as_str(), effort.current, effort.default))
            .collect::<Vec<_>>(),
        [("low", false, true), ("high", true, false)]
    );
    assert_eq!(
        astra
            .permissions
            .iter()
            .map(|permission| (permission.value.as_str(), permission.current))
            .collect::<Vec<_>>(),
        [("default", false), ("auto", true)]
    );

    let elsewhere = new_agent_settings(
        &catalogue,
        &NewAgentChoices {
            model: Some("gpt-old".into()),
            effort: Some("xhigh".into()),
            permission: Some("yolo".into()),
            mode: Some("pair".into()),
        },
    );
    let unlisted = |current: bool, unlisted: bool| current && unlisted;
    let last = elsewhere.models.last().unwrap();
    assert!(unlisted(last.current, last.unlisted) && last.display_name == "gpt-old");
    let last = elsewhere.efforts.last().unwrap();
    assert!(unlisted(last.current, last.unlisted));
    let last = elsewhere.permissions.last().unwrap();
    assert!(unlisted(last.current, last.unlisted) && !last.settable);
    let last = elsewhere.modes.last().unwrap();
    assert!(unlisted(last.current, last.unlisted));
}

#[test]
fn a_new_agents_pick_keeps_its_choices_consistent() {
    let catalogue = new_agent_catalogue();
    let pick =
        |chosen: &NewAgentChoices, pick: NewAgentPick| new_agent_pick(&catalogue, chosen, &pick);
    let chosen = NewAgentChoices {
        model: Some("astra".into()),
        effort: Some("low".into()),
        permission: Some("auto".into()),
        mode: None,
    };
    let sol = pick(&chosen, NewAgentPick::Model(Some("sol".into())));
    assert_eq!(sol.effort.as_deref(), Some("medium"), "its own default");
    assert_eq!(
        sol.permission.as_deref(),
        Some("default"),
        "sol does not take auto: back to the normal one"
    );
    let again = pick(&chosen, NewAgentPick::Model(Some("astra".into())));
    assert_eq!(again, chosen, "the same model changes nothing");
    let host = pick(&chosen, NewAgentPick::Model(None));
    assert_eq!(
        (host.effort, host.permission.as_deref()),
        (None, Some("default")),
        "the host's model: its default effort, a permission every model takes"
    );
    let typed = pick(&chosen, NewAgentPick::Model(Some("gpt-old".into())));
    assert_eq!(
        typed.effort.as_deref(),
        Some("low"),
        "nothing is known of a model the catalogue does not list"
    );
    let plan = pick(&chosen, NewAgentPick::Mode(Some("plan".into())));
    assert_eq!(plan.mode.as_deref(), Some("plan"));
    let normal = pick(&plan, NewAgentPick::Mode(Some("default".into())));
    assert_eq!(normal.mode, None, "the normal mode is no choice at all");
    let effort = pick(&chosen, NewAgentPick::Effort(None));
    assert_eq!(effort.effort, None);
}

fn tool_item(
    order: u64,
    name: &str,
    input: serde_json::Value,
    outcome: serde_json::Value,
    state: ToolState,
) -> Item {
    let class = if name == "Read" {
        ToolClass::Read
    } else {
        ToolClass::Consequential
    };
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body: wire::ClaudeSdkItem {
            kind: Some(wire::claude_sdk_item::Kind::Tool(wire::ToolCall {
                name: name.into(),
                input_json: input.to_string().into_bytes(),
                outcome_json: outcome.to_string().into_bytes(),
                state: state as i32,
                class: class as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1000,
        ..Item::default()
    }
}

/// A landed edit's patch head is its first lines, numbered on the side each
/// belongs to, with the count of the rest; an edit still running has none.
/// A collapsed run names its newest distinct subjects.
#[test]
fn a_landed_edit_shows_its_numbered_patch_head_and_a_run_its_newest_subjects() {
    use serde_json::json;
    let mut state = SessionState::new(agent(Kind::ClaudeSdk), CAP);
    let edit = json!({"file_path": "src/retry.rs", "old_string": "x", "new_string": "y"});
    let patch = json!({"structuredPatch": [{
        "oldStart": 20, "newStart": 20,
        "lines": [" fn retry() {", "-    sleep(1);", "+    sleep(delay);", "+    attempts += 1;", " }"],
    }]});
    page(
        &mut state,
        vec![
            tool_item(
                1,
                "Read",
                json!({"file_path": "src/a.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(
                2,
                "Read",
                json!({"file_path": "src/b.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(
                3,
                "Read",
                json!({"file_path": "src/c.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(4, "Edit", edit.clone(), patch, ToolState::Succeeded),
            tool_item(5, "Edit", edit, json!({}), ToolState::Running),
        ],
        true,
    );
    let head = patch_head(&state, &"k4".to_owned(), 3).expect("the landed edit has a head");
    let seen: Vec<(Option<u32>, LineKind, &str)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(20), LineKind::Context, "fn retry() {"),
            (Some(21), LineKind::Removed, "    sleep(1);"),
            (Some(21), LineKind::Added, "    sleep(delay);"),
        ]
    );
    assert_eq!(head.more, 2);
    assert_eq!(
        patch_head(&state, &"k5".to_owned(), 3),
        None,
        "still running"
    );
    assert_eq!(patch_head(&state, &"k1".to_owned(), 3), None, "not an edit");

    // Codex's patch is a unified diff, numbered from its hunk header.
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    let change = |state: ToolState| {
        Item {
        key: "change".into(),
        order: 1,
        revision: 1,
        kind: wire::kind_tag(Kind::Codex).into(),
        body: wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Work(wire::Work {
                of: Some(wire::work::Of::FileChange(wire::FileChangeWork {
                    changes: vec![wire::FileChange {
                        path: "lexer.rs".into(),
                        patch: "diff --git a/lexer.rs b/lexer.rs\n--- a/lexer.rs\n+++ b/lexer.rs\n@@ -7,2 +7,2 @@\n-a\n+b\n c\n".into(),
                        ..Default::default()
                    }],
                })),
                state: state as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        ..Item::default()
    }
    };
    page(&mut codex, vec![change(ToolState::Succeeded)], true);
    let head = patch_head(&codex, &"change".to_owned(), 10).unwrap();
    let seen: Vec<(Option<u32>, LineKind)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(7), LineKind::Removed),
            (Some(7), LineKind::Added),
            (Some(8), LineKind::Context),
        ]
    );
    assert_eq!(head.more, 0);
}

/// Inside a hunk a removed "-- comment" reads "--- comment" and an added
/// "++ x" reads "+++ x"; they are lines of the patch, not file headers,
/// and the lines after them keep their numbers.
#[test]
fn a_patch_head_keeps_hunk_lines_that_look_like_file_headers() {
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    let patch = "diff --git a/q.sql b/q.sql\n--- a/q.sql\n+++ b/q.sql\n@@ -3,2 +3,2 @@\n--- old note\n+++ new note\n select 1;\ndiff --git a/r.sql b/r.sql\n--- a/r.sql\n+++ b/r.sql\n@@ -10 +10 @@\n-a\n+b\n";
    let change = Item {
        key: "change".into(),
        order: 1,
        revision: 1,
        kind: wire::kind_tag(Kind::Codex).into(),
        body: wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Work(wire::Work {
                of: Some(wire::work::Of::FileChange(wire::FileChangeWork {
                    changes: vec![wire::FileChange {
                        path: "q.sql".into(),
                        patch: patch.into(),
                        ..Default::default()
                    }],
                })),
                state: ToolState::Succeeded as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        ..Item::default()
    };
    page(&mut codex, vec![change], true);
    let head = patch_head(&codex, &"change".to_owned(), 10).unwrap();
    let seen: Vec<(Option<u32>, LineKind, &str)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(3), LineKind::Removed, "-- old note"),
            (Some(3), LineKind::Added, "++ new note"),
            (Some(4), LineKind::Context, "select 1;"),
            (Some(10), LineKind::Removed, "a"),
            (Some(10), LineKind::Added, "b"),
        ]
    );
}

fn sdk_item(order: u64, revision: u64, kind: wire::claude_sdk_item::Kind, text: &str) -> Item {
    Item {
        key: format!("k{order}"),
        order,
        revision,
        text: text.into(),
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body: wire::ClaudeSdkItem { kind: Some(kind) }.encode_to_vec(),
        at_ms: order as i64 * 1000,
        ..Item::default()
    }
}

fn said(order: u64, revision: u64, text: &str, complete: bool) -> Item {
    sdk_item(
        order,
        revision,
        wire::claude_sdk_item::Kind::Message(wire::Text { complete }),
        text,
    )
}

fn running(order: u64, revision: u64, command: &str) -> Item {
    sdk_item(
        order,
        revision,
        wire::claude_sdk_item::Kind::Tool(wire::ToolCall {
            name: "Bash".into(),
            state: ToolState::Running as i32,
            class: ToolClass::Consequential as i32,
            input_json: format!(r#"{{"command":"{command}"}}"#).into_bytes(),
            ..Default::default()
        }),
        "",
    )
}

/// A caught-up session on `entry` with a headless Claude snapshot and items.
fn session_on(
    entry: &wire::Agent,
    body: wire::ClaudeSdkSnapshot,
    items: Vec<Item>,
) -> SessionState {
    let mut state = SessionState::new(entry.clone(), CAP);
    state.update(snapshot(
        Kind::ClaudeSdk,
        entry.phase(),
        body.encode_to_vec(),
        vec![],
    ));
    for item in items {
        state.update(event(session_event::Of::Item(item)));
    }
    state.update(caught_up());
    state
}

fn second_lines(view: &FleetView) -> HashMap<String, SecondLine> {
    view.sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| (row.card.name.clone(), row.second_line.clone()))
        .collect()
}

#[test]
fn each_row_says_what_its_state_calls_for() {
    let mut fleet = FleetState::new();
    fleet.update(host_entry("a", None));
    let FleetMsg::Event(mut far) = host_entry("b", None) else {
        unreachable!()
    };
    if let Some(wire::inventory_event::Of::Host(entry)) = &mut far.of {
        entry.presence = wire::Presence::Offline as i32;
    }
    fleet.update(FleetMsg::Event(far));
    let entry = |id: &str, phase| fleet_entry("a", id, phase, None, 1);
    let ended = |id: &str, cause: Option<&str>| wire::Agent {
        lifecycle: wire::Lifecycle::Exited as i32,
        exit_cause: cause.map(str::to_owned),
        ..entry(id, Phase::Idle)
    };
    let mut branched = entry("idle", Phase::Idle);
    branched.git = Some(wire::Git {
        branch: Some("fix-login".into()),
        ..Default::default()
    });
    let question = wire::Ask {
        key: "q-1".into(),
        body: Some(wire::ask::Body::Question(wire::QuestionAsk {
            questions: vec![wire::Question {
                question: "Which environment?".into(),
                ..Default::default()
            }],
            ..Default::default()
        })),
        ..Default::default()
    };
    let signed_out = wire::SignIn {
        state: wire::SignInState::Expired as i32,
        account: "me@example.com".into(),
        message: String::new(),
    };
    let window = |limit: wire::ClaudeLimit, used_percent, resets_at_ms, state: wire::UsageState| {
        wire::ClaudeUsageWindow {
            limit: limit as i32,
            model: None,
            provider_name: String::new(),
            meter: Some(wire::UsageMeter {
                used_percent,
                resets_at_ms: Some(resets_at_ms),
                state: state as i32,
            }),
        }
    };
    let spent = wire::ClaudeUsage {
        state: wire::UsageState::Blocked as i32,
        windows: vec![
            window(
                wire::ClaudeLimit::FiveHour,
                100.0,
                9_000,
                wire::UsageState::Blocked,
            ),
            window(wire::ClaudeLimit::Weekly, 40.0, 3_000, wire::UsageState::Ok),
        ],
    };
    let rows = [
        (
            entry("asker", Phase::NeedsYou),
            wire::ClaudeSdkSnapshot {
                asks: vec![question],
                ..Default::default()
            },
            vec![],
        ),
        (
            entry("worker", Phase::Working),
            wire::ClaudeSdkSnapshot::default(),
            vec![
                said(1, 1, "Running the tests.", true),
                running(2, 2, "cargo test"),
            ],
        ),
        (
            branched,
            wire::ClaudeSdkSnapshot::default(),
            vec![said(
                1,
                1,
                "\nDone: the tests pass.\nNothing else changed.",
                true,
            )],
        ),
        (
            entry("signed-out", Phase::Idle),
            wire::ClaudeSdkSnapshot {
                sign_in: Some(signed_out),
                ..Default::default()
            },
            vec![said(1, 1, "Done.", true)],
        ),
        (
            entry("limited", Phase::Idle),
            wire::ClaudeSdkSnapshot {
                usage: Some(spent),
                ..Default::default()
            },
            vec![],
        ),
        (
            entry("starting", Phase::Starting),
            wire::ClaudeSdkSnapshot::default(),
            vec![],
        ),
        (
            ended("finished", Some("finished")),
            wire::ClaudeSdkSnapshot::default(),
            vec![said(1, 1, "All done.", true)],
        ),
        (
            fleet_entry("b", "far", Phase::Working, None, 1),
            wire::ClaudeSdkSnapshot::default(),
            vec![running(1, 1, "make")],
        ),
    ];
    let mut lines = HashMap::new();
    for (entry, body, items) in rows {
        let state = session_on(&entry, body, items);
        lines.insert(ui_state::agent_key(&entry), session_line(&state, 5_000));
        fleet.update(listed(entry));
    }
    // Exited agents need no session to say why they ended.
    fleet.update(listed(ended("stopped", Some("stopped"))));
    fleet.update(listed(ended("crashed", Some("provider crashed"))));
    // A live agent whose session has not opened says nothing yet.
    fleet.update(listed(entry("unopened", Phase::Idle)));
    // An exited agent's row is history: how it ended, whatever its host.
    fleet.update(listed(wire::Agent {
        lifecycle: wire::Lifecycle::Exited as i32,
        exit_cause: Some("exited".into()),
        ..fleet_entry("b", "far-done", Phase::Idle, None, 1)
    }));

    let view = fleet_view(&fleet, b"a", &lines, &HashSet::new(), &|_| true);
    let said = second_lines(&view);
    assert_eq!(
        said["asker"],
        SecondLine::Ask(AskSummary {
            subject: AskSubject::Question {
                question: "Which environment?".into(),
                count: 1
            },
            count: 1
        })
    );
    let SecondLine::Step(step) = &said["worker"] else {
        panic!("a working agent shows its step: {:?}", said["worker"]);
    };
    assert_eq!(step.step.as_deref(), Some("cargo test"));
    assert_eq!(
        step.activity.kind,
        ui_state::ActivityKind::Running { key: "k2".into() }
    );
    assert_eq!(
        said["idle"],
        SecondLine::LastSaid("Done: the tests pass.".into())
    );
    assert_eq!(
        said["signed-out"],
        SecondLine::Stuck(StuckReason::SignedOut {
            state: wire::SignInState::Expired,
            account: "me@example.com".into()
        })
    );
    assert_eq!(
        said["limited"],
        SecondLine::Stuck(StuckReason::UsageLimit {
            resets_at_ms: Some(9_000)
        })
    );
    assert_eq!(said["starting"], SecondLine::Blank);
    assert_eq!(said["unopened"], SecondLine::Blank);
    assert_eq!(said["finished"], SecondLine::Exited(ExitCause::Finished));
    assert_eq!(said["stopped"], SecondLine::Exited(ExitCause::Ended));
    assert_eq!(
        said["crashed"],
        SecondLine::Exited(ExitCause::Failed("provider crashed".into()))
    );
    assert_eq!(said["far"], SecondLine::HostAway);
    assert_eq!(said["far-done"], SecondLine::Exited(ExitCause::Ended));
    let far = fleet_card(&fleet, b"a", b"far-done").unwrap();
    assert_eq!(
        (far.exit_cause, far.host_reach),
        (Some(ExitCause::Ended), Reach::Offline)
    );
    assert_eq!(
        fleet_card(&fleet, b"a", b"worker").unwrap().exit_cause,
        None
    );
    let idle = fleet_card(&fleet, b"a", b"idle").unwrap();
    assert_eq!(idle.branch.as_deref(), Some("fix-login"));
    assert_eq!(fleet_card(&fleet, b"a", b"asker").unwrap().branch, None);
}

#[test]
fn rows_hold_their_order_while_an_agent_streams() {
    let mut fleet = FleetState::new();
    let streamer = fleet_entry("a", "streamer", Phase::Working, None, 200);
    for msg in [
        fleet_row("a", "older", Phase::Idle, None, 100),
        listed(streamer.clone()),
        fleet_row("a", "newer", Phase::Idle, None, 300),
        fleet_row("a", "asker", Phase::NeedsYou, None, 50),
    ] {
        fleet.update(msg);
    }
    let key = ui_state::agent_key(&streamer);
    let mut state = session_on(&streamer, wire::ClaudeSdkSnapshot::default(), vec![]);
    let view = |fleet: &FleetState, state: &SessionState, now: i64| {
        let lines = HashMap::from([(key.clone(), session_line(state, now))]);
        fleet_view(fleet, b"a", &lines, &HashSet::new(), &|_| true)
    };
    let before = home(&view(&fleet, &state, 0));
    let mut steps = HashSet::new();
    let mut text = String::new();
    for n in 1..=24u64 {
        // Text streams into a message, a command starts and the next
        // message begins; the inventory re-lists the agent as it goes.
        let order = n.div_ceil(3);
        let item = if n % 3 == 0 {
            running(order * 10 + 1, n, &format!("step {n}"))
        } else {
            text.push_str(" more");
            said(order * 10, n, &text, false)
        };
        state.update(event(session_event::Of::Item(item)));
        fleet.update(listed(streamer.clone()));
        let now = view(&fleet, &state, n as i64 * 1_000);
        assert_eq!(home(&now), before, "rows moved at step {n}");
        steps.insert(format!("{:?}", second_lines(&now)["streamer"]));
    }
    assert!(
        steps.len() > 1,
        "the streaming row's second line follows it"
    );

    // When the agent's state changes, since-when moves and so does the row.
    fleet.update(listed(wire::Agent {
        phase: Phase::Idle as i32,
        phase_since_ms: 400,
        ..streamer.clone()
    }));
    let after = home(&view(&fleet, &state, 30_000));
    assert_eq!(after[1].1, "streamer");
}

#[test]
fn changed_files_group_by_folder_with_root_files_first() {
    let file = |path: &str, added, removed, change: wire::DiffFileChange| wire::DiffFile {
        path: path.into(),
        added,
        removed,
        change: change as i32,
        binary: false,
    };
    let diff = wire::Diff {
        files: vec![
            file("src/b.rs", 3, 1, wire::DiffFileChange::Changed),
            file("README.md", 2, 0, wire::DiffFileChange::Changed),
            file("docs/x/y.md", 0, 9, wire::DiffFileChange::Deleted),
            file("src/a.rs", 5, 0, wire::DiffFileChange::Created),
            file("Cargo.toml", 1, 1, wire::DiffFileChange::Changed),
        ],
        ..Default::default()
    };
    let changes = changes(&diff);
    assert_eq!(
        changes.totals,
        ChangeTotals {
            files: 5,
            added: 11,
            removed: 11
        }
    );
    let folders: Vec<(&str, Vec<&str>)> = changes
        .folders
        .iter()
        .map(|folder| {
            let names = folder.files.iter().map(|file| file.name.as_str()).collect();
            (folder.path.as_str(), names)
        })
        .collect();
    assert_eq!(
        folders,
        vec![
            ("", vec!["Cargo.toml", "README.md"]),
            ("docs/x/", vec!["y.md"]),
            ("src/", vec!["a.rs", "b.rs"]),
        ]
    );
    let a = &changes.folders[2].files[0];
    assert_eq!((a.path.as_str(), a.status), ("src/a.rs", FileStatus::Added));
    assert_eq!(changes.folders[1].files[0].status, FileStatus::Deleted);
}

#[test]
fn the_overview_holds_the_changes_it_is_given() {
    let mut state = SessionState::new(agent(Kind::Codex), CAP);
    state.update(snapshot(Kind::Codex, Phase::Idle, vec![], vec![]));
    assert_eq!(overview(&state, None).changes, None);
    let diff = wire::Diff::default();
    assert_eq!(
        overview(&state, Some(&diff)).changes,
        Some(Changes::default()),
        "a fetched clean tree is no changes, not unknown"
    );
    assert!(
        diff_base(&state, Comparison::OnBranch).is_none(),
        "no base branch, nothing to compare the branch with"
    );
    assert!(diff_base(&state, Comparison::Uncommitted).is_some());
}

/// A prompt on its way stays where it was first drawn: at the feed's end
/// if the agent was idle, in the queue otherwise. While the link is down it
/// waits for the host there; after catching up, one that never showed up
/// may not have arrived and waits last in the queue for the person. A
/// refused one is left for the composer.
#[test]
fn prompts_on_their_way_follow_the_sending_rules() {
    let kind = Kind::Codex;
    let mut state = SessionState::new(agent(kind), CAP);
    state.update(snapshot(kind, Phase::Idle, vec![], vec![]));
    state.update(caught_up());
    assert!(sends_to_feed(&state), "an idle agent with nothing queued");
    state.update(Msg::Send(prompt_input(kind, b"p1", "first")));
    let feed = |id: &[u8]| id == b"p1";
    let underway = prompts_underway(&state, feed);
    assert_eq!(underway.len(), 1);
    assert_eq!(underway[0].lands, Lands::Feed);
    assert_eq!(underway[0].underway, Underway::Sending { waiting: false });

    state.update(snapshot(kind, Phase::Working, vec![], vec![]));
    assert!(!sends_to_feed(&state), "a working agent queues the next");
    state.update(Msg::Send(prompt_input(kind, b"p2", "second")));
    state.update(Msg::Sent(b"p1".to_vec(), InputOutcome::Lost));
    state.update(Msg::Connection(ui_state::Connection::Reconnecting));
    let underway = prompts_underway(&state, feed);
    assert_eq!(
        underway
            .iter()
            .map(|prompt| (prompt.input_id.as_slice(), prompt.lands, prompt.underway))
            .collect::<Vec<_>>(),
        vec![
            (&b"p1"[..], Lands::Feed, Underway::Sending { waiting: true }),
            (
                &b"p2"[..],
                Lands::Queue,
                Underway::Sending { waiting: true }
            ),
        ],
        "both wait for the host where they were first drawn"
    );

    state.update(Msg::Send(prompt_input(kind, b"p3", "third")));
    state.update(Msg::Sent(
        b"p3".to_vec(),
        InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                reason: "host_unreachable".into(),
            })),
        }),
    ));
    state.update(snapshot(kind, Phase::Working, vec![], vec![]));
    state.update(caught_up());
    let underway = prompts_underway(&state, feed);
    assert_eq!(
        underway
            .iter()
            .map(|prompt| (prompt.input_id.as_slice(), prompt.lands, prompt.underway))
            .collect::<Vec<_>>(),
        vec![
            (
                &b"p2"[..],
                Lands::Queue,
                Underway::Sending { waiting: false }
            ),
            (&b"p1"[..], Lands::Queue, Underway::MayNotHaveArrived),
        ],
        "caught up, the lost one may not have arrived and waits last"
    );
    assert_eq!(
        refused_prompts(&state),
        vec![RefusedPrompt {
            input_id: b"p3".to_vec(),
            reason: ui_view::RefusalReason::HostUnreachable,
        }]
    );
}

/// A tool server's form reads once, here, for every client: its fields in
/// the order the schema writes them (not sorted), each kind with its
/// limits, the required ones, and what each holds before it is touched:
/// the schema's default, else nothing, with a toggle off.
#[test]
fn a_form_reads_its_fields_in_the_schemas_order() {
    use ui_view::{FormFieldKind, FormValue, form_fields};
    let fields = form_fields(
        br#"{"type":"object","properties":{
            "title":{"type":"string","title":"Title","description":"One line","maxLength":80},
            "labels":{"type":"array","items":{"enum":["bug","ios"]},"maxItems":1,"default":["ios"]},
            "team":{"enum":["core","apps"]},
            "urgent":{"type":"boolean"},
            "estimate":{"type":"integer","default":3,"minimum":1},
            "ratio":{"type":"number"},
            "lane":{"enum":["core","apps"],"default":"apps"},
            "notify":{"type":"boolean","default":true}
        },"required":["title"]}"#,
    );
    let read: Vec<_> = fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.title.as_str(),
                field.required,
                field.kind.clone(),
                field.initial.clone(),
            )
        })
        .collect();
    let options = |all: &[&str]| all.iter().map(|o| (*o).to_owned()).collect::<Vec<_>>();
    let text = |s: &str| FormValue::Text(s.to_owned());
    assert_eq!(
        read,
        vec![
            (
                "title",
                "Title",
                true,
                FormFieldKind::Text {
                    min_length: None,
                    max_length: Some(80)
                },
                text("")
            ),
            (
                "labels",
                "labels",
                false,
                FormFieldKind::Many {
                    options: options(&["bug", "ios"]),
                    min_items: None,
                    max_items: Some(1),
                },
                FormValue::Many(vec![1])
            ),
            (
                "team",
                "team",
                false,
                FormFieldKind::Choice {
                    options: options(&["core", "apps"])
                },
                FormValue::Choice(None)
            ),
            (
                "urgent",
                "urgent",
                false,
                FormFieldKind::Toggle,
                FormValue::Toggle(false)
            ),
            (
                "estimate",
                "estimate",
                false,
                FormFieldKind::Number {
                    integer: true,
                    minimum: Some(1.0),
                    maximum: None
                },
                text("3")
            ),
            (
                "ratio",
                "ratio",
                false,
                FormFieldKind::Number {
                    integer: false,
                    minimum: None,
                    maximum: None
                },
                text("")
            ),
            (
                "lane",
                "lane",
                false,
                FormFieldKind::Choice {
                    options: options(&["core", "apps"])
                },
                FormValue::Choice(Some(1))
            ),
            (
                "notify",
                "notify",
                false,
                FormFieldKind::Toggle,
                FormValue::Toggle(true)
            ),
        ]
    );
    assert_eq!(fields[0].description, "One line");
    assert!(form_fields(b"not json").is_empty());
}

/// Whether a form can go, and what goes, is decided once for every client:
/// each field's problem, else the JSON object the schema describes, with
/// fields left empty left out and an untouched toggle sent off.
#[test]
fn a_form_is_checked_and_encoded_once_for_every_client() {
    use ui_view::{FieldProblem, FormProblem, FormValue, form_fields, form_problems};
    let fields = form_fields(
        br#"{"type":"object","properties":{
            "title":{"type":"string","minLength":3,"maxLength":10},
            "team":{"enum":["core","apps"]},
            "estimate":{"type":"integer","minimum":1,"maximum":8},
            "ratio":{"type":"number"},
            "labels":{"type":"array","items":{"enum":["bug","ios","docs"]},"minItems":2,"maxItems":2},
            "urgent":{"type":"boolean"}
        },"required":["title","team","urgent"]}"#,
    );
    let text = |s: &str| FormValue::Text(s.to_owned());
    let problems = |values: &[FormValue]| {
        form_problems(&fields, values)
            .into_iter()
            .map(|FormProblem { field, problem }| (field, problem))
            .collect::<Vec<_>>()
    };
    // Untouched: the required text and choice are missing; the required
    // toggle is answered, off.
    assert_eq!(
        problems(&[]),
        vec![(0, FieldProblem::Required), (1, FieldProblem::Required)]
    );
    // Spaces alone are nothing entered.
    assert_eq!(problems(&[text("   ")])[0], (0, FieldProblem::Required));
    assert_eq!(
        problems(&[
            text("ab"),
            FormValue::Choice(Some(0)),
            text("2.5"),
            text("inf"),
            FormValue::Many(vec![0]),
        ]),
        vec![
            (0, FieldProblem::TooShort { min_length: 3 }),
            (2, FieldProblem::NotWholeNumber),
            (3, FieldProblem::NotANumber),
            (4, FieldProblem::TooFew { min_items: 2 }),
        ]
    );
    assert_eq!(
        problems(&[
            text("a much longer title"),
            FormValue::Choice(Some(0)),
            text("9"),
            text(""),
            FormValue::Many(vec![0, 1, 2]),
        ]),
        vec![
            (0, FieldProblem::TooLong { max_length: 10 }),
            (2, FieldProblem::AboveMaximum { maximum: 8.0 }),
            (4, FieldProblem::TooMany { max_items: 2 }),
        ]
    );
    assert_eq!(
        problems(&[text("flake"), FormValue::Choice(Some(1)), text("0")]),
        vec![(2, FieldProblem::BelowMinimum { minimum: 1.0 })]
    );
    // A value of another field's shape, or an option out of range, is
    // nothing entered.
    assert_eq!(
        problems(&[FormValue::Toggle(true), FormValue::Choice(Some(7))]),
        vec![(0, FieldProblem::Required), (1, FieldProblem::Required)]
    );
}

/// The answer a form sends carries the object the schema describes, and
/// is refused with the problems while any field has one.
#[test]
fn a_forms_answer_carries_the_checked_values() {
    use ui_view::{FieldProblem, FormProblem, FormValue, form_answer, form_fields};
    let schema = br#"{"type":"object","properties":{
        "title":{"type":"string"},
        "team":{"enum":["core","apps"]},
        "estimate":{"type":"integer"},
        "ratio":{"type":"number"},
        "labels":{"type":"array","items":{"enum":["bug","ios","docs"]}},
        "urgent":{"type":"boolean"},
        "note":{"type":"string"}
    },"required":["title","team"]}"#;
    let card = |kind: wire::Kind| {
        let accept = wire::FormAnswer {
            action: wire::FormAction::Accept as i32,
            content_json: vec![],
        };
        AskCard {
            kind,
            key: "ask".into(),
            item_key: "item".into(),
            position: 1,
            count: 1,
            body: ui_view::AskBody::Form {
                server: "linear".into(),
                message: "Details".into(),
                fields: form_fields(schema),
            },
            choices: vec![ui_view::Choice {
                outcome: ui_view::ChoiceOutcome::Submit,
                primary: true,
                takes_note: false,
                answer: match kind {
                    wire::Kind::Codex => ui_view::Answer::Codex(wire::CodexAnswer {
                        of: Some(wire::codex_answer::Of::Form(accept)),
                        ..Default::default()
                    }),
                    _ => ui_view::Answer::Claude(wire::ClaudeAnswer {
                        of: Some(wire::claude_answer::Of::Form(accept)),
                        ..Default::default()
                    }),
                },
            }],
            question_note: false,
            question_skip: false,
            question_reply: false,
            stops_turn: true,
            state: ui_view::CardState::Open,
        }
    };
    let text = |s: &str| FormValue::Text(s.to_owned());
    let values = [
        text("  Reconnect flake "),
        FormValue::Choice(Some(1)),
        text("3"),
        text("2"),
        FormValue::Many(vec![2, 0, 2]),
    ];
    let content = |answer: ui_view::Answer| {
        let form = match answer {
            ui_view::Answer::Claude(wire::ClaudeAnswer {
                of: Some(wire::claude_answer::Of::Form(form)),
                ..
            })
            | ui_view::Answer::Codex(wire::CodexAnswer {
                of: Some(wire::codex_answer::Of::Form(form)),
                ..
            }) => form,
            other => panic!("not a form answer: {other:?}"),
        };
        assert_eq!(form.action(), wire::FormAction::Accept);
        serde_json::from_slice::<serde_json::Value>(&form.content_json).unwrap()
    };
    for kind in [wire::Kind::ClaudeSdk, wire::Kind::Codex] {
        let answer = form_answer(&card(kind), &values).unwrap().unwrap();
        assert_eq!(
            content(answer.clone()),
            serde_json::json!({
                "title": "Reconnect flake",
                "team": "apps",
                "estimate": 3,
                "ratio": 2,
                "labels": ["bug", "docs"],
                "urgent": false,
            }),
            "{kind:?}: trimmed, whole numbers whole, picks in the options' order, the untouched toggle off, the empty note left out"
        );
        let input = ui_view::answer_input(&card(kind), &answer, "").unwrap();
        assert!(!input.encode_to_vec().is_empty());
    }
    assert_eq!(
        form_answer(&card(wire::Kind::ClaudeSdk), &[text("Flake")]),
        Err(vec![FormProblem {
            field: 1,
            problem: FieldProblem::Required
        }])
    );
    let mut question = card(wire::Kind::ClaudeSdk);
    question.body = ui_view::AskBody::Question(vec![]);
    assert_eq!(form_answer(&question, &values), Ok(None), "not a form");
}

/// A Codex command whose output lost its start to the bound says so on its
/// row, so every client can say the first lines shown are not the first.
#[test]
fn a_codex_command_that_dropped_output_is_marked_trimmed() {
    let command = |order: u64, dropped: u64| Item {
        key: format!("k{order}"),
        order,
        revision: 1,
        text: "test b ... ok\n".into(),
        kind: wire::kind_tag(Kind::Codex).into(),
        body: wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Work(wire::Work {
                of: Some(wire::work::Of::Command(wire::CommandWork {
                    command: "cargo test".into(),
                    output_dropped_bytes: dropped,
                    ..Default::default()
                })),
                state: ToolState::Succeeded as i32,
                class: ToolClass::Consequential as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1000,
        ..Item::default()
    };
    let mut state = SessionState::new(agent(Kind::Codex), CAP);
    state.update(snapshot(Kind::Codex, Phase::Idle, vec![], vec![]));
    state.update(event(session_event::Of::Item(command(1, 4096))));
    state.update(event(session_event::Of::Item(command(2, 0))));
    state.update(caught_up());
    let opts = ChatOptions {
        tools: ToolRows::ShowAll,
    };
    let trimmed: Vec<bool> = all_rows(&state, &opts)
        .into_iter()
        .filter_map(|row| match row.kind {
            RowKind::Command { output_trimmed, .. } => Some(output_trimmed),
            _ => None,
        })
        .collect();
    assert_eq!(trimmed, vec![true, false]);
}
