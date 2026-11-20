//! The reviews experiment in the daemon core: writes landing whole with the
//! row's entry and one publish, the draft count and the counter, rounds
//! closed by a send, and the moved check each turn end asks for.

use store::{AgentKey, AgentRow, Sqlite};
use wire::{Step, inventory_event};

use super::*;
use crate::payload::{ClientRequest, Status};
use crate::{Input, StartOptions};

const OWN: [u8; 16] = *b"own-host-0000000";
const LEAD: AgentId = AgentId(*b"lead-00000000000");
const MATE: AgentId = AgentId(*b"mate-00000000000");
const FAR: AgentId = AgentId(*b"far0-00000000000");
const IDLE: AgentId = AgentId(*b"idle-00000000000");
const REVIEW_ID: [u8; 16] = [0x5e; 16];

struct Rig {
    daemon: Daemon,
    next_call: u64,
    /// The open inventory stream, which hears every row published.
    inventory: CallId,
    dir: tempfile::TempDir,
}

/// A daemon with the experiment `on` or off, and its own agents in their
/// folders.
fn rig(on: bool, agents: &[(AgentId, &str)]) -> Rig {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
    let versions: &[&str] = if on { &[VERSION] } else { &[] };
    store
        .settle_experiments(versions)
        .expect("the experiments settle");
    for (agent, cwd) in agents {
        store
            .put_agent(&AgentRow::new(
                AgentKey::new(OWN, agent.0),
                wire::Kind::Codex,
                *cwd,
            ))
            .expect("the store takes its agent");
    }
    let daemon = Daemon::new(
        store,
        StartOptions {
            generation: 1,
            agents: dir.path().to_path_buf(),
            reviews: on,
            ..StartOptions::default()
        },
    );
    let mut rig = Rig {
        daemon,
        next_call: 0,
        inventory: CallId(0),
        dir,
    };
    let (inventory, _) = rig.call(ClientRequest::SubscribeInventory);
    rig.inventory = inventory;
    rig
}

fn lead() -> Rig {
    rig(true, &[(LEAD, "/src/shop")])
}

fn begin() -> Change {
    Change::Begin {
        review: REVIEW_ID.to_vec(),
        base_branch: "main".into(),
        began_at: "c0ffee".into(),
        common_dir: "/src/shop/.git".into(),
    }
}

fn snapshot(commit: &str) -> Change {
    Change::Snapshot {
        commit: commit.into(),
        base_commit: "base".into(),
        base_name: "main".into(),
        omitted: vec!["big.bin".into()],
        merging: false,
    }
}

fn put(id: u8, commit: &str, text: &str) -> Change {
    Change::Put(Box::new(Put {
        comment: vec![id; 16],
        anchor: Anchor {
            commit: commit.into(),
            path: "src/lib.rs".into(),
            start: 3,
            end: 3,
        },
        text: text.into(),
        snippet: vec![ReviewLine {
            text: format!("let shown = {id};"),
            new_line: 3,
            ..Default::default()
        }],
        scope: Vec::new(),
    }))
}

impl Rig {
    fn step(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        self.daemon.step(input, &mut out);
        let dumps: Vec<_> = out
            .iter()
            .filter(|output| matches!(output, Output::Dump { .. }))
            .collect();
        assert!(dumps.is_empty(), "no check fails: {dumps:?}");
        out
    }

    fn call(&mut self, request: ClientRequest) -> (CallId, Vec<Output>) {
        self.next_call += 1;
        let call = CallId(self.next_call);
        (call, self.step(Input::ClientCall { call, request }))
    }

    /// Writes `changes` to `agent`'s review: what the write left, or why it
    /// was refused, and the outputs of its step.
    fn write(
        &mut self,
        agent: AgentId,
        changes: Vec<Change>,
    ) -> (Result<Written, CallError>, Vec<Output>) {
        let (call, out) = self.call(ClientRequest::Review(Box::new(Write { agent, changes })));
        let answer = out.iter().find_map(|output| match output {
            Output::Answer {
                call: to,
                response: Response::Review(written),
            } if *to == call => Some(Ok((**written).clone())),
            Output::EndCall {
                call: to,
                status: Status::Failed(error),
            } if *to == call => Some(Err(error.clone())),
            _ => None,
        });
        (answer.expect("the write is answered"), out)
    }

