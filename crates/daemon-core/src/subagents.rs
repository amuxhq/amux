//! Provider subagents: the agents Claude's Agent tool and Codex's child
//! threads start inside the provider process of one of this host's agents,
//! their host.
//!
//! A provider subagent has a row, a chat and a fleet entry like any agent,
//! and no process, directory or journal of its own. Its host's journal
//! carries its parts of each step (`Step.agents`), which ingest files under
//! the subagent's own row. Its parent is always its host, so the family
//! rules hold unchanged: every parent has a process. What the registry does
//! for one:
//!
//! - its row is made before the commit of the first part that starts it,
//!   so the row exists before its first item;
//! - after the commit, a start on a row that ended is a continuation and an
//!   end goes through the exit path any agent takes, with no delivery,
//!   since a subagent's answer goes back through the provider;
//! - it ends with its host's process, in the same write;
//! - a session on it reads as current when its host's does.
//!
//! What a person may do to one is narrower than for an agent with a
//! process: they answer its asks and stop its job, both through its host's
//! process; resuming, deleting, killing and pinning it are refused, since
//! it goes with its host.

use store::{AgentKey, AgentRow, Marker, StoreError, SubagentChange};
use wire::{ExitCause, Lifecycle, NativeStart, Step, ctl_frame};

use crate::families::{Began, Caller, Doing, Op, Refusal};
use crate::ids::{AgentId, TimerId};
use crate::io::Output;
use crate::{Daemon, Filing};

/// Why a resume of a provider subagent is refused.
pub const NO_RESUME: &str = "a provider subagent is continued only by its parent's model, \
through its provider; it cannot be resumed";
/// Why a message to a provider subagent, or a prompt, is refused.
pub const NO_MESSAGES: &str = "Claude or Codex runs this subagent itself, and it takes no \
messages: only its parent's model reaches it";
/// Why a delete of a provider subagent is refused.
pub const NO_DELETE: &str = "a provider subagent goes with the agent that runs it and has no \
directory of its own; delete that agent instead";
/// Why a kill of a provider subagent is refused.
pub const NO_KILL: &str = "a provider subagent has no process of its own; stop it from its \
chat where its provider allows, or stop the agent that runs it";
/// Why closing a running provider subagent is refused when its provider
/// offers no stop for it.
pub const NO_STOP: &str = "its provider offers no stop for this subagent while it runs; stop \
the agent that runs it, or close it once it has ended";
/// Why closing a running provider subagent is refused while its host's
/// process cannot hear the stop.
pub const HOST_AWAY: &str = "the agent that runs this subagent is not connected, so the stop \
cannot reach its provider";

impl Daemon {
    /// Whether the agent is one of this host's provider subagents.
    pub(crate) fn is_subagent(&self, agent: AgentId) -> bool {
        self.store
            .agent(&self.own_key(agent))
            .ok()
            .flatten()
            .is_some_and(|row| row.native.is_some())
    }

    /// Before a batch commits: makes a row for each provider subagent its
    /// parts start that has none, under `host`, and returns them. The row
    /// is published and counted as a spawned child's is, so a subscriber
    /// hears of it before its first item.
    pub(crate) fn make_subagents(
        &mut self,
        host: &AgentKey,
        frames: &[(u64, Step)],
        out: &mut Vec<Output>,
    ) -> Result<Vec<AgentKey>, StoreError> {
        let mut made = Vec::new();
        let mut host_row = None;
        for (_, step) in frames {
            for part in &step.agents {
                let Some(start) = &part.start else {
                    continue;
                };
                let key = AgentKey::new(host.host.clone(), part.agent_id.clone());
                if AgentId::from_slice(&part.agent_id).is_none()
                    || made.contains(&key)
                    || self.store.agent(&key)?.is_some()
                {
                    continue;
                }
                if host_row.is_none() {
                    host_row = self.store.agent(host)?;
                }
                // A subagent hosts none: a nested one is its host's too,
                // and the host's parts name it.
                let Some(host_row) = host_row.as_ref().filter(|row| row.native.is_none()) else {
                    return Ok(made);
                };
                let row = self.new_subagent(host_row, key.clone(), start)?;
                self.put_own_row(&row, out)?;
                made.push(key);
            }
        }
        Ok(made)
    }

