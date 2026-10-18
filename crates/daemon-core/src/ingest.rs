//! Ingest: an agent's journal read into the store, a bounded batch per
//! step, and each record answered to its subscribers after the commit that
//! made it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::{AgentKey, CommitClock, Marker, Record};
use wire::{CaughtUp, session_event};

use crate::Daemon;
use crate::ids::{AgentId, TimerId};
use crate::io::{IngestEnd, Ingested, Output, Wake};
use crate::serve::event;

/// The most journal frames one step commits. A step delays every other
/// input for its duration: at the measured ingest cost of about 23 µs a
/// frame, a full batch takes about 6 ms, which is the most a subscribe, an
/// input or a spawn waits behind a draining agent. Larger batches commit a
/// backlog slightly faster per frame and hold everything else off for
/// longer; smaller ones pay a transaction per fewer frames.
pub const INGEST_BATCH: usize = 256;
/// Fully ingested journal segments kept below the cursor, for dumps.
pub const KEPT_SEGMENTS: usize = 2;
/// Where an agent's directory keeps its journal.
const JOURNAL: &str = "journal";

/// What ingest remembers between steps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journals {
    /// Agents that said Hello since ingest last read their journal to its
    /// end: the next time it does, CaughtUp is answered, once per Hello.
    caught_up_due: BTreeSet<AgentId>,
    /// Agents whose next batch waits on an armed timer.
    more: BTreeSet<AgentId>,
}

#[derive(Debug)]
enum IngestError {
    Store(store::StoreError),
    Journal(std::io::Error),
}

impl From<store::StoreError> for IngestError {
    fn from(error: store::StoreError) -> Self {
        IngestError::Store(error)
    }
}

impl From<std::io::Error> for IngestError {
    fn from(error: std::io::Error) -> Self {
        IngestError::Journal(error)
    }
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::Store(error) => write!(f, "the store: {error}"),
            IngestError::Journal(error) => write!(f, "the journal: {error}"),
        }
    }
}

impl Daemon {
    /// The agent's process said Hello: CaughtUp is due once its journal is
    /// next read to its end.
    pub(crate) fn hello(&mut self, agent: AgentId) {
        self.journals.caught_up_due.insert(agent);
    }

    /// Reads one batch of the agent's journal from its row's cursor and
    /// commits it in one transaction, then answers each record to the
    /// agent's streams. A journal holding more than a batch arms a timer
    /// for the next, so other inputs are stepped between batches.
    /// While a checkpoint is taken the batch waits for it, and is read
    /// once it is done.
    pub(crate) fn ingest(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if self.hold_ingest(agent) {
            return;
        }
        let report = self
            .ingest_batch(agent, out)
            .unwrap_or_else(|error| Ingested {
                frames: 0,
                turn_ends: 0,
                kind: 0,
                end: IngestEnd::Failed(error.to_string()),
            });
        if report.end == IngestEnd::More {
            self.journals.more.insert(agent);
            out.push(Output::ArmTimer {
                id: TimerId::Ingest(agent),
                at_ms: self.timers.now_ms,
            });
        } else if self.journals.more.remove(&agent) {
            out.push(Output::CancelTimer {
                id: TimerId::Ingest(agent),
            });
        }
        if report.end != IngestEnd::More {
            self.journal_read(agent, out);
        }
        out.push(Output::Ingested { agent, report });
    }

    /// The timer armed for the agent's next batch fired.
    pub(crate) fn ingest_again(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        self.journals.more.remove(&agent);
        self.ingest(agent, out);
    }

