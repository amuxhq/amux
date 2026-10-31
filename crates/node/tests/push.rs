//! The notifications outbox on a driven clock: a needs-you transition
//! enqueues one push keyed by its snapshot's revision, handed to the
//! sender once due; an answer before then cancels it; the agent's exit and
//! delete remove it.

use std::sync::{Arc, Mutex};

use agent_dir::{Clock as _, ManualClock};
use node::{Daemon, NoopSender, ProfileRuntime, Push, PushFuture, PushSender, StartOptions};
use support::synthetic::*;
use support::*;
use wire::Phase;

use crate::support;

const SEGMENTS: u64 = 1 << 20;
const DELAY: i64 = 30_000;
const T0: i64 = 1_000_000;

/// The no-op sender, with a record of what it was handed.
#[derive(Default)]
struct Recording {
    sent: Mutex<Vec<Push>>,
}

impl PushSender for Recording {
    fn send<'a>(&'a self, push: &'a Push) -> PushFuture<'a> {
        self.sent.lock().unwrap().push(push.clone());
        NoopSender.send(push)
    }
}

impl Recording {
    fn sent(&self) -> Vec<Push> {
        self.sent.lock().unwrap().clone()
    }
}

struct Rig {
    install: Install,
    clock: ManualClock,
    pushes: Arc<Recording>,
    daemon: Daemon,
    runtime: Arc<ProfileRuntime>,
    log: Vec<String>,
}

impl Rig {
    async fn start(install: Install) -> Self {
        let clock = ManualClock::new(T0);
        let pushes = Arc::new(Recording::default());
        let mut launch = quiet_launch();
        launch.notify_delay_ms = DELAY;
        let options = StartOptions {
            clock: Arc::new(clock.clone()),
            push: pushes.clone(),
            ..install.options("boot-1", launch)
        };
        let daemon = node::start(options, None).await.unwrap();
        let runtime = runtime(&daemon, &install);
        Self {
            install,
            clock,
            pushes,
            daemon,
            runtime,
            log: Vec::new(),
        }
    }

    fn at(&self) -> String {
        format!("t+{:>6}ms", self.clock.now_ms() - T0)
    }

    fn note(&mut self, line: impl Into<String>) {
        let line = format!("{}  {}", self.at(), line.into());
        println!("{line}");
        self.log.push(line);
    }

    async fn rows(&self) -> Vec<(u64, i64)> {
        self.runtime
            .read_store(|store| store.notifications().unwrap())
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.revision, row.due_at_ms - T0))
            .collect()
    }

    async fn turn(&mut self, agent: &mut SyntheticAgent, phase: Phase, what: &str) {
        agent.append(&snapshot(phase, &[], self.clock.now_ms()));
        agent.nudge().await;
        let revision = agent.journal().offset();
        until("the snapshot to commit", || async {
            let key = agent.key(&self.install);
            let cursor = self
                .runtime
                .read_store(move |store| store.agent(&key).unwrap().unwrap().ingest_cursor)
                .await
                .unwrap();
            (cursor == revision)
                .then_some(())
                .ok_or_else(|| format!("ingest cursor {cursor} of {revision}"))
        })
        .await
        .unwrap();
        let rows = self.rows().await;
        self.note(format!("{what}: phase {phase:?}; outbox {rows:?}"));
    }

    /// Moves the clock to `ms` past the start, once the drain is asleep
    /// until then.
    async fn advance_to(&mut self, ms: i64) {
        self.clock.armed(T0 + ms).await;
        self.clock.set(T0 + ms);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_needs_you_push_is_sent_once_due_and_cancelled_by_an_answer_exit_or_delete() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "deployer", SEGMENTS);
    agent.register(&install);
    agent.append(&item("ask", "May I run the migration?"));
    agent.go_live();
    let mut rig = Rig::start(install).await;
    rig.note("outbox (revision, due) pairs are shown relative to the start");

    // Needs you: one row, due after the delay; nothing is sent before.
    rig.turn(&mut agent, Phase::NeedsYou, "the agent asks")
        .await;
    let rows = rig.rows().await;
    assert_eq!(
        rows,
        vec![(2, DELAY)],
        "one row, keyed by the snapshot's revision"
    );
    rig.advance_to(DELAY).await;
    until("the push", || async {
        let sent = rig.pushes.sent().len();
        (sent == 1)
            .then_some(())
            .ok_or_else(|| format!("{sent} sent"))
    })
    .await
    .unwrap();
    let push = rig.pushes.sent().remove(0);
    assert_eq!(
        (
            push.name.as_str(),
            push.working_on.as_deref(),
            push.text.as_str(),
            push.revision
        ),
        ("deployer", Some("the task"), "May I run the migration?", 2),
        "envelope fields only"
    );
    assert_eq!(push.agent_id, agent.id.as_bytes().to_vec());
    until("the sent row to go", || async {
        let rows = rig.rows().await;
        rows.is_empty()
            .then_some(())
            .ok_or_else(|| format!("rows {rows:?}"))
    })
    .await
    .unwrap();
    rig.note(format!(
        "due: handed to the no-op sender ({:?}, working on {:?}: {:?}); outbox {:?}",
        push.name.as_str(),
        push.working_on.as_deref().unwrap_or_default(),
        push.text,
        rig.rows().await
    ));

    // Answered from the desktop before the delay: cancelled unsent.
    rig.turn(&mut agent, Phase::Working, "the person answers")
        .await;
    rig.turn(&mut agent, Phase::NeedsYou, "the agent asks again")
        .await;
    assert_eq!(rig.rows().await, vec![(4, DELAY + DELAY)]);
    rig.clock.set(T0 + DELAY + DELAY / 2);
    rig.note("half the delay passes");
    rig.turn(&mut agent, Phase::Working, "answered before it was due")
        .await;
    assert!(rig.rows().await.is_empty(), "cancelled unsent");
    rig.clock.set(T0 + 3 * DELAY);
    rig.note("the delay passes");
    // Only a row is ever sent, and the drain reads the rows afresh under
    // the store lock before sending: with the outbox empty there is
    // nothing the delay could send.
    assert_eq!(rig.pushes.sent().len(), 1, "nothing more was sent");

    // The agent exits while it needs you: the row goes with it.
    rig.turn(&mut agent, Phase::NeedsYou, "the agent asks a third time")
        .await;
    assert_eq!(rig.rows().await.len(), 1);
    agent.die().await;
    until("the exit to remove the row", || async {
        let rows = rig.rows().await;
        rows.is_empty()
            .then_some(())
            .ok_or_else(|| format!("rows {rows:?}"))
    })
    .await
    .unwrap();
    rig.note(format!("the agent exits: outbox {:?}", rig.rows().await));

    // Delete removes it too.
    let mut other = SyntheticAgent::new(&rig.install, "reviewer", SEGMENTS);
    other.register(&rig.install);
    other.append(&item("ask", "Approve the plan?"));
    other.append(&snapshot(Phase::NeedsYou, &[], rig.clock.now_ms()));
    rig.runtime.ingest(other.id).await.unwrap();
    assert_eq!(rig.rows().await.len(), 1);
    rig.note(format!("another agent asks: outbox {:?}", rig.rows().await));
    rig.runtime.delete(other.id).await.unwrap();
    assert!(rig.rows().await.is_empty());
    rig.note("it is deleted: outbox []");
    rig.clock.set(T0 + 10 * DELAY);
    assert_eq!(rig.pushes.sent().len(), 1, "one push in all");
    rig.note(format!("pushes sent in all: {}", rig.pushes.sent().len()));

    drop(rig.runtime);
    drop(rig.daemon);
}
