//! One agent's life: the interpreter between the provider and the journal,
//! the daemon's control connection, and the lifecycle that decides when the
//! process ends.
//!
//! Starting and running are the interpreter's business. The host adds what
//! only a process can know: whether a daemon is connected, how long it has
//! been gone, and whether this incarnation is on its way out.
//!
//! - With no daemon connected, at start or after ctl.sock's end of stream,
//!   a grace timer runs; a daemon dialling in cancels it. When it runs out
//!   the agent drains: it finishes the running turn, writes its final
//!   boundary and exits. An ask left open with nobody to answer it gets the
//!   drain deadline and then exits "orphaned while waiting for you".
//! - A stop cancels the turn the provider's way and drains the same way; a
//!   kill, the daemon's escalation past its stop deadline, ends the process
//!   group at once and writes nothing. A draining agent answers new input as
//!   `REFUSAL_DRAINING`.
//! - Asking the provider to finish sends it SIGTERM, on which every provider
//!   cleans up its commands; whatever still runs below it once it has
//!   exited is killed (see the tree module).
//! - An agent with a parent is one-shot: at a turn end with nothing queued,
//!   running or accepted-but-unconsumed, and no agent below it live (the
//!   daemon says how many are), it exits, and input arriving while it does
//!   is refused as `REFUSAL_EXITING`.
//! - The provider exiting ends the agent, whatever state it was in.

use std::path::PathBuf;
use std::sync::Arc;

use interpret::{Effect, Event, Interpreter, Stepped, reply};
use tokio::io::AsyncReadExt;
use tokio::sync::{Notify, mpsc, watch};
use wire::{AgentHello, AgentSpec, CtlFrame, Nudge, Phase, Verdict, ctl_frame};

use crate::clock::Clock;
use crate::dir::{self, PtyLog};
use crate::local_socket::{LocalListener, LocalStream};
use crate::provider::{Provider, ProviderEvent};
use crate::ring::{self, Entry, Ring};
use crate::{AgentError, ExitCause, VERSION, attach, ctl};

/// Grace after the daemon leaves, when the spec leaves it unset.
const DEFAULT_GRACE_MS: i64 = 5 * 60 * 1000;
/// Drain deadline for an orphaned ask, when the spec leaves it unset: the
/// installation setting's own default.
const DEFAULT_DRAIN_MS: i64 = 5 * 60 * 1000;
/// How long a provider asked to finish gets before its group is killed.
const PROVIDER_STOP_MS: i64 = 10 * 1000;
/// Journal segment size, when the spec leaves it unset.
const DEFAULT_SEGMENT_BYTES: u64 = 1024 * 1024;
/// The facts ring's size across its two segments, when the spec leaves it
/// unset.
const DEFAULT_RING_BYTES: u64 = 4 * 1024 * 1024;
/// Terminal log segments: enough to rebuild a screen.
const PTY_SEGMENT_BYTES: u64 = 256 * 1024;
const PTY_SEGMENTS_KEPT: usize = 4;