    fn ingest_batch(
        &mut self,
        agent: AgentId,
        out: &mut Vec<Output>,
    ) -> Result<Ingested, IngestError> {
        let key = AgentKey::new(self.store.own_host(), agent.0);
        let dir = self.journal_dir(agent);
        let cursor = self.store.cursor(&key)?;
        let mut reader = journal::Reader::new(&dir, cursor);
        let mut batch = reader.read_up_to(INGEST_BATCH)?;
        if batch.frames.is_empty() {
            // Nothing past the cursor may mean the cursor is past the
            // journal: the store reached the drive after committing frames
            // the unsynced journal then lost to a power cut. The agent
            // re-derives from the last whole frame, so that is where the
            // cursor goes; revisions keep counting up, so what it writes
            // again is new to every subscriber.
            let end = journal::end(&dir)?;
            if end < cursor {
                tracing::warn!(
                    ?agent,
                    cursor,
                    end,
                    "the journal ends below the cursor; rewinding"
                );
                self.store.rewind_cursor(&key, end)?;
                self.rewound(agent, end);
                reader.seek(end);
                batch = reader.read_up_to(INGEST_BATCH)?;
            }
        }
        // A torn frame in the newest segment is a write still under way:
        // the journal has not ended yet, and its Nudge will come.
        let at_end =
            !batch.more && !matches!(batch.torn, Some(journal::Torn { skipped: false, .. }));
        let mut report = Ingested {
            frames: batch.frames.len() as u32,
            turn_ends: 0,
            kind: 0,
            end: if batch.more {
                IngestEnd::More
            } else {
                IngestEnd::Read
            },
        };
        if !batch.frames.is_empty() {
            let clock = CommitClock {
                now_ms: self.timers.now_ms as i64,
                notify_delay_ms: self.settings.notify_delay_ms,
            };
            let committed = self.store.commit(&key, &batch.frames, clock)?;
            self.committed(agent, out);
            report.turn_ends = batch
                .frames
                .iter()
                .filter(|(_, step)| step.turn_end.is_some())
                .count() as u32;
            if report.turn_ends > 0 {
                out.push(Output::ArmTimer {
                    id: TimerId::Deliveries,
                    at_ms: self.timers.now_ms,
                });
                report.kind = self.store.agent(&key)?.map_or(0, |row| row.kind);
            }
            // Only now, with the transaction committed: a subscriber never
            // sees a revision the store could not serve it.
            let mut envelope = false;
            for record in committed.records {
                let of = match record {
                    Record::Item(item) => session_event::Of::Item(item),
                    Record::Append(append) => session_event::Of::Append(append),
                    Record::Snapshot(snapshot) => {
                        envelope = true;
                        session_event::Of::Snapshot(snapshot)
                    }
                };
                self.publish_session(&key, Arc::new(event(of)), out);
            }
            if envelope {
                // Phase, working_on, since-when and git ride the inventory row.
                self.publish_row(&key, out)?;
                out.push(Output::Wake(Wake::Notifications));
            }
        }
        if at_end && self.journals.caught_up_due.remove(&agent) {
            self.announce_caught_up(&key, out)?;
        }
        Ok(report)
    }

    /// Sets the agent's CaughtUp marker and answers it to the agent's
    /// streams: a stream opened since reads it with its cut.
    pub(crate) fn announce_caught_up(
        &mut self,
        key: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), store::StoreError> {
        let Some(row) = self.store.agent(key)? else {
            return Ok(());
        };
        self.store.set_marker(key, Some(Marker::CaughtUp));
        let caught_up = session_event::Of::CaughtUp(CaughtUp {
            revision: row.next_revision.saturating_sub(1),
        });
        self.publish_session(key, Arc::new(event(caught_up)), out);
        Ok(())
    }

    /// The agent's journal, where this core reads it.
    pub fn journal_dir(&self, agent: AgentId) -> PathBuf {
        journal_dir(&self.options.agents, agent)
    }
}

/// An agent's journal under `agents`: in the agent's directory.
pub fn journal_dir(agents: &Path, agent: AgentId) -> PathBuf {
    agent_dir(agents, agent).join(JOURNAL)
}

/// An agent's directory under `agents`, named by its id in the hyphenated
/// form.
pub fn agent_dir(agents: &Path, agent: AgentId) -> PathBuf {
    let mut name = String::with_capacity(36);
    for (at, byte) in agent.0.iter().enumerate() {
        if matches!(at, 4 | 6 | 8 | 10) {
            name.push('-');
        }
        name.push_str(&format!("{byte:02x}"));
    }
    agents.join(name)
}

#[cfg(test)]
mod tests {
    use store::{AgentRow, Sqlite};
    use wire::{Item, Step};