    fn written(&mut self, agent: AgentId, changes: Vec<Change>) -> Written {
        self.write(agent, changes).0.expect("the write is made")
    }

    /// The agent's row's entry, as the store holds it.
    fn entry(&self, agent: AgentId) -> Option<ReviewRowEntry> {
        let row = self
            .daemon
            .store()
            .agent(&AgentKey::new(OWN, agent.0))
            .unwrap()
            .expect("the agent has a row");
        REVIEW_ROW.get(&row.experimental)
    }

    /// Ends a turn of `agent`, as its journal says it: the step's outputs.
    fn turn_ends(&mut self, agent: AgentId) -> Vec<Output> {
        let mut journal =
            journal::synthetic::SyntheticWriter::open(self.daemon.journal_dir(agent), 1 << 30)
                .expect("the journal opens");
        journal
            .append(&Step {
                turn_end: Some(wire::TurnEnd::default()),
                ..Step::default()
            })
            .expect("the journal takes a step");
        self.step(Input::Nudge { agent })
    }

    fn moved(&mut self, agent: AgentId, snapshot: &str, moved: bool) -> Vec<Output> {
        self.step(Input::JobDone {
            job: JobId(900),
            result: JobResult::Review(Done::Moved {
                agent,
                snapshot: snapshot.into(),
                moved: Ok(moved),
            }),
        })
    }
}

