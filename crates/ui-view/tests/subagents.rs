//! Calls that open agents: a provider subagent's or an amux child's call
//! row names the agent, reads its state from the fleet, and folds with the
//! other calls of its turn into one row. And the failure that made
//! provider subagents agents of their own: four busy parallel subagents
//! filling their parent's window until its history was gone.

use std::collections::HashSet;
use std::fmt::Write as _;

use prost::Message;
use ui_state::{Composer, FleetMsg, FleetState, JobStop, Key, Msg, PhaseView, SessionState};
use ui_view::{
    GroupState, Row, ToolRows, chat_agents, chat_rows_for, group_state, opened_agent,
    subagent_rows_moved, toggle_group,
};
use wire::{Item, Kind, Phase, ToolState, session_event};

use crate::runs::{It, event, item};

/// The window's cap while the reader follows the newest rows: the phone's
/// default tail, the window the reported failure filled.
const CAP: usize = 200;

fn parent() -> wire::Agent {
    wire::Agent {
        agent_id: b"parent".to_vec(),
        host_id: b"host".to_vec(),
        kind: Kind::ClaudeSdk as i32,
        name: "lead".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: Phase::Working as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn subagent_id(n: usize) -> Vec<u8> {
    format!("sub-{n}").into_bytes()
}

/// A provider subagent's fleet row under the parent.
fn subagent(n: usize, phase: Phase, exited: Option<wire::ExitCause>) -> wire::Agent {
    wire::Agent {
        agent_id: subagent_id(n),
        host_id: b"host".to_vec(),
        kind: Kind::ClaudeSdk as i32,
        name: format!("sub-{n:08}"),
        title: Some(format!("survey part {n}")),
        lifecycle: if exited.is_some() {
            wire::Lifecycle::Exited
        } else {
            wire::Lifecycle::Live
        } as i32,
        exit_cause: exited.map_or(0, |cause| cause as i32),
        phase: phase as i32,
        incarnation: 1,
        parent: Some(wire::AgentRef {
            host_id: b"host".to_vec(),
            agent_id: b"parent".to_vec(),
        }),
        native: Some(wire::NativeChild {
            provider_child_id: format!("task-{n}"),
        }),
        ..wire::Agent::default()
    }
}

fn listed(agent: wire::Agent) -> FleetMsg {
    FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Agent(agent)),
    }))
}

/// Claude's Agent call starting subagent `n`; it names the agent it opens
/// unless it is from before calls did.
fn agent_call(order: u64, n: usize, opens: bool) -> Item {
    let mut call = item(order, &It::Subagent("survey"));
    let mut body = wire::ClaudeItem::decode(call.body.as_slice()).unwrap();
    if let Some(wire::claude_item::Kind::Tool(tool)) = &mut body.kind {
        tool.state = ToolState::Running as i32;
        if opens {
            tool.child_agent_id = subagent_id(n);
        }
    }
    call.key = format!("agent-call-{n}");
    call.body = body.encode_to_vec();
    call
}

/// A subagent's own step: a command it ran. History filed it in the
/// parent's chat under the call that started it (`parent_key`).
fn step(order: u64, n: usize, i: usize, parent_key: Option<&str>) -> Item {
    let mut step = item(order, &It::Run("cargo test", 0));
    let mut body = wire::ClaudeItem::decode(step.body.as_slice()).unwrap();
    if let (Some(wire::claude_item::Kind::Tool(tool)), Some(parent)) = (&mut body.kind, parent_key)
    {
        tool.parent_key = parent.to_owned();
    }
    step.key = format!("sub-{n}-step-{i}");
    step.body = body.encode_to_vec();
    step
}

fn session(agent: wire::Agent) -> SessionState {
    let mut state = SessionState::new(agent, CAP);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(event(
        session_event::Of::Snapshot(wire::Snapshot::default()),
    ));
    state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    })));
    state
}

fn put(state: &mut SessionState, item: Item) -> Vec<Key> {
    state.update(event(session_event::Of::Item(item))).changed
}