enum Mode {
    Running,
    Draining {
        why: Drain,
        /// Set while an ask is open during a drain with no daemon.
        ask_until: Option<i64>,
    },
    Exiting {
        cause: ExitCause,
        /// When the provider's group is killed if it has not exited.
        until: i64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// The daemon was gone past the grace: the running turn finishes.
    DaemonLost,
    /// The daemon stopped the agent: the turn was cancelled the provider's
    /// own way, and the agent exits once it has ended.
    Stop,
}

impl Drain {
    fn cause(self) -> ExitCause {
        match self {
            Drain::DaemonLost => ExitCause::DaemonLost,
            Drain::Stop => ExitCause::Stopped,
        }
    }
}

/// The daemon's connection: its id, so frames from a replaced one are
/// ignored, and its outgoing frames.
struct Daemon {
    id: u64,
    frames: mpsc::UnboundedSender<CtlFrame>,
}

pub(crate) struct Host<I: Interpreter> {
    dir: PathBuf,
    spec: AgentSpec,
    clock: Arc<dyn Clock>,
    state: I::State,
    journal: journal::Writer,
    ring: Ring,
    pty_log: Option<PtyLog>,
    /// Where the terminal log ends, for attached terminal clients.
    written: Option<watch::Sender<u64>>,
    provider: Option<Provider>,
    daemon: Option<Daemon>,
    connections: u64,
    grace_until: Option<i64>,
    mode: Mode,
    phase: Phase,
    queue_empty: bool,
    turn_ended: bool,
    /// The interpreter asked the process to exit.
    asked_to_exit: Option<ExitCause>,
    last_tick: i64,
    done: Option<ExitCause>,
    /// Asks for the folder's git facts to be read again.
    git_wanted: Arc<Notify>,
}

/// Everything the host listens to.
struct Channels {
    provider: mpsc::Receiver<ProviderEvent>,
    provider_tx: mpsc::Sender<ProviderEvent>,
    connections: mpsc::Receiver<LocalStream>,
    frames: mpsc::UnboundedReceiver<(u64, Option<CtlFrame>)>,
    frames_tx: mpsc::UnboundedSender<(u64, Option<CtlFrame>)>,
    /// What attached terminal clients type and their sizes.
    control: mpsc::UnboundedReceiver<attach::Control>,
    /// The folder's git facts, each time they are read.
    git: mpsc::UnboundedReceiver<Option<wire::Git>>,
}

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
    // Read once at start; each turn end asks again.
    let git_wanted = Arc::new(Notify::new());
    git_wanted.notify_one();
    let (git_tx, git_rx) = mpsc::unbounded_channel();
    tasks.push(crate::git::reader(
        PathBuf::from(&spec.cwd),
        spec.base_branch.clone(),
        git_wanted.clone(),
        git_tx,
    ));
    let facts = dir.join(dir::PRIVATE).join(dir::FACTS);
    let (state, first) = start::<I>(&facts, &spec);
    let ring = Ring::open(
        facts,
        match config.facts_ring_bytes {
            0 => DEFAULT_RING_BYTES,
            bytes => bytes,
        },
        &interpret::encode_checkpoint(&state),
    )
    .map_err(AgentError::Journal)?;
    let now = clock.now_ms();
    let mut host = Host::<I> {
        dir,
        grace_until: Some(now + ms(config.grace_ms, DEFAULT_GRACE_MS)),
        spec,
        clock,
        state,
        journal,
        ring,
        pty_log,
        written,
        provider: None,
        daemon: None,
        connections: 0,
        mode: Mode::Running,
        phase: Phase::Starting,
        queue_empty: true,
        turn_ended: false,
        asked_to_exit: None,
        last_tick: i64::MIN,
        done: None,
        git_wanted,
    };
    for step in first {
        host.apply(Stepped {
            step,
            effects: Vec::new(),
        })
        .await?;
    }

    let mut channels = Channels {
        provider: provider_rx,
        provider_tx,
        connections: connections_rx,
        frames: frames_rx,
        frames_tx,
        control: control_rx,
        git: git_rx,
    };
    let result = host.run(&mut channels).await;
    // The listeners live in these tasks; waiting for each to be dropped
    // removes the sockets before the lock is released, so a next
    // incarnation never has its socket removed by this one.
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    result
}

impl<I: Interpreter> Host<I> {
    async fn run(&mut self, channels: &mut Channels) -> Result<ExitCause, AgentError> {
        match Provider::spawn(&self.spec, &self.dir, channels.provider_tx.clone()).await {
            Ok(provider) => self.provider = Some(provider),
            Err(error) => {
                let cause = ExitCause::Unstarted(error.to_string());
                self.feed(Event::Exiting(cause.end())).await?;
                return Ok(cause);
            }
        }
        loop {
            let sleep = match self.deadline() {
                Some(at) => self.clock.sleep_until(at),
                None => Box::pin(std::future::pending()),
            };
            let handled = tokio::select! {
                event = channels.provider.recv() => {
                    // The host holds a sender, so the channel never closes.
                    match event {
                        Some(ProviderEvent::Exited(code)) => {
                            match self.last_words(&mut channels.provider).await {
                                Ok(()) => self.on_provider(ProviderEvent::Exited(code)).await,
                                Err(error) => Err(error),
                            }
                        }
                        Some(event) => self.on_provider(event).await,
                        None => Ok(()),
                    }
                }
                Some(stream) = channels.connections.recv() => {
                    self.on_connect(stream, &channels.frames_tx);
                    Ok(())
                }
                Some((id, frame)) = channels.frames.recv() => {
                    self.on_frame(id, frame).await
                }
                Some(control) = channels.control.recv() => {
                    if let Some(provider) = &self.provider {
                        match control {
                            attach::Control::Keys(keys) => provider.type_raw(keys),
                            attach::Control::Resize { rows, cols } => provider.resize(rows, cols),
                        }
                    }
                    Ok(())
                }
                Some(git) = channels.git.recv() => self.on_git(git).await,
                () = sleep => self.on_deadline().await,
            };
            if let Err(error) = handled {
                self.write_failed(error).await?;
            }
            if self.done.is_none()
                && let Err(error) = self.settle().await
            {
                self.write_failed(error).await?;
            }
            if let Some(cause) = self.done.take() {
                return Ok(cause);
            }
        }
    }

