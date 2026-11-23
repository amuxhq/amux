//! Sessions built from the interpreter's recorded fixtures, committed the
//! way the owning daemon commits them, and frames drawn from them: what
//! the TUI's tests look at.

pub use interpret::Committer;
use interpret::acp::Acp;
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Replayed, replay};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ui_state::{InputOutcome, Msg, SessionState};
use wire::Kind;

use crate::chat::ChatView;
pub use crate::sessions::{agent, caught_up, host_entry, offered_providers, text};
use crate::theme::Theme;

/// The session after each frame of one fixture, with its label and the
/// newest item time: caught up after the first frame, as a live chat is.
pub fn frames(kind: Kind, name: &str) -> Vec<(String, SessionState, i64)> {
    let dir = match kind {
        Kind::ClaudePty => "claude_pty",
        Kind::ClaudeSdk => "claude_sdk",
        Kind::Acp => "acp",
        _ => "codex",
    };
    let path = source_tree::crate_dir!()
        .join("../interpret/fixtures")
        .join(dir)
        .join(format!("{name}.json"));
    let replayed: Vec<Replayed> = match kind {
        Kind::ClaudePty => replay::<ClaudePty>(&path),
        Kind::ClaudeSdk => replay::<ClaudeSdk>(&path),
        Kind::Acp => replay::<Acp>(&path),
        _ => replay::<Codex>(&path),
    }
    .unwrap_or_else(|error| panic!("{dir}/{name}: {error}"));
    // An ACP agent is the program its fixture's spec names, which the host
    // offers under the name amux knows it by.
    let mut row = agent(kind);
    let mut providers = offered_providers();
    if kind == Kind::Acp {
        let text = std::fs::read_to_string(&path).expect("read above");
        let spec: serde_json::Value = serde_json::from_str(&text).expect("parsed above");
        let provider = spec["spec"]["provider"].as_str().unwrap_or_default();
        let name = wire::agents::name_or_id(provider);
        providers.push(crate::sessions::offered(provider, name, Kind::Acp));
        row.provider = provider.to_owned();
    }
    let mut state = SessionState::new(row, crate::chat::layout::CAP as usize);
    state.update(Msg::Host(host_entry(providers)));
    state.update(Msg::Connection(ui_state::Connection::Live));
    let mut committer = Committer::default();
    let mut out = Vec::new();
    let mut at = 0;
    for (index, frame) in replayed.iter().enumerate() {
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        for event in committer.commit(&frame.step) {
            state.update(Msg::Event(event));
        }
        if index == 0 {
            state.update(caught_up(committer.revision));
        }
        // The catalogue as a client fetches it once the snapshot names it.
        if let Some((hash, catalogue)) = &frame.catalogue {
            state.update(Msg::Catalogue {
                hash: hash.clone(),
                catalogue: catalogue.clone(),
            });
        }
        at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(at)
            .max(at);
        out.push((frame.label.clone(), state.clone(), at));
    }
    out
}

/// The first frame of a fixture where `pred` holds.
pub fn frame_where(
    kind: Kind,
    name: &str,
    pred: impl Fn(&SessionState) -> bool,
) -> (SessionState, i64) {
    frames(kind, name)
        .into_iter()
        .find(|(_, state, _)| pred(state))
        .map(|(_, state, at)| (state, at))
        .unwrap_or_else(|| panic!("{name}: no frame matches"))
}

/// A chat drawn at `width`x`height`.
pub fn draw(
    view: &mut ChatView,
    state: &SessionState,
    now_ms: i64,
    width: u16,
    height: u16,
    theme: Theme,
) -> (Buffer, Option<u32>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    let mut page = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            page = view.draw(frame, area, state, None, None, now_ms, theme);
        })
        .expect("draw");
    (terminal.backend().buffer().clone(), page)
}

/// The three frames every theme is held to: a turn at work, a permission
/// ask, and a Codex approval.
#[derive(Clone, Copy, Debug)]
pub enum Named {
    ClaudeWorking,
    ClaudePermissionAsk,
    CodexApproval,
}

impl Named {
    pub fn name(self) -> &'static str {
        match self {
            Named::ClaudeWorking => "claude working",
            Named::ClaudePermissionAsk => "claude permission ask",
            Named::CodexApproval => "codex approval",
        }
    }

    pub fn state(self) -> (SessionState, i64) {
        match self {
            Named::ClaudeWorking => frame_where(Kind::ClaudeSdk, "recorded_multi_turn", |state| {
                state.phase() == ui_state::PhaseView::Working && state.transcript().len() > 2
            }),
            Named::ClaudePermissionAsk => {
                frame_where(Kind::ClaudeSdk, "recorded_permission_callback", |state| {
                    ui_view::ask_card(state).is_some()
                })
            }
            Named::CodexApproval => frame_where(Kind::Codex, "recorded_approval_scopes", |state| {
                ui_view::ask_card(state).is_some()
            }),
        }
    }

    pub fn render(self, theme: Theme) -> Buffer {
        let (state, at) = self.state();
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        draw(&mut view, &state, at, 120, 40, theme).0
    }
}

/// A provider subagent as a replay holds it: its own session, and its row
/// in the fleet under the agent that started it.
#[derive(Clone)]
pub struct Sub {
    pub state: SessionState,
    pub row: wire::Agent,
    committer: Committer,
}

