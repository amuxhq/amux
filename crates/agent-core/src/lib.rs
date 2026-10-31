//! The agent process around its interpreter, as one function of its state
//! and an input.
//!
//! [`Agent::step`] takes one decoded [`Input`] (a provider fact, the
//! daemon's arrival or departure, a control frame, a timer, a job's result)
//! and appends the [`Output`]s it wants done. Facts-ring lines, blobs and
//! journal steps are outputs the driver writes inline, in order; provider
//! writes, socket writes and the process's end are the driver's too. Every
//! step ends by checking the cheap invariants, and a violation is reported
//! as [`Output::Dump`]. See the principles in `docs/ARCHITECTURE.md`.
//!
//! The lifecycle the core decides:
//!
//! - With no daemon connected, at start or after its connection ends, a
//!   grace timer runs; a daemon dialling in cancels it. When it runs out
//!   the agent drains: it finishes the running turn, writes its final
//!   boundary and exits. An ask left open with nobody to answer it gets the
//!   drain deadline and then exits "orphaned while waiting for you".
//! - A stop cancels the turn the provider's way and drains the same way; a
//!   kill, the daemon's escalation past its stop deadline, ends the process
//!   group at once and writes nothing. A draining agent answers new input as
//!   `REFUSAL_DRAINING`.
//! - Every decision to end goes through one action: the interpreter writes
//!   the final boundary, the provider is asked to finish, and the agent
//!   waits for it to exit, killing it past a deadline.
//! - An agent with a parent is one-shot: at a turn end with nothing queued,
//!   running or accepted-but-unconsumed, and the last report of every child
//!   incarnation the daemon named live already taken, it exits, and input
//!   arriving while it does is refused as `REFUSAL_EXITING`, on which the
//!   daemon resumes it with that input. A turn end after which the
//!   incarnation exits is marked last, so its parent can tell its final
//!   report from an earlier one.
//! - The provider exiting ends the agent, whatever state it was in.

#![forbid(unsafe_code)]

mod awaited;
mod start;

use std::collections::BTreeSet;
use std::fmt;
use std::time::Instant;

use interpret::ring::Entry;
use interpret::{Effect, Event, Interpreter, Stepped, reply};
use wire::{AgentHello, AgentSpec, CtlFrame, Nudge, Phase, Step, Verdict, ctl_frame};

use crate::awaited::Awaited;
pub use crate::start::Resumed;

/// A provider fact exactly as it arrived.
pub type ProviderEvent = interpret::Fact;

/// Grace after the daemon leaves, when the spec leaves it unset.
pub const DEFAULT_GRACE_MS: u64 = 5 * 60 * 1000;
/// Drain deadline for an orphaned ask, when the spec leaves it unset: the
/// installation setting's own default.
pub const DEFAULT_DRAIN_MS: u64 = 5 * 60 * 1000;
/// How long a provider asked to finish gets before its group is killed.
pub const PROVIDER_STOP_MS: u64 = 10 * 1000;

/// A timer the core asks the driver to arm. One of each at a time: arming
/// one replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TimerId {
    /// No daemon has been connected for the grace.
    Grace,
    /// An ask left open while draining for a lost daemon has waited its
    /// deadline.
    Drain,
    /// A provider asked to finish has not exited.
    ProviderStop,
}

/// A job the driver runs; its outcome returns as [`Input::JobDone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// One connection from a daemon to the agent's control socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// Everything that can happen to the agent process, decoded by the driver.
/// The driver feeds [`Input::Tick`] with its clock before every other
/// input, so the core's time is the time that input happened.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A provider fact, as it arrived. Before the provider's exit the driver
    /// feeds everything the provider said first.
    Provider(ProviderEvent),
    /// The provider child could not be started.
    ProviderUnstarted {
        why: String,
    },
    /// The provider child exited.
    ProviderExited {
        code: Option<i32>,
    },
    /// A daemon connected to the control socket. A new connection replaces
    /// the old: a restarted daemon dials in while the old one's socket may
    /// not have closed yet.
    DaemonDials {
        connection: ConnId,
    },
    /// A daemon's connection closed. Deliberate stops are Stop frames, so
    /// this starts the grace timer.
    DaemonGone {
        connection: ConnId,
    },
    /// A frame from a daemon: a person's input, an agent message, a stop, a
    /// rename, the live children, a dump request.
    Control {
        connection: ConnId,
        frame: CtlFrame,
    },
    Timer(TimerId),
    /// The clock, as an input.
    Tick {
        now_ms: u64,
    },
    /// A write the driver performed for the core failed: a facts-ring line,
    /// a blob, a journal step, or the provider's own state in `private/`
    /// that the next incarnation resumes from. In practice the disk is full.
    WriteFailed {
        why: String,
    },
    JobDone {
        job: JobId,
        result: JobResult,
    },
}

