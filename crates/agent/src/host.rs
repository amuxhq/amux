//! The agent process's driver: the provider child, its terminal or
//! app-server socket, the hooks socket, the control socket, attach serving
//! and the journal writer around the agent core.
//!
//! Everything the process hears becomes an [`Input`] to
//! [`agent_core::Agent`], stepped with the clock as of that moment, and
//! every [`Output`] it asks for is carried out here, in order: facts-ring
//! lines, blobs and journal steps written inline, frames to the daemon,
//! writes to the provider, timers, jobs and the process's end. What the
//! lifecycle decides (grace, drain, stop and the one-shot exit) is the
//! core's; see its crate.
//!
//! Some of what the process does is not the agent's state and never meets
//! the core: raw terminal bytes go to the terminal log, keys and sizes an
//! attached terminal sends go straight to the provider, and the messaging
//! credentials terminal Claude's hooks report go to the provider that uses
//! them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_core::{
    Agent, AgentJob, ConnId, DumpReason, Input, JobId, JobResult, Output, Resumed, TimerId,
};
use core_ring::{DumpKind, DumpLimit, Ring};
use interpret::Interpreter;
use interpret::ring::Entry;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use wire::{AgentSpec, CtlFrame, ctl_frame};

use crate::clock::Clock;
use crate::dir::{self, PtyLog};
use crate::local_socket::{LocalListener, LocalStream};
use crate::provider::{Provider, ProviderEvent};
use crate::{AgentError, ExitCause, VERSION, attach, ctl};

/// Journal segment size, when the spec leaves it unset.
const DEFAULT_SEGMENT_BYTES: u64 = 1024 * 1024;
/// The facts ring's size across its two segments, when the spec leaves it
/// unset.
const DEFAULT_RING_BYTES: u64 = 4 * 1024 * 1024;
/// Terminal log segments: enough to rebuild a screen.
const PTY_SEGMENT_BYTES: u64 = 256 * 1024;
const PTY_SEGMENTS_KEPT: usize = 4;

/// How the agent's stderr names a dump its core asked for.
pub const AGENT_CORE_DUMP: &str = "the agent core: ";

/// The daemon's connection: its id, so frames for a replaced one are
/// dropped, and its outgoing frames.
struct Daemon {
    id: ConnId,
    frames: mpsc::UnboundedSender<CtlFrame>,
    /// The agent's Hello has been written to it: the daemon reads the Hello
    /// first and drops a connection that starts with anything else.
    greeted: bool,
}

pub(crate) struct Driver<I: Interpreter> {
    dir: PathBuf,
    spec: AgentSpec,
    clock: Arc<dyn Clock>,
    agent: Agent<I>,
    journal: journal::Writer,
    ring: Ring<Entry>,
    pty_log: Option<PtyLog>,
    /// Where the terminal log ends, for attached terminal clients.
    written: Option<watch::Sender<u64>>,
    provider: Option<Provider>,
    daemon: Option<Daemon>,
    connections: u64,
    /// The timers the core armed, by when they are due.
    timers: BTreeMap<TimerId, u64>,
    jobs: mpsc::UnboundedSender<(JobId, JobResult)>,
    /// The core's word that the process exits, and why.
    exit: Option<ExitCause>,
    /// The dumps the core asked for that were reported, by kind.
    dumps: DumpLimit,
    /// Dumps asked of the daemon while none was connected.
    asks: Asks,
}

/// The agent's own record already has a dump's shape on disk (the facts
/// ring and its checkpoints), and the daemon's bundle gathers it: so a dump
/// the core asks for is asked of the daemon, which writes the bundle. One
/// asked with no daemon connected, or before the Hello on a new connection
/// (the step of the clock before a dial can run over), waits, the newest of
/// each kind, and goes right after the next Hello.
#[derive(Debug, Default)]
struct Asks {
    waiting: BTreeMap<String, CtlFrame>,
}

impl Asks {
    /// Asks the connected daemon for a dump once it has the Hello, or keeps
    /// the ask until it does.
    fn ask(&mut self, daemon: Option<&Daemon>, reason: String, invariant: String) {
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::DumpAsked(wire::DumpAsked {
                reason,
                invariant: invariant.clone(),
            })),
        };
        match daemon {
            Some(daemon) if daemon.greeted && daemon.frames.send(frame.clone()).is_ok() => {}
            _ => {
                self.waiting.insert(invariant, frame);
            }
        }
    }

    /// The daemon has the agent's Hello: what waited goes now.
    fn hello_sent(&mut self, daemon: &Daemon) {
        for (_, frame) in std::mem::take(&mut self.waiting) {
            let _ = daemon.frames.send(frame);
        }
    }
}