/// One frame of a family's replay: the agent's chat, each subagent's in
/// the order they started, and the fleet holding all of them.
#[derive(Clone)]
pub struct Family {
    pub label: String,
    pub state: SessionState,
    pub subs: Vec<Sub>,
    pub fleet: ui_state::FleetState,
    pub at: i64,
}

impl Family {
    /// The subagent `n`th to start.
    pub fn sub(&self, n: usize) -> &Sub {
        &self.subs[n]
    }
}

fn listed(agent: &wire::Agent) -> ui_state::FleetMsg {
    ui_state::FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Agent(agent.clone())),
    }))
}

/// A fixture's frames as a family: the agent's own part of each step
/// filed under it, and each provider subagent's part filed under the
/// subagent, whose fleet row is made at its start, follows its snapshots'
/// phases and ends with it, as the daemon files them.
pub fn family_frames(kind: Kind, name: &str) -> Vec<Family> {
    let dir = match kind {
        Kind::ClaudePty => "claude_pty",
        Kind::ClaudeSdk => "claude_sdk",
        _ => "codex",
    };
    let path = source_tree::crate_dir!()
        .join("../interpret/fixtures")
        .join(dir)
        .join(format!("{name}.json"));
    let replayed: Vec<Replayed> = match kind {
        Kind::ClaudePty => replay::<ClaudePty>(&path),
        Kind::ClaudeSdk => replay::<ClaudeSdk>(&path),
        _ => replay::<Codex>(&path),
    }
    .unwrap_or_else(|error| panic!("{dir}/{name}: {error}"));
    let host = agent(kind);
    let mut state = SessionState::new(host.clone(), crate::chat::layout::CAP as usize);
    state.update(Msg::Host(host_entry(offered_providers())));
    state.update(Msg::Connection(ui_state::Connection::Live));
    let mut committer = Committer::default();
    let mut fleet = ui_state::FleetState::new();
    fleet.update(ui_state::FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(host_entry(
            offered_providers(),
        ))),
    })));
    fleet.update(listed(&host));
    let mut subs: Vec<Sub> = Vec::new();
    let mut out = Vec::new();
    let mut at = 0;
    for (index, frame) in replayed.iter().enumerate() {
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        for event in committer.commit(&frame.step) {
            state.update(Msg::Event(event));
        }
        if index == 0 {
            state.update(caught_up(committer.revision));
        }
        for part in &frame.step.agents {
            let known = subs
                .iter()
                .position(|sub| sub.row.agent_id == part.agent_id);
            let at_sub = match (known, &part.start) {
                (Some(at), _) => at,
                (None, Some(start)) => {
                    let hex = interpret::to_hex(&part.agent_id);
                    let row = wire::Agent {
                        agent_id: part.agent_id.clone(),
                        host_id: host.host_id.clone(),
                        kind: kind as i32,
                        name: format!("sub-{}", &hex[..hex.len().min(8)]),
                        title: Some(start.title.clone()).filter(|title| !title.is_empty()),
                        lifecycle: wire::Lifecycle::Live as i32,
                        phase: wire::Phase::Working as i32,
                        incarnation: 1,
                        parent: Some(wire::AgentRef {
                            host_id: host.host_id.clone(),
                            agent_id: host.agent_id.clone(),
                        }),
                        native: Some(wire::NativeChild {
                            provider_child_id: start.provider_child_id.clone(),
                        }),
                        ..wire::Agent::default()
                    };
                    let mut state =
                        SessionState::new(row.clone(), crate::chat::layout::CAP as usize);
                    state.update(Msg::Host(host_entry(offered_providers())));
                    state.update(Msg::Connection(ui_state::Connection::Live));
                    state.update(caught_up(0));
                    subs.push(Sub {
                        state,
                        row,
                        committer: Committer::default(),
                    });
                    subs.len() - 1
                }
                // A part for a subagent whose start was never seen.
                (None, None) => continue,
            };
            let sub = &mut subs[at_sub];
            if part.start.is_some() && sub.row.lifecycle == wire::Lifecycle::Exited as i32 {
                sub.row.lifecycle = wire::Lifecycle::Live as i32;
                sub.row.phase = wire::Phase::Working as i32;
                sub.row.exit_cause = 0;
                sub.row.exit_detail = None;
            }
            for event in sub.committer.commit_agent(part) {
                sub.state.update(Msg::Event(event));
            }
            if let Some(snapshot) = &part.snapshot {
                sub.row.phase = snapshot.phase;
            }
            if let Some(end) = &part.end {
                sub.row.lifecycle = wire::Lifecycle::Exited as i32;
                sub.row.exit_cause = end.cause;
                sub.row.exit_detail = end.detail.clone();
            }
            sub.state.update(Msg::Entry(sub.row.clone()));
            fleet.update(listed(&sub.row));
        }
        at = frame
            .step
            .items
            .iter()
            .chain(frame.step.agents.iter().flat_map(|part| &part.items))
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(at)
            .max(at);
        out.push(Family {
            label: frame.label.clone(),
            state: state.clone(),
            subs: subs.clone(),
            fleet: fleet.clone(),
            at,
        });
    }
    out
}
