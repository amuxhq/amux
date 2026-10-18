//! Agents with no process: a directory, a journal written by the synthetic
//! writer, and, while "live", the directory's lock and a control socket
//! that answers the daemon's dial with a Hello and sends Nudges on demand.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_dir::local_socket::{LocalListener, LocalStream};
use journal::synthetic::SyntheticWriter;
use node::{Launch, ProfileRuntime};
use store::{AgentKey, AgentRow};
use tokio::io::WriteHalf;
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Accepted, AgentHello, CtlFrame, Envelope, Input, InventoryEvent, Item, Nudge, Phase,
    QueuedInput, Rejected, SendInputResponse, SessionEvent, Snapshot, Step, TurnEnd, Verdict,
    ctl_frame, send_input_response, session_event,
};

use super::{Install, PATIENCE, until};

/// What daemons in these tests start with: nothing is ever spawned.
pub fn quiet_launch() -> Launch {
    Launch {
        install_path: PathBuf::from("/nonexistent/amux"),
        ..Launch::default()
    }
}

pub const KIND: wire::Kind = wire::Kind::ClaudeSdk;

/// How a live synthetic agent answers an input.
#[derive(Clone, Debug)]
pub enum Answer {
    /// An agent message: write its item, then reply accepted. Anything
    /// else: reply queued.
    Accept,
    /// Write the agent message's item but never reply: the agent accepted
    /// and the answer was lost on the way back.
    AcceptSilently,
    /// Write the agent message's item and reply accepted, but hold the
    /// Nudge back: the item is in the journal and not yet committed.
    AcceptWithoutNudge,
    Reject(wire::Refusal),
}

/// What the daemon handed an agent on its control connection: a person's
/// input or an agent message.
#[derive(Clone, Debug)]
pub enum Handed {
    Input(Input),
    Message(Box<Envelope>),
}

impl Handed {
    /// The id the verdict is keyed by.
    pub fn id(&self) -> &[u8] {
        match self {
            Self::Input(input) => &input.input_id,
            Self::Message(envelope) => &envelope.id,
        }
    }
}

pub struct SyntheticAgent {
    pub id: Uuid,
    pub name: String,
    pub dir: PathBuf,
    pub parent: Option<AgentKey>,
    pub incarnation: u32,
    journal: Arc<Mutex<SyntheticWriter>>,
    segment_bytes: u64,
    answer: Arc<Mutex<Answer>>,
    inputs: Arc<Mutex<Vec<Handed>>>,
    /// Set when the agent stops reading its control connection.
    wedged: Arc<tokio::sync::watch::Sender<bool>>,
    /// Set while the agent holds every answer back.
    frozen: Arc<tokio::sync::watch::Sender<bool>>,
    cwd: PathBuf,
    live: Option<Live>,
}

struct Live {
    _lock: agent_dir::Lock,
    accept: JoinHandle<()>,
    conn: Arc<tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>>,
    /// The task holding the current connection's read half.
    reading: Arc<Mutex<Option<JoinHandle<()>>>>,
    hellos: Arc<AtomicUsize>,
}

impl Live {
    /// Closes the current control connection whole: its write half and the
    /// task holding its read half. Shutting the write half is not enough: a
    /// named pipe has no half-close, so the daemon sees the connection end
    /// only once neither half is open, as when a process's handles close.
    async fn close_connection(&self) {
        drop(self.conn.lock().await.take());
        let reading = self.reading.lock().unwrap().take();
        if let Some(reading) = reading {
            reading.abort();
            let _ = reading.await;
        }
    }
}

