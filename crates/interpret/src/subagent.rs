//! Provider subagents: the agents Claude's Agent tool and Codex's child
//! threads start inside an agent's own provider process.
//!
//! Each is an amux agent of its own, with a row and a chat but no process:
//! the agent whose process runs it (its host) reads the one provider stream,
//! sorts what it reads by the agent it belongs to, and writes each
//! subagent's items, appends, snapshot, start and end into its own journal
//! as that subagent's part of the step ([`wire::AgentStep`] in
//! `Step.agents`). One frame per step still holds every agent's part,
//! because a step often touches two agents at once (a subagent's start and
//! the call row in its parent's chat) and the frame is what survives a
//! crash whole.
//!
//! An interpreter writes a subagent's part through [`Subagent`], a view of
//! one subagent scoped to the step in hand, taken from
//! [`crate::Shared::subagent`] by the provider's own id for it. It mirrors
//! what `Shared` offers for the agent's own part (items, appends, a
//! snapshot, asks), with the subagent's own open items, last snapshot and
//! asks kept in the checkpointed state, so a resume re-emits a running
//! subagent's open items the way it re-emits the agent's own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wire::{AgentStep, IncarnationEnd, Item, NativeStart, Phase, Refusal, Snapshot};

use crate::checks::{self, Watch};
use crate::shared::{Asks, Emit, ItemDraft, Lane, OpenAsk, Output};
use crate::{Effect, reply, serde_pb};

/// The namespace every host's own namespace is derived in. Fixed for ever:
/// a subagent's id is stored in rows, journals and clients' records, and
/// a host that derived another one after an update would orphan them.
const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x9f20f1f2_6cb4_4fc3_aef0_c6714e5b59c3);

/// The agent id of the provider subagent `provider_child_id` that the
/// agent `host_agent_id` runs: Claude's agent id or Codex's thread id for
/// it, under its host.
///
/// A UUIDv5, so it is the same across restarts, replays and continuations
/// and nothing has to be looked up or checkpointed to find it. The host's
/// id names a namespace of its own, in which the provider's id names the
/// subagent: two hosts whose providers happen to reuse an id get different
/// agents, and no pair of ids can run together into another's.
pub fn subagent_id(host_agent_id: &[u8], provider_child_id: &str) -> Vec<u8> {
    let host = uuid::Uuid::new_v5(&NAMESPACE, host_agent_id);
    uuid::Uuid::new_v5(&host, provider_child_id.as_bytes())
        .as_bytes()
        .to_vec()
}

/// What the shared core keeps of one running subagent between steps, by
/// the provider's id for it. Present from the subagent's first part until
/// its end; a continuation after the end starts a new one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub(crate) struct Native<A: OpenAsk> {
    #[serde(with = "serde_pb::bytes")]
    pub(crate) agent: Vec<u8>,
    /// Its start was written. A part written without one is outside the
    /// subagent's run and breaks its check.
    pub(crate) started: bool,
    /// Its items emitted and not complete, at their full current state.
    #[serde(with = "serde_pb::msg_map")]
    pub(crate) open_items: BTreeMap<String, Item>,
    /// Its last snapshot emitted, so a step carries one only when it
    /// changed.
    #[serde(with = "serde_pb::opt_msg")]
    pub(crate) last_snapshot: Option<Snapshot>,
    pub(crate) asks: Asks<A>,
}

impl<A: OpenAsk> Native<A> {
    pub(crate) fn new(agent: Vec<u8>) -> Self {
        Self {
            agent,
            started: false,
            open_items: BTreeMap::new(),
            last_snapshot: None,
            asks: Asks::default(),
        }
    }
}

/// The part of the step in hand that belongs to `agent`: the newest one,
/// unless it already ended, when what follows is a part of its own so the
/// order of an end and a new start stays readable.
pub(crate) fn part<'e>(emit: &'e mut Emit, agent: &[u8]) -> &'e mut AgentStep {
    let parts = &mut emit.step.agents;
    match parts.iter().rposition(|part| part.agent_id == agent) {
        Some(at) if parts[at].end.is_none() => &mut parts[at],
        _ => {
            parts.push(AgentStep {
                agent_id: agent.to_vec(),
                ..AgentStep::default()
            });
            parts.last_mut().expect("just pushed")
        }
    }
}

/// One provider subagent's view of the step in hand: what an interpreter
/// writes here goes into that subagent's part of the step, and what it
/// reads is that subagent's own state. Taken from
/// [`crate::Shared::subagent`]; it holds the shared state and the step
/// until it is dropped.
///
/// A subagent's run is its [`start`](Self::start), its parts, and its
/// [`end`](Self::end), which consumes the view: anything written for it
/// outside that breaks `agent.subagent_run`, as does an end with an item
/// still streaming. A continuation starts it again.
pub struct Subagent<'a, A: OpenAsk> {
    pub(crate) natives: &'a mut BTreeMap<String, Native<A>>,
    pub(crate) child: String,
    pub(crate) agent: Vec<u8>,
    pub(crate) watch: &'a mut Watch,
    pub(crate) emit: &'a mut Emit,
    pub(crate) kind: i32,
    pub(crate) now_ms: i64,
}