/// The review entries the inventory stream heard in `out`, by agent.
fn published(rig: &Rig, out: &[Output]) -> Vec<(Vec<u8>, Option<ReviewRowEntry>)> {
    out.iter()
        .filter_map(|output| match output {
            Output::Answer {
                call,
                response: Response::Inventory(event),
            } if *call == rig.inventory => match &event.of {
                Some(inventory_event::Of::Agent(row)) => {
                    Some((row.agent_id.clone(), REVIEW_ROW.get(&row.experimental)))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The moved checks a step asked for, by agent and folder.
fn checks(out: &[Output]) -> Vec<(AgentId, PathBuf, PathBuf, String)> {
    out.iter()
        .filter_map(|output| match output {
            Output::Job {
                op:
                    CoreJob::Review(Job::Moved {
                        agent,
                        cwd,
                        objects,
                        snapshot,
                        ..
                    }),
                ..
            } => Some((*agent, cwd.clone(), objects.clone(), snapshot.clone())),
            _ => None,
        })
        .collect()
}

fn entry(drafts: u32, counter: u64, moved: bool) -> ReviewRowEntry {
    ReviewRowEntry {
        drafts,
        counter,
        moved,
        review_id: REVIEW_ID.to_vec(),
    }
}

/// A send of the comments `ids`, each at its first revision.
fn sent(review: [u8; 16], ids: &[u8]) -> Change {
    Change::Sent {
        review: review.to_vec(),
        comments: ids.iter().map(|id| (vec![*id; 16], 1)).collect(),
    }
}

#[test]
fn a_write_lands_whole_bumps_the_counter_once_and_publishes_the_row_once() {
    let mut rig = lead();
    assert_eq!(rig.entry(LEAD), None, "no review, no entry");
    let (written, out) = rig.write(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "why"),
            put(2, "s1", "how"),
        ],
    );
    let written = written.expect("the write is made");
    assert_eq!(written.entry, Some(entry(2, 1, false)));
    assert_eq!(rig.entry(LEAD), written.entry);
    assert_eq!(
        published(&rig, &out),
        [(LEAD.0.to_vec(), Some(entry(2, 1, false)))],
        "the row is published once, carrying the entry"
    );
    let review = written.review.expect("the review is answered");
    assert_eq!(review.id, REVIEW_ID);
    assert_eq!(review.comments.len(), 2);
    assert_eq!(
        review.latest().map(|taken| taken.commit.as_str()),
        Some("s1")
    );

    let (written, out) = rig.write(LEAD, vec![put(1, "s1", "why, said better")]);
    assert_eq!(written.unwrap().entry, Some(entry(2, 2, false)));
    assert_eq!(published(&rig, &out).len(), 1);

    // A change that changes nothing says nothing new.
    let (written, out) = rig.write(
        LEAD,
        vec![Change::Delete {
            comment: vec![9; 16],
        }],
    );
    assert_eq!(written.unwrap().entry, Some(entry(2, 2, false)));
    assert!(published(&rig, &out).is_empty());
}

#[test]
fn a_refused_write_makes_none_of_its_changes() {
    let mut rig = rig(true, &[(LEAD, "/src/shop"), (MATE, "/src/shop")]);
    rig.written(LEAD, vec![begin(), snapshot("s1"), put(1, "s1", "mine")]);
    rig.written(
        MATE,
        vec![Change::Begin {
            review: vec![0x6f; 16],
            base_branch: "main".into(),
            began_at: "c0ffee".into(),
            common_dir: "/src/shop/.git".into(),
        }],
    );

    let (refused, out) = rig.write(MATE, vec![put(2, "s1", "fine"), put(1, "s1", "not yours")]);
    assert_eq!(
        refused.unwrap_err(),
        CallError::Review(Refusal::Taken(vec![1; 16]))
    );
    assert!(published(&rig, &out).is_empty());
    let mate = rig.daemon.store().review(&MATE.0).unwrap().unwrap();
    assert!(
        mate.comments.is_empty(),
        "the first put went with the second"
    );
    assert_eq!(
        rig.entry(MATE),
        Some(ReviewRowEntry {
            review_id: vec![0x6f; 16],
            ..entry(0, 1, false)
        })
    );

    assert_eq!(
        rig.write(LEAD, vec![begin()]).0.unwrap_err(),
        CallError::Review(Refusal::Begun)
    );
    assert_eq!(
        rig.write(LEAD, vec![Change::StartOver, put(3, "s1", "into nothing")])
            .0
            .unwrap_err(),
        CallError::Review(Refusal::NoReview)
    );
    assert!(
        rig.daemon.store().review(&LEAD.0).unwrap().is_some(),
        "not started over"
    );
    assert_eq!(
        rig.write(IDLE, vec![begin()]).0.unwrap_err(),
        CallError::NoAgent
    );
}

#[test]
fn with_the_experiment_off_a_write_is_refused() {
    let mut rig = rig(false, &[(LEAD, "/src/shop")]);
    assert_eq!(
        rig.write(LEAD, vec![begin()]).0.unwrap_err(),
        CallError::Review(Refusal::Off)
    );
    assert_eq!(rig.entry(LEAD), None);
}

#[test]
fn the_draft_count_follows_puts_deletes_discards_and_sends() {
    let mut rig = lead();
    let drafts = |written: Written| written.entry.map(|entry| entry.drafts);
    let w = rig.written(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "a"),
            put(2, "s1", "b"),
        ],
    );
    assert_eq!(drafts(w), Some(2));
    let w = rig.written(LEAD, vec![put(3, "s1", "c")]);
    assert_eq!(drafts(w), Some(3));
    let w = rig.written(
        LEAD,
        vec![Change::Delete {
            comment: vec![3; 16],
        }],
    );
    assert_eq!(drafts(w), Some(2));
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[1])]);
    assert_eq!(w.round, Some(1));
    assert_eq!(drafts(w), Some(1), "the one sent is no longer a draft");
    let w = rig.written(LEAD, vec![put(4, "s1", "d")]);
    assert_eq!(drafts(w), Some(2));
    let w = rig.written(LEAD, vec![Change::DiscardDrafts]);
    assert_eq!(drafts(w.clone()), Some(0));
    let review = w.review.unwrap();
    assert_eq!(
        review
            .comments
            .iter()
            .map(|c| (c.id[0], c.round))
            .collect::<Vec<_>>(),
        [(1, Some(1))],
        "discarding leaves the sent comment"
    );
    let w = rig.written(
        LEAD,
        vec![Change::Delete {
            comment: vec![1; 16],
        }],
    );
    assert!(
        w.review.unwrap().comments.is_empty(),
        "a sent comment is deleted too"
    );
}