impl SyntheticAgent {
    /// A directory and an empty journal, rotating at `segment_bytes`.
    pub fn new(install: &Install, name: &str, segment_bytes: u64) -> Self {
        let id = Uuid::new_v4();
        let dir = install.agent_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        let journal = SyntheticWriter::open(dir.join(agent_dir::JOURNAL), segment_bytes).unwrap();
        Self {
            id,
            name: name.to_owned(),
            dir,
            parent: None,
            incarnation: 1,
            journal: Arc::new(Mutex::new(journal)),
            segment_bytes,
            answer: Arc::new(Mutex::new(Answer::Accept)),
            inputs: Arc::default(),
            wedged: Arc::new(tokio::sync::watch::Sender::new(false)),
            frozen: Arc::new(tokio::sync::watch::Sender::new(false)),
            cwd: install.work.clone(),
            live: None,
        }
    }

    /// The journal writer, for torn frames and cuts.
    pub fn journal(&self) -> MutexGuard<'_, SyntheticWriter> {
        self.journal.lock().unwrap()
    }

    /// What a restarted agent process does: reopen its journal, which
    /// drops a torn frame at its end.
    pub fn reopen_journal(&self) {
        *self.journal() =
            SyntheticWriter::open(self.dir.join(agent_dir::JOURNAL), self.segment_bytes).unwrap();
    }

    pub fn answer_with(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    /// The agent stops reading its control connection and keeps it open,
    /// as a process wedged in a blocking call would: what the daemon writes
    /// fills the socket's buffer and then blocks.
    pub fn stop_reading(&self) {
        self.wedged.send_replace(true);
    }

    /// The agent stops answering, as a process stopped by the scheduler
    /// would: it takes in what the daemon sends and answers nothing until
    /// thawed.
    pub fn freeze(&self) {
        self.frozen.send_replace(true);
    }

    /// Lets a frozen agent answer again, starting with what it took in.
    pub fn thaw(&self) {
        self.frozen.send_replace(false);
    }

    /// Every input and agent message the daemon relayed to this agent.
    pub fn inputs(&self) -> Vec<Handed> {
        self.inputs.lock().unwrap().clone()
    }

    pub fn key(&self, install: &Install) -> AgentKey {
        AgentKey::new(
            host(install).as_bytes().to_vec(),
            self.id.as_bytes().to_vec(),
        )
    }

    pub fn row(&self, install: &Install) -> AgentRow {
        let mut row = AgentRow::new(self.key(install), KIND, self.cwd.to_string_lossy());
        row.name = self.name.clone();
        row.parent = self.parent.clone();
        row.incarnation = self.incarnation;
        row.created_at_ms = 1;
        row
    }

    /// Writes the row with no daemon running, as a spawn before a crash
    /// would have left it.
    pub fn register_offline(&self, install: &Install) {
        let mut store = store::Sqlite::open(
            &install.profile_dir().join(node::STORE),
            host(install).as_bytes().to_vec(),
        )
        .unwrap();
        store.put_agent(&self.row(install)).unwrap();
    }

    /// Writes the row through a running daemon's store.
    pub async fn register(&self, install: &Install, runtime: &ProfileRuntime) {
        let row = self.row(install);
        runtime
            .write_store_beneath(move |store| store.put_agent(&row).unwrap())
            .await
            .unwrap();
    }

    pub fn append(&mut self, step: &Step) -> u64 {
        self.journal().append(step).unwrap()
    }

    /// Writes the first incarnation's spec, as the spawn that created this
    /// agent would have, so a resume can start its next incarnation.
    pub fn write_spec(&self, install: &Install) {
        let spec = wire::AgentSpec {
            agent_id: self.id.as_bytes().to_vec(),
            profile_id: install.profile.as_bytes().to_vec(),
            kind: KIND as i32,
            cwd: self.cwd.to_string_lossy().into_owned(),
            name: self.name.clone(),
            parent: self.parent.as_ref().map(|parent| wire::AgentRef {
                host_id: parent.host.clone(),
                agent_id: parent.agent.clone(),
            }),
            incarnation: 1,
            ..wire::AgentSpec::default()
        };
        std::fs::write(
            self.dir.join("spec.1"),
            prost::Message::encode_to_vec(&spec),
        )
        .unwrap();
    }

    /// Takes the directory's lock and answers dials with a Hello, as a
    /// running agent process does.
    pub fn go_live(&mut self) {
        assert!(self.live.is_none(), "already live");
        let lock = agent_dir::lock(&self.dir)
            .unwrap()
            .expect("nothing else holds the lock");
        let mut listener = LocalListener::bind(&self.dir.join(agent_dir::CTL_SOCK)).unwrap();
        let conn = Arc::new(tokio::sync::Mutex::new(None));
        let reading = Arc::new(Mutex::new(None));
        let hellos = Arc::new(AtomicUsize::new(0));
        let id = self.id;
        let journal = self.journal.clone();
        let answer = self.answer.clone();
        let inputs = self.inputs.clone();
        let wedged = self.wedged.clone();
        let frozen = self.frozen.clone();
        let accept = tokio::spawn({
            let conn = conn.clone();
            let reading = reading.clone();
            let hellos = hellos.clone();
            async move {
                while let Ok(stream) = listener.accept().await {
                    let (mut reader, mut writer) = tokio::io::split(stream);
                    let hello = CtlFrame {
                        of: Some(ctl_frame::Of::Hello(AgentHello {
                            agent_id: id.as_bytes().to_vec(),
                        })),
                    };
                    if agent_dir::write_frame(&mut writer, &hello).await.is_err() {
                        continue;
                    }
                    hellos.fetch_add(1, Ordering::SeqCst);
                    *conn.lock().await = Some(writer);
                    let (conn, journal, answer, inputs) = (
                        conn.clone(),
                        journal.clone(),
                        answer.clone(),
                        inputs.clone(),
                    );
                    let mut wedged = wedged.subscribe();
                    let mut frozen = frozen.subscribe();
                    let task = tokio::spawn(async move {
                        loop {
                            let frame = tokio::select! {
                                biased;
                                () = async {
                                    let _ = wedged.wait_for(|wedged| *wedged).await;
                                } => {
                                    // Holds the connection open, unread.
                                    std::future::pending::<()>().await;
                                    return;
                                }
                                frame = agent_dir::read_frame(&mut reader) => frame,
                            };
                            let Ok(Some(frame)) = frame else { break };
                            let input = match frame.of {
                                Some(ctl_frame::Of::Input(input)) => Handed::Input(input),
                                Some(ctl_frame::Of::AgentMessage(envelope)) => {
                                    Handed::Message(Box::new(envelope))
                                }
                                _ => continue,
                            };
                            inputs.lock().unwrap().push(input.clone());
                            let _ = frozen.wait_for(|frozen| !*frozen).await;
                            let answer = answer.lock().unwrap().clone();
                            answer_input(&input, &answer, &journal, &conn).await;
                        }
                    });
                    *reading.lock().unwrap() = Some(task);
                }
            }
        });
        self.live = Some(Live {
            _lock: lock,
            accept,
            conn,
            reading,
            hellos,
        });
    }

    /// How many Hellos this agent has sent.
    pub fn hellos(&self) -> usize {
        self.live
            .as_ref()
            .map_or(0, |live| live.hellos.load(Ordering::SeqCst))
    }

    /// Tells the daemon the journal grew. Waits for a connection first.
    pub async fn nudge(&self) {
        self.send(ctl_frame::Of::Nudge(Nudge {})).await;
    }

    /// Sends one frame to the daemon, once one is connected.
    async fn send(&self, of: ctl_frame::Of) {
        let live = self.live.as_ref().expect("a live agent");
        let frame = CtlFrame { of: Some(of) };
        until("a daemon to connect for the frame", || async {
            let mut conn = live.conn.lock().await;
            let Some(writer) = conn.as_mut() else {
                return Err("no daemon connected");
            };
            agent_dir::write_frame(writer, &frame).await.unwrap();
            Ok(())
        })
        .await
        .unwrap();
    }

    /// The process ends the way an agent does: its last step says why, it
    /// nudges, then it dies.
    pub async fn exit(&mut self, cause: wire::ExitCause, detail: Option<&str>) {
        self.append(&Step {
            incarnation_end: Some(wire::IncarnationEnd {
                cause: cause as i32,
                detail: detail.map(str::to_owned),
            }),
            ..Step::default()
        });
        self.nudge().await;
        self.die().await;
    }

    /// Closes the control connection with the process still alive: the
    /// daemon dials again and gets a new Hello.
    pub async fn drop_connection(&self) {
        let live = self.live.as_ref().expect("a live agent");
        live.close_connection().await;
    }

    /// The process dies: its connection closes and its lock is released.
    pub async fn die(&mut self) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        live.accept.abort();
        let _ = (&mut live.accept).await;
        live.close_connection().await;
        let _ = std::fs::remove_file(self.dir.join(agent_dir::CTL_SOCK));
    }
}