/// The parent's earlier history: a dozen turns of prompt and answer.
fn history(state: &mut SessionState) -> u64 {
    let mut order = 0;
    for turn in 0..12 {
        for it in [
            It::Prompt("next part"),
            It::Say("done with that part"),
            It::Turn,
        ] {
            order += 1;
            let mut item = item(order, &it);
            item.key = format!("history-{turn}-{order}");
            put(state, item);
        }
    }
    order
}

/// What a person sees of a window: the rows drawn, oldest first.
fn drawn(state: &SessionState) -> Vec<Row> {
    let open = HashSet::new();
    crate::support::all_rows(state, ToolRows::Collapse { open: &open })
        .into_iter()
        .filter(|row| !row.collapsed)
        .collect()
}

fn picture(title: &str, state: &SessionState) -> String {
    let rows = drawn(state);
    let mut out = format!(
        "{title}: {} rows held (oldest order {:?}, older history {}), {} drawn\n",
        state.transcript().len(),
        state.transcript().oldest_held(),
        state.transcript().has_older(),
        rows.len()
    );
    for row in rows {
        let _ = writeln!(out, "  {:>4} {}", row.order, row.id);
    }
    out
}

const SUBAGENTS: usize = 4;
/// Each subagent's steps. In the report four subagents' steps came to 180
/// of the 200 rows and kept coming; past 200 they take the calls too.
const STEPS: usize = 55;

/// How a parent's chat held four busy parallel subagents while their steps
/// were filed under it: 180 steps fill the window, the trim takes the
/// parent's history and then the calls the steps hang under, and the steps
/// fold under calls no longer held, so almost nothing is drawn. Journals
/// from before still read this way; nothing repairs them.
#[test]
fn four_busy_parallel_subagents_filed_under_their_parent_took_its_history() {
    let mut state = session(parent());
    let mut order = history(&mut state);
    order += 1;
    put(
        &mut state,
        item(order, &It::Prompt("survey the four parts")),
    );
    for n in 0..SUBAGENTS {
        order += 1;
        put(&mut state, agent_call(order, n, false));
    }
    for i in 0..STEPS {
        for n in 0..SUBAGENTS {
            order += 1;
            put(
                &mut state,
                step(order, n, i, Some(&format!("agent-call-{n}"))),
            );
        }
    }
    let picture = picture("before", &state);
    println!("{picture}");
    let held = |key: &str| state.transcript().get(key).is_some();
    assert!(!held("history-0-1"), "{picture}");
    assert!(!held("agent-call-0"), "{picture}");
    assert!(state.transcript().has_older());
    assert!(drawn(&state).len() <= 2, "{picture}");
}

/// The same work now: each subagent's steps are in its own chat, so the
/// parent's window holds its whole history and the one row its four calls
/// fold into, and each subagent's chat holds every step it took.
#[test]
fn four_busy_parallel_subagents_leave_their_parents_history_in_view() {
    let mut state = session(parent());
    let mut subagents: Vec<SessionState> = (0..SUBAGENTS)
        .map(|n| session(subagent(n, Phase::Working, None)))
        .collect();
    let mut order = history(&mut state);
    order += 1;
    put(
        &mut state,
        item(order, &It::Prompt("survey the four parts")),
    );
    for n in 0..SUBAGENTS {
        order += 1;
        put(&mut state, agent_call(order, n, true));
    }
    for i in 0..STEPS {
        for (n, subagent) in subagents.iter_mut().enumerate() {
            put(subagent, step(i as u64 + 1, n, i, None));
        }
    }
    let picture = picture("after", &state);
    println!("{picture}");
    assert!(state.transcript().get("history-0-1").is_some(), "{picture}");
    assert!(!state.transcript().has_older(), "{picture}");
    let rows = drawn(&state);
    // Every prompt and answer of the history, and the turn under way's
    // prompt and its four calls folded into one row.
    let history_rows = rows
        .iter()
        .filter(|row| row.id.starts_with("history-"))
        .count();
    assert_eq!(history_rows, 12 * 3, "{picture}");
    let calls: Vec<&Row> = rows.iter().filter(|row| row.opens.is_some()).collect();
    assert_eq!(calls.len(), 1, "{picture}");
    let group = calls[0].group.as_ref().unwrap();
    assert_eq!(
        (group.first.as_str(), group.agents.len()),
        ("agent-call-0", 4)
    );
    for subagent in &subagents {
        assert_eq!(subagent.transcript().len(), STEPS);
        assert_eq!(subagent.transcript().oldest_held(), Some(1));
    }
}