    // --- events ----------------------------------------------------------

    async fn on_git(&mut self, git: Option<wire::Git>) -> Result<(), AgentError> {
        if matches!(self.mode, Mode::Exiting { .. }) {
            // The final boundary is written; the incarnation is over.
            return Ok(());
        }
        self.feed(Event::Git(git)).await
    }

    async fn on_provider(&mut self, event: ProviderEvent) -> Result<(), AgentError> {
        match event {
            ProviderEvent::Output(bytes) => {
                if let Some(log) = &mut self.pty_log {
                    log.append(&bytes).map_err(AgentError::Journal)?;
                    if let Some(written) = &self.written {
                        written.send_replace(log.offset());
                    }
                }
            }
            ProviderEvent::Fact(fact) => {
                if matches!(self.mode, Mode::Exiting { .. }) {
                    // The final boundary is written; what the provider says
                    // while it shuts down is not part of the incarnation.
                    return Ok(());
                }
                self.feed(Event::Fact(fact)).await?;
            }
            ProviderEvent::Messaging(messaging) => {
                if let Some(provider) = &mut self.provider {
                    provider.set_messaging(messaging);
                }
            }
            ProviderEvent::WriteFailed { path, error } => {
                return Err(AgentError::Private {
                    path,
                    source: error,
                });
            }
            ProviderEvent::Exited(code) => {
                if let Mode::Exiting { cause, .. } = &self.mode {
                    if let Some(provider) = &mut self.provider {
                        provider.finish();
                    }
                    self.done = Some(cause.clone());
                    return Ok(());
                }
                self.feed(Event::ProviderExit { code }).await?;
                self.done = Some(ExitCause::ProviderExited(code));
            }
        }
        Ok(())
    }

    /// Before a provider's exit is recorded, everything it said first:
    /// what is already queued, then the transcript rows no follower has
    /// read yet, since it may have written its last rows just before it
    /// exited.
    async fn last_words(
        &mut self,
        events: &mut mpsc::Receiver<ProviderEvent>,
    ) -> Result<(), AgentError> {
        let rows = match &mut self.provider {
            Some(provider) => provider.finish_transcripts().await,
            None => Vec::new(),
        };
        while let Ok(event) = events.try_recv() {
            if !matches!(event, ProviderEvent::Exited(_)) {
                self.on_provider(event).await?;
            }
        }
        for row in rows {
            self.on_provider(ProviderEvent::Fact(row)).await?;
        }
        Ok(())
    }

    fn on_connect(
        &mut self,
        stream: LocalStream,
        frames_tx: &mpsc::UnboundedSender<(u64, Option<CtlFrame>)>,
    ) {
        self.connections += 1;
        let id = self.connections;
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
        self.daemon = Some(Daemon { id, frames });
        self.grace_until = None;
        self.send(ctl_frame::Of::Hello(AgentHello {
            agent_id: self.spec.agent_id.clone(),
            agent_version: VERSION.to_owned(),
        }));
    }