    /// A provider subagent's new row: live and working, of its host's kind
    /// and in its host's folder, named for its id rather than its content.
    fn new_subagent(
        &self,
        host: &AgentRow,
        key: AgentKey,
        start: &NativeStart,
    ) -> Result<AgentRow, StoreError> {
        let names = self.own_names()?;
        let mut row = AgentRow::new(key, host.kind, host.cwd.clone());
        // Run by its host's provider process, so of its host's program.
        row.provider = host.provider.clone();
        // Never a word pair or a name taken from what it is about: those
        // are names a person would choose, and the provider's description
        // is the title.
        let hex: String = row.agent.agent[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        row.name = crate::names::free_suffix(&format!("sub-{hex}"), &|candidate| {
            names.contains(candidate)
        });
        row.parent = Some(host.agent.clone());
        let now = self.timers.now_ms as i64;
        row.status.phase = wire::Phase::Working as i32;
        row.status.phase_since_ms = now;
        row.created_at_ms = now;
        // Its answer goes back through its parent's provider.
        row.owes_report = false;
        row.title = Some(start.title.clone()).filter(|title| !title.is_empty());
        row.native = Some(wire::NativeChild {
            provider_child_id: start.provider_child_id.clone(),
        });
        Ok(row)
    }

    /// After a batch committed: what its parts said of each subagent's
    /// life, and the sessions of the subagents made for it brought as
    /// current as their host's.
    ///
    /// Only each subagent's last change in the batch is acted on, since the
    /// commit stored its row as the batch left it. An end that a later
    /// start in the same batch overtook, as a catch-up on a long journal
    /// reads of a finished subagent its parent's model continued, would
    /// otherwise mark exited a row the provider is still running.
    pub(crate) fn subagents_committed(
        &mut self,
        host: &AgentKey,
        made: &[AgentKey],
        changes: Vec<SubagentChange>,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        for key in made {
            self.follow_hosts_marker(host, key, out)?;
        }
        let of = |change: &SubagentChange| match change {
            SubagentChange::Continued(key) | SubagentChange::Ended(key, _) => key.clone(),
        };
        let last: Vec<bool> = (0..changes.len())
            .map(|at| {
                let key = of(&changes[at]);
                changes[at + 1..].iter().all(|later| of(later) != key)
            })
            .collect();
        for (change, last) in changes.into_iter().zip(last) {
            if !last {
                continue;
            }
            match change {
                SubagentChange::Continued(key) => self.subagent_continued(host, &key, out)?,
                SubagentChange::Ended(key, end) => self.subagent_ended(&key, end, out)?,
            }
        }
        Ok(())
    }

    /// Its parent's model continued it: the commit made it live and open
    /// again, and its chat carries on. Its incarnation stays: nothing that
    /// reads incarnations applies to a row that takes no reports or prompts.
    fn subagent_continued(
        &mut self,
        host: &AgentKey,
        key: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        self.publish_row(key, out)?;
        self.recount_from(Some(host.clone()), out)?;
        self.follow_hosts_marker(host, key, out)
    }

    /// It ended: the commit marked it exited, filing a finished one closed.
    /// What any agent's exit sets off follows, its session is current,
    /// since nothing more comes, and a close waiting on it is answered.
    fn subagent_ended(
        &mut self,
        key: &AgentKey,
        end: wire::IncarnationEnd,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let Some(row) = self.store.agent(key)? else {
            return Ok(());
        };
        self.record_exit(row, end, out)?;
        self.subagent_settled(key, out)
    }

    /// A subagent is current exactly when its host's journal is: one read
    /// into the store as it is written is read for the subagent too.
    fn follow_hosts_marker(
        &mut self,
        host: &AgentKey,
        key: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        if self.store.markers().get(host) == Some(&Marker::CaughtUp) {
            self.announce_caught_up(key, out)
        } else {
            self.store.set_marker(key, None);
            Ok(())
        }
    }

    /// The host's journal was read to its end after its Hello: each of its
    /// live subagents is as current as it is.
    pub(crate) fn subagents_caught_up(
        &mut self,
        host: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        for row in self.store.agents_of_parent(host)? {
            if row.native.is_some() && row.lifecycle == Lifecycle::Live as i32 {
                self.announce_caught_up(&row.agent, out)?;
            }
        }
        Ok(())
    }

    /// The start's sweep is done: a subagent that had ended before this
    /// daemon started is as current as it will ever be. One still live is
    /// its host's to settle: caught up with it after its Hello, or ended
    /// with it.
    pub(crate) fn subagents_swept(&mut self, out: &mut Vec<Output>) {
        let own = self.store.own_host().to_vec();
        let ended = self
            .store
            .agents()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| {
                row.agent.host == own
                    && row.native.is_some()
                    && row.lifecycle == Lifecycle::Exited as i32
            });
        for row in ended.collect::<Vec<_>>() {
            if self.store.markers().get(&row.agent).is_none()
                && let Err(error) = self.announce_caught_up(&row.agent, out)
            {
                tracing::warn!(%error, "marking a provider subagent caught up failed");
            }
        }
    }

    /// The live provider subagents of `host`, each as it reads once it has
    /// ended with its host's process: with its own cause if a part already
    /// said one, else the host's.
    pub(crate) fn subagents_ending_with(
        &self,
        host: &AgentKey,
        cause: &wire::IncarnationEnd,
    ) -> Result<Vec<AgentRow>, StoreError> {
        let now = self.timers.now_ms as i64;
        Ok(self
            .store
            .agents_of_parent(host)?
            .into_iter()
            .filter(|row| {
                row.agent.host == host.host
                    && row.native.is_some()
                    && row.lifecycle == Lifecycle::Live as i32
            })
            .map(|mut row| {
                row.lifecycle = Lifecycle::Exited as i32;
                if row.exit_cause == ExitCause::Unspecified as i32 {
                    row.exit_cause = cause.cause;
                    row.exit_detail = cause.detail.clone();
                }
                if row.exit_cause == ExitCause::Finished as i32 && !row.is_closed() {
                    row.filing = wire::Filing::Closed as i32;
                    row.filed_at_ms = now;
                }
                row
            })
            .collect())
    }

    /// What follows subagents written exited with their host: subscribers
    /// hear their rows, no push may name them, their sessions are current
    /// and a close waiting on one is answered. Their host counts them again.
    pub(crate) fn subagents_ended_with_host(
        &mut self,
        host: &AgentKey,
        ended: &[AgentRow],
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        for row in ended {
            self.publish_row(&row.agent, out)?;
            self.store.remove_notifications(&row.agent.agent)?;
            self.subagent_settled(&row.agent, out)?;
        }
        self.recount_from(Some(host.clone()), out)
    }

    /// A host's row already says it exited, as a sweep finds an agent that
    /// ended while no daemon was there, and some of its subagents still say
    /// live: they end now, with its cause.
    pub(crate) fn end_subagents_of(
        &mut self,
        host: &AgentRow,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let cause = wire::IncarnationEnd {
            cause: host.exit_cause,
            detail: host.exit_detail.clone(),
        };
        let ended = self.subagents_ending_with(&host.agent, &cause)?;
        if ended.is_empty() {
            return Ok(());
        }
        self.store.put_agents(&ended)?;
        self.subagents_ended_with_host(&host.agent, &ended, out)
    }