/// Everything the agent process wants done, in order.
///
/// `Record`, `WriteBlob` and `AppendJournal` are written inline. When one
/// fails, the driver performs none of the outputs after it, takes back the
/// facts-ring lines recorded since the last step that reached the journal,
/// so the ring never claims more than the journal holds, and feeds
/// [`Input::WriteFailed`]. A reply is sent and a provider written to only
/// once the step that explains it is on disk.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Append the event the interpreter is about to be fed to the facts
    /// ring.
    Record(Entry),
    /// Write `bytes` to the agent's blob directory under the hex of `hash`,
    /// before the journal step that names it.
    WriteBlob {
        hash: Vec<u8>,
        bytes: Vec<u8>,
    },
    /// Append the step to the journal.
    AppendJournal(Step),
    /// Hand the provider what the interpreter asked of it: bytes, keys, an
    /// injected message, a transcript to follow. Never a reply, a blob or
    /// an exit, which are outputs of their own.
    WriteProvider(Effect),
    WriteControl {
        connection: ConnId,
        frame: CtlFrame,
    },
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    CancelTimer {
        id: TimerId,
    },
    Job {
        id: JobId,
        op: AgentJob,
    },
    /// Ask the provider to finish the way it cleans up after itself; what
    /// still runs below it once it has exited is killed.
    CloseProvider,
    /// Kill the provider's process group and everything below it, at once.
    KillProvider,
    /// The process exits; the cause is the journal's last step, except
    /// after a kill or a write that failed, which write nothing.
    Exit(ExitCause),
    /// Take a dump: the core saw something it must not carry on from.
    Dump {
        reason: DumpReason,
    },
}

/// A result-only mechanism the driver runs off the core's step.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentJob {
    /// Read what the agent's folder says about its repository.
    ReadGit,
    /// Gather the agent's part of a dump bundle.
    DumpPart { dump_id: Vec<u8> },
}

/// What a job produced.
#[derive(Clone, Debug, PartialEq)]
pub enum JobResult {
    /// The folder's git facts; None outside a repository.
    Git(Option<wire::Git>),
    DumpPart(wire::DumpPart),
}

/// Why an agent process ended: the cause its final step records, on the
/// boundary and as the incarnation's end the daemon puts on the row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExitCause {
    /// The provider exited on its own, with its code when it had one.
    ProviderExited(Option<i32>),
    /// The daemon went away and did not come back within the grace.
    DaemonLost,
    /// An ask stayed open past the drain deadline with no daemon to carry an
    /// answer.
    Orphaned,
    /// Stopped: the turn was cancelled the provider's own way, then the
    /// agent exited.
    Stopped,
    /// Killed: the process group ended at once; nothing more was written.
    Killed,
    /// One-shot: a child agent finished its work.
    Finished,
    /// The provider could not be started.
    Unstarted(String),
    /// The interpreter ended the incarnation.
    Interpreter(String),
    /// A write to the agent directory failed, most often because the disk
    /// is full.
    WriteFailed(String),
}

impl ExitCause {
    /// The cause as the wire carries it, with its own words as the detail.
    pub fn end(&self) -> wire::IncarnationEnd {
        let (cause, detail) = match self {
            ExitCause::ProviderExited(code) => return interpret::provider_exit(*code),
            ExitCause::DaemonLost => (wire::ExitCause::DaemonLost, None),
            ExitCause::Orphaned => (wire::ExitCause::Orphaned, None),
            ExitCause::Stopped => (wire::ExitCause::Stopped, None),
            ExitCause::Killed => (wire::ExitCause::Killed, None),
            ExitCause::Finished => (wire::ExitCause::Finished, None),
            ExitCause::Unstarted(why) => (
                wire::ExitCause::CouldNotStart,
                Some(format!("the provider: {why}")),
            ),
            // The interpreter's words, such as Codex refusing a thread, are
            // all that says why.
            ExitCause::Interpreter(why) => (wire::ExitCause::EndedUnexpectedly, Some(why.clone())),
            ExitCause::WriteFailed(why) => (
                wire::ExitCause::DirectoryFailed,
                Some(format!("could not write to it: {why}")),
            ),
        };
        wire::IncarnationEnd {
            cause: cause as i32,
            detail,
        }
    }

    /// Decided by the lifecycle itself, which may end an incarnation only
    /// at a turn end or a drain deadline; the other causes are things that
    /// happened to it.
    fn decided_by_the_lifecycle(&self) -> bool {
        matches!(
            self,
            ExitCause::DaemonLost | ExitCause::Orphaned | ExitCause::Stopped | ExitCause::Finished
        )
    }
}