#[test]
fn a_send_marks_exactly_the_named_drafts_in_a_round_numbered_on_arrival() {
    let mut rig = lead();
    rig.written(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "a"),
            put(2, "s1", "b"),
            put(3, "s1", "c"),
        ],
    );
    // Another review's send, as one started over leaves in a chat, changes
    // nothing.
    let (written, out) = rig.write(LEAD, vec![sent([0x6f; 16], &[1])]);
    let written = written.unwrap();
    assert_eq!(written.round, None);
    assert_eq!(written.entry, Some(entry(3, 1, false)));
    assert!(published(&rig, &out).is_empty());

    // A send naming no comments, or none still a draft, closes nothing.
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[])]);
    assert_eq!(w.round, None);
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[9])]);
    assert_eq!(w.round, None);

    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[1, 9])]);
    assert_eq!(w.round, Some(1));
    // Sent again, as a message resent under a new id: its comment went
    // with the first, so no second round closes.
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[1])]);
    assert_eq!(w.round, None);
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[1, 3])]);
    assert_eq!(w.round, Some(2), "numbered by arrival");
    let review = w.review.unwrap();
    assert_eq!(
        review
            .comments
            .iter()
            .map(|c| (c.id[0], c.round))
            .collect::<Vec<_>>(),
        [(1, Some(1)), (2, None), (3, Some(2))],
        "a comment sent before keeps its round; one never named stays a draft"
    );
    assert_eq!(
        review
            .rounds
            .iter()
            .map(|round| round.n)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(rig.entry(LEAD), Some(entry(1, 3, false)));

    // A sent comment's words change in place, still sent.
    let w = rig.written(LEAD, vec![put(1, "s1", "a, said better")]);
    assert_eq!(w.review.unwrap().comments[0].round, Some(1));
}

#[test]
fn a_send_marks_only_the_revision_it_was_written_from_on_the_latest_snapshot() {
    let mut rig = lead();
    rig.written(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "a"),
            put(2, "s1", "b"),
        ],
    );
    // Comment 2 is edited after the sending client built its text.
    let w = rig.written(LEAD, vec![put(2, "s1", "b, said better")]);
    let revisions: Vec<u64> = w
        .review
        .unwrap()
        .comments
        .iter()
        .map(|comment| comment.revision)
        .collect();
    assert_eq!(revisions, [1, 2], "a put raises the revision");
    // A refresh took s2 meanwhile, whose base the rule found anew.
    rig.written(
        LEAD,
        vec![Change::Snapshot {
            commit: "s2".into(),
            base_commit: "base 2".into(),
            base_name: "main".into(),
            omitted: Vec::new(),
            merging: false,
        }],
    );
    let w = rig.written(LEAD, vec![sent(REVIEW_ID, &[1, 2])]);
    assert_eq!(w.round, Some(1));
    let review = w.review.unwrap();
    assert_eq!(
        review
            .comments
            .iter()
            .map(|c| (c.id[0], c.round))
            .collect::<Vec<_>>(),
        [(1, Some(1)), (2, None)],
        "the comment edited since stays a draft"
    );
    let [round] = review.rounds.as_slice() else {
        panic!("one round: {:?}", review.rounds);
    };
    assert_eq!(
        (round.snapshot.as_str(), round.base_commit.as_str()),
        ("s2", "base 2"),
        "recorded on the latest snapshot, with its base"
    );

    // The next message carries comment 2 at revision 2 and closes round 2
    // with it; each sent comment keeps the revision the agent took, which
    // names the message that sent it, through later edits.
    let w = rig.written(
        LEAD,
        vec![Change::Sent {
            review: REVIEW_ID.to_vec(),
            comments: vec![(vec![2; 16], 2)],
        }],
    );
    assert_eq!(w.round, Some(2));
    let w = rig.written(LEAD, vec![put(1, "s1", "a, edited after it went")]);
    let sent: Vec<(u8, Option<u32>, u64, Option<u64>)> = w
        .review
        .unwrap()
        .comments
        .iter()
        .map(|c| (c.id[0], c.round, c.revision, c.sent_revision))
        .collect();
    assert_eq!(sent, [(1, Some(1), 2, Some(1)), (2, Some(2), 2, Some(2))]);
}

#[test]
fn a_rebased_snapshot_keeps_its_new_base_and_raises_the_counter_once() {
    let mut rig = lead();
    rig.written(LEAD, vec![begin(), snapshot("s1")]);
    let rebase = |base: &str| Change::Rebased {
        snapshot: "s1".into(),
        base_commit: base.into(),
        base_name: "dev".into(),
    };
    let w = rig.written(LEAD, vec![rebase("picked")]);
    let review = w.review.unwrap();
    let s1 = review.snapshot("s1").unwrap();
    assert_eq!(
        (s1.base_commit.as_str(), s1.base_name.as_str()),
        ("picked", "dev")
    );
    assert_eq!(s1.bases, ["base", "picked"], "the base shown before stays");
    assert_eq!(rig.entry(LEAD), Some(entry(0, 2, false)));
    // The same base again changes nothing.
    let (_, out) = rig.write(LEAD, vec![rebase("picked")]);
    assert!(published(&rig, &out).is_empty());
    assert_eq!(rig.entry(LEAD), Some(entry(0, 2, false)));
}