    /// Nothing more comes for the subagent: its session is current, and a
    /// close that waited for it to end is answered.
    fn subagent_settled(
        &mut self,
        key: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        if let Some(agent) = AgentId::from_slice(&key.agent)
            && matches!(self.doing(agent), Some(Doing::StoppingSubagent { .. }))
        {
            out.push(Output::CancelTimer {
                id: TimerId::Exit(agent),
            });
            let outcome = self.row_outcome(agent);
            self.finish(agent, outcome, out);
        }
        if self.store.markers().get(key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        self.announce_caught_up(key, out)
    }

    // --- operations ------------------------------------------------------

    /// How a registry operation on a provider subagent goes where it is not
    /// the operation any agent gets: `None` for any other agent, and for
    /// what a subagent takes as any agent does. Its family (its host
    /// closing or being deleted, or exiting) takes it along as usual; a
    /// person, or another host, is refused what the subagent cannot do.
    pub(crate) fn subagent_operation(
        &mut self,
        agent: AgentId,
        call: Caller,
        op: &Op,
        out: &mut Vec<Output>,
    ) -> Option<Result<Began, Refusal>> {
        let row = match self.store.agent(&self.own_key(agent)) {
            Ok(Some(row)) if row.native.is_some() => row,
            _ => return None,
        };
        let refused = |words: &str| Some(Err(Refusal::Subagent(words.to_owned())));
        let family = call == Caller::Family;
        match op {
            Op::Resume(_) => refused(NO_RESUME),
            Op::Delete { .. } if !family => refused(NO_DELETE),
            Op::Kill if !family => refused(NO_KILL),
            // A close a person asks of a running subagent stops it, or is
            // refused: one that hid it would leave it running unseen. Its
            // host closing takes it along without a stop of its own, since
            // the host's stop ends it.
            Op::File(Filing::Closed) if !family && row.lifecycle == Lifecycle::Live as i32 => {
                Some(self.stop_subagent(agent, row, out))
            }
            _ => None,
        }
    }

    /// Files a running subagent closed and asks its host's process to stop
    /// it the provider's way, as the job stop in its chat does; answered
    /// once it has ended. Refused when its provider offers no stop for it,
    /// as its snapshot says.
    fn stop_subagent(
        &mut self,
        agent: AgentId,
        mut row: AgentRow,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        let Some(job) = stoppable_job(&row) else {
            return Err(Refusal::Subagent(NO_STOP.to_owned()));
        };
        let host = row
            .parent
            .as_ref()
            .and_then(|parent| AgentId::from_slice(&parent.agent))
            .filter(|host| self.families.connected(*host))
            .ok_or_else(|| Refusal::Subagent(HOST_AWAY.to_owned()))?;
        // The core mints no randomness; the input's id only has to tell
        // this stop's verdict from any other input's.
        let minted = self.pending.mint();
        let mut input_id = agent.0[..8].to_vec();
        input_id.extend_from_slice(&minted.0.to_be_bytes());
        let (filing, filed_at_ms) = (row.filing, row.filed_at_ms);
        row.filing = wire::Filing::Closed as i32;
        row.filed_at_ms = self.timers.now_ms as i64;
        self.put_own_row(&row, out)?;
        out.push(Output::WriteControl {
            agent: host,
            frame: wire::CtlFrame {
                of: Some(ctl_frame::Of::ChildInput(wire::ChildInput {
                    agent_id: agent.0.to_vec(),
                    input_id,
                    of: Some(wire::child_input::Of::StopJob(wire::StopJob { key: job })),
                })),
            },
        });
        self.arm_after(TimerId::Exit(agent), self.settings.stop_deadline_ms, out);
        Ok(Began::Waiting(Doing::StoppingSubagent {
            filing,
            filed_at_ms,
        }))
    }

    /// A close's stop was not followed by the subagent's end within the
    /// stop deadline: refused, and filed as it was, since a running
    /// subagent filed closed would be hidden while it runs.
    pub(crate) fn subagent_stop_timed_out(
        &mut self,
        agent: AgentId,
        filing: i32,
        filed_at_ms: i64,
        out: &mut Vec<Output>,
    ) {
        let key = self.own_key(agent);
        if let Ok(Some(mut row)) = self.store.agent(&key)
            && row.lifecycle == Lifecycle::Live as i32
        {
            row.filing = filing;
            row.filed_at_ms = filed_at_ms;
            if let Err(error) = self.put_own_row(&row, out) {
                tracing::warn!(%error, "filing a subagent back after its stop failed");
            }
        }
        self.finish(agent, Err(Refusal::StopTimeout), out);
    }
}

/// The key of the subagent's own job in its snapshot, when the provider
/// offers a stop for it: one background job of the subagent kind keyed by
/// the provider's id for it, on a snapshot whose capabilities stop one job.
/// Its host's interpreter lists it there exactly when a stop can reach it,
/// so a close follows what the chat's own Stop offers.
pub fn stoppable_job(row: &AgentRow) -> Option<String> {
    let native = row.native.as_ref()?;
    let snapshot = row.snapshot.as_ref()?;
    if !snapshot
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.stops_job)
    {
        return None;
    }
    snapshot
        .jobs
        .as_ref()?
        .jobs
        .iter()
        .find(|job| {
            job.kind == wire::BackgroundKind::Subagent as i32 && job.key == native.provider_child_id
        })
        .map(|job| job.key.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use store::Sqlite;
    use wire::{AgentSpec, AgentStep, CtlFrame, Item};

    use super::*;
    use crate::families::Ended;
    use crate::payload::{CallError, ClientRequest, MessageRefusal, Response, Status};
    use crate::{CallId, Create, Input, Resume, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const HOST: AgentId = AgentId(*b"host-00000000000");
    const SUB: AgentId = AgentId(*b"sub-agent-000001");
    const SUB_TWO: AgentId = AgentId(*b"sub-agent-000002");
    const CHILD: AgentId = AgentId(*b"child-0000000000");
    const PROVIDER_ID: &str = "task-1";

    struct Rig {
        daemon: Daemon,
        journal: journal::Writer,
        next_call: u64,
        _dir: tempfile::TempDir,
    }

    /// A daemon past its sweep with one live headless Claude agent, HOST,
    /// whose process said Hello.
    fn rig() -> Rig {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let daemon = Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                ..StartOptions::default()
            },
        );
        let journal =
            journal::Writer::open(daemon.journal_dir(HOST), 1 << 20).expect("the journal opens");
        let mut rig = Rig {
            daemon,
            journal,
            next_call: 0,
            _dir: dir,
        };
        rig.step(Input::Swept);
        rig.live(HOST, "lead", None);
        rig
    }