    async fn on_frame(&mut self, id: u64, frame: Option<CtlFrame>) -> Result<(), AgentError> {
        if self.daemon.as_ref().is_none_or(|daemon| daemon.id != id) {
            return Ok(());
        }
        let Some(frame) = frame else {
            // End of stream: the daemon is gone. Deliberate stops are Stop
            // frames, so this starts the grace timer.
            self.daemon = None;
            if matches!(self.mode, Mode::Running) {
                self.grace_until = Some(self.clock.now_ms() + self.grace_ms());
            }
            return Ok(());
        };
        match frame.of {
            // A dump is answered in any mode: a draining agent is often the
            // one someone wants to look at.
            Some(ctl_frame::Of::DumpRequest(request)) => {
                let part = crate::dump::part::<I>(&self.dir, request.dump_id);
                self.send(ctl_frame::Of::Dump(part));
            }
            Some(ctl_frame::Of::Input(input)) => {
                let id = input.input_id.clone();
                self.take(id, Event::Input(input)).await?
            }
            Some(ctl_frame::Of::AgentMessage(envelope)) => {
                let id = envelope.id.clone();
                self.take(id, Event::AgentMessage(envelope)).await?
            }
            // A rename that arrives once the agent is leaving has no thread
            // left to name.
            Some(ctl_frame::Of::Rename(rename)) => {
                if matches!(self.mode, Mode::Running) {
                    self.feed(Event::Rename { name: rename.name }).await?
                }
            }
            Some(ctl_frame::Of::Stop(stop)) => self.stop(stop.kill).await?,
            // Hello, nudges, dump parts and replies travel the other way.
            _ => {}
        }
        Ok(())
    }

    /// An input or agent message reaches the interpreter while the agent
    /// runs; once it is leaving, the verdict says so.
    async fn take(&mut self, id: Vec<u8>, event: Event) -> Result<(), AgentError> {
        match self.mode {
            Mode::Running => self.feed(event).await?,
            Mode::Draining { .. } => self.reply(id, reply::rejected(wire::Refusal::Draining)),
            Mode::Exiting { .. } => self.reply(id, reply::rejected(wire::Refusal::Exiting)),
        }
        Ok(())
    }

    /// The daemon's stop: the turn is cancelled the provider's own way and
    /// the agent drains; with `kill`, the provider's group ends at once.
    async fn stop(&mut self, kill: bool) -> Result<(), AgentError> {
        if kill {
            if let Some(provider) = &mut self.provider {
                provider.kill();
            }
            self.done = Some(ExitCause::Killed);
            return Ok(());
        }
        let stopping = matches!(
            self.mode,
            Mode::Draining {
                why: Drain::Stop,
                ..
            } | Mode::Exiting { .. }
        );
        if !stopping {
            self.feed(Event::StopRequested).await?;
            self.mode = Mode::Draining {
                why: Drain::Stop,
                ask_until: None,
            };
        }
        Ok(())
    }

    async fn on_deadline(&mut self) -> Result<(), AgentError> {
        let now = self.clock.now_ms();
        match &self.mode {
            Mode::Running => {
                if self.grace_until.is_some_and(|until| until <= now) {
                    self.grace_until = None;
                    self.feed(Event::DaemonLost).await?;
                    self.mode = Mode::Draining {
                        why: Drain::DaemonLost,
                        ask_until: None,
                    };
                }
            }
            Mode::Draining { ask_until, .. } => {
                if ask_until.is_some_and(|until| until <= now) {
                    self.exit(ExitCause::Orphaned).await?;
                }
            }
            Mode::Exiting { cause, until } => {
                if *until <= now {
                    self.done = Some(cause.clone());
                    if let Some(provider) = &mut self.provider {
                        provider.kill();
                    }
                }
            }
        }
        Ok(())
    }

    fn deadline(&self) -> Option<i64> {
        match &self.mode {
            Mode::Running if self.daemon.is_none() => self.grace_until,
            Mode::Running => None,
            Mode::Draining { ask_until, .. } => *ask_until,
            Mode::Exiting { until, .. } => Some(*until),
        }
    }

    /// Decides, after anything happened, whether this incarnation ends.
    async fn settle(&mut self) -> Result<(), AgentError> {
        let turn_ended = std::mem::take(&mut self.turn_ended);
        let asked = self.asked_to_exit.take();
        let now = self.clock.now_ms();
        let drain_ms = self.drain_ms();
        let quiescent = self.quiescent();
        let one_shot = self.spec.parent.is_some();
        let turn_running = matches!(self.phase, Phase::Working | Phase::NeedsYou);
        let phase = self.phase;
        let cause = match &mut self.mode {
            Mode::Exiting { .. } => None,
            _ if asked.is_some() => asked,
            Mode::Running => (turn_ended && one_shot && quiescent).then_some(ExitCause::Finished),
            Mode::Draining { why, ask_until } => {
                if turn_ended || !turn_running {
                    Some(why.cause())
                } else {
                    if *why == Drain::DaemonLost && phase == Phase::NeedsYou {
                        ask_until.get_or_insert(now + drain_ms);
                    } else {
                        *ask_until = None;
                    }
                    None
                }
            }
        };
        if let Some(cause) = cause {
            self.exit(cause).await?;
        }
        Ok(())
    }