/// What the agent's journal says it took, carrying a sent review of the
/// comments `ids`: the item a prompt became when the agent took it.
fn taken_prompt(input: &[u8], ids: &[u8]) -> Step {
    let mut attachment = wire::Attachment::default();
    SENT_REVIEW.put(
        &mut attachment.experimental,
        &SentReview {
            review_id: REVIEW_ID.to_vec(),
            round: 1,
            snapshot: "s1".into(),
            comments: ids
                .iter()
                .map(|id| wire::experimental::ReviewSentComment {
                    id: vec![*id; 16],
                    revision: 1,
                })
                .collect(),
        },
    );
    Step {
        items: vec![wire::Item {
            key: format!("prompt-{}", input[0]),
            input_id: input.to_vec(),
            text: "Have a look \u{fffc}".into(),
            attachments: vec![attachment],
            kind: wire::Kind::Codex as i32,
            at_ms: 1_000,
            ..wire::Item::default()
        }],
        ..Step::default()
    }
}

impl Rig {
    /// `step` lands in `agent`'s journal and is ingested.
    fn journal(&mut self, agent: AgentId, step: &Step) -> Vec<Output> {
        let mut journal =
            journal::synthetic::SyntheticWriter::open(self.daemon.journal_dir(agent), 1 << 30)
                .expect("the journal opens");
        journal.append(step).expect("the journal takes a step");
        self.step(Input::Nudge { agent })
    }
}

#[test]
fn a_round_closes_when_the_agent_takes_the_prompt_not_when_it_queues() {
    let mut rig = lead();
    rig.written(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "a"),
            put(2, "s1", "b"),
        ],
    );
    // Queued: the queue holds the prompt, and the journal no item of it. A
    // person may still withdraw it, so nothing is sent.
    let mut queued = wire::QueuedInput {
        input_id: b"queued".to_vec(),
        ..wire::QueuedInput::default()
    };
    queued.attachments = taken_prompt(b"queued", &[1]).items[0].attachments.clone();
    let out = rig.journal(
        LEAD,
        &Step {
            snapshot: Some(wire::Snapshot {
                kind: wire::Kind::Codex as i32,
                queue: vec![queued],
                ..wire::Snapshot::default()
            }),
            ..Step::default()
        },
    );
    // The row is published for the snapshot's phase, its entry as it was.
    assert!(
        published(&rig, &out)
            .iter()
            .all(|(_, told)| *told == Some(entry(2, 1, false))),
        "{:?}",
        published(&rig, &out)
    );
    assert_eq!(rig.entry(LEAD), Some(entry(2, 1, false)));

    // Taken: its item is in the journal, and the round closes with the row.
    let out = rig.journal(LEAD, &taken_prompt(b"taken", &[1]));
    assert_eq!(
        published(&rig, &out),
        [(LEAD.0.to_vec(), Some(entry(1, 2, false)))]
    );
    // The same item again, as a later revision of it, closes nothing more.
    let out = rig.journal(LEAD, &taken_prompt(b"taken", &[1]));
    assert!(published(&rig, &out).is_empty());
    assert_eq!(rig.entry(LEAD), Some(entry(1, 2, false)));
}