/// The spec's word on this incarnation's lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Params {
    agent_id: Vec<u8>,
    /// The agent has a parent: it exits when its work is done.
    one_shot: bool,
    grace_ms: u64,
    drain_ms: u64,
    /// How long one step may take before it asks for a dump.
    budget: StepBudget,
}

impl Params {
    fn of(spec: &AgentSpec) -> Self {
        let config = spec.config.clone().unwrap_or_default();
        let ms = |configured: u32, default: u64| match configured {
            0 => default,
            ms => u64::from(ms),
        };
        Self {
            agent_id: spec.agent_id.clone(),
            one_shot: spec.parent.is_some(),
            grace_ms: ms(config.grace_ms, DEFAULT_GRACE_MS),
            drain_ms: ms(config.drain_ms, DEFAULT_DRAIN_MS),
            budget: match config.step_budget_micros {
                0 => StepBudget::AGENT,
                micros => StepBudget { micros },
            },
        }
    }
}

/// The process's lifecycle; not the phase a client shows, which the
/// interpreter derives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Mode {
    /// Ordinary life: inputs go to the interpreter.
    Running,
    /// On its way out once the turn in progress ends.
    Draining(Drain),
    /// The final boundary is written and the provider asked to finish; it
    /// is killed at `until` if it has not exited.
    Exiting { cause: ExitCause, until: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// Inputs accepted and not yet taken by the provider: the queue, by input
/// id, as the journal's newest snapshot states it. An input is taken when
/// the journal records it, on an item that carries its id. Agent messages
/// are not here: each is handed to the provider at once, and its item is
/// the record that it arrived, not that the provider took it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pending {
    queued: BTreeSet<Vec<u8>>,
}

/// The ask the drain deadline waits on: an ask left open, while the agent
/// drains for a lost daemon, with nobody to answer it. However many asks
/// the provider holds open, the agent keeps one deadline for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpenAsk {
    /// When the agent exits orphaned if the ask is still open.
    until: u64,
}

/// What the journal says of the turn, and what this step did to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Turn {
    /// The phase of the journal's newest snapshot.
    phase: Phase,
    /// A turn ended in this step.
    ended: bool,
    /// The turn that ended was marked this one-shot incarnation's last: the
    /// step ends by exiting it finished.
    finishing: bool,
}

impl Default for Turn {
    fn default() -> Self {
        Self {
            phase: Phase::Starting,
            ended: false,
            finishing: false,
        }
    }
}

impl Turn {
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Working | Phase::NeedsYou)
    }
}

/// The folder's git facts are read at start and at each turn end; turn
/// ends during a read ask for one more after it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct GitReads {
    reading: Option<JobId>,
    again: bool,
}

/// What this step did that the invariants judge. Not carried from one
/// step to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Touched {
    /// The input ids the journal recorded as taken in this step.
    taken: Vec<Vec<u8>>,
    /// The lifecycle's decision to end the incarnation, made in this step.
    decided: Option<Decided>,
}

/// What the lifecycle saw when it decided to end the incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Decided {
    cause: ExitCause,
    turn_ended: bool,
    turn_running: bool,
    deadline_passed: bool,
}

/// The incarnation, the grace and drain timers, the pending set, the open
/// ask and the turn around one interpreter.
pub struct Agent<I: Interpreter> {
    interpreter: I::State,
    life: Life,
    touched: Touched,
}

/// Everything of the core between steps but the interpreter's state: what
/// the lifecycle decides from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Life {
    incarnation: u32,
    params: Params,
    now_ms: u64,
    /// The interpreter's clock: the last tick it was fed.
    ticked: Option<u64>,
    connection: Option<ConnId>,
    /// When the grace runs out, while no daemon is connected.
    grace: Option<u64>,
    mode: Mode,
    pending: Pending,
    /// The drain timer: armed while an ask waits with nobody to answer it.
    ask: Option<OpenAsk>,
    turn: Turn,
    awaited: Awaited,
    /// The interpreter asked the process to exit.
    asked_to_exit: Option<ExitCause>,
    git: GitReads,
    next_job: u64,
    /// The process exits: nothing more happens to this incarnation.
    exited: Option<ExitCause>,
}

