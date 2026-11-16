//! The calls that open agents, and how they fold. A call that starts or
//! continues an agent (a provider subagent, or a child amux's spawn made)
//! names the agent, and its row opens that agent's chat. How the agent
//! stands (running, waiting on the person, done) is read from its fleet
//! row, which every client holds, never from the call: a call can return
//! while its subagent runs on, and the provider's own progress on the call
//! says nothing of an ask the subagent is waiting on.
//!
//! The calls that opened more than one agent in one turn fold into one
//! row, "3 subagents", with their combined state; opening it shows each
//! call's own row. Like a run of tool steps, the group is an attribute on
//! its members, decided here so every client draws the same: it draws at
//! its first call, and a client holds it open by any of its calls, in the
//! same set that holds open runs.
//!
//! A turn here is what lies between two prompts, steers or turn ends, as
//! for every fold.

use std::collections::{BTreeSet, HashSet};
use std::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{
    AgentKey, Attention, FleetState, Held, Key, PhaseView, SessionState, Transcript, agent_key,
    attention, exit_cause, shown_name,
};
use wire::{Agent, Kind};

use crate::ToolRows;
use crate::fleet::ExitCause;
use crate::turn::bounds_turn;

/// The calls in one turn that opened more than one agent, stated on each
/// of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SubagentGroup {
    /// The group's oldest held call, where its one row draws, folded or
    /// open. It moves as the window's low edge does, so it names no group:
    /// hold an open group by any of its calls (see [`toggle_group`]).
    pub first: Key,
    /// Each agent its calls opened, once, in the order first opened: what
    /// [`group_state`] sums.
    pub agents: Vec<Vec<u8>>,
    /// The reader opened it, so each call shows its own row. Always open
    /// for a view that folds nothing, or hides only tool steps: these calls
    /// are not steps.
    pub open: bool,
}

/// The agent a call opens, as its fleet row has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OpenedAgent {
    /// The chat a client opens from the row.
    pub agent: AgentKey,
    /// What a person reads it as: its title, else its name.
    pub name: String,
    pub kind: Kind,
    /// Where it stands: working, waiting on the person, idle, or how it
    /// ended.
    pub phase: PhaseView,
}

/// What the agents of a group come to, by where each stands: "2 running ·
/// 1 waiting on you".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GroupState {
    /// How many agents the group opened.
    pub agents: u32,
    /// Starting or working.
    pub running: u32,
    pub needs_you: u32,
    /// Live and waiting for its parent, as an amux child between turns.
    pub idle: u32,
    pub finished: u32,
    /// Stopped, or ended some way that is not failure.
    pub ended: u32,
    pub failed: u32,
    /// Not in this client's fleet: not listed yet, or deleted.
    pub unknown: u32,
}

/// The agents a chat's held rows open, with each group's combined state:
/// what a client that keeps rows apart from the fleet redraws when the
/// fleet moves, since no row changes then.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ChatAgents {
    /// Each agent a held row opens and the fleet holds, once, oldest call
    /// first.
    pub agents: Vec<OpenedAgent>,
    /// Each group the held rows make, by its first call.
    pub groups: Vec<GroupView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GroupView {
    pub first: Key,
    pub state: GroupState,
}

/// The agent `agent_id` as the fleet has it; None when the fleet does not
/// hold it, and then the call's own state is all a client can say.
pub fn opened_agent(fleet: &FleetState, agent_id: &[u8]) -> Option<OpenedAgent> {
    let agent = fleet.find(agent_id)?;
    Some(OpenedAgent {
        agent: agent_key(agent),
        name: shown_name(agent).to_owned(),
        kind: agent.kind(),
        phase: phase_of(agent),
    })
}

/// An agent's phase from its fleet row alone.
fn phase_of(agent: &Agent) -> PhaseView {
    match attention(agent) {
        Attention::Exited => PhaseView::Exited {
            cause: exit_cause(agent),
        },
        Attention::Idle => PhaseView::Idle,
        Attention::Starting => PhaseView::Starting,
        Attention::Working => PhaseView::Working,
        Attention::NeedsYou => PhaseView::NeedsYou,
    }
}