    impl Rig {
        fn step(&mut self, input: Input) -> Vec<Output> {
            let mut out = Vec::new();
            self.daemon.step(input, &mut out);
            out
        }

        fn call(&mut self, request: ClientRequest) -> (CallId, Vec<Output>) {
            self.next_call += 1;
            let call = CallId(self.next_call);
            (call, self.step(Input::ClientCall { call, request }))
        }

        fn live(&mut self, agent: AgentId, name: &str, parent: Option<AgentId>) {
            let (_, out) = self.call(ClientRequest::CreateAgent(Box::new(Create {
                agent,
                cwd: "/src".into(),
                given: Some(name.into()),
                branches: BTreeSet::new(),
                new_workspace: false,
                workspace: None,
                recent: None,
                spec: AgentSpec {
                    agent_id: agent.0.to_vec(),
                    kind: wire::Kind::ClaudeSdk as i32,
                    parent: parent.map(|parent| wire::AgentRef {
                        host_id: OWN.to_vec(),
                        agent_id: parent.0.to_vec(),
                    }),
                    ..AgentSpec::default()
                },
            })));
            assert!(out.contains(&Output::DialAgent { agent }), "{out:?}");
            self.step(Input::AgentHello { agent });
        }

        /// Writes the host's steps and has ingest read them.
        fn host_writes(&mut self, steps: &[wire::Step]) -> Vec<Output> {
            for step in steps {
                self.journal.append(step).expect("a frame is written");
            }
            self.step(Input::Nudge { agent: HOST })
        }

        fn row(&self, agent: AgentId) -> Option<AgentRow> {
            self.daemon
                .store
                .agent(&self.daemon.own_key(agent))
                .unwrap()
        }

        fn key(&self, agent: AgentId) -> AgentKey {
            self.daemon.own_key(agent)
        }

        fn marker(&self, agent: AgentId) -> Option<Marker> {
            self.daemon.store.markers().get(&self.key(agent)).copied()
        }
    }

    fn item(key: &str) -> Item {
        Item {
            key: key.into(),
            text: key.into(),
            at_ms: 1_000,
            ..Item::default()
        }
    }

    fn native_start(title: &str) -> NativeStart {
        NativeStart {
            provider_child_id: PROVIDER_ID.into(),
            call_agent_id: HOST.0.to_vec(),
            call_key: "agent-call".into(),
            title: title.into(),
        }
    }

    /// The host's step that starts `sub` with `items`, beside the host's
    /// own call row that started it.
    fn starts(sub: AgentId, items: &[&str]) -> wire::Step {
        wire::Step {
            items: vec![item("agent-call")],
            agents: vec![AgentStep {
                agent_id: sub.0.to_vec(),
                start: Some(native_start("Look at the logs")),
                items: items.iter().map(|key| item(key)).collect(),
                ..AgentStep::default()
            }],
            ..wire::Step::default()
        }
    }

    fn part(sub: AgentId, items: &[&str]) -> AgentStep {
        AgentStep {
            agent_id: sub.0.to_vec(),
            items: items.iter().map(|key| item(key)).collect(),
            ..AgentStep::default()
        }
    }

    fn ends(sub: AgentId, cause: ExitCause) -> wire::Step {
        wire::Step {
            agents: vec![AgentStep {
                end: Some(wire::IncarnationEnd {
                    cause: cause as i32,
                    detail: None,
                }),
                ..part(sub, &[])
            }],
            ..wire::Step::default()
        }
    }

    fn with_parts(parts: Vec<AgentStep>) -> wire::Step {
        wire::Step {
            agents: parts,
            ..wire::Step::default()
        }
    }

    fn ingested(out: &[Output]) -> &crate::Ingested {
        out.iter()
            .find_map(|output| match output {
                Output::Ingested { report, .. } => Some(report),
                _ => None,
            })
            .expect("the step ingested")
    }

    fn refusal_of(out: &[Output], to: CallId) -> Option<Refusal> {
        out.iter().find_map(|output| match output {
            Output::EndCall {
                call,
                status: Status::Failed(CallError::Registry(refusal)),
            } if *call == to => Some(refusal.clone()),
            _ => None,
        })
    }

    fn answer_of(out: &[Output], to: CallId) -> Option<wire::Agent> {
        out.iter().find_map(|output| match output {
            Output::Answer {
                call,
                response: Response::Agent(agent),
            } if *call == to => Some(*agent.clone()),
            _ => None,
        })
    }

    /// The control frames written to `agent`.
    fn frames_to(out: &[Output], agent: AgentId) -> Vec<&CtlFrame> {
        out.iter()
            .filter_map(|output| match output {
                Output::WriteControl { agent: to, frame } if *to == agent => Some(frame),
                _ => None,
            })
            .collect()
    }

    fn item_order(rig: &Rig, agent: AgentId, key: &str) -> Option<(u64, u64)> {
        let item = rig.daemon.store.get(&rig.key(agent), key).unwrap()?;
        Some((item.order, item.revision))
    }