/// The round a taken prompt closes is written in the transaction that
/// commits the prompt's item and moves the journal's cursor past it: when
/// the round cannot be written, nothing of the batch is, and the journal is
/// read again from where it was, so a delivered review never stays a draft
/// behind a cursor that has moved on.
#[test]
fn a_round_lands_with_the_cursor_or_neither_does() {
    let mut rig = lead();
    rig.written(LEAD, vec![begin(), snapshot("s1"), put(1, "s1", "a")]);
    let key = AgentKey::new(OWN, LEAD.0);
    // The round's write fails, as a full disk or a crash between two
    // transactions would leave it.
    rig.daemon
        .store()
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER no_rounds BEFORE INSERT ON exp_reviews_1_rounds \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END;",
        )
        .unwrap();
    let out = rig.journal(LEAD, &taken_prompt(b"taken", &[1]));
    let failed = out.iter().any(|output| {
        matches!(output, Output::Ingested { report, .. }
            if matches!(&report.end, crate::io::IngestEnd::Failed(why) if why.contains("the disk is full")))
    });
    assert!(failed, "the batch fails: {out:?}");
    assert_eq!(
        rig.daemon.store().cursor(&key).unwrap(),
        0,
        "the cursor stays"
    );
    assert!(published(&rig, &out).is_empty());
    assert_eq!(rig.entry(LEAD), Some(entry(1, 1, false)), "still a draft");

    // Once the round can be written, the same item is read again and
    // closes it, with the cursor past it.
    rig.daemon
        .store()
        .connection()
        .execute_batch("DROP TRIGGER no_rounds;")
        .unwrap();
    let out = rig.step(Input::Nudge { agent: LEAD });
    assert_eq!(
        published(&rig, &out),
        [(LEAD.0.to_vec(), Some(entry(0, 2, false)))]
    );
    assert!(rig.daemon.store().cursor(&key).unwrap() > 0);
    let review = rig.daemon.store().review(&LEAD.0).unwrap().unwrap();
    assert_eq!(review.rounds.len(), 1);
    assert_eq!(review.comments[0].round, Some(1));
}

#[test]
fn moved_drafts_take_their_new_anchors_and_sent_ones_stay() {
    let mut rig = lead();
    rig.written(
        LEAD,
        vec![
            begin(),
            snapshot("s1"),
            put(1, "s1", "a"),
            put(2, "s1", "b"),
        ],
    );
    rig.written(LEAD, vec![sent(REVIEW_ID, &[2])]);
    let to = |id: u8, start: u32| MovedDraft {
        comment: vec![id; 16],
        anchor: Anchor {
            commit: "s2".into(),
            path: "src/lib.rs".into(),
            start,
            end: start,
        },
    };
    let w = rig.written(
        LEAD,
        vec![
            snapshot("s2"),
            Change::DraftsMoved {
                drafts: vec![to(1, 7), to(2, 9)],
            },
        ],
    );
    let review = w.review.unwrap();
    let anchors: Vec<_> = review
        .comments
        .iter()
        .map(|c| (c.id[0], c.anchor.commit.as_str(), c.anchor.start))
        .collect();
    assert_eq!(anchors, [(1, "s2", 7), (2, "s1", 3)]);
}