/// The group's state is the fleet's: two running, one waiting on the
/// person, one done, whatever the calls themselves say.
#[test]
fn a_group_reads_how_its_agents_stand_from_the_fleet() {
    let mut fleet = FleetState::new();
    fleet.update(listed(parent()));
    for (n, phase, exited) in [
        (0, Phase::Working, None),
        (1, Phase::Working, None),
        (2, Phase::NeedsYou, None),
        (3, Phase::Idle, Some(wire::ExitCause::Finished)),
    ] {
        fleet.update(listed(subagent(n, phase, exited)));
    }
    let agents: Vec<Vec<u8>> = (0..5).map(subagent_id).collect();
    assert_eq!(
        group_state(&fleet, &agents),
        GroupState {
            agents: 5,
            running: 2,
            needs_you: 1,
            finished: 1,
            unknown: 1,
            ..GroupState::default()
        }
    );
    let waiting = opened_agent(&fleet, &subagent_id(2)).unwrap();
    assert_eq!(
        (waiting.name.as_str(), waiting.phase),
        ("survey part 2", PhaseView::NeedsYou)
    );

    let mut state = session(parent());
    put(&mut state, item(1, &It::Prompt("go")));
    for n in 0..4 {
        put(&mut state, agent_call(n as u64 + 2, n, true));
    }
    let seen = chat_agents(&state, &fleet);
    assert_eq!(seen.agents.len(), 4);
    assert_eq!(seen.groups.len(), 1);
    assert_eq!(seen.groups[0].first, "agent-call-0");
    assert_eq!(seen.groups[0].state.needs_you, 1);
    // What the activity line says while the agent waits on them: the live
    // ones, without the one that finished.
    assert_eq!(
        seen.live,
        GroupState {
            agents: 3,
            running: 2,
            needs_you: 1,
            ..GroupState::default()
        }
    );
}

/// Calls in different turns do not group, and one call alone is not a
/// group; a call joining a group moves the rows of the calls already in it.
#[test]
fn calls_group_by_turn_and_a_joining_call_moves_the_rest() {
    let mut state = session(parent());
    put(&mut state, item(1, &It::Prompt("one")));
    put(&mut state, agent_call(2, 0, true));
    put(&mut state, item(3, &It::Turn));
    put(&mut state, item(4, &It::Prompt("two")));
    put(&mut state, agent_call(5, 1, true));
    let rows = chat_rows_for(
        &state,
        &["agent-call-0".into(), "agent-call-1".into()],
        ToolRows::ShowAll,
    );
    assert!(rows.iter().all(|row| row.group.is_none() && !row.collapsed));
    let changed = put(&mut state, agent_call(6, 2, true));
    assert_eq!(
        subagent_rows_moved(&state, &changed),
        vec!["agent-call-1".to_owned()]
    );
    // A run of steps does not swallow the calls: they end it.
    put(&mut state, item(7, &It::Read("a.rs")));
    let open = HashSet::new();
    let rows = chat_rows_for(
        &state,
        &["agent-call-1".into(), "agent-call-2".into(), "k7".into()],
        ToolRows::Collapse { open: &open },
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_str(), row.collapsed, row.run.is_some()))
            .collect::<Vec<_>>(),
        vec![
            ("agent-call-1", false, false),
            ("agent-call-2", true, false),
            ("k7", false, true),
        ]
    );
}