    use super::*;
    use crate::payload::{ClientRequest, Response};
    use crate::{CallId, Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const AGENT: AgentId = AgentId(*b"agent-0000000000");

    struct Rig {
        daemon: Daemon,
        journal: journal::synthetic::SyntheticWriter,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        rig_with(1 << 30)
    }

    fn rig_with(segment_bytes: u64) -> Rig {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
        let row = AgentRow::new(AgentKey::new(OWN, AGENT.0), wire::Kind::Codex, "/src");
        store.put_agent(&row).expect("the store takes its agent");
        let daemon = Daemon::new(
            store,
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                ..StartOptions::default()
            },
        );
        let journal =
            journal::synthetic::SyntheticWriter::open(daemon.journal_dir(AGENT), segment_bytes)
                .expect("the journal opens");
        Rig {
            daemon,
            journal,
            _dir: dir,
        }
    }

    fn item(n: usize) -> Step {
        Step {
            items: vec![Item {
                key: format!("k{n:03}"),
                text: "x".into(),
                at_ms: 1_000,
                ..Item::default()
            }],
            ..Step::default()
        }
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    /// The live answers to `call`, as the keys or markers they carry.
    fn answered(out: &[Output], to: CallId) -> Vec<String> {
        out.iter()
            .filter_map(|output| match output {
                Output::Answer {
                    call,
                    response: Response::Session(event),
                } if *call == to => Some(match &event.of {
                    Some(session_event::Of::Item(item)) => item.key.clone(),
                    Some(session_event::Of::CaughtUp(c)) => format!("caught_up r{}", c.revision),
                    other => format!("{other:?}"),
                }),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_backlog_is_read_a_batch_a_step_answered_after_its_commit_and_caught_up_once_per_hello() {
        let mut rig = rig();
        for n in 0..INGEST_BATCH + 10 {
            rig.journal
                .append(&item(n))
                .expect("the journal takes a step");
        }
        let subscribe = ClientRequest::Subscribe {
            agent_id: AGENT.0.to_vec(),
            tail: 10,
        };
        let call = CallId(1);
        step(
            &mut rig.daemon,
            Input::ClientCall {
                call,
                request: subscribe,
            },
        );
        step(
            &mut rig.daemon,
            Input::AgentHello {
                agent: AGENT,
                hello: Default::default(),
            },
        );

        let first = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        let keys = answered(&first, call);
        assert_eq!(keys.len(), INGEST_BATCH, "one batch, each record once");
        assert_eq!(keys.first().map(String::as_str), Some("k000"));
        let key = AgentKey::new(OWN, AGENT.0);
        for answered in &keys {
            assert!(
                rig.daemon.store.get(&key, answered).unwrap().is_some(),
                "{answered} is answered only once the store holds it"
            );
        }
        assert!(first.contains(&Output::ArmTimer {
            id: TimerId::Ingest(AGENT),
            at_ms: 0,
        }));
        assert!(first.contains(&Output::Ingested {
            agent: AGENT,
            report: Ingested {
                frames: INGEST_BATCH as u32,
                turn_ends: 0,
                kind: 0,
                end: IngestEnd::More,
            },
        }));

        let rest = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Ingest(AGENT),
            },
        );
        let mut tail: Vec<String> = (INGEST_BATCH..INGEST_BATCH + 10)
            .map(|n| format!("k{n:03}"))
            .collect();
        tail.push(format!("caught_up r{}", INGEST_BATCH + 10));
        assert_eq!(answered(&rest, call), tail, "the rest, then CaughtUp");
        assert!(
            rest.iter()
                .all(|output| !matches!(output, Output::ArmTimer { .. }))
        );

        rig.journal
            .append(&item(999))
            .expect("the journal takes a step");
        let again = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(
            answered(&again, call),
            vec!["k999"],
            "CaughtUp is answered once per Hello"
        );
    }

    #[test]
    fn a_turn_end_arms_the_deliveries_timer_and_its_firing_wakes_the_drain() {
        let mut rig = rig();
        let mut ended = item(0);
        ended.turn_end = Some(wire::TurnEnd::default());
        rig.journal
            .append(&ended)
            .expect("the journal takes a step");
        let out = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert!(out.contains(&Output::ArmTimer {
            id: TimerId::Deliveries,
            at_ms: 0,
        }));
        assert_eq!(
            step(
                &mut rig.daemon,
                Input::Timer {
                    id: TimerId::Deliveries,
                },
            ),
            vec![Output::Wake(Wake::Deliveries)]
        );
    }

    #[test]
    fn a_failed_commit_answers_nothing_and_says_why() {
        let mut rig = rig();
        rig.journal
            .append(&item(0))
            .expect("the journal takes a step");
        let call = CallId(1);
        step(
            &mut rig.daemon,
            Input::ClientCall {
                call,
                request: ClientRequest::Subscribe {
                    agent_id: AGENT.0.to_vec(),
                    tail: 10,
                },
            },
        );
        rig.daemon
            .store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER cut BEFORE INSERT ON agents \
                 BEGIN SELECT RAISE(ABORT, 'cut'); END;",
            )
            .expect("the trigger installs");
        let out = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert!(answered(&out, call).is_empty());
        assert!(matches!(
            out.last(),
            Some(Output::Ingested {
                report: Ingested {
                    end: IngestEnd::Failed(_),
                    ..
                },
                ..
            })
        ));
    }

    /// The dumps a step asked for.
    fn dumps_in(out: &[Output]) -> Vec<&crate::DumpReason> {
        out.iter()
            .filter_map(|output| match output {
                Output::Dump { reason } => Some(reason),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_step_over_its_budget_asks_for_one_dump_saying_how_long_it_took() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
        let row = AgentRow::new(AgentKey::new(OWN, AGENT.0), wire::Kind::Codex, "/src");
        store.put_agent(&row).expect("the store takes its agent");
        let budget = crate::StepBudget { micros: 0 };
        let mut daemon = Daemon::new(
            store,
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                budget,
            },
        );
        let mut journal =
            journal::synthetic::SyntheticWriter::open(daemon.journal_dir(AGENT), 1 << 30)
                .expect("the journal opens");
        journal.append(&item(0)).expect("the journal takes a step");

        let out = step(&mut daemon, Input::Nudge { agent: AGENT });
        let dumps = dumps_in(&out);
        let [crate::DumpReason::OverBudget { micros, budget: of }] = dumps[..] else {
            panic!("one dump for the step that ran over: {dumps:?}");
        };
        println!("dump: {}", dumps[0]);
        assert!(*micros > 0);
        assert_eq!(*of, budget);
        assert!(
            matches!(out.last(), Some(Output::Dump { .. })),
            "after everything the step asked for"
        );
    }

    #[test]
    fn an_answer_ahead_of_the_store_breaks_an_invariant_and_asks_for_a_dump_naming_it() {
        let mut rig = rig();
        let call = CallId(1);
        subscribed(&mut rig.daemon, call);
        let ahead = wire::SessionEvent {
            of: Some(session_event::Of::Item(Item {
                key: "k000".into(),
                revision: 7,
                ..Item::default()
            })),
        };
        // The shell hands over an event of this host's agent that its
        // store never committed.
        let out = step(
            &mut rig.daemon,
            Input::FromShell(crate::FromShell::Session {
                agent: AgentKey::new(OWN, AGENT.0),
                event: Arc::new(ahead),
            }),
        );
        let dumps = dumps_in(&out);
        let [crate::DumpReason::Violation(violation)] = dumps[..] else {
            panic!("one dump for the violation: {dumps:?}");
        };
        println!("dump: {}", dumps[0]);
        assert_eq!(violation.invariant, crate::ANSWERED_UNCOMMITTED);
        assert!(
            violation.detail.contains("revision 7") && violation.detail.contains("through 0"),
            "{violation}"
        );

        // Committed, the same revisions break nothing.
        for n in 0..7 {
            rig.journal
                .append(&item(n))
                .expect("the journal takes a step");
        }
        let out = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(answered(&out, call).len(), 7);
        assert_eq!(dumps_in(&out), Vec::<&crate::DumpReason>::new());
    }

    /// The checkpoint job a step asked for.
    fn checkpoint_job(out: &[Output]) -> Option<crate::JobId> {
        out.iter().find_map(|output| match output {
            Output::Job {
                id,
                op: crate::Job::Checkpoint { .. },
            } => Some(*id),
            _ => None,
        })
    }

    fn subscribed(daemon: &mut Daemon, call: CallId) {
        step(
            daemon,
            Input::ClientCall {
                call,
                request: ClientRequest::Subscribe {
                    agent_id: AGENT.0.to_vec(),
                    tail: 10,
                },
            },
        );
    }

    #[test]
    fn commits_wait_while_a_checkpoint_is_taken_and_go_ahead_once_it_is_done() {
        let mut rig = rig();
        let call = CallId(1);
        subscribed(&mut rig.daemon, call);
        rig.journal
            .append(&item(0))
            .expect("the journal takes a step");
        let first = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(answered(&first, call), vec!["k000"]);
        assert!(
            first.contains(&Output::ArmTimer {
                id: TimerId::Checkpoint,
                at_ms: crate::Settings::default().checkpoint_interval_ms,
            }),
            "a commit makes the next checkpoint due an interval on"
        );

        let taken = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let job = checkpoint_job(&taken).expect("the timer takes a checkpoint");
        assert!(rig.daemon.checkpoints().taking());

        rig.journal
            .append(&item(1))
            .expect("the journal takes a step");
        let held = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(
            held,
            Vec::new(),
            "nothing is committed, answered or reported"
        );
        let key = AgentKey::new(OWN, AGENT.0);
        assert!(rig.daemon.store.get(&key, "k001").unwrap().is_none());
        assert_eq!(
            step(
                &mut rig.daemon,
                Input::Timer {
                    id: TimerId::Checkpoint,
                },
            ),
            Vec::new(),
            "one checkpoint at a time"
        );

        let done = step(
            &mut rig.daemon,
            Input::JobDone {
                job,
                result: crate::payload::JobResult::Done,
            },
        );
        assert_eq!(
            answered(&done, call),
            vec!["k001"],
            "the held commit goes ahead"
        );
        assert!(rig.daemon.store.get(&key, "k001").unwrap().is_some());
        assert!(done.iter().any(|output| matches!(
            output,
            Output::Ingested {
                report: Ingested {
                    frames: 1,
                    end: IngestEnd::Read,
                    ..
                },
                ..
            }
        )));
        assert!(!rig.daemon.checkpoints().taking());
    }

    #[test]
    fn segments_go_only_below_the_cursor_of_the_last_checkpoint_on_the_drive() {
        // A frame a segment.
        let mut rig = rig_with(1);
        let journal_dir = rig.daemon.journal_dir(AGENT);
        let key = AgentKey::new(OWN, AGENT.0);
        for n in 0..8 {
            rig.journal
                .append(&item(n))
                .expect("the journal takes a step");
        }
        step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        let all = journal::segments(&journal_dir).unwrap();
        assert!(all.len() > KEPT_SEGMENTS + 2, "{all:?}");
        let checkpointed = rig.daemon.store.cursor(&key).unwrap();

        // A checkpoint that failed vouches for nothing.
        let taken = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let failed = step(
            &mut rig.daemon,
            Input::JobDone {
                job: checkpoint_job(&taken).unwrap(),
                result: crate::payload::JobResult::Failed("the drive is full".into()),
            },
        );
        assert_eq!(
            journal::segments(&journal_dir).unwrap(),
            all,
            "nothing went"
        );
        assert!(
            failed.iter().any(|output| matches!(
                output,
                Output::ArmTimer {
                    id: TimerId::Checkpoint,
                    ..
                }
            )),
            "the next is tried an interval on"
        );

        let taken = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        // Committed after the checkpoint was taken, so not on the drive by it.
        for n in 8..16 {
            rig.journal
                .append(&item(n))
                .expect("the journal takes a step");
        }
        step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        step(
            &mut rig.daemon,
            Input::JobDone {
                job: checkpoint_job(&taken).unwrap(),
                result: crate::payload::JobResult::Done,
            },
        );
        assert_eq!(
            rig.daemon.checkpoints().durable_cursor(AGENT),
            Some(checkpointed)
        );
        assert!(
            rig.daemon.store.cursor(&key).unwrap() > checkpointed,
            "the held batch was committed once the checkpoint was done"
        );
        let left = journal::segments(&journal_dir).unwrap();
        assert!(left[0] > 0, "segments below the checkpoint went: {left:?}");
        assert_eq!(
            journal::reclaimable(&journal_dir, checkpointed, KEPT_SEGMENTS).unwrap(),
            Vec::<u64>::new(),
            "everything below the checkpoint's cursor went but the kept segments"
        );
        let above = journal::reclaimable(
            &journal_dir,
            rig.daemon.store.cursor(&key).unwrap(),
            KEPT_SEGMENTS,
        )
        .unwrap();
        assert!(
            !above.is_empty(),
            "segments ingested after the checkpoint wait for the next one"
        );
    }
}
