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
use crate::io::{IngestEnd, Ingested, Output};
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
    pub(crate) fn ingest(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let report = self
            .ingest_batch(agent, out)
            .unwrap_or_else(|(error, seen)| Ingested {
                frames: 0,
                turn_ends: 0,
                kind: 0,
                end: IngestEnd::Failed(error.to_string()),
                seen,
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

    /// One batch, and how far the journal was seen when it failed.
    fn ingest_batch(
        &mut self,
        agent: AgentId,
        out: &mut Vec<Output>,
    ) -> Result<Ingested, (IngestError, u64)> {
        let mut seen = 0;
        self.read_and_commit(agent, &mut seen, out)
            .map_err(|error| (error, seen))
    }

    fn read_and_commit(
        &mut self,
        agent: AgentId,
        seen: &mut u64,
        out: &mut Vec<Output>,
    ) -> Result<Ingested, IngestError> {
        let key = AgentKey::new(self.store.own_host(), agent.0);
        let dir = self.journal_dir(agent);
        let cursor = self.store.cursor(&key)?;
        let mut reader = journal::Reader::new(&dir, cursor);
        let mut batch = reader.read_up_to(INGEST_BATCH)?;
        *seen = batch.seen;
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
                *seen = (*seen).max(end).max(batch.seen);
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
            seen: *seen,
        };
        if !batch.frames.is_empty() {
            let clock = CommitClock {
                now_ms: self.timers.now_ms as i64,
                notify_delay_ms: self.settings.notify_delay_ms,
            };
            let committed = self.store.commit(&key, &batch.frames, clock)?;
            self.committed(agent);
            report.turn_ends = batch
                .frames
                .iter()
                .filter(|(_, step)| step.turn_end.is_some())
                .count() as u32;
            if report.turn_ends > 0 {
                // A turn end may have written an outbox row.
                self.deliveries_due();
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
                self.drain_notifications(out);
            }
            // A message waiting for its acceptance item is answered after
            // the subscribers were told of the item.
            self.lane_committed(agent, out);
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
        // The batch stops at its limit, but the read saw the whole journal.
        let end = journal::end(&rig.daemon.journal_dir(AGENT)).unwrap();
        assert!(first.contains(&Output::Ingested {
            agent: AGENT,
            report: Ingested {
                frames: INGEST_BATCH as u32,
                turn_ends: 0,
                kind: 0,
                end: IngestEnd::More,
                seen: end,
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
    fn a_turn_end_runs_a_drain_pass_once_the_sweep_is_done() {
        let mut rig = rig();
        let mut ended = item(0);
        ended.turn_end = Some(wire::TurnEnd::default());
        rig.journal
            .append(&ended)
            .expect("the journal takes a step");
        let drained = |out: &[Output]| {
            out.iter()
                .filter_map(|output| match output {
                    Output::Drained { pass, .. } => Some(*pass),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let out = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(
            drained(&out),
            Vec::<u64>::new(),
            "nothing drains before the sweep"
        );
        assert_eq!(drained(&step(&mut rig.daemon, Input::Swept)), vec![1]);

        let mut again = item(1);
        again.turn_end = Some(wire::TurnEnd::default());
        rig.journal
            .append(&again)
            .expect("the journal takes a step");
        let out = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(drained(&out), vec![2]);
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
        // The rolled-back statements still count as writes, so the step
        // may also make a checkpoint due.
        assert!(
            out.iter().any(|output| matches!(
                output,
                Output::Ingested {
                    report: Ingested {
                        end: IngestEnd::Failed(_),
                        ..
                    },
                    ..
                }
            )),
            "{out:?}"
        );
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

    /// The checkpoint's write job a step asked for.
    fn checkpoint_job(out: &[Output]) -> Option<crate::JobId> {
        out.iter().find_map(|output| match output {
            Output::Job {
                id,
                op: crate::Job::Checkpoint { .. },
            } => Some(*id),
            _ => None,
        })
    }

    /// The checkpoint's WAL job a step asked for.
    fn wal_job(out: &[Output]) -> Option<crate::JobId> {
        out.iter().find_map(|output| match output {
            Output::Job {
                id,
                op: crate::Job::CheckpointWal,
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
    fn an_ingest_waits_out_the_wal_job_and_commits_go_ahead_while_the_state_is_written() {
        let mut rig = rig();
        let call = CallId(1);
        subscribed(&mut rig.daemon, call);
        rig.journal
            .append(&item(0))
            .expect("the journal takes a step");
        let first = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(answered(&first, call), vec!["k000"]);
        let interval = crate::Settings::default().checkpoint_interval_ms;
        assert!(
            first.contains(&Output::ArmTimer {
                id: TimerId::Checkpoint,
                at_ms: interval,
            }),
            "a commit makes the next checkpoint due an interval on"
        );

        let started = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let wal = wal_job(&started).expect("the timer starts the WAL job");
        assert!(rig.daemon.checkpoints().holding());

        // While the WAL moves into the main file, the nudge waits.
        rig.journal
            .append(&item(1))
            .expect("the journal takes a step");
        let held = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(answered(&held, call), Vec::<String>::new());
        assert_eq!(rig.daemon.checkpoints().held(), 1);
        let copied = step(
            &mut rig.daemon,
            Input::JobDone {
                job: wal,
                result: crate::payload::JobResult::Done,
            },
        );
        let job = checkpoint_job(&copied).expect("the WAL job's end serializes the core");
        assert_eq!(answered(&copied, call), Vec::<String>::new());

        // It lands at the next step, and commits go ahead while the state
        // is written, making the next checkpoint due.
        let landed = step(&mut rig.daemon, Input::Tick { now_ms: 1 });
        assert_eq!(answered(&landed, call), vec!["k001"]);
        assert!(
            landed.contains(&Output::ArmTimer {
                id: TimerId::Checkpoint,
                at_ms: 1 + interval,
            }),
            "the commit makes the next checkpoint due: {landed:?}"
        );
        rig.journal
            .append(&item(2))
            .expect("the journal takes a step");
        let during = step(&mut rig.daemon, Input::Nudge { agent: AGENT });
        assert_eq!(answered(&during, call), vec!["k002"]);
        let again = step(
            &mut rig.daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        assert_eq!(wal_job(&again), None, "one checkpoint at a time");
        assert!(
            again.contains(&Output::ArmTimer {
                id: TimerId::Checkpoint,
                at_ms: 1 + interval,
            }),
            "the next waits an interval on: {again:?}"
        );

        step(
            &mut rig.daemon,
            Input::JobDone {
                job,
                result: crate::payload::JobResult::Done,
            },
        );
        assert!(!rig.daemon.checkpoints().taking());
        assert_eq!(dumps_in(&during), Vec::<&crate::DumpReason>::new());
    }
}