/// What `agents` come to, each by its fleet row.
pub fn group_state(fleet: &FleetState, agents: &[Vec<u8>]) -> GroupState {
    let mut state = GroupState {
        agents: agents.len() as u32,
        ..GroupState::default()
    };
    for agent in agents {
        let count = match opened_agent(fleet, agent).map(|opened| opened.phase) {
            None => &mut state.unknown,
            Some(PhaseView::Starting | PhaseView::Working) => &mut state.running,
            Some(PhaseView::NeedsYou) => &mut state.needs_you,
            Some(PhaseView::Idle) => &mut state.idle,
            Some(PhaseView::Exited { cause }) => match cause {
                ExitCause::Finished => &mut state.finished,
                ExitCause::Ended => &mut state.ended,
                ExitCause::Failed(_) => &mut state.failed,
            },
        };
        *count += 1;
    }
    state
}

/// Every agent the held rows open and every group they make, with what the
/// fleet says of each.
pub fn chat_agents(state: &SessionState, fleet: &FleetState) -> ChatAgents {
    let transcript = state.transcript();
    let mut seen = BTreeSet::new();
    let mut agents = Vec::new();
    let mut groups = Vec::new();
    let mut turn: Vec<(Key, Vec<u8>)> = Vec::new();
    let mut close = |turn: &mut Vec<(Key, Vec<u8>)>| {
        let opened = distinct(turn.iter().map(|(_, agent)| agent));
        if opened.len() > 1 {
            groups.push(GroupView {
                first: turn[0].0.clone(),
                state: group_state(fleet, &opened),
            });
        }
        turn.clear();
    };
    for held in transcript.iter() {
        if bounds_turn(&held.class) {
            close(&mut turn);
            continue;
        }
        let Some(agent) = ui_state::opens(&held.body) else {
            continue;
        };
        turn.push((held.item.key.clone(), agent.to_vec()));
        if seen.insert(agent.to_vec())
            && let Some(opened) = opened_agent(fleet, agent)
        {
            agents.push(opened);
        }
    }
    close(&mut turn);
    ChatAgents { agents, groups }
}

/// Each agent once, in the order first met.
fn distinct<'a>(agents: impl Iterator<Item = &'a Vec<u8>>) -> Vec<Vec<u8>> {
    let mut seen = HashSet::new();
    agents
        .filter(|agent| seen.insert(agent.as_slice()))
        .cloned()
        .collect()
}

/// The held orders of the turn the item at `order` sits in, without its
/// bounds: from just after the prompt, steer or turn end before it (else the
/// window's low edge) to just before the next one (else the head).
fn turn_span(transcript: &Transcript, order: u64) -> Option<RangeInclusive<u64>> {
    let (oldest, head) = (transcript.oldest_held()?, transcript.head()?);
    let lo = transcript
        .range(oldest..=order.saturating_sub(1).max(oldest))
        .rev()
        .filter(|held| held.item.order < order)
        .find(|held| bounds_turn(&held.class))
        .map_or(oldest, |held| held.item.order + 1);
    let hi = transcript
        .range(order.saturating_add(1).min(head)..=head)
        .filter(|held| held.item.order > order)
        .find(|held| bounds_turn(&held.class))
        .map_or(head, |held| held.item.order - 1);
    Some(lo..=hi)
}

/// The calls in `span` that open an agent: each one's order, key and agent.
fn calls(transcript: &Transcript, span: RangeInclusive<u64>) -> Vec<(u64, Key, Vec<u8>)> {
    transcript
        .range(span)
        .filter_map(|held| {
            ui_state::opens(&held.body)
                .map(|agent| (held.item.order, held.item.key.clone(), agent.to_vec()))
        })
        .collect()
}