/// The interpreter's part: an accepted agent message leaves its item in
/// the journal, carrying the envelope id, before the reply goes back.
async fn answer_input(
    input: &Handed,
    answer: &Answer,
    journal: &Mutex<SyntheticWriter>,
    conn: &tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>,
) {
    let message = match input {
        Handed::Message(envelope) => Some(&**envelope),
        Handed::Input(_) => None,
    };
    let verdict = match answer {
        Answer::Reject(refusal) => send_input_response::Of::Rejected(Rejected {
            refusal: *refusal as i32,
            detail: None,
        }),
        Answer::Accept | Answer::AcceptSilently | Answer::AcceptWithoutNudge => {
            if let Some(envelope) = message {
                let step = Step {
                    items: vec![Item {
                        key: format!(
                            "message:{}",
                            Uuid::from_slice(&envelope.id).unwrap_or_default()
                        ),
                        text: match &envelope.body {
                            Some(wire::envelope::Body::Message(message)) => message.text.clone(),
                            Some(wire::envelope::Body::Report(wire::ChildReport {
                                end: Some(wire::child_report::End::TurnEnded(turn)),
                                ..
                            })) => turn.text.clone(),
                            _ => String::new(),
                        },
                        input_id: envelope.id.clone(),
                        kind: KIND as i32,
                        at_ms: 1_000,
                        ..Item::default()
                    }],
                    ..Step::default()
                };
                journal.lock().unwrap().append(&step).unwrap();
            }
            send_input_response::Of::Accepted(Accepted {
                queued: message.is_none(),
            })
        }
    };
    let mut conn = conn.lock().await;
    let Some(writer) = conn.as_mut() else { return };
    if !matches!(answer, Answer::AcceptSilently) {
        let reply = CtlFrame {
            of: Some(ctl_frame::Of::Reply(Verdict {
                id: input.id().to_vec(),
                verdict: Some(SendInputResponse { of: Some(verdict) }),
            })),
        };
        let _ = agent_dir::write_frame(writer, &reply).await;
    }
    if message.is_some() && !matches!(answer, Answer::AcceptWithoutNudge) {
        let nudge = CtlFrame {
            of: Some(ctl_frame::Of::Nudge(Nudge {})),
        };
        let _ = agent_dir::write_frame(writer, &nudge).await;
    }
}