/// Everything the driver listens to.
struct Channels {
    provider: mpsc::Receiver<ProviderEvent>,
    provider_tx: mpsc::Sender<ProviderEvent>,
    connections: mpsc::Receiver<LocalStream>,
    frames: mpsc::UnboundedReceiver<(ConnId, Option<CtlFrame>)>,
    frames_tx: mpsc::UnboundedSender<(ConnId, Option<CtlFrame>)>,
    /// What attached terminal clients type and their sizes.
    control: mpsc::UnboundedReceiver<attach::Control>,
    /// Jobs' results.
    jobs: mpsc::UnboundedReceiver<(JobId, JobResult)>,
}

/// A write the core asked for failed.
struct WriteFailed(String);

pub(crate) async fn run<I: Interpreter>(
    dir: PathBuf,
    spec: AgentSpec,
    clock: Arc<dyn Clock>,
) -> Result<ExitCause, AgentError> {
    let config = spec.config.clone().unwrap_or_default();
    let terminal = spec.kind() == wire::Kind::ClaudePty;

    // Listen first, so a daemon can dial in as soon as the process exists.
    let ctl = LocalListener::bind(&dir.join(dir::CTL_SOCK)).map_err(AgentError::Socket)?;
    let pty = if terminal || spec.kind() == wire::Kind::Codex {
        Some(LocalListener::bind(&dir.join(dir::PTY_SOCK)).map_err(AgentError::Socket)?)
    } else {
        None
    };
    let hooks = if terminal {
        Some(
            LocalListener::bind(&dir.join(dir::PRIVATE).join(dir::HOOKS_SOCK))
                .map_err(AgentError::Socket)?,
        )
    } else {
        None
    };

    let (provider_tx, provider_rx) = mpsc::channel(256);
    let (connections_tx, connections_rx) = mpsc::channel(4);
    let (frames_tx, frames_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
    let mut tasks = vec![tokio::spawn(accept(ctl, connections_tx))];
    if let Some(hooks) = hooks {
        tasks.push(tokio::spawn(hook_payloads(hooks, provider_tx.clone())));
    }

    let segment_bytes = match config.journal_segment_bytes {
        0 => DEFAULT_SEGMENT_BYTES,
        bytes => bytes,
    };
    let journal = journal::Writer::open(dir.join(dir::JOURNAL), segment_bytes)
        .map_err(AgentError::Journal)?;
    let pty_log = if terminal {
        Some(
            PtyLog::open(dir.join(dir::PTY), PTY_SEGMENT_BYTES, PTY_SEGMENTS_KEPT)
                .map_err(AgentError::Journal)?,
        )
    } else {
        None
    };
    // Terminal clients can connect as soon as the socket exists; they are
    // served once the terminal log they read is open.
    let written = pty_log.as_ref().map(|log| watch::channel(log.offset()).0);
    if let Some(pty) = pty {
        let how = match &written {
            Some(written) => attach::Serve::Files {
                pty: dir.join(dir::PTY),
                written: written.subscribe(),
                control: control_tx,
            },
            None => attach::Serve::Stream {
                spec: Arc::new(spec.clone()),
                dir: dir.clone(),
            },
        };
        tasks.push(tokio::spawn(attach::serve(pty, how)));
    }
    let facts = dir.join(dir::PRIVATE).join(dir::FACTS);
    let mut first = Vec::new();
    let agent = Agent::<I>::start(
        &spec,
        resumed::<I>(&facts),
        VERSION,
        now_ms(clock.as_ref()),
        &mut first,
    );
    let ring = Ring::open(
        facts,
        match config.facts_ring_bytes {
            0 => DEFAULT_RING_BYTES,
            bytes => bytes,
        },
        &agent.checkpoint().state,
    )
    .map_err(AgentError::Journal)?;
    let mut driver = Driver::<I> {
        dir,
        spec,
        clock,
        agent,
        journal,
        ring,
        pty_log,
        written,
        provider: None,
        daemon: None,
        connections: 0,
        timers: BTreeMap::new(),
        jobs: jobs_tx,
        exit: None,
        dumps: DumpLimit::default(),
        asks: Asks::default(),
    };
    // An incarnation that cannot write its first steps does not start.
    driver
        .execute(first)
        .await
        .map_err(|WriteFailed(why)| AgentError::Journal(std::io::Error::other(why)))?;

    let mut channels = Channels {
        provider: provider_rx,
        provider_tx,
        connections: connections_rx,
        frames: frames_rx,
        frames_tx,
        control: control_rx,
        jobs: jobs_rx,
    };
    let cause = driver.run(&mut channels).await;
    // The listeners live in these tasks; waiting for each to be dropped
    // removes the sockets before the lock is released, so a next
    // incarnation never has its socket removed by this one.
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    Ok(cause)
}

/// What the last incarnation left in the facts ring: the interpreter's
/// state at its newest checkpoint and the events after it. A ring that
/// cannot be read starts the agent over.
fn resumed<I: Interpreter>(facts: &std::path::Path) -> Option<Resumed<I::State>> {
    let saved = match core_ring::saved::<Entry>(facts) {
        Ok(saved) => saved?,
        Err(error) => {
            eprintln!("amux agent: reading the facts ring: {error}");
            return None;
        }
    };
    match interpret::decode_checkpoint::<I::State>(&saved.checkpoint) {
        Ok(state) => Some(Resumed {
            state,
            inputs: saved.inputs,
        }),
        Err(error) => {
            eprintln!("amux agent: reading the checkpoint: {error}");
            None
        }
    }
}

impl<I: Interpreter> Driver<I> {
    async fn run(&mut self, channels: &mut Channels) -> ExitCause {
        match Provider::spawn(&self.spec, &self.dir, channels.provider_tx.clone()).await {
            Ok(provider) => self.provider = Some(provider),
            Err(error) => {
                self.feed(Input::ProviderUnstarted {
                    why: error.to_string(),
                })
                .await;
            }
        }
        loop {
            if let Some(cause) = self.exit.take() {
                return cause;
            }
            let sleep = match self.timers.values().min() {
                Some(&at) => self
                    .clock
                    .sleep_until(i64::try_from(at).unwrap_or(i64::MAX)),
                None => Box::pin(std::future::pending()),
            };
            tokio::select! {
                event = channels.provider.recv() => {
                    // The driver holds a sender, so the channel never closes.
                    match event {
                        Some(ProviderEvent::Exited(code)) => {
                            self.last_words(&mut channels.provider).await;
                            self.feed(Input::ProviderExited { code }).await;
                        }
                        Some(event) => self.on_provider(event).await,
                        None => {}
                    }
                }
                Some(stream) = channels.connections.recv() => {
                    self.on_connect(stream, &channels.frames_tx).await;
                }
                Some((connection, frame)) = channels.frames.recv() => {
                    match frame {
                        Some(frame) => self.feed(Input::Control { connection, frame }).await,
                        None => {
                            if self.daemon.as_ref().is_some_and(|daemon| daemon.id == connection) {
                                self.daemon = None;
                            }
                            self.feed(Input::DaemonGone { connection }).await;
                        }
                    }
                }
                Some(control) = channels.control.recv() => {
                    if let Some(provider) = &self.provider {
                        match control {
                            attach::Control::Keys(keys) => provider.type_raw(keys),
                            attach::Control::Resize { rows, cols } => provider.resize(rows, cols),
                        }
                    }
                }
                Some((job, result)) = channels.jobs.recv() => {
                    self.feed(Input::JobDone { job, result }).await;
                }
                () = sleep => self.on_timers().await,
            }
        }
    }

    // --- inputs ----------------------------------------------------------

    /// Steps the core with `input` at the clock's time and carries out what
    /// it asks. A write that fails is fed back as the input that says so,
    /// in place of the rest of the step's outputs.
    async fn feed(&mut self, input: Input) {
        let mut next = Some(input);
        while let Some(input) = next.take() {
            if self.agent.exited().is_some() {
                return;
            }
            // A full segment rotates first, checkpointing the state the
            // input meets. A rotation that fails is a write that fails.
            if !matches!(input, Input::WriteFailed { .. })
                && self.ring.full()
                && let Err(error) = self.ring.rotate(&self.agent.checkpoint().state)
            {
                next = Some(write_failed(error.to_string()));
                continue;
            }
            let mut out = Vec::new();
            self.agent.step(
                Input::Tick {
                    now_ms: now_ms(self.clock.as_ref()),
                },
                &mut out,
            );
            self.agent.step(input, &mut out);
            if let Err(WriteFailed(why)) = self.execute(out).await {
                next = Some(write_failed(why));
            }
        }
    }

    /// Every timer that is due, in the order of their ids.
    /// Steps the timers due, soonest first, one at a time: a step may
    /// cancel or re-arm the next (a grace period ending cancels the drain
    /// and arms the provider's stop), so the map is read again after each.
    async fn on_timers(&mut self) {
        loop {
            let now = now_ms(self.clock.as_ref());
            let due = self
                .timers
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(id, at)| (*at, *id))
                .min();
            let Some((_, id)) = due else {
                return;
            };
            self.timers.remove(&id);
            self.feed(Input::Timer(id)).await;
        }
    }

    async fn on_provider(&mut self, event: ProviderEvent) {
        match event {
            ProviderEvent::Output(bytes) => {
                if let Some(log) = &mut self.pty_log {
                    if let Err(error) = log.append(&bytes) {
                        self.feed(write_failed(error.to_string())).await;
                        return;
                    }
                    if let Some(written) = &self.written {
                        written.send_replace(log.offset());
                    }
                }
            }
            ProviderEvent::Fact(fact) => self.feed(Input::Provider(fact)).await,
            ProviderEvent::Messaging(messaging) => {
                if let Some(provider) = &mut self.provider {
                    provider.set_messaging(messaging);
                }
            }
            ProviderEvent::WriteFailed { path, error } => {
                self.feed(write_failed(format!("{}: {error}", path.display())))
                    .await;
            }
            ProviderEvent::Exited(code) => self.feed(Input::ProviderExited { code }).await,
        }
    }

    /// Before a provider's exit is recorded, everything it said first:
    /// what is already queued, then the transcript rows no follower has
    /// read yet, since it may have written its last rows just before it
    /// exited.
    async fn last_words(&mut self, events: &mut mpsc::Receiver<ProviderEvent>) {
        let rows = match &mut self.provider {
            Some(provider) => provider.finish_transcripts().await,
            None => Vec::new(),
        };
        while let Ok(event) = events.try_recv() {
            if !matches!(event, ProviderEvent::Exited(_)) {
                self.on_provider(event).await;
            }
        }
        for row in rows {
            self.feed(Input::Provider(row)).await;
        }
    }

    async fn on_connect(
        &mut self,
        stream: LocalStream,
        frames_tx: &mpsc::UnboundedSender<(ConnId, Option<CtlFrame>)>,
    ) {
        self.connections += 1;
        let id = ConnId(self.connections);
        let (mut reader, mut writer) = tokio::io::split(stream);
        let incoming = frames_tx.clone();
        tokio::spawn(async move {
            loop {
                match ctl::read_frame(&mut reader).await {
                    Ok(Some(frame)) => {
                        if incoming.send((id, Some(frame))).is_err() {
                            return;
                        }
                    }
                    Ok(None) | Err(_) => {
                        let _ = incoming.send((id, None));
                        return;
                    }
                }
            }
        });
        let (frames, mut outgoing) = mpsc::unbounded_channel::<CtlFrame>();
        tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if ctl::write_frame(&mut writer, &frame).await.is_err() {
                    return;
                }
            }
        });
        // A new connection replaces the old: a restarted daemon dials in
        // while the old one's socket may not have closed yet.
        self.daemon = Some(Daemon {
            id,
            frames,
            greeted: false,
        });
        self.feed(Input::DaemonDials { connection: id }).await;
    }

    // --- outputs ---------------------------------------------------------

    /// Carries out one step's outputs, in order. When a facts-ring line, a
    /// blob or a journal step cannot be written, the rest are not carried
    /// out, and the ring gives back the line of the event whose step did
    /// not reach the journal, so it never claims more than the journal
    /// holds.
    async fn execute(&mut self, out: Vec<Output>) -> Result<(), WriteFailed> {
        // Where the ring stood before the newest event's line.
        let mut mark = None;
        for output in out {
            let written = match output {
                Output::Record(entry) => {
                    mark = Some(self.ring.offset());
                    self.ring.append(&entry)
                }
                Output::WriteBlob { hash, bytes } => dir::write_blob(&self.dir, &hash, &bytes),
                Output::AppendJournal(step) => self.journal.append(&step).map(|_| ()),
                Output::WriteProvider(effect) => {
                    if let Some(provider) = &mut self.provider
                        && let Err(error) = provider.perform(effect).await
                    {
                        // A provider that stopped reading is about to report
                        // its exit, which is what ends the agent.
                        eprintln!("amux agent: writing to the provider: {error}");
                    }
                    Ok(())
                }
                Output::WriteControl { connection, frame } => {
                    // Frames for a daemon that is no longer connected are
                    // dropped: whoever sent the input is gone with it.
                    if let Some(daemon) = &mut self.daemon
                        && daemon.id == connection
                    {
                        let hello = matches!(frame.of, Some(ctl_frame::Of::Hello(_)));
                        let _ = daemon.frames.send(frame);
                        if hello {
                            daemon.greeted = true;
                            self.asks.hello_sent(daemon);
                        }
                    }
                    Ok(())
                }
                Output::ArmTimer { id, at_ms } => {
                    self.timers.insert(id, at_ms);
                    Ok(())
                }
                Output::CancelTimer { id } => {
                    self.timers.remove(&id);
                    Ok(())
                }
                Output::Job { id, op } => {
                    self.job(id, op);
                    Ok(())
                }
                Output::CloseProvider => {
                    if let Some(provider) = &mut self.provider {
                        provider.close();
                    }
                    Ok(())
                }
                Output::KillProvider => {
                    if let Some(provider) = &mut self.provider {
                        provider.kill();
                    }
                    Ok(())
                }
                Output::Exit(cause) => {
                    // What a provider asked to finish left running below
                    // it ends with the agent.
                    if let Some(provider) = &mut self.provider {
                        provider.finish();
                    }
                    self.exit = Some(cause);
                    Ok(())
                }
                Output::Dump { reason } => {
                    let now = now_ms(self.clock.as_ref());
                    if let Some(said) = limited_dump_reason(&mut self.dumps, &reason, now) {
                        eprintln!("amux agent: {said}");
                        let invariant = match &reason {
                            DumpReason::Violation(violation) => violation.invariant.to_owned(),
                            DumpReason::OverBudget { .. } => String::new(),
                        };
                        self.asks.ask(self.daemon.as_ref(), said, invariant);
                    }
                    Ok(())
                }
            };
            if let Err(error) = written {
                if let Some(mark) = mark {
                    self.ring.undo(mark);
                }
                return Err(WriteFailed(error.to_string()));
            }
        }
        Ok(())
    }

    /// Runs a job off the step; its result comes back as an input.
    fn job(&self, id: JobId, op: AgentJob) {
        let done = self.jobs.clone();
        match op {
            AgentJob::ReadGit => {
                let cwd = PathBuf::from(&self.spec.cwd);
                let base = self.spec.base_branch.clone();
                tokio::spawn(async move {
                    let git = crate::git::read(&cwd, base.as_deref()).await;
                    let _ = done.send((id, JobResult::Git(git)));
                });
            }
            AgentJob::DumpPart { dump_id } => {
                let dir = self.dir.clone();
                tokio::task::spawn_blocking(move || {
                    let part = crate::dump::part::<I>(&dir, dump_id);
                    let _ = done.send((id, JobResult::DumpPart(part)));
                });
            }
        }
    }
}