impl<A: OpenAsk> Subagent<'_, A> {
    /// The subagent's agent id, which a call row that opens its chat
    /// carries and a person's input to it arrives under.
    pub fn agent_id(&self) -> &[u8] {
        &self.agent
    }

    /// The provider's own id for it.
    pub fn provider_child_id(&self) -> &str {
        &self.child
    }

    /// Whether its start has been written and its end has not.
    pub fn is_running(&self) -> bool {
        self.native_ref().started
    }

    fn native_ref(&self) -> &Native<A> {
        self.natives
            .get(&self.child)
            .expect("a subagent view's state lives until its end")
    }

    fn native(&mut self) -> &mut Native<A> {
        self.natives
            .get_mut(&self.child)
            .expect("a subagent view's state lives until its end")
    }

    /// Notes a write outside the subagent's run.
    fn within_run(&mut self, what: &str) {
        if !self.native_ref().started {
            let detail = format!("{what} for subagent {} before its start", self.child);
            self.watch.broke(checks::SUBAGENT_RUN, detail);
        }
    }

    /// The subagent starts, or its parent's model continues it: the row is
    /// made or brought back from what `start` says. Its provider child id is
    /// this view's own, whatever `start` held. Write it in the step that
    /// writes the call row naming it, so the two land in one frame.
    pub fn start(&mut self, mut start: NativeStart) {
        start.provider_child_id.clone_from(&self.child);
        self.native().started = true;
        part(self.emit, &self.agent).start = Some(start);
    }

    /// Emits one of its items at its full current state, as
    /// [`crate::Shared::item`] does the agent's own.
    pub fn item(&mut self, draft: ItemDraft) {
        self.within_run("an item");
        let native = self
            .natives
            .get_mut(&self.child)
            .expect("a subagent view's state lives until its end");
        let part = part(self.emit, &self.agent);
        Lane {
            open: &mut native.open_items,
            items: &mut part.items,
            appends: &mut part.appends,
            watch: self.watch,
            scope: Some(&self.agent),
        }
        .item(self.kind, self.now_ms, draft);
    }

    /// Extends the text of one of its open items. False, and nothing
    /// emitted, when the key is not open: emit a full item instead.
    pub fn append(&mut self, key: &str, text: &str) -> bool {
        self.lane_for_open(key)
            .is_some_and(|mut lane| lane.append(key, text))
    }

    /// Extends the output of one of its running commands, cut as
    /// [`crate::Shared::append_output`] cuts the agent's own.
    pub fn append_output(&mut self, key: &str, text: &str) -> Output {
        match self.lane_for_open(key) {
            Some(mut lane) => lane.append_output(key, text),
            None => Output::NotOpen,
        }
    }

    /// The lane of one of its open items; None when the key is not open, so
    /// nothing is touched.
    fn lane_for_open(&mut self, key: &str) -> Option<Lane<'_>> {
        let native = self.natives.get_mut(&self.child)?;
        if !native.open_items.contains_key(key) {
            return None;
        }
        let part = part(self.emit, &self.agent);
        Some(Lane {
            open: &mut native.open_items,
            items: &mut part.items,
            appends: &mut part.appends,
            watch: self.watch,
            scope: Some(&self.agent),
        })
    }

    pub fn open_item(&self, key: &str) -> Option<&Item> {
        self.native_ref().open_items.get(key)
    }

    /// The keys of its items still open whose key starts with `prefix`.
    pub fn open_keys(&self, prefix: &str) -> Vec<String> {
        self.native_ref()
            .open_items
            .range(prefix.to_owned()..)
            .map(|(key, _)| key)
            .take_while(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Its snapshot: what its row and chat draw without items. The
    /// interpreter fills the phase (see [`phase`](Self::phase)), the kind's
    /// body, the jobs and whatever else it knows; the kind, the clock and
    /// when the phase began are stamped here, the sign-in, which its host's
    /// provider holds, is left unknown, and the step carries it only when
    /// something other than its timestamp changed.
    pub fn snapshot(&mut self, mut snapshot: Snapshot) {
        self.within_run("a snapshot");
        snapshot.revision = 0;
        snapshot.kind = self.kind;
        snapshot.at_ms = self.now_ms;
        snapshot.sign_in.get_or_insert_with(crate::unknown::sign_in);
        let now_ms = self.now_ms;
        let native = self.native();
        let phase = snapshot.status().phase;
        let phase_since_ms = match &native.last_snapshot {
            Some(last) if last.status().phase == phase => last.status().phase_since_ms,
            _ => now_ms,
        };
        snapshot.status_mut().phase_since_ms = phase_since_ms;
        let changed = native.last_snapshot.as_ref().is_none_or(|last| {
            Snapshot {
                at_ms: snapshot.at_ms,
                ..last.clone()
            } != snapshot
        });
        if changed {
            native.last_snapshot = Some(snapshot.clone());
            part(self.emit, &self.agent).snapshot = Some(snapshot);
        }
    }

    /// The phase a running subagent reads as: needs you while one of its
    /// asks is open, working otherwise. It takes no prompts, so it is never
    /// idle while it runs.
    pub fn phase(&self) -> Phase {
        if self.native_ref().asks.is_empty() {
            Phase::Working
        } else {
            Phase::NeedsYou
        }
    }

    // --- asks ------------------------------------------------------------

    /// Its open asks, which its snapshot body lists and its phase follows.
    pub fn asks(&self) -> &Asks<A> {
        &self.native_ref().asks
    }

    pub fn open_ask(&mut self, ask: A) {
        self.native().asks.open(ask);
    }

    pub fn close_ask(&mut self, key: &str) -> Option<A> {
        self.native().asks.close(key)
    }

    pub fn close_asks_for_item(&mut self, item_key: &str) -> Vec<A> {
        self.native().asks.close_for_item(item_key)
    }

    pub fn close_all_asks(&mut self) -> Vec<A> {
        self.native().asks.close_all()
    }

    /// The stale-answer rule for its asks, as [`crate::Shared::answer`] for
    /// the agent's own: an answer to an open ask closes it and hands it
    /// back, so the kind answers the provider and accepts; one to an ask
    /// that is not open is refused as `REFUSAL_CLOSED_ASK`.
    pub fn answer(&mut self, input_id: &[u8], ask_key: &str) -> Option<A> {
        let ask = self.native().asks.close(ask_key);
        self.watch.answers.push((input_id.to_vec(), ask.is_some()));
        if ask.is_none() {
            self.reject(input_id, Refusal::ClosedAsk);
        }
        ask
    }

    // --- replies and effects ----------------------------------------------

    /// Accepts a person's input to the subagent, keyed by its input id.
    pub fn accept(&mut self, input_id: &[u8]) {
        self.emit.effect(Effect::Reply {
            input_id: input_id.to_vec(),
            verdict: reply::accepted(false),
        });
    }

    pub fn reject(&mut self, input_id: &[u8], refusal: Refusal) {
        self.emit.effect(Effect::Reply {
            input_id: input_id.to_vec(),
            verdict: reply::rejected(refusal),
        });
    }

    /// An effect for the agent process, such as the provider write that
    /// answers the subagent's ask, in order with the step's others.
    pub fn effect(&mut self, effect: Effect) {
        self.emit.effect(effect);
    }

    // --- the end ----------------------------------------------------------

    /// The subagent ended, for `end`'s cause: its last part. Close its
    /// items and asks and write its final snapshot first. What is written
    /// for it after this is outside its run until a new
    /// [`start`](Self::start) continues it.
    pub fn end(mut self, end: IncarnationEnd) {
        self.within_run("an end");
        let Some(native) = self.natives.remove(&self.child) else {
            return;
        };
        if !native.open_items.is_empty() {
            let detail = format!(
                "subagent {} ended with {} items still streaming",
                self.child,
                native.open_items.len()
            );
            self.watch.broke(checks::SUBAGENT_RUN, detail);
        }
        part(self.emit, &self.agent).end = Some(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation is a promise: ids already stored stay the ids their
    /// hosts derive. These are what it derived when it was written.
    #[test]
    fn a_subagents_id_is_the_same_wherever_and_whenever_it_is_derived() {
        let host = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let id = subagent_id(host.as_bytes(), "a1b2c3d4e5f6a7b8c");
        assert_eq!(id, subagent_id(host.as_bytes(), "a1b2c3d4e5f6a7b8c"));
        let derived = uuid::Uuid::from_slice(&id).unwrap();
        assert_eq!(derived.get_version_num(), 5);
        assert_eq!(derived.to_string(), "b04d99b8-c432-55f8-8f02-8232c79171f8");
    }

    #[test]
    fn another_host_or_another_child_is_another_agent() {
        let host = [7u8; 16];
        let other_host = [8u8; 16];
        let id = subagent_id(&host, "thread-1");
        assert_ne!(id, subagent_id(&other_host, "thread-1"));
        assert_ne!(id, subagent_id(&host, "thread-2"));
        assert_ne!(id, host.to_vec());
        assert_eq!(id.len(), 16);
    }
}