/// A step that ends a turn whose last message is the item `key`, marked as
/// the incarnation's last, as a one-shot agent's final turn is.
pub fn turn_end(turn_id: u64, key: &str) -> Step {
    Step {
        turn_end: Some(TurnEnd {
            turn_id,
            last_message_key: key.to_owned(),
            outcome: wire::TurnOutcome::Completed as i32,
            last: true,
        }),
        ..snapshot(Phase::Idle, &[], 4_000)
    }
}

pub fn host(install: &Install) -> Uuid {
    node::host_id(&install.profile_dir()).unwrap()
}

// --- steps --------------------------------------------------------------

pub fn item(key: &str, text: &str) -> Step {
    Step {
        items: vec![Item {
            key: key.to_owned(),
            text: text.to_owned(),
            kind: KIND as i32,
            at_ms: 1_000,
            producer_version: "synthetic".to_owned(),
            ..Item::default()
        }],
        ..Step::default()
    }
}

pub fn append(key: &str, text: &str) -> Step {
    Step {
        appends: vec![wire::Append {
            key: key.to_owned(),
            text: text.to_owned(),
            ..wire::Append::default()
        }],
        ..Step::default()
    }
}

/// A snapshot whose queue holds the given input ids.
pub fn snapshot(phase: Phase, queue: &[&str], at_ms: i64) -> Step {
    Step {
        snapshot: Some(Snapshot {
            kind: KIND as i32,
            phase: phase as i32,
            queue: queue
                .iter()
                .map(|id| QueuedInput {
                    input_id: id.as_bytes().to_vec(),
                    text: format!("prompt {id}"),
                    ..QueuedInput::default()
                })
                .collect(),
            working_on: Some("the task".to_owned()),
            at_ms,
            ..Snapshot::default()
        }),
        ..Step::default()
    }
}