/// The turns one read of rows has met, so a turn is walked once however
/// many of its calls the read places.
#[derive(Default)]
pub(crate) struct Groups {
    met: Vec<(RangeInclusive<u64>, Vec<(u64, Key, Vec<u8>)>)>,
}

impl Groups {
    /// The group the call `held` belongs to, when its turn's calls opened
    /// more than one agent.
    pub(crate) fn of(
        &mut self,
        state: &SessionState,
        held: &Held,
        tools: ToolRows,
    ) -> Option<SubagentGroup> {
        let order = held.item.order;
        let index = match self.met.iter().position(|(span, _)| span.contains(&order)) {
            Some(index) => index,
            None => {
                let span = turn_span(state.transcript(), order)?;
                let found = calls(state.transcript(), span.clone());
                self.met.push((span, found));
                self.met.len() - 1
            }
        };
        let calls = &self.met[index].1;
        let agents = distinct(calls.iter().map(|(_, _, agent)| agent));
        if agents.len() < 2 {
            return None;
        }
        let open = match tools {
            ToolRows::ShowAll | ToolRows::Hide => true,
            ToolRows::Collapse { open } | ToolRows::Turns { runs: open, .. } => {
                calls.iter().any(|(_, key, _)| open.contains(key))
            }
        };
        Some(SubagentGroup {
            first: calls[0].1.clone(),
            agents,
            open,
        })
    }
}

/// Opens the group the call `member` belongs to, holding its newest call,
/// which no trim at the low edge takes first, or closes it, forgetting
/// every call of it the set holds. The set is the one that holds open runs.
pub fn toggle_group(state: &SessionState, member: &Key, open: &mut HashSet<Key>) {
    let transcript = state.transcript();
    let Some(held) = transcript.get(member) else {
        return;
    };
    let Some(span) = turn_span(transcript, held.item.order) else {
        return;
    };
    let calls = calls(transcript, span);
    let held: Vec<&Key> = calls
        .iter()
        .map(|(_, key, _)| key)
        .filter(|key| open.contains(*key))
        .collect();
    if held.is_empty() {
        if let Some((_, newest, _)) = calls.last() {
            open.insert(newest.clone());
        }
    } else {
        for key in held {
            open.remove(key);
        }
    }
}

/// The calls whose group a change to `changed` moves without touching
/// them: another call joining their turn, a prompt or turn end splitting
/// or joining turns, or a cut at the window's low edge taking the group's
/// first call. A client that holds rows by key redraws these too.
pub fn subagent_rows_moved(state: &SessionState, changed: &[Key]) -> Vec<Key> {
    let transcript = state.transcript();
    let Some(oldest) = transcript.oldest_held() else {
        return Vec::new();
    };
    let mut spans: Vec<RangeInclusive<u64>> = Vec::new();
    let mut add = |span: Option<RangeInclusive<u64>>| {
        if let Some(span) = span
            && !span.is_empty()
            && !spans.contains(&span)
        {
            spans.push(span);
        }
    };
    let mut cut = false;
    for key in changed {
        let Some(held) = transcript.get(key) else {
            cut = true;
            continue;
        };
        let order = held.item.order;
        if bounds_turn(&held.class) {
            // The turns on either side of a bound, which it may have split.
            add(order
                .checked_sub(1)
                .filter(|before| *before >= oldest)
                .and_then(|before| turn_span(transcript, before)));
            add(turn_span(transcript, order + 1).filter(|span| span.contains(&(order + 1))));
        } else if ui_state::opens(&held.body).is_some() {
            add(turn_span(transcript, order));
        }
    }
    if cut {
        add(turn_span(transcript, oldest));
    }
    let changed: HashSet<&Key> = changed.iter().collect();
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    for span in spans {
        for (_, key, _) in calls(transcript, span) {
            if !changed.contains(&key) && seen.insert(key.clone()) {
                keys.push(key);
            }
        }
    }
    keys
}