    /// Nothing runs, nothing is queued and no accepted agent message waits
    /// to be consumed.
    fn quiescent(&self) -> bool {
        self.phase == Phase::Idle && self.queue_empty && I::pending_messages(&self.state).is_empty()
    }

    /// Writes the final boundary and asks the provider to finish.
    async fn exit(&mut self, cause: ExitCause) -> Result<(), AgentError> {
        self.feed(Event::Exiting(cause.end())).await?;
        if let Some(provider) = &mut self.provider {
            provider.close();
        }
        self.mode = Mode::Exiting {
            cause,
            until: self.clock.now_ms() + PROVIDER_STOP_MS,
        };
        Ok(())
    }

    /// A write to the agent directory failed, which in practice is a full
    /// disk: nothing the provider does from here can be recorded, so the
    /// incarnation ends now, whether the write was the journal's or the
    /// provider's own state in private/ that the next incarnation resumes
    /// from. The provider is stopped and the final boundary written if the
    /// disk takes it; if it does not, the ring holds nothing the journal
    /// lacks, so the next incarnation writes the missing boundary when it
    /// starts. Any other error ends the agent.
    async fn write_failed(&mut self, error: AgentError) -> Result<(), AgentError> {
        let why = match error {
            AgentError::Journal(error) => error.to_string(),
            AgentError::Private { path, source } => {
                format!("{}: {source}", path.display())
            }
            other => return Err(other),
        };
        eprintln!("amux agent: could not write to its directory: {why}; exiting");
        let cause = ExitCause::WriteFailed(why);
        if matches!(self.mode, Mode::Exiting { .. }) || self.exit(cause.clone()).await.is_err() {
            if let Some(provider) = &mut self.provider {
                provider.kill();
            }
            self.done = Some(cause);
        }
        Ok(())
    }

    // --- the interpreter -------------------------------------------------

    async fn feed(&mut self, event: Event) -> Result<(), AgentError> {
        let now = self.clock.now_ms();
        if now > self.last_tick {
            self.last_tick = now;
            self.feed_one(Event::Tick { at_ms: now }).await?;
        }
        self.feed_one(event).await
    }

    /// One event through the ring, the interpreter and the journal. An
    /// event whose step could not be journaled leaves the ring too, so the
    /// ring never claims more than the journal holds.
    async fn feed_one(&mut self, event: Event) -> Result<(), AgentError> {
        let mark = self.ring.offset();
        let stepped = self.step(event)?;
        let applied = self.apply(stepped).await;
        if applied.is_err() {
            self.ring.undo(mark);
        }
        applied
    }

    /// Records the event in the facts ring, then steps the interpreter. A
    /// full segment rotates first, checkpointing the state the event meets.
    fn step(&mut self, event: Event) -> Result<Stepped, AgentError> {
        if self.ring.full() {
            self.ring
                .rotate(&interpret::encode_checkpoint(&self.state))
                .map_err(AgentError::Journal)?;
        }
        self.ring
            .append(&Entry::of(&event))
            .map_err(AgentError::Journal)?;
        Ok(I::step(&mut self.state, event))
    }