/// A write to the agent directory failed, which in practice is a full
/// disk: whether the write was the journal's or the provider's own state
/// in private/ that the next incarnation resumes from, the core ends the
/// incarnation.
fn write_failed(why: String) -> Input {
    eprintln!("amux agent: could not write to its directory: {why}; exiting");
    Input::WriteFailed { why }
}

/// The reason to report for a dump the core asked for at `now_ms`, or
/// `None` when the limit holds it back: a provider stuck in a loop meets
/// the same broken invariant with every event.
fn limited_dump_reason(limit: &mut DumpLimit, reason: &DumpReason, now_ms: u64) -> Option<String> {
    let (kind, why) = match reason {
        DumpReason::Violation(violation) => (
            DumpKind::Violation(violation.invariant.into()),
            violation.to_string(),
        ),
        DumpReason::OverBudget { micros, budget } => (
            DumpKind::OverBudget,
            format!(
                "a step took {micros} µs against its budget of {} µs",
                budget.micros
            ),
        ),
    };
    let suppressed = limit.admit(kind, now_ms)?;
    Some(core_ring::with_suppressed(
        format!("{AGENT_CORE_DUMP}{why}"),
        suppressed,
    ))
}

/// The clock, as the core counts it: milliseconds, never before the epoch.
fn now_ms(clock: &dyn Clock) -> u64 {
    u64::try_from(clock.now_ms()).unwrap_or(0)
}