/// Opening a group shows each call; it is held by its newest call, in the
/// set that holds open runs, and closing forgets every call it holds.
#[test]
fn a_group_opens_and_closes_by_any_of_its_calls() {
    let mut state = session(parent());
    put(&mut state, item(1, &It::Prompt("go")));
    for n in 0..3 {
        put(&mut state, agent_call(n as u64 + 2, n, true));
    }
    let keys: Vec<Key> = (0..3).map(|n| format!("agent-call-{n}")).collect();
    let mut open = HashSet::new();
    let collapsed = |open: &HashSet<Key>| {
        chat_rows_for(&state, &keys, ToolRows::Collapse { open })
            .iter()
            .map(|row| row.collapsed)
            .collect::<Vec<_>>()
    };
    assert_eq!(collapsed(&open), vec![false, true, true]);
    toggle_group(&state, &keys[0], &mut open);
    assert_eq!(open, HashSet::from(["agent-call-2".to_owned()]));
    assert_eq!(collapsed(&open), vec![false, false, false]);
    assert!(
        chat_rows_for(&state, &keys, ToolRows::Collapse { open: &open })
            .iter()
            .all(|row| row.group.as_ref().unwrap().open)
    );
    toggle_group(&state, &keys[1], &mut open);
    assert!(open.is_empty());
}

/// A group opened while its turn goes on stays open as more calls join it
/// and the window trims its oldest: the hold moves to its newest call, as
/// an open run's does, since these calls are no run's steps.
#[test]
fn an_opened_group_stays_open_while_the_window_trims_its_oldest_calls() {
    const CAP: usize = 20;
    let mut state = SessionState::new(parent(), CAP);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    })));
    put(&mut state, item(1, &It::Prompt("go")));
    put(&mut state, agent_call(2, 0, true));
    put(&mut state, agent_call(3, 1, true));
    let mut open = HashSet::new();
    toggle_group(&state, &"agent-call-0".to_owned(), &mut open);
    for n in 2..60 {
        put(&mut state, agent_call(n as u64 + 2, n, true));
        ui_view::keep_open_runs(&state, &mut open);
        let keys: Vec<Key> = state
            .transcript()
            .iter()
            .map(|held| held.item.key.clone())
            .collect();
        let rows = chat_rows_for(&state, &keys, ToolRows::Collapse { open: &open });
        assert!(
            rows.iter().filter(|row| row.opens.is_some()).all(|row| row
                .group
                .as_ref()
                .is_some_and(|group| group.open)
                && !row.collapsed),
            "after call {n}: {open:?}"
        );
    }
    let oldest = state.transcript().oldest_held().unwrap();
    assert!(oldest > 40, "the window trimmed the group's oldest calls");
    assert_eq!(open, HashSet::from(["agent-call-59".to_owned()]));
}

/// A history call names no agent: it draws as it did, with its steps
/// folded under it, and opens nothing.
#[test]
fn a_call_from_history_opens_nothing_and_keeps_its_steps_under_it() {
    let mut state = session(parent());
    put(&mut state, item(1, &It::Prompt("go")));
    put(&mut state, agent_call(2, 0, false));
    put(&mut state, step(3, 0, 0, Some("agent-call-0")));
    let rows = chat_rows_for(
        &state,
        &["agent-call-0".into(), "sub-0-step-0".into()],
        ToolRows::ShowAll,
    );
    assert_eq!(rows[0].opens, None);
    assert_eq!(rows[1].parent.as_deref(), Some("agent-call-0"));
    assert!(rows[1].collapsed);
}