    #[test]
    fn a_start_makes_the_subagents_row_before_its_items_and_each_part_is_filed_under_its_own_agent()
    {
        let mut rig = rig();
        let (inventory, _) = rig.call(ClientRequest::SubscribeInventory);
        let out = rig.host_writes(&[starts(SUB, &["s1", "s2"])]);

        let row = rig.row(SUB).expect("the start made the subagent's row");
        assert_eq!(
            row.native
                .as_ref()
                .map(|native| native.provider_child_id.as_str()),
            Some(PROVIDER_ID)
        );
        assert_eq!(row.parent, Some(rig.key(HOST)));
        assert_eq!(row.kind, wire::Kind::ClaudeSdk as i32, "its host's kind");
        assert_eq!(row.cwd, "/src", "its host's folder");
        assert_eq!(row.name, "sub-7375622d", "named for its id");
        assert_eq!(row.title.as_deref(), Some("Look at the logs"));
        assert_eq!(row.lifecycle, Lifecycle::Live as i32);
        assert_eq!(row.phase(), wire::Phase::Working);
        assert!(!row.owes_report);
        // Orders and revisions count per agent.
        assert_eq!(item_order(&rig, SUB, "s1"), Some((1, 1)));
        assert_eq!(item_order(&rig, SUB, "s2"), Some((2, 2)));
        assert_eq!(item_order(&rig, HOST, "agent-call"), Some((1, 1)));
        assert_eq!(item_order(&rig, HOST, "s1"), None, "not the host's");
        // The inventory heard of the row; the host was not told of a child
        // it would wait on for a report.
        let added = out.iter().any(|output| match output {
            Output::Answer {
                call,
                response: Response::Inventory(event),
            } if *call == inventory => matches!(
                &event.of,
                Some(wire::inventory_event::Of::Agent(agent))
                    if agent.agent_id == SUB.0 && agent.native.is_some()
            ),
            _ => false,
        });
        assert!(added, "{out:?}");
        assert!(
            frames_to(&out, HOST)
                .iter()
                .all(|frame| !matches!(frame.of, Some(ctl_frame::Of::Children(_)))),
            "{out:?}"
        );

        // A second subagent whose id starts alike gets the next free name.
        let mut twin = SUB_TWO;
        twin.0[..4].copy_from_slice(&SUB.0[..4]);
        rig.host_writes(&[starts(twin, &["t1"])]);
        assert_eq!(rig.row(twin).unwrap().name, "sub-7375622d-2");
    }

    fn dumped(out: &[Output]) -> Vec<&'static str> {
        out.iter()
            .filter_map(|output| match output {
                Output::Dump {
                    reason: crate::DumpReason::Violation(violation),
                } => Some(violation.invariant.id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_part_naming_no_subagent_of_the_host_is_skipped_and_counted() {
        let mut rig = rig();
        rig.live(CHILD, "helper", Some(HOST));
        // No row, and no start to make one: a resumed provider may report
        // the end of a subagent this store never held, so this is no fault.
        let out = rig.host_writes(&[with_parts(vec![part(SUB, &["lost"])])]);
        assert_eq!(ingested(&out).skipped, 1, "{out:?}");
        assert!(rig.row(SUB).is_none());
        assert_eq!(dumped(&out), Vec::<&str>::new());

        // An agent amux started, which the host does not run: the
        // interpreter that wrote it is at fault.
        let out = rig.host_writes(&[with_parts(vec![part(CHILD, &["not-its-own"])])]);
        assert_eq!(ingested(&out).skipped, 1, "{out:?}");
        assert!(
            rig.daemon
                .store
                .get(&rig.key(CHILD), "not-its-own")
                .unwrap()
                .is_none()
        );
        assert_eq!(dumped(&out), vec![crate::PART_OF_ANOTHER_AGENT.id]);
    }

    #[test]
    fn items_for_a_subagent_after_its_end_are_kept_and_named_in_a_dump() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"]), ends(SUB, ExitCause::Finished)]);
        let out = rig.host_writes(&[with_parts(vec![part(SUB, &["late"])])]);
        assert_eq!(dumped(&out), vec![crate::SUBAGENT_AFTER_ITS_END.id]);
        assert!(
            item_order(&rig, SUB, "late").is_some(),
            "nothing the provider said is lost"
        );
    }

    /// A subagent calling amux's spawn calls it on its host's tool server,
    /// so the new agent is the host's child: a create naming the host.
    #[test]
    fn a_spawn_while_the_host_runs_subagents_is_the_hosts_child_and_the_host_hears_only_of_it() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        let (_, out) = rig.call(ClientRequest::CreateAgent(Box::new(Create {
            agent: CHILD,
            cwd: "/src".into(),
            given: Some("helper".into()),
            branches: BTreeSet::new(),
            new_workspace: false,
            workspace: None,
            recent: None,
            spec: AgentSpec {
                agent_id: CHILD.0.to_vec(),
                kind: wire::Kind::ClaudeSdk as i32,
                parent: Some(wire::AgentRef {
                    host_id: OWN.to_vec(),
                    agent_id: HOST.0.to_vec(),
                }),
                ..AgentSpec::default()
            },
        })));
        assert_eq!(rig.row(CHILD).unwrap().parent, Some(rig.key(HOST)));
        let told: Vec<Vec<Vec<u8>>> = frames_to(&out, HOST)
            .into_iter()
            .filter_map(|frame| match &frame.of {
                Some(ctl_frame::Of::Children(children)) => Some(
                    children
                        .children
                        .iter()
                        .map(|child| child.agent_id.clone())
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(
            told,
            vec![vec![CHILD.0.to_vec()]],
            "the subagent sends no report to wait on"
        );
        assert_eq!(
            rig.row(HOST).unwrap().descendants,
            2,
            "both count under the host"
        );
    }

    #[test]
    fn a_driver_reporting_a_subagents_process_ended_changes_nothing() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        rig.step(Input::ControlClosed {
            agent: SUB,
            ended: Ended::Away,
        });
        rig.step(Input::Adopted { agent: SUB });
        assert_eq!(rig.row(SUB).unwrap().lifecycle, Lifecycle::Live as i32);
        assert!(!rig.daemon.families().processes().any(|agent| agent == SUB));
    }

    #[test]
    fn a_subagent_ends_as_any_agent_does_and_never_owes_its_parent_a_report() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        // Working, as a snapshot says: an agent with a parent would owe one.
        let working = wire::Snapshot {
            kind: wire::Kind::ClaudeSdk as i32,
            status: wire::AgentStatus::in_phase(wire::Phase::Working),
            ..wire::Snapshot::default()
        };
        rig.host_writes(&[with_parts(vec![AgentStep {
            snapshot: Some(working),
            ..part(SUB, &["s2"])
        }])]);
        assert!(!rig.row(SUB).unwrap().owes_report);

        let out = rig.host_writes(&[ends(SUB, ExitCause::Finished)]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::Finished as i32)
        );
        assert!(row.is_closed(), "a finished subagent is filed closed");
        assert_eq!(
            rig.daemon.store.deliveries().unwrap(),
            Vec::new(),
            "{out:?}"
        );
        assert_eq!(
            rig.marker(SUB),
            Some(Marker::CaughtUp),
            "its end makes it current"
        );
    }

    #[test]
    fn a_start_on_a_subagent_that_ended_continues_it_and_an_end_for_one_already_ended_changes_nothing()
     {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"]), ends(SUB, ExitCause::Finished)]);
        assert!(rig.row(SUB).unwrap().is_closed());

        // Its parent's model continues it: live, open, its chat carrying on.
        rig.host_writes(&[starts(SUB, &["s2"])]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(row.lifecycle, Lifecycle::Live as i32);
        assert_eq!(row.exit_cause, ExitCause::Unspecified as i32);
        assert!(!row.is_closed());
        assert_eq!(row.incarnation, 1, "a continuation is no new incarnation");
        assert_eq!(item_order(&rig, SUB, "s2").map(|(order, _)| order), Some(2));

        // Stopped, then reported stopped again, as a resumed Claude reports
        // a subagent the last process left: the first end stands.
        rig.host_writes(&[ends(SUB, ExitCause::Stopped)]);
        rig.host_writes(&[ends(SUB, ExitCause::Finished)]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::Stopped as i32)
        );

        // A continuation and its end in one batch leave it ended by that end.
        rig.host_writes(&[starts(SUB, &["s3"]), ends(SUB, ExitCause::Killed)]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::Killed as i32)
        );
    }