async fn accept(mut listener: LocalListener, connections: mpsc::Sender<LocalStream>) {
    while let Ok(stream) = listener.accept().await {
        if connections.send(stream).await.is_err() {
            return;
        }
    }
}

/// Each connection on hooks.sock carries one hook payload, then closes.
/// The hook binary wraps the payload with the messaging socket's
/// credentials when Claude gave it them; they go to the provider, and only
/// the payload Claude wrote becomes a fact. Payloads are read one
/// connection at a time, in the order Claude ran its hooks.
async fn hook_payloads(mut listener: LocalListener, events: mpsc::Sender<ProviderEvent>) {
    while let Ok(mut stream) = listener.accept().await {
        let mut bytes = Vec::new();
        if stream.read_to_end(&mut bytes).await.is_err() || bytes.is_empty() {
            continue;
        }
        let (payload, messaging) = claude::hooks::unwrap_forwarded(&bytes).unwrap_or((bytes, None));
        if let Some(messaging) = messaging
            && events
                .send(ProviderEvent::Messaging(messaging))
                .await
                .is_err()
        {
            return;
        }
        let fact = interpret::Fact {
            channel: interpret::Channel::Hook,
            payload: crate::notebook::annotate_hook(payload),
        };
        if events.send(ProviderEvent::Fact(fact)).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> (Daemon, mpsc::UnboundedReceiver<CtlFrame>) {
        let (frames, sent) = mpsc::unbounded_channel();
        (
            Daemon {
                id: ConnId(1),
                frames,
                greeted: true,
            },
            sent,
        )
    }

    fn asked(frame: CtlFrame) -> wire::DumpAsked {
        match frame.of {
            Some(ctl_frame::Of::DumpAsked(asked)) => asked,
            other => panic!("not a dump ask: {other:?}"),
        }
    }

    /// An admitted dump is asked of the connected daemon at once; with
    /// none connected, or one not yet sent the Hello, the newest of each
    /// kind waits for the next Hello.
    #[test]
    fn an_admitted_dump_is_asked_of_the_daemon_or_waits_for_its_hello() {
        let mut asks = Asks::default();
        let (connected, mut sent) = daemon();
        asks.ask(Some(&connected), "the agent core: one".into(), "one".into());
        let first = asked(sent.try_recv().expect("asked at once"));
        assert_eq!(
            (first.reason.as_str(), first.invariant.as_str()),
            ("the agent core: one", "one")
        );

        asks.ask(None, "the agent core: two, older".into(), "two".into());
        asks.ask(None, "the agent core: two".into(), "two".into());
        assert!(sent.try_recv().is_err(), "nothing goes with no daemon");
        // A daemon that has dialled in but not yet been sent the Hello
        // reads the Hello first.
        let (mut next, mut sent) = daemon();
        next.greeted = false;
        asks.ask(
            Some(&next),
            "the agent core: a slow step".into(),
            String::new(),
        );
        assert!(sent.try_recv().is_err(), "nothing goes before the Hello");
        next.greeted = true;
        asks.hello_sent(&next);
        let mut waited: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok().map(asked))
            .map(|asked| (asked.invariant, asked.reason))
            .collect();
        waited.sort();
        assert_eq!(
            waited,
            [
                (String::new(), "the agent core: a slow step".to_owned()),
                ("two".to_owned(), "the agent core: two".to_owned()),
            ]
        );
        asks.hello_sent(&next);
        assert!(sent.try_recv().is_err(), "each waited once");
    }

    #[test]
    fn a_violation_the_agent_core_repeats_is_reported_once_per_interval() {
        let broke = DumpReason::Violation(agent_core::Violation {
            invariant: "one open ask at most",
            detail: "two asks open".into(),
        });
        let mut limit = DumpLimit::default();
        let first = limited_dump_reason(&mut limit, &broke, 0).unwrap();
        assert!(first.starts_with(AGENT_CORE_DUMP), "{first}");
        for at in [1, DumpLimit::INTERVAL_MS - 1] {
            assert_eq!(limited_dump_reason(&mut limit, &broke, at), None);
        }
        let second = limited_dump_reason(&mut limit, &broke, DumpLimit::INTERVAL_MS).unwrap();
        assert!(
            second.ends_with("(2 more like it were not dumped since the last)"),
            "{second}"
        );
    }
}