/// Codex 0.166 puts the child's whole task on its spawn call, and the call
/// returns at once while the child works on. A spawn that opens its child
/// is named by the child's title, as the fleet has it, and says no time of
/// its own; one from history, which opens nothing, still says its task.
#[test]
fn a_codex_spawn_is_named_by_its_child_not_its_task() {
    let spawn = |order: u64, opens: bool| {
        let work = wire::Work {
            state: ToolState::Succeeded as i32,
            started_at_ms: Some(1_000),
            ended_at_ms: Some(1_005),
            of: Some(wire::work::Of::Collab(wire::CollabWork {
                tool: "spawnAgent".into(),
                thread_ids: vec!["child".into()],
                prompt: "You are agent \"count files\". Run ls -la and report.".into(),
                agent_ids: if opens { vec![subagent_id(0)] } else { vec![] },
            })),
            ..wire::Work::default()
        };
        Item {
            key: format!("spawn-{order}"),
            order,
            kind: Kind::Codex as i32,
            body: wire::CodexItem {
                kind: Some(wire::codex_item::Kind::Work(work)),
            }
            .encode_to_vec(),
            ..Item::default()
        }
    };
    let mut state = session(wire::Agent {
        kind: Kind::Codex as i32,
        ..parent()
    });
    put(&mut state, item(1, &It::Prompt("go")));
    put(&mut state, spawn(2, true));
    put(&mut state, spawn(3, false));
    let rows = chat_rows_for(
        &state,
        &["spawn-2".into(), "spawn-3".into()],
        ToolRows::ShowAll,
    );
    let said = |row: &Row| match &row.kind {
        ui_view::RowKind::Subagent {
            description,
            duration_ms,
            ..
        } => (description.clone(), *duration_ms),
        other => panic!("not a subagent's call: {other:?}"),
    };
    assert_eq!(said(&rows[0]), (String::new(), None));
    assert_eq!(
        said(&rows[1]),
        (
            "You are agent \"count files\". Run ls -la and report.".to_owned(),
            None
        )
    );
}

/// A provider subagent's chat has no text box: its composer offers its own
/// Stop while its snapshot lists its job as stoppable, says so while a stop
/// is on its way, and offers nothing once it ended or on terminal Claude.
#[test]
fn a_subagents_composer_offers_only_its_own_stop() {
    let job = |stopping: bool| wire::BackgroundJob {
        kind: wire::BackgroundKind::Subagent as i32,
        key: "task-0".into(),
        command: "survey part 0".into(),
        stopping,
        ..Default::default()
    };
    let snapshot = |jobs: Vec<wire::BackgroundJob>| {
        event(session_event::Of::Snapshot(wire::Snapshot {
            phase: Phase::Working as i32,
            body: wire::ClaudeSnapshot {
                background_jobs: Some(wire::BackgroundJobs { known: true, jobs }),
                ..Default::default()
            }
            .encode_to_vec(),
            ..Default::default()
        }))
    };
    let mut state = session(subagent(0, Phase::Working, None));
    assert!(!state.can_send());
    state.update(snapshot(vec![job(false)]));
    assert_eq!(
        state.composer(),
        Composer::Native {
            stop: JobStop::Offered {
                key: "task-0".into()
            }
        }
    );
    // Its own job is the composer's to stop, not the overview's.
    assert!(ui_view::overview(&state, None).jobs.is_empty());
    state.update(snapshot(vec![job(true)]));
    assert_eq!(
        state.composer(),
        Composer::Native {
            stop: JobStop::Stopping
        }
    );
    state.update(Msg::Entry(subagent(
        0,
        Phase::Idle,
        Some(wire::ExitCause::Stopped),
    )));
    assert_eq!(
        state.composer(),
        Composer::Native {
            stop: JobStop::Unavailable
        }
    );

    let mut pty = subagent(1, Phase::Working, None);
    pty.kind = Kind::ClaudePty as i32;
    let mut state = session(pty);
    state.update(snapshot(vec![job(false)]));
    assert_eq!(
        state.composer(),
        Composer::Native {
            stop: JobStop::Unavailable
        }
    );
    // Nothing to pick and nothing to type: it runs on its parent's settings.
    let settings = ui_view::settings(&state);
    assert_eq!(settings.changeable, ui_view::Changeability::default());
    assert!(settings.commands.is_empty());
    assert!(!ui_view::lists_changes(&state));
}