    /// Blobs first, then the journal, then the effects: a
    /// reply is sent and a provider written to only once the step that
    /// explains it is on disk, and a step never names bytes that are not.
    async fn apply(&mut self, stepped: Stepped) -> Result<(), AgentError> {
        let Stepped { step, effects } = stepped;
        for effect in &effects {
            let written = match effect {
                Effect::WriteBlob { hash, bytes } => dir::write_blob(&self.dir, hash, bytes),
                _ => Ok(()),
            };
            written.map_err(AgentError::Journal)?;
        }
        let before = self.journal.offset();
        self.journal.append(&step).map_err(AgentError::Journal)?;
        if let Some(snapshot) = &step.snapshot {
            self.phase = snapshot.phase();
            self.queue_empty = snapshot.queue.is_empty();
        }
        if step.turn_end.is_some() {
            self.turn_ended = true;
            // A turn changes the working tree; the row's totals follow.
            self.git_wanted.notify_one();
        }
        if self.journal.offset() > before {
            self.send(ctl_frame::Of::Nudge(Nudge {}));
        }
        for effect in effects {
            match effect {
                Effect::WriteBlob { .. } => {}
                Effect::Reply { input_id, verdict } => self.reply(input_id, verdict),
                Effect::Exit { cause } => {
                    self.asked_to_exit
                        .get_or_insert(ExitCause::Interpreter(cause));
                }
                effect => {
                    if let Some(provider) = &mut self.provider
                        && let Err(error) = provider.perform(effect).await
                    {
                        // A provider that stopped reading is about to report
                        // its exit, which is what ends the agent.
                        eprintln!("amux agent: writing to the provider: {error}");
                    }
                }
            }
        }
        Ok(())
    }

    // --- the daemon ------------------------------------------------------

    /// A verdict, keyed by the input id or the envelope id it answers.
    fn reply(&self, id: Vec<u8>, verdict: wire::SendInputResponse) {
        self.send(ctl_frame::Of::Reply(Verdict {
            id,
            verdict: Some(verdict),
        }));
    }

    /// Frames for a daemon that is not connected are dropped: whoever sent
    /// the input is gone with it.
    fn send(&self, of: ctl_frame::Of) {
        if let Some(daemon) = &self.daemon {
            let _ = daemon.frames.send(CtlFrame { of: Some(of) });
        }
    }

    fn grace_ms(&self) -> i64 {
        ms(
            self.spec
                .config
                .as_ref()
                .map_or(0, |config| config.grace_ms),
            DEFAULT_GRACE_MS,
        )
    }

    fn drain_ms(&self) -> i64 {
        ms(
            self.spec
                .config
                .as_ref()
                .map_or(0, |config| config.drain_ms),
            DEFAULT_DRAIN_MS,
        )
    }
}

/// The interpreter's state for this incarnation and the steps it starts
/// with. A first incarnation starts from nothing. A later one continues the
/// state the last one ended in, rebuilt from the ring's newest checkpoint
/// and the events after it, so its item keys carry on from there. An
/// incarnation that ended without its final boundary (killed, or crashed)
/// gets it now, then the resume step re-emits whatever is still open in
/// full. A ring that cannot be read starts over.
fn start<I: Interpreter>(facts: &std::path::Path, spec: &AgentSpec) -> (I::State, Vec<wire::Step>) {
    let fresh = || {
        let (state, step) = I::initial(spec, VERSION);
        (state, vec![step])
    };
    let saved = match ring::saved(facts) {
        Ok(Some(saved)) => saved,
        Ok(None) => return fresh(),
        Err(error) => {
            eprintln!("amux agent: reading the facts ring: {error}");
            return fresh();
        }
    };
    let mut state = match interpret::decode_checkpoint::<I::State>(&saved.checkpoint) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("amux agent: reading the checkpoint: {error}");
            return fresh();
        }
    };
    let ended = saved
        .entries
        .last()
        .is_none_or(|entry| matches!(entry, Entry::ProviderExit { .. } | Entry::Exiting { .. }));
    for event in saved.entries.into_iter().filter_map(Entry::event) {
        let _ = I::step(&mut state, event);
    }
    let mut steps = Vec::new();
    if !ended {
        let mut stepped = I::step(
            &mut state,
            Event::Exiting(wire::IncarnationEnd {
                cause: wire::ExitCause::EndedUnexpectedly as i32,
                detail: None,
            }),
        );
        // The incarnation this boundary closes was marked exited, with the
        // daemon's own cause, when its process was found gone; a cause here
        // would be read as this incarnation's.
        stepped.step.incarnation_end = None;
        steps.push(stepped.step);
    }
    let (state, resume) = I::reincarnate(state, spec, VERSION);
    steps.push(resume);
    (state, steps)
}

fn ms(configured: u32, default: i64) -> i64 {
    match configured {
        0 => default,
        ms => i64::from(ms),
    }
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
            payload,
        };
        if events.send(ProviderEvent::Fact(fact)).await.is_err() {
            return;
        }
    }
}