impl<I: Interpreter> Agent<I> {
    /// The agent's incarnation under `spec`, at `now_ms`, from what the last
    /// incarnation left in the facts ring, if anything: its first journal
    /// steps, the grace timer (no daemon is connected yet) and the first
    /// read of the folder's git facts are appended to `out`. The driver
    /// then starts the provider child.
    pub fn start(
        spec: &AgentSpec,
        resumed: Option<Resumed<I::State>>,
        now_ms: u64,
        out: &mut Vec<Output>,
    ) -> Self {
        let (interpreter, first) = start::starting::<I>(spec, resumed);
        let params = Params::of(spec);
        let grace = now_ms.saturating_add(params.grace_ms);
        let mut agent = Self {
            interpreter,
            life: Life {
                incarnation: spec.incarnation,
                params,
                now_ms,
                ticked: None,
                connection: None,
                grace: Some(grace),
                mode: Mode::Running,
                pending: Pending::default(),
                ask: None,
                turn: Turn::default(),
                awaited: Awaited::default(),
                asked_to_exit: None,
                git: GitReads::default(),
                next_job: 0,
                exited: None,
            },
            touched: Touched::default(),
        };
        for step in first {
            agent.apply(
                Stepped {
                    step,
                    effects: Vec::new(),
                },
                out,
            );
        }
        out.push(Output::ArmTimer {
            id: TimerId::Grace,
            at_ms: grace,
        });
        agent.read_git(out);
        agent.touched = Touched::default();
        agent
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`], and so does a step
    /// that took longer than its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        // The step's own duration is the one reading of the world a core
        // makes: it decides nothing but whether to ask for a dump.
        let started = Instant::now();
        self.touched = Touched::default();
        if self.life.exited.is_none() {
            self.take_input(input, out);
            if self.life.exited.is_none() {
                self.settle(out);
            }
        }
        let checked = self.check_step_invariants();
        out.extend(
            core_ring::finish_step(started, self.life.params.budget, 0, checked)
                .map(|reason| Output::Dump { reason }),
        );
    }

    /// The incarnation this core runs.
    pub fn incarnation(&self) -> u32 {
        self.life.incarnation
    }

    /// Why the process exits, once it does.
    pub fn exited(&self) -> Option<&ExitCause> {
        self.life.exited.as_ref()
    }

    /// Why the incarnation ends, once the lifecycle has decided it does:
    /// from the decision until the process exits.
    pub fn exiting(&self) -> Option<&ExitCause> {
        match &self.life.mode {
            Mode::Exiting { cause, .. } => Some(cause),
            Mode::Running | Mode::Draining(_) => None,
        }
    }

    /// The whole core between steps as a value, which a model clones,
    /// hashes and compares: the interpreter's state as its checkpoint
    /// bytes, beside the lifecycle.
    pub fn value(&self) -> AgentValue {
        AgentValue {
            interpreter: interpret::encode_checkpoint(&self.interpreter),
            life: self.life.clone(),
        }
    }

    /// The core a [`value`](Self::value) was taken of, ready for its next
    /// step.
    pub fn from_value(value: &AgentValue) -> Result<Self, RestoreError> {
        Ok(Self {
            interpreter: interpret::decode_checkpoint(&value.interpreter)
                .map_err(|error| RestoreError(error.to_string()))?,
            life: value.life.clone(),
            touched: Touched::default(),
        })
    }

    /// The cheap predicates checked after every step, each in constant time
    /// per input taken or decision made:
    ///
    /// - the lifecycle ends an incarnation only at a turn end, with no turn
    ///   running, or at a drain deadline;
    /// - the pending set never holds an input the journal recorded as taken;
    /// - the drain timer waits on one open ask at most, and only while the
    ///   agent drains for a lost daemon with an ask open.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        if let Some(decided) = &self.touched.decided
            && decided.cause.decided_by_the_lifecycle()
            && decided.turn_running
            && !decided.turn_ended
            && !decided.deadline_passed
        {
            return Err(Violation {
                invariant: EXIT_OUTSIDE_TURN_END,
                detail: format!("{:?} decided while a turn ran", decided.cause),
            });
        }
        if let Some(taken) = self
            .touched
            .taken
            .iter()
            .find(|id| self.life.pending.queued.contains(*id))
        {
            return Err(Violation {
                invariant: TAKEN_STILL_PENDING,
                detail: format!(
                    "input {} is taken and still queued",
                    interpret::to_hex(taken)
                ),
            });
        }
        if let Some(ask) = &self.life.ask {
            let orphanable = self.life.mode == Mode::Draining(Drain::DaemonLost)
                && self.life.turn.phase == Phase::NeedsYou;
            if !orphanable {
                return Err(Violation {
                    invariant: DEADLINE_WITHOUT_ORPHAN,
                    detail: format!(
                        "deadline {} armed in {:?} at phase {:?}",
                        ask.until, self.life.mode, self.life.turn.phase
                    ),
                });
            }
        }
        Ok(())
    }

    /// The core as of this step, serialized: the interpreter's state, which
    /// is what the next incarnation resumes from. The lifecycle starts over
    /// with every incarnation.
    pub fn checkpoint(&self) -> Vec<u8> {
        interpret::encode_checkpoint(&self.interpreter)
    }

    // --- inputs ----------------------------------------------------------

    fn take_input(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Tick { now_ms } => self.life.now_ms = self.life.now_ms.max(now_ms),
            Input::Provider(fact) => {
                // Once the final boundary is written, what the provider says
                // while it shuts down is not part of the incarnation.
                if !matches!(self.life.mode, Mode::Exiting { .. }) {
                    self.feed(Event::Fact(fact), out);
                }
            }
            Input::ProviderUnstarted { why } => {
                let cause = ExitCause::Unstarted(why);
                self.feed(Event::Exiting(cause.end()), out);
                self.finish(cause, out);
            }
            Input::ProviderExited { code } => match &self.life.mode {
                Mode::Exiting { cause, .. } => {
                    let cause = cause.clone();
                    self.finish(cause, out);
                }
                _ => {
                    self.feed(Event::ProviderExit { code }, out);
                    self.finish(ExitCause::ProviderExited(code), out);
                }
            },
            Input::DaemonDials { connection } => {
                self.life.connection = Some(connection);
                if self.life.grace.take().is_some() {
                    out.push(Output::CancelTimer { id: TimerId::Grace });
                }
                self.send(
                    ctl_frame::Of::Hello(AgentHello {
                        agent_id: self.life.params.agent_id.clone(),
                    }),
                    out,
                );
            }
            Input::DaemonGone { connection } => {
                if self.life.connection != Some(connection) {
                    return;
                }
                self.life.connection = None;
                if self.life.mode == Mode::Running {
                    let at_ms = self.life.now_ms.saturating_add(self.life.params.grace_ms);
                    self.life.grace = Some(at_ms);
                    out.push(Output::ArmTimer {
                        id: TimerId::Grace,
                        at_ms,
                    });
                }
            }
            Input::Control { connection, frame } => {
                if self.life.connection == Some(connection) {
                    self.control(frame, out);
                }
            }
            Input::Timer(id) => self.timer(id, out),
            Input::WriteFailed { why } => self.write_failed(why, out),
            Input::JobDone { job, result } => self.job_done(job, result, out),
        }
    }

    fn control(&mut self, frame: CtlFrame, out: &mut Vec<Output>) {
        match frame.of {
            // A dump is answered in any mode: a draining agent is often the
            // one someone wants to look at.
            Some(ctl_frame::Of::DumpRequest(request)) => {
                let id = self.job();
                out.push(Output::Job {
                    id,
                    op: AgentJob::DumpPart {
                        dump_id: request.dump_id,
                    },
                });
            }
            Some(ctl_frame::Of::Input(input)) => {
                let id = input.input_id.clone();
                self.take(id, Event::Input(input), out);
            }
            Some(ctl_frame::Of::AgentMessage(envelope)) => {
                let id = envelope.id.clone();
                // Kept even before the daemon names the child: the report
                // can come first.
                if let Some(wire::envelope::Body::Report(report)) = &envelope.body {
                    // An exit report is always its incarnation's last.
                    let last = match &report.end {
                        Some(wire::child_report::End::TurnEnded(turn)) => turn.last,
                        Some(wire::child_report::End::Exited(_)) => true,
                        None => false,
                    };
                    self.life
                        .awaited
                        .report(sender_id(&envelope), report.from_incarnation, last);
                }
                self.take(id, Event::AgentMessage(envelope), out);
            }
            // A rename that arrives once the agent is leaving has no thread
            // left to name.
            Some(ctl_frame::Of::Rename(rename)) => {
                if self.life.mode == Mode::Running {
                    self.feed(Event::Rename { name: rename.name }, out);
                }
            }
            Some(ctl_frame::Of::Stop(stop)) => self.stop(stop.kill, out),
            Some(ctl_frame::Of::Children(children)) => self.life.awaited.children(&children),
            // Hello, nudges, dump parts and replies travel the other way.
            _ => {}
        }
    }

    /// An input or agent message reaches the interpreter while the agent
    /// runs; once it is leaving, the verdict says so.
    fn take(&mut self, id: Vec<u8>, event: Event, out: &mut Vec<Output>) {
        match self.life.mode {
            Mode::Running => self.feed(event, out),
            Mode::Draining(_) => self.reply(id, reply::rejected(wire::Refusal::Draining), out),
            Mode::Exiting { .. } => self.reply(id, reply::rejected(wire::Refusal::Exiting), out),
        }
    }

    /// The daemon's stop: the turn is cancelled the provider's own way and
    /// the agent drains; with `kill`, the provider's group ends at once.
    fn stop(&mut self, kill: bool, out: &mut Vec<Output>) {
        if kill {
            out.push(Output::KillProvider);
            self.finish(ExitCause::Killed, out);
            return;
        }
        let stopping = matches!(
            self.life.mode,
            Mode::Draining(Drain::Stop) | Mode::Exiting { .. }
        );
        if !stopping {
            self.feed(Event::StopRequested, out);
            self.life.mode = Mode::Draining(Drain::Stop);
        }
    }

    fn timer(&mut self, id: TimerId, out: &mut Vec<Output>) {
        let now = self.life.now_ms;
        match id {
            TimerId::Grace => {
                let due = self.life.mode == Mode::Running
                    && self.life.connection.is_none()
                    && self.life.grace.is_some_and(|until| until <= now);
                if due {
                    self.life.grace = None;
                    self.feed(Event::DaemonLost, out);
                    self.life.mode = Mode::Draining(Drain::DaemonLost);
                }
            }
            TimerId::Drain => {
                let due = matches!(self.life.mode, Mode::Draining(_))
                    && self.life.ask.is_some_and(|ask| ask.until <= now);
                if due {
                    self.orphaned(out);
                }
            }
            TimerId::ProviderStop => {
                if let Mode::Exiting { cause, until } = &self.life.mode
                    && *until <= now
                {
                    let cause = cause.clone();
                    out.push(Output::KillProvider);
                    self.finish(cause, out);
                }
            }
        }
    }

    /// A write to the agent directory failed, which in practice is a full
    /// disk: nothing the provider does from here can be recorded, so the
    /// incarnation ends now. The final boundary is written if the disk
    /// takes it; if even that fails, the provider is killed, and the next
    /// incarnation writes the missing boundary when it starts.
    fn write_failed(&mut self, why: String, out: &mut Vec<Output>) {
        let cause = ExitCause::WriteFailed(why);
        if matches!(self.life.mode, Mode::Exiting { .. }) {
            out.push(Output::KillProvider);
            self.finish(cause, out);
        } else {
            self.exit(cause, out);
        }
    }

    fn job_done(&mut self, job: JobId, result: JobResult, out: &mut Vec<Output>) {
        match result {
            JobResult::Git(git) => {
                if self.life.git.reading != Some(job) {
                    return;
                }
                self.life.git.reading = None;
                // Once the final boundary is written, the incarnation is
                // over.
                if !matches!(self.life.mode, Mode::Exiting { .. }) {
                    self.feed(Event::Git(git), out);
                }
                if std::mem::take(&mut self.life.git.again) {
                    self.read_git(out);
                }
            }
            JobResult::DumpPart(part) => self.send(ctl_frame::Of::Dump(part), out),
        }
    }

    // --- deciding the end ------------------------------------------------

    /// Decides, after anything happened, whether this incarnation ends.
    fn settle(&mut self, out: &mut Vec<Output>) {
        let turn_ended = std::mem::take(&mut self.life.turn.ended);
        let finishing = std::mem::take(&mut self.life.turn.finishing);
        let asked = self.life.asked_to_exit.take();
        let quiescent = self.quiescent();
        let one_shot = self.life.params.one_shot;
        let turn_running = self.life.turn.running();
        let cause = match self.life.mode {
            Mode::Exiting { .. } => None,
            _ if asked.is_some() => asked,
            Mode::Running => finishing.then_some(ExitCause::Finished),
            // A one-shot agent whose work ends while it drains for a lost
            // daemon has finished it: its parent hears it finished, and so
            // does its row.
            Mode::Draining(Drain::DaemonLost) if turn_ended && one_shot && quiescent => {
                Some(ExitCause::Finished)
            }
            Mode::Draining(why) => {
                if turn_ended || !turn_running {
                    Some(why.cause())
                } else {
                    let orphanable =
                        why == Drain::DaemonLost && self.life.turn.phase == Phase::NeedsYou;
                    if orphanable {
                        if self.life.ask.is_none() {
                            let until = self.life.now_ms.saturating_add(self.life.params.drain_ms);
                            self.life.ask = Some(OpenAsk { until });
                            out.push(Output::ArmTimer {
                                id: TimerId::Drain,
                                at_ms: until,
                            });
                        }
                    } else {
                        self.cancel_drain(out);
                    }
                    None
                }
            }
        };
        if let Some(cause) = cause {
            self.touched.decided = Some(Decided {
                cause: cause.clone(),
                turn_ended,
                turn_running,
                deadline_passed: false,
            });
            self.exit(cause, out);
        }
    }

    /// The drain deadline passed with the ask still open.
    fn orphaned(&mut self, out: &mut Vec<Output>) {
        self.touched.decided = Some(Decided {
            cause: ExitCause::Orphaned,
            turn_ended: false,
            turn_running: self.life.turn.running(),
            deadline_passed: true,
        });
        self.exit(ExitCause::Orphaned, out);
    }

    /// Nothing runs, nothing is queued, nothing handed to the provider
    /// waits for it, and every child has sent its last report: a child's
    /// report starts the next turn, which reports up as usual.
    fn quiescent(&self) -> bool {
        self.life.turn.phase == Phase::Idle
            && self.life.pending.queued.is_empty()
            && self.nothing_pending()
    }

    /// No accepted agent message waits to be consumed, no typed prompt
    /// waits for the provider to record it, and no child's last report is
    /// still to come. A prompt typed into one of terminal Claude's own
    /// screens is never recorded, so a one-shot child stays, idle, until a
    /// later prompt row takes it, a stop ends it or its daemon is lost.
    fn nothing_pending(&self) -> bool {
        self.life.awaited.none()
            && I::pending_messages(&self.interpreter).is_empty()
            && !I::typed_unrecorded(&self.interpreter)
    }

    /// Whether the incarnation exits after a turn end in `step`: a
    /// one-shot agent left quiescent by it, an agent the interpreter asks
    /// to exit, and one already on its way out. Its parent hears the turn's
    /// report as the incarnation's last.
    fn last_turn(&self, step: &Step, asks_exit: bool) -> bool {
        match self.life.mode {
            Mode::Running => {
                let (phase, queue_empty) = step.snapshot.as_ref().map_or(
                    (self.life.turn.phase, self.life.pending.queued.is_empty()),
                    |snapshot| (snapshot.phase(), snapshot.queue.is_empty()),
                );
                asks_exit
                    || (self.life.params.one_shot
                        && phase == Phase::Idle
                        && queue_empty
                        && self.nothing_pending())
            }
            Mode::Draining(_) | Mode::Exiting { .. } => true,
        }
    }

    /// The one way an incarnation ends by decision: the interpreter writes
    /// the final boundary, the provider is asked to finish, and it is
    /// killed if it has not exited by the deadline.
    fn exit(&mut self, cause: ExitCause, out: &mut Vec<Output>) {
        self.feed(Event::Exiting(cause.end()), out);
        out.push(Output::CloseProvider);
        if self.life.grace.take().is_some() {
            out.push(Output::CancelTimer { id: TimerId::Grace });
        }
        self.cancel_drain(out);
        let until = self.life.now_ms.saturating_add(PROVIDER_STOP_MS);
        self.life.mode = Mode::Exiting { cause, until };
        out.push(Output::ArmTimer {
            id: TimerId::ProviderStop,
            at_ms: until,
        });
    }

    /// The process exits.
    fn finish(&mut self, cause: ExitCause, out: &mut Vec<Output>) {
        self.life.grace = None;
        self.life.ask = None;
        self.life.exited = Some(cause.clone());
        out.push(Output::Exit(cause));
    }

    fn cancel_drain(&mut self, out: &mut Vec<Output>) {
        if self.life.ask.take().is_some() {
            out.push(Output::CancelTimer { id: TimerId::Drain });
        }
    }

    // --- the interpreter -------------------------------------------------

    /// One event through the facts ring, the interpreter and the journal,
    /// after a tick when the clock moved since the interpreter last heard
    /// it.
    fn feed(&mut self, event: Event, out: &mut Vec<Output>) {
        let now = self.life.now_ms;
        if self.life.ticked.is_none_or(|ticked| now > ticked) {
            self.life.ticked = Some(now);
            let at_ms = i64::try_from(now).unwrap_or(i64::MAX);
            self.feed_one(Event::Tick { at_ms }, out);
        }
        self.feed_one(event, out);
    }

    fn feed_one(&mut self, event: Event, out: &mut Vec<Output>) {
        out.push(Output::Record(Entry::of(&event)));
        let stepped = I::step(&mut self.interpreter, event);
        self.apply(stepped, out);
    }

    /// Blobs first, then the journal, then the effects: a reply is sent and
    /// a provider written to only once the step that explains it is on
    /// disk, and a step never names bytes that are not.
    fn apply(&mut self, stepped: Stepped, out: &mut Vec<Output>) {
        let Stepped { mut step, effects } = stepped;
        // Marked here, not by the interpreter: only the process knows its
        // children and whether it is leaving.
        if step.turn_end.is_some() {
            let asks_exit = effects
                .iter()
                .any(|effect| matches!(effect, Effect::Exit { .. }));
            let last = self.last_turn(&step, asks_exit);
            self.life.turn.finishing = last && !asks_exit && self.life.mode == Mode::Running;
            if let Some(turn_end) = &mut step.turn_end {
                turn_end.last = last;
            }
        }
        for effect in &effects {
            if let Effect::WriteBlob { hash, bytes } = effect {
                out.push(Output::WriteBlob {
                    hash: hash.clone(),
                    bytes: bytes.clone(),
                });
            }
        }
        if let Some(snapshot) = &step.snapshot {
            self.life.turn.phase = snapshot.phase();
            self.life.pending.queued = snapshot
                .queue
                .iter()
                .map(|queued| queued.input_id.clone())
                .collect();
        }
        self.touched.taken.extend(
            step.items
                .iter()
                .filter(|item| !item.input_id.is_empty())
                .map(|item| item.input_id.clone()),
        );
        let turn_ended = step.turn_end.is_some();
        // The journal skips an empty step, and an empty step tells the
        // daemon nothing.
        if step != Step::default() {
            out.push(Output::AppendJournal(step));
            self.send(ctl_frame::Of::Nudge(Nudge {}), out);
        }
        if turn_ended {
            self.life.turn.ended = true;
            // A turn changes the working tree; the row's totals follow.
            if !matches!(self.life.mode, Mode::Exiting { .. }) {
                self.read_git(out);
            }
        }
        for effect in effects {
            match effect {
                Effect::WriteBlob { .. } => {}
                Effect::Reply { input_id, verdict } => self.reply(input_id, verdict, out),
                Effect::Exit { cause } => {
                    self.life
                        .asked_to_exit
                        .get_or_insert(ExitCause::Interpreter(cause));
                }
                effect => out.push(Output::WriteProvider(effect)),
            }
        }
    }

    fn read_git(&mut self, out: &mut Vec<Output>) {
        if self.life.git.reading.is_some() {
            self.life.git.again = true;
            return;
        }
        let id = self.job();
        self.life.git.reading = Some(id);
        out.push(Output::Job {
            id,
            op: AgentJob::ReadGit,
        });
    }

    fn job(&mut self) -> JobId {
        self.life.next_job += 1;
        JobId(self.life.next_job)
    }

    // --- the daemon ------------------------------------------------------

    /// A verdict, keyed by the input id or the envelope id it answers.
    fn reply(&self, id: Vec<u8>, verdict: wire::SendInputResponse, out: &mut Vec<Output>) {
        self.send(
            ctl_frame::Of::Reply(Verdict {
                id,
                verdict: Some(verdict),
            }),
            out,
        );
    }

    /// Frames for a daemon that is not connected are dropped: whoever sent
    /// the input is gone with it.
    fn send(&self, of: ctl_frame::Of, out: &mut Vec<Output>) {
        if let Some(connection) = self.life.connection {
            out.push(Output::WriteControl {
                connection,
                frame: CtlFrame { of: Some(of) },
            });
        }
    }
}

/// An [`Agent`] between steps as a value: equal values step alike. What a
/// step did is not part of it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AgentValue {
    interpreter: Vec<u8>,
    life: Life,
}

impl fmt::Debug for AgentValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentValue")
            .field("life", &self.life)
            .field("interpreter", &String::from_utf8_lossy(&self.interpreter))
            .finish()
    }
}

/// The agent that sent a message; empty for a person's.
fn sender_id(envelope: &wire::Envelope) -> &[u8] {
    match envelope.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(wire::sender::Value::Agent(wire::AgentSender {
            agent: Some(agent), ..
        })) => &agent.agent_id,
        _ => &[],
    }
}

pub const EXIT_OUTSIDE_TURN_END: Check = Check {
    id: "agent.exit_outside_turn_end",
    sentence: "an agent exits only at a turn end or a drain deadline",
};
pub const TAKEN_STILL_PENDING: Check = Check {
    id: "agent.taken_still_pending",
    sentence: "the pending set never holds an input the journal recorded as taken",
};
pub const DEADLINE_WITHOUT_ORPHAN: Check = Check {
    id: "agent.deadline_without_orphan",
    sentence: "a drain deadline waits only on an ask left open while draining for a lost daemon",
};

/// Every check the agent core makes at the end of a step.
pub const CHECKS: &[Check] = &[
    EXIT_OUTSIDE_TURN_END,
    TAKEN_STILL_PENDING,
    DEADLINE_WITHOUT_ORPHAN,
];

pub use core_ring::{Check, DumpReason, StepBudget, Violation};

/// Why a value's interpreter state could not be restored: its bytes do not
/// decode under this build's layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreError(String);

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the checkpoint does not decode: {}", self.0)
    }
}

impl std::error::Error for RestoreError {}

#[cfg(test)]
mod tests;