#[test]
fn a_new_snapshot_forgets_those_the_review_no_longer_keeps() {
    let mut rig = lead();
    rig.written(LEAD, vec![begin(), snapshot("s1"), put(1, "s1", "a")]);
    rig.written(LEAD, vec![snapshot("s2")]);
    let w = rig.written(LEAD, vec![snapshot("s3")]);
    let commits = |w: &Written| {
        w.review
            .as_ref()
            .unwrap()
            .snapshots
            .iter()
            .map(|taken| taken.commit.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        commits(&w),
        ["s1", "s2", "s3"],
        "s2 is named by nothing, but a page opened before s3 may show it"
    );
    let w = rig.written(LEAD, vec![snapshot("s3a")]);
    assert_eq!(
        commits(&w),
        ["s1", "s3", "s3a"],
        "s2 is neither named, the latest, nor the one before"
    );

    // Once the drafts on s1 move, s1 goes with the snapshot that moved them.
    let w = rig.written(
        LEAD,
        vec![
            snapshot("s4"),
            Change::DraftsMoved {
                drafts: vec![MovedDraft {
                    comment: vec![1; 16],
                    anchor: Anchor {
                        commit: "s4".into(),
                        path: "src/lib.rs".into(),
                        start: 0,
                        end: 0,
                    },
                }],
            },
        ],
    );
    assert_eq!(commits(&w), ["s3a", "s4"]);
}

#[test]
fn start_over_removes_the_review_and_its_entry() {
    let mut rig = lead();
    rig.written(LEAD, vec![begin(), snapshot("s1"), put(1, "s1", "a")]);
    let (written, out) = rig.write(LEAD, vec![Change::StartOver]);
    let written = written.unwrap();
    assert_eq!((written.review, written.entry), (None, None));
    assert_eq!(rig.entry(LEAD), None);
    assert_eq!(published(&rig, &out), [(LEAD.0.to_vec(), None)]);
    let w = rig.written(
        LEAD,
        vec![Change::Begin {
            review: vec![0x6f; 16],
            base_branch: "main".into(),
            began_at: "c0ffee".into(),
            common_dir: "/src/shop/.git".into(),
        }],
    );
    assert!(
        w.review.unwrap().comments.is_empty(),
        "the next look is a first look"
    );
    let entry = w.entry.unwrap();
    assert_eq!(
        (entry.review_id, entry.counter),
        (vec![0x6f; 16], 1),
        "a review begun anew counts afresh under its own id"
    );
}

#[test]
fn a_turn_end_asks_after_each_reviewed_tree_in_its_checkout() {
    let mut rig = rig(
        true,
        &[
            (LEAD, "/src/shop"),
            (MATE, "/src/shop/crates/cart"),
            (FAR, "/src/elsewhere"),
            (IDLE, "/src/shop"),
        ],
    );
    for (agent, review) in [(LEAD, 1), (MATE, 2), (FAR, 3)] {
        rig.written(
            agent,
            vec![
                Change::Begin {
                    review: vec![review; 16],
                    base_branch: "main".into(),
                    began_at: "c0ffee".into(),
                    common_dir: "/src/.git".into(),
                },
                snapshot(&format!("s{review}")),
            ],
        );
    }
    // A review that has taken no snapshot has nothing to have moved from.
    rig.written(
        IDLE,
        vec![Change::Begin {
            review: vec![4; 16],
            base_branch: "main".into(),
            began_at: "c0ffee".into(),
            common_dir: "/src/.git".into(),
        }],
    );
    let out = rig.turn_ends(LEAD);
    let objects = |agent: AgentId| {
        rig.dir
            .path()
            .join(uuid::Uuid::from_bytes(agent.0).to_string())
            .join("review")
            .join("objects")
    };
    let mut asked = checks(&out);
    asked.sort();
    let mut expected = vec![
        (
            LEAD,
            PathBuf::from("/src/shop"),
            objects(LEAD),
            "s1".to_owned(),
        ),
        (
            MATE,
            PathBuf::from("/src/shop/crates/cart"),
            objects(MATE),
            "s2".to_owned(),
        ),
    ];
    expected.sort();
    assert_eq!(asked, expected);
    let out = rig.turn_ends(IDLE);
    let mut asked: Vec<_> = checks(&out).into_iter().map(|(agent, ..)| agent).collect();
    asked.sort();
    assert_eq!(
        asked,
        [LEAD, MATE],
        "an agent with no snapshot asks after the others"
    );
}

#[test]
fn without_the_experiment_a_turn_end_asks_nothing() {
    let mut rig = rig(false, &[(LEAD, "/src/shop")]);
    assert!(checks(&rig.turn_ends(LEAD)).is_empty());
}

#[test]
fn a_moved_answer_sets_the_flag_without_raising_the_counter() {
    let mut rig = lead();
    rig.written(LEAD, vec![begin(), snapshot("s1"), put(1, "s1", "a")]);
    let out = rig.moved(LEAD, "s1", true);
    assert_eq!(rig.entry(LEAD), Some(entry(1, 1, true)));
    assert_eq!(
        published(&rig, &out),
        [(LEAD.0.to_vec(), Some(entry(1, 1, true)))]
    );
    // Told again, nothing is new.
    let out = rig.moved(LEAD, "s1", true);
    assert!(published(&rig, &out).is_empty());

    // An answer about a snapshot no longer the latest says nothing.
    rig.written(LEAD, vec![snapshot("s2")]);
    assert_eq!(
        rig.entry(LEAD),
        Some(entry(1, 2, false)),
        "a snapshot is of the tree as it is"
    );
    let out = rig.moved(LEAD, "s1", true);
    assert!(published(&rig, &out).is_empty());
    assert_eq!(rig.entry(LEAD), Some(entry(1, 2, false)));

    // An answer for an agent whose review is gone is dropped.
    rig.written(LEAD, vec![Change::StartOver]);
    let out = rig.moved(LEAD, "s2", true);
    assert!(published(&rig, &out).is_empty());
    assert_eq!(rig.entry(LEAD), None);
}