    /// A catch-up batch can hold a subagent's end and the start of its
    /// continuation: it is running again, so it reads live and open.
    #[test]
    fn an_end_and_a_continuation_in_one_batch_leave_the_subagent_live_and_open() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);

        rig.host_writes(&[ends(SUB, ExitCause::Finished), starts(SUB, &["s2"])]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Live as i32, ExitCause::Unspecified as i32)
        );
        assert!(!row.is_closed(), "a running subagent is not filed closed");

        // Ended, continued and ended again: the last end stands.
        rig.host_writes(&[
            ends(SUB, ExitCause::Finished),
            starts(SUB, &["s3"]),
            ends(SUB, ExitCause::Stopped),
        ]);
        let row = rig.row(SUB).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::Stopped as i32)
        );
    }

    #[test]
    fn a_hosts_process_ending_ends_its_live_subagents_with_its_cause() {
        let mut rig = rig();
        rig.host_writes(&[
            starts(SUB, &["s1"]),
            starts(SUB_TWO, &["t1"]),
            ends(SUB_TWO, ExitCause::Finished),
        ]);
        // The journal stops without saying why: the daemon's cause is the
        // host's and its live subagent's.
        rig.step(Input::ControlClosed {
            agent: HOST,
            ended: Ended::Exited,
        });
        let host = rig.row(HOST).unwrap();
        let sub = rig.row(SUB).unwrap();
        assert_eq!(host.exit_cause, ExitCause::EndedUnexpectedly as i32);
        assert_eq!(
            (sub.lifecycle, sub.exit_cause),
            (
                Lifecycle::Exited as i32,
                ExitCause::EndedUnexpectedly as i32
            )
        );
        assert_eq!(rig.marker(SUB), Some(Marker::CaughtUp));
        assert_eq!(
            rig.row(SUB_TWO).unwrap().exit_cause,
            ExitCause::Finished as i32,
            "one that had ended keeps its own end"
        );
        assert_eq!(rig.row(HOST).unwrap().descendants, 0);
    }

    #[test]
    fn a_hosts_own_end_is_its_live_subagents_end() {
        let mut rig = rig();
        rig.host_writes(&[
            starts(SUB, &["s1"]),
            wire::Step {
                incarnation_end: Some(wire::IncarnationEnd {
                    cause: ExitCause::Stopped as i32,
                    detail: None,
                }),
                ..wire::Step::default()
            },
        ]);
        rig.step(Input::ControlClosed {
            agent: HOST,
            ended: Ended::Exited,
        });
        assert_eq!(rig.row(SUB).unwrap().exit_cause, ExitCause::Stopped as i32);
    }

    /// A host killed while its subagent ran leaves the subagent's steps
    /// open. Its exit ended the subagent's row; the resumed process writes
    /// the previous run's missing last step, closing those steps with the
    /// subagent's end. That is no fault of the interpreter's.
    #[test]
    fn a_resumed_host_closing_the_steps_its_killed_process_left_raises_no_dump() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        rig.step(Input::ControlClosed {
            agent: HOST,
            ended: Ended::Exited,
        });
        assert_eq!(rig.row(SUB).unwrap().lifecycle, Lifecycle::Exited as i32);

        let (_, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: HOST,
            bootstrap: None,
            if_suspended: false,
        })));
        let job = |out: &[Output]| {
            out.iter()
                .find_map(|output| match output {
                    Output::Job { id, .. } => Some(*id),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("a job: {out:?}"))
        };
        let out = rig.step(Input::JobDone {
            job: job(&out),
            result: crate::payload::JobResult::Done,
        });
        let out = rig.step(Input::JobDone {
            job: job(&out),
            result: crate::payload::JobResult::Spec(Some(Box::new(AgentSpec {
                agent_id: HOST.0.to_vec(),
                kind: wire::Kind::ClaudeSdk as i32,
                incarnation: 2,
                ..AgentSpec::default()
            }))),
        });
        assert!(out.contains(&Output::DialAgent { agent: HOST }), "{out:?}");
        rig.step(Input::AgentHello { agent: HOST });
        let out = rig.host_writes(&[with_parts(vec![AgentStep {
            end: Some(wire::IncarnationEnd {
                cause: ExitCause::Stopped as i32,
                detail: None,
            }),
            ..part(SUB, &["s1"])
        }])]);
        assert_eq!(dumped(&out), Vec::<&str>::new(), "{out:?}");
        assert_eq!(
            rig.row(SUB).unwrap().exit_cause,
            ExitCause::EndedUnexpectedly as i32,
            "the end its host's exit gave it stands"
        );
    }

    /// Catch-up as a second client opening the subagent's chat meets it.
    fn opened_caught_up(rig: &mut Rig, agent: AgentId) -> bool {
        let (call, out) = rig.call(ClientRequest::SubscribeSession {
            agent_id: agent.0.to_vec(),
            tail: 10,
        });
        out.iter().any(|output| {
            matches!(output, Output::Answer {
                call: to,
                response: Response::Session(event),
            } if *to == call && matches!(event.of, Some(wire::session_event::Of::CaughtUp(_))))
        })
    }

    #[test]
    fn a_subagent_reads_as_current_when_its_hosts_journal_does() {
        let mut rig = rig();
        // The host's Hello made CaughtUp due: the first read to the end
        // sets the host's and its live subagent's.
        rig.host_writes(&[starts(SUB, &["s1"])]);
        assert_eq!(rig.marker(HOST), Some(Marker::CaughtUp));
        assert_eq!(rig.marker(SUB), Some(Marker::CaughtUp));
        assert!(
            opened_caught_up(&mut rig, SUB),
            "a second client's chat opens current"
        );

        // One started once the host is current is current from its start.
        rig.host_writes(&[starts(SUB_TWO, &["t1"])]);
        assert_eq!(rig.marker(SUB_TWO), Some(Marker::CaughtUp));

        // A daemon that starts again: the sweep's end marks an ended one.
        rig.host_writes(&[ends(SUB_TWO, ExitCause::Finished)]);
        rig.daemon.store.set_marker(&rig.key(SUB_TWO), None);
        rig.step(Input::Swept);
        assert_eq!(rig.marker(SUB_TWO), Some(Marker::CaughtUp));
    }

    /// The snapshot of a subagent whose provider offers a stop for it.
    fn stoppable() -> wire::Snapshot {
        wire::Snapshot {
            kind: wire::Kind::ClaudeSdk as i32,
            status: wire::AgentStatus::in_phase(wire::Phase::Working),
            capabilities: Some(Box::new(wire::AgentCapabilities::claude_sdk())),
            jobs: Some(wire::BackgroundJobs {
                known: true,
                jobs: vec![wire::BackgroundJob {
                    key: PROVIDER_ID.into(),
                    kind: wire::BackgroundKind::Subagent as i32,
                    ..wire::BackgroundJob::default()
                }],
            }),
            ..wire::Snapshot::default()
        }
    }

    fn close(agent: AgentId) -> ClientRequest {
        ClientRequest::SetFiling {
            agent,
            filing: Filing::Closed,
        }
    }

    #[test]
    fn closing_a_running_subagent_stops_its_job_through_its_host_and_answers_at_its_end() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        // No stop offered: refused, and left open.
        let (call, out) = rig.call(close(SUB));
        assert_eq!(
            refusal_of(&out, call),
            Some(Refusal::Subagent(NO_STOP.into()))
        );
        assert!(!rig.row(SUB).unwrap().is_closed());

        rig.host_writes(&[with_parts(vec![AgentStep {
            snapshot: Some(stoppable()),
            ..part(SUB, &[])
        }])]);
        let (call, out) = rig.call(close(SUB));
        let stops: Vec<_> = frames_to(&out, HOST)
            .into_iter()
            .filter_map(|frame| match &frame.of {
                Some(ctl_frame::Of::ChildInput(child)) => Some(child.clone()),
                _ => None,
            })
            .collect();
        let [stop] = &stops[..] else {
            panic!("one stop to the host: {out:?}");
        };
        assert_eq!(stop.agent_id, SUB.0);
        assert_eq!(
            stop.of,
            Some(wire::child_input::Of::StopJob(wire::StopJob {
                key: PROVIDER_ID.into()
            }))
        );
        assert!(answer_of(&out, call).is_none(), "answered once it ends");
        assert!(rig.row(SUB).unwrap().is_closed());

        let out = rig.host_writes(&[ends(SUB, ExitCause::Stopped)]);
        let row = answer_of(&out, call).expect("its end answers the close");
        assert_eq!(
            (row.lifecycle(), row.filing()),
            (Lifecycle::Exited, wire::Filing::Closed)
        );
    }

    #[test]
    fn a_close_whose_stop_is_not_followed_by_an_end_is_refused_and_files_it_back() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        rig.host_writes(&[with_parts(vec![AgentStep {
            snapshot: Some(stoppable()),
            ..part(SUB, &[])
        }])]);
        let (call, _) = rig.call(close(SUB));
        let out = rig.step(Input::Timer {
            id: TimerId::Exit(SUB),
        });
        assert_eq!(refusal_of(&out, call), Some(Refusal::StopTimeout));
        assert!(
            !rig.row(SUB).unwrap().is_closed(),
            "not hidden while it runs"
        );
    }

    #[test]
    fn a_subagent_goes_with_its_host_and_takes_only_what_needs_no_process_of_its_own() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        let resume = ClientRequest::ResumeAgent(Box::new(Resume {
            agent: SUB,
            bootstrap: None,
            if_suspended: false,
        }));
        for (request, refused) in [
            (resume, Refusal::Subagent(NO_RESUME.into())),
            (
                ClientRequest::DeleteAgent {
                    agent: SUB,
                    discard: false,
                },
                Refusal::Subagent(NO_DELETE.into()),
            ),
            (
                ClientRequest::KillAgent { agent: SUB },
                Refusal::Subagent(NO_KILL.into()),
            ),
            (
                ClientRequest::SetFiling {
                    agent: SUB,
                    filing: Filing::Pinned,
                },
                Refusal::HasParent,
            ),
        ] {
            let (call, out) = rig.call(request);
            assert_eq!(refusal_of(&out, call), Some(refused), "{out:?}");
        }
        assert!(rig.row(SUB).is_some());

        let (call, out) = rig.call(ClientRequest::RenameAgent {
            agent: SUB,
            name: "log-reader".into(),
        });
        assert_eq!(
            answer_of(&out, call).map(|row| row.name),
            Some("log-reader".into())
        );
        assert!(
            frames_to(&out, HOST).is_empty(),
            "nothing to tell a process"
        );
        // A rename is the person's name, as for any agent; its snapshot's
        // title does not take it back.
        rig.host_writes(&[with_parts(vec![AgentStep {
            snapshot: Some(stoppable()),
            ..part(SUB, &[])
        }])]);
        assert_eq!(rig.row(SUB).unwrap().title, None);

        // Ended, a close files it as any agent's.
        rig.host_writes(&[ends(SUB, ExitCause::Stopped)]);
        let (call, out) = rig.call(close(SUB));
        assert_eq!(
            answer_of(&out, call).map(|row| row.filing()),
            Some(wire::Filing::Closed)
        );

        // A message is refused: only its parent's model reaches it.
        let (call, out) = rig.call(ClientRequest::SendMessage {
            envelope: Box::new(wire::Envelope {
                id: b"message-00000001".to_vec(),
                to: Some(wire::AgentRef {
                    host_id: OWN.to_vec(),
                    agent_id: SUB.0.to_vec(),
                }),
                ..wire::Envelope::default()
            }),
        });
        let refused = out.iter().find_map(|output| match output {
            Output::EndCall {
                call: to,
                status: Status::Failed(CallError::Message(MessageRefusal::Rejected(rejected))),
            } if *to == call => Some(rejected.refusal()),
            _ => None,
        });
        assert_eq!(refused, Some(wire::Refusal::Unsupported), "{out:?}");
    }

    #[test]
    fn a_hosts_close_files_its_subagents_and_its_delete_takes_their_rows_and_no_directory() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"]), starts(SUB_TWO, &["t1"])]);
        rig.host_writes(&[ends(SUB_TWO, ExitCause::Stopped)]);
        rig.call(close(HOST));
        assert!(rig.row(SUB).unwrap().is_closed(), "closed with its host");
        assert!(rig.row(SUB_TWO).unwrap().is_closed());
        rig.step(Input::ControlClosed {
            agent: HOST,
            ended: Ended::Exited,
        });
        assert_eq!(
            rig.row(SUB).unwrap().exit_cause,
            ExitCause::EndedUnexpectedly as i32
        );

        let (_, out) = rig.call(ClientRequest::DeleteAgent {
            agent: HOST,
            discard: false,
        });
        assert!(
            rig.row(SUB).is_none() && rig.row(SUB_TWO).is_none(),
            "{out:?}"
        );
        let removed: Vec<_> = out
            .iter()
            .filter_map(|output| match output {
                Output::Job {
                    op: crate::Job::RemoveDir { path },
                    ..
                } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            removed,
            vec![rig.daemon.agent_dir(HOST)],
            "only the host has a directory"
        );
    }

    #[test]
    fn a_subagents_files_are_its_hosts() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        let hash = vec![7u8; 32];
        let (call, out) = rig.call(ClientRequest::GetBlob(wire::GetBlobRequest {
            agent_id: SUB.0.to_vec(),
            hash: hash.clone(),
        }));
        let sent = out.iter().find_map(|output| match output {
            Output::SendFile { call: to, path, .. } if *to == call => Some(path.clone()),
            _ => None,
        });
        assert_eq!(
            sent,
            Some(
                rig.daemon
                    .agent_dir(HOST)
                    .join(crate::blobs::BLOBS)
                    .join(crate::hex(&hash))
            )
        );
    }

    #[test]
    fn a_subagents_ask_is_pushed_under_its_title() {
        let mut rig = rig();
        rig.host_writes(&[starts(SUB, &["s1"])]);
        let asking = wire::Snapshot {
            status: wire::AgentStatus::in_phase(wire::Phase::NeedsYou),
            notice: Some(Box::new(wire::Notice {
                of: Some(wire::notice::Of::Ask(wire::AskNotice {
                    key: "ask-1".into(),
                    ..wire::AskNotice::default()
                })),
            })),
            ..wire::Snapshot::default()
        };
        rig.host_writes(&[with_parts(vec![AgentStep {
            snapshot: Some(asking),
            ..part(SUB, &[])
        }])]);
        let names: Vec<_> = rig
            .daemon
            .store
            .notifications()
            .unwrap()
            .into_iter()
            .map(|notification| (notification.agent_id, notification.name))
            .collect();
        assert_eq!(names, vec![(SUB.0.to_vec(), "Look at the logs".to_owned())]);
    }
}