// --- reading streams ----------------------------------------------------

/// A short name for a session event, for logs and comparisons.
pub fn describe(event: &SessionEvent) -> String {
    match event.of.as_ref().expect("an event") {
        session_event::Of::Snapshot(s) => format!(
            "snapshot r{} {:?} queue={:?}",
            s.revision,
            Phase::try_from(s.phase).unwrap_or(Phase::Starting),
            s.queue
                .iter()
                .map(|q| String::from_utf8_lossy(&q.input_id).into_owned())
                .collect::<Vec<_>>()
        ),
        session_event::Of::Item(i) => {
            format!("item {} o{} r{} {:?}", i.key, i.order, i.revision, i.text)
        }
        session_event::Of::Append(a) => {
            format!(
                "append {} r{} base r{} {:?}",
                a.key, a.revision, a.base_revision, a.text
            )
        }
        session_event::Of::CaughtUp(c) => format!("caught_up r{}", c.revision),
        session_event::Of::Lagged(_) => "lagged".to_owned(),
        session_event::Of::Reset(_) => "reset".to_owned(),
        session_event::Of::Detached(_) => "detached".to_owned(),
        session_event::Of::Opening(o) => format!("opening g{}", o.generation),
    }
}

/// Reads events until `done` holds for what was read; fails after
/// [`PATIENCE`] or when the stream ends first.
pub async fn read_until(
    subscription: &mut node::Subscription,
    seen: &mut Vec<SessionEvent>,
    what: &str,
    done: impl Fn(&[SessionEvent]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !done(seen) {
        let next = tokio::time::timeout_at(deadline, subscription.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}; saw {:#?}", log(seen)));
        match next {
            Some(event) => seen.push((*event).clone()),
            None => panic!("the stream ended before {what}; saw {:#?}", log(seen)),
        }
    }
}

/// Reads whatever arrives within `quiet` of the last event.
pub async fn drain(
    subscription: &mut node::Subscription,
    seen: &mut Vec<SessionEvent>,
    quiet: Duration,
) {
    while let Ok(Some(event)) = tokio::time::timeout(quiet, subscription.next()).await {
        seen.push((*event).clone());
    }
}

pub fn log(events: &[SessionEvent]) -> Vec<String> {
    events.iter().map(describe).collect()
}

pub fn caught_ups(events: &[SessionEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
        .count()
}

pub async fn read_inventory_until(
    subscription: &mut node::InventorySubscription,
    seen: &mut Vec<InventoryEvent>,
    what: &str,
    done: impl Fn(&[InventoryEvent]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !done(seen) {
        let next = tokio::time::timeout_at(deadline, subscription.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}; saw {seen:#?}"));
        match next {
            Some(event) => seen.push((*event).clone()),
            None => panic!("the inventory stream ended before {what}"),
        }
    }
}
