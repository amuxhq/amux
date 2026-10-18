//! The driver around a profile's daemon core.
//!
//! One thread per profile owns the core and steps it, one input at a time,
//! from a bounded channel. The driver decides nothing: it turns calls and
//! nudges into inputs, and carries out what each step asks for, answers
//! into the call table, timers and jobs on tokio, whose outcomes come back
//! as inputs.
//!
//! The profile's shell still writes the store under its own lock for what
//! has not moved into the core, and the core thread holds that lock across
//! each step, lending the store to the core. What the shell commits it
//! hands over before letting go of the lock, and the core thread steps
//! those changes first each time it takes the lock, so every stream reads
//! them in commit order: after its opening if they were committed after
//! it, in its opening if before.
//!
//! Links and channels report to the core through a queue of their own,
//! stepped ahead of everything else and without the store: no link input
//! reads it, so a link never waits behind the shell's writes. A channel is
//! opened only once the core has said so and given the key its handshake
//! must find. What the core decides about each host's presence goes to the
//! profile's connection manager, which keeps it for the shell's reads and
//! makes the tries the core asks for.
//!
//! Every input the core steps is recorded in the profile's input ring
//! first, and the ring starts a new segment at each checkpoint the core
//! takes, so the newest checkpoint and the inputs since rebuild the core.

mod calls;
mod links;
mod origin;
mod streams;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub(crate) use calls::{Answers, Delivered};
use calls::{CallTable, Carried};
use core_ring::Ring;
use daemon_core::payload::{
    Answer, BlobRefusal, CallError, ChannelClass, ChannelError, ClientRequest, JobResult,
    PeerFailure, PeerReply, PeerRequest, PublicKey, Response, Status,
};
use daemon_core::{
    CallId, ChannelId, Daemon, FromShell, HostId, IngestEnd, Ingested, Input, Job, JobId, LinkId,
    Output, Settings, ShellWork, StartOptions, TimerId,
};
pub(crate) use links::{CoreLinks, core_error};
use store::Sqlite;
pub use streams::{InventorySubscription, Subscription};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use wire::{FetchRequest, GetRequest};

use crate::connection::ConnectionManager;
use crate::install::{CORE_CHECKPOINT, CORE_RING, STORE, write_durably};
use crate::runtime::ProfileRuntime;

/// How the reason of a dump the core asked for begins.
pub const CORE_DUMP: &str = "the daemon core: ";
/// Inputs waiting for the core. A full channel holds its senders back
/// rather than growing.
const INPUT_ROOM: usize = 1024;
/// How long a page the core asks of a replica's origin waits on it before
/// the client is answered with what is held, or that the origin cannot be
/// reached.
const PAGE_PATIENCE: Duration = Duration::from_secs(10);
/// The bytes the input ring keeps across its segments. A segment that
/// fills before the checkpoint interval is up asks for a checkpoint early.
const RING_BYTES: u64 = 8 << 20;

/// What reaches the core thread.
pub(crate) enum Fed {
    Input(Input),
    /// A client call, whose request the thread completes from the shell as
    /// it steps it.
    Call {
        call: CallId,
        ask: Ask,
    },
    /// A paired host's call. One for the inventory is completed from the
    /// shell as it is stepped, as a client's is.
    PeerCall {
        from: HostId,
        call: CallId,
        request: PeerRequest,
    },
    /// A client call forwarded to the host that owns its agent. It reads
    /// nothing of the store, so it is stepped without it: whoever forwards
    /// may hold the store meanwhile.
    Forward {
        call: CallId,
        host: HostId,
        request: PeerRequest,
    },
    /// A nudge from someone who waits for the journal to be read to its
    /// end: told the frames committed meanwhile, or why ingest failed.
    Ingest {
        agent: daemon_core::AgentId,
        done: oneshot::Sender<Result<u64, String>>,
    },
    /// A checkpoint, for someone who waits for it to be on the drive.
    Checkpoint {
        done: oneshot::Sender<Result<(), String>>,
    },
    /// The shell wants a channel, and waits for the core's word on it.
    Channel {
        want: Input,
        reply: oneshot::Sender<Result<Opened, ChannelError>>,
    },
    /// The shell handed changes over.
    Wake,
    /// The profile is gone.
    Stop,
}

/// A client call as it arrives. What the request also carries from the
/// shell is read on the core thread with the store held, at the point the
/// core answers.
pub(crate) enum Ask {
    Subscribe {
        agent_id: Vec<u8>,
        tail: u32,
    },
    Fetch(FetchRequest),
    Get(GetRequest),
    SubscribeInventory,
    /// A request the shell already completed.
    Ready(ClientRequest),
}

/// A channel the core said to open, under the id it gave, handshaking
/// against the key it gave.
pub(crate) struct Opened {
    pub(crate) channel: ChannelId,
    pub(crate) pin: PublicKey,
}

/// A profile's core thread, from the shell's side.
pub(crate) struct Driver {
    inputs: mpsc::Sender<Fed>,
    /// What links and channels reported, in the order they did.
    reports: Arc<Mutex<VecDeque<Input>>>,
    calls: Arc<CallTable>,
    /// What the shell committed, waiting for the core thread to take the
    /// store next.
    shell: Arc<Mutex<Vec<FromShell>>>,
    /// Where what the core decides about hosts goes, once the profile's
    /// edge is up.
    connections: Arc<Mutex<Weak<ConnectionManager>>>,
    /// The calls the core made of peers that are still out.
    outgoing: Outgoing,
}

/// The tasks carrying the calls the core made of peers, by call.
type Outgoing = Arc<Mutex<HashMap<CallId, AbortHandle>>>;

impl Driver {
    /// Starts the core thread for `runtime`'s profile. It ends when the
    /// driver is dropped, or the runtime is.
    pub(crate) fn start(
        runtime: Weak<ProfileRuntime>,
        own_host: Vec<u8>,
        dir: PathBuf,
        options: StartOptions,
    ) -> Self {
        let (inputs, fed) = mpsc::channel(INPUT_ROOM);
        let calls = Arc::new(CallTable::default());
        let reports = Arc::new(Mutex::new(VecDeque::new()));
        let shell = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(Weak::new()));
        let outgoing = Outgoing::default();
        // The core owns a store of its own between steps; the profile's is
        // lent to it for each one.
        let store = Sqlite::open_in_memory(own_host).expect("an in-memory store opens");
        let daemon = Daemon::new(store, options);
        let ring = Ring::open(
            dir.join(CORE_RING),
            RING_BYTES,
            &daemon.checkpoint().to_bytes(),
        )
        .inspect_err(|error| tracing::warn!(%error, "the core's input ring does not open"))
        .ok();
        let core = CoreThread {
            daemon,
            ring,
            checkpointing: None,
            checkpoint_waiting: Vec::new(),
            fed,
            reports: reports.clone(),
            channel_reply: None,
            again: VecDeque::new(),
            inputs: inputs.downgrade(),
            runtime,
            calls: calls.clone(),
            shell: shell.clone(),
            connections: connections.clone(),
            outgoing: outgoing.clone(),
            lent: false,
            timers: HashMap::new(),
            waiting: HashMap::new(),
            ticked: None,
            settings: None,
        };
        let handle = Handle::current();
        std::thread::Builder::new()
            .name("amux-core".to_owned())
            .spawn(move || {
                let _entered = handle.enter();
                core.run();
            })
            .expect("the core thread starts");
        Self {
            inputs,
            reports,
            calls,
            shell,
            connections,
            outgoing,
        }
    }

    /// From now on what the core decides about hosts goes to `connections`.
    pub(crate) fn attach_connections(&self, connections: Weak<ConnectionManager>) {
        *self.connections.lock().unwrap() = connections;
    }

    /// Reports what a link or channel did. Never waits, so a stream can
    /// say it ended as it is dropped.
    pub(crate) fn report(&self, input: Input) {
        self.reports.lock().unwrap().push_back(input);
        // A full channel wakes the core anyway.
        let _ = self.inputs.try_send(Fed::Wake);
    }

    /// Asks the core for a channel to `peer` over `link`, with the key the
    /// shell pinned for the peer, and waits for its word.
    pub(crate) async fn want_channel(
        &self,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: Option<PublicKey>,
    ) -> Result<Opened, ChannelError> {
        let (reply, word) = oneshot::channel();
        let want = Input::WantChannel {
            link,
            peer,
            class,
            pin,
        };
        if self
            .inputs
            .send(Fed::Channel { want, reply })
            .await
            .is_err()
        {
            return Err(ChannelError::LinkClosed);
        }
        word.await.unwrap_or(Err(ChannelError::LinkClosed))
    }

    /// Hands a client's call to the core and returns its answers. `room` is
    /// how many live answers the call's queue holds for a reader that has
    /// not taken them.
    pub(crate) async fn call(&self, ask: Ask, room: usize) -> Answers {
        self.open(room, |call| Fed::Call { call, ask }).await
    }

    /// Hands a paired host's call to the core and returns its answers.
    pub(crate) async fn peer_call(
        &self,
        from: HostId,
        request: PeerRequest,
        room: usize,
    ) -> Answers {
        self.open(room, |call| Fed::PeerCall {
            from,
            call,
            request,
        })
        .await
    }

    /// Hands the core a client's call on an agent `host` owns, to be made
    /// of that host, and returns its answer.
    pub(crate) async fn forward(&self, host: HostId, request: PeerRequest) -> Answers {
        self.open(1, |call| Fed::Forward {
            call,
            host,
            request,
        })
        .await
    }

    async fn open(&self, room: usize, fed: impl FnOnce(CallId) -> Fed) -> Answers {
        let queue = self.calls.open(room);
        let call = queue.call();
        let answers = Answers::new(queue.clone(), self.calls.clone(), self.inputs.clone());
        if self.inputs.send(fed(call)).await.is_err() {
            self.calls.end(call, Status::Ok);
        }
        answers
    }

    /// Nudges the core to ingest the agent's journal and waits until it has
    /// been read to its end: the frames committed meanwhile, or why ingest
    /// failed.
    pub(crate) async fn ingest(&self, agent: uuid::Uuid) -> Result<u64, String> {
        let (done, ingested) = oneshot::channel();
        let agent = daemon_core::AgentId(*agent.as_bytes());
        if self.inputs.send(Fed::Ingest { agent, done }).await.is_err() {
            return Err("the profile is no longer running".to_owned());
        }
        ingested
            .await
            .unwrap_or_else(|_| Err("the profile is no longer running".to_owned()))
    }

    /// Asks the core for a checkpoint and waits until it is on the drive,
    /// or says why it is not.
    pub(crate) async fn checkpoint(&self) -> Result<(), String> {
        let (done, taken) = oneshot::channel();
        if self.inputs.send(Fed::Checkpoint { done }).await.is_err() {
            return Err("the profile is no longer running".to_owned());
        }
        taken
            .await
            .unwrap_or_else(|_| Err("the profile is no longer running".to_owned()))
    }

    /// Feeds the core an input.
    pub(crate) async fn feed(&self, input: Input) {
        let _ = self.inputs.send(Fed::Input(input)).await;
    }

    /// Feeds the core an input without waiting: at once if the channel has
    /// room, otherwise from a task once it does.
    pub(crate) fn post(&self, input: Input) {
        if let Err(mpsc::error::TrySendError::Full(fed)) = self.inputs.try_send(Fed::Input(input)) {
            let inputs = self.inputs.clone();
            tokio::spawn(async move {
                let _ = inputs.send(fed).await;
            });
        }
    }

    /// Lets go of every call the core made of a peer, without waiting and
    /// without telling the core: the profile's run is ending.
    pub(crate) fn hang_up_all(&self) {
        for (_, task) in self.outgoing.lock().unwrap().drain() {
            task.abort();
        }
    }

    /// Hands the core a change the shell committed. Called with the store
    /// still held, so the core steps it before anything that reads the
    /// store after it.
    pub(crate) fn hand_over(&self, change: FromShell) {
        self.shell.lock().unwrap().push(change);
        // A full channel wakes the core anyway.
        let _ = self.inputs.try_send(Fed::Wake);
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Every stream ends with the profile; the thread sees the stop or,
        // if the channel is full, the runtime gone.
        self.calls.end_all(Status::Ok);
        self.hang_up_all();
        let _ = self.inputs.try_send(Fed::Stop);
    }
}

/// A call as the core thread takes it, before its request is completed.
enum Arriving {
    Client(CallId, Ask),
    Peer(HostId, CallId, Box<PeerRequest>),
}

/// Someone waiting for an agent's journal to be read to its end.
struct Waiting {
    frames: u64,
    done: oneshot::Sender<Result<u64, String>>,
}

struct CoreThread {
    daemon: Daemon,
    /// Every input stepped since the last checkpoint, when the ring opened.
    ring: Option<Ring<Input>>,
    /// The checkpoint job in flight.
    checkpointing: Option<JobId>,
    /// Whoever waits for the next checkpoint to be on the drive.
    checkpoint_waiting: Vec<oneshot::Sender<Result<(), String>>>,
    fed: mpsc::Receiver<Fed>,
    reports: Arc<Mutex<VecDeque<Input>>>,
    /// Whoever waits for the core's word on the channel the step under way
    /// was asked for.
    channel_reply: Option<oneshot::Sender<Result<Opened, ChannelError>>>,
    /// Inputs the driver raised itself while carrying out a step, stepped
    /// before anything newer.
    again: VecDeque<Input>,
    inputs: mpsc::WeakSender<Fed>,
    runtime: Weak<ProfileRuntime>,
    calls: Arc<CallTable>,
    shell: Arc<Mutex<Vec<FromShell>>>,
    connections: Arc<Mutex<Weak<ConnectionManager>>>,
    outgoing: Outgoing,
    /// Whether the shell's store is lent to the core now.
    lent: bool,
    timers: HashMap<TimerId, AbortHandle>,
    waiting: HashMap<daemon_core::AgentId, Vec<Waiting>>,
    /// The clock as last told to the core.
    ticked: Option<u64>,
    /// The settings as last told to the core.
    settings: Option<Settings>,
}

impl CoreThread {
    fn run(mut self) {
        let mut out = Vec::new();
        loop {
            let fed = match self.again.pop_front() {
                Some(input) => Fed::Input(input),
                None => match self.fed.blocking_recv() {
                    Some(Fed::Stop) | None => return,
                    Some(fed) => fed,
                },
            };
            let Some(runtime) = self.runtime.upgrade() else {
                return;
            };
            // The clock first, so a timer the step arms counts from now;
            // then links and channels, and without the store: none of
            // their inputs reads it.
            self.tick(&runtime, &mut out);
            let reports = std::mem::take(&mut *self.reports.lock().unwrap());
            for report in reports {
                self.step(&runtime, report, &mut out);
            }
            let fed = match fed {
                Fed::Channel { want, reply } => {
                    self.channel_reply = Some(reply);
                    self.step(&runtime, want, &mut out);
                    self.channel_reply = None;
                    continue;
                }
                Fed::Forward {
                    call,
                    host,
                    request,
                } => {
                    let request = ClientRequest::Forward {
                        host,
                        request: Box::new(request),
                    };
                    self.step(&runtime, Input::ClientCall { call, request }, &mut out);
                    continue;
                }
                Fed::Wake if self.shell.lock().unwrap().is_empty() => continue,
                fed => fed,
            };
            let mut store = runtime.store.blocking_lock();
            let mut call = None;
            let input = match fed {
                Fed::Input(input) => Some(input),
                Fed::Call { call: id, ask } => {
                    call = Some(Arriving::Client(id, ask));
                    None
                }
                Fed::PeerCall {
                    from,
                    call: id,
                    request,
                } => {
                    call = Some(Arriving::Peer(from, id, Box::new(request)));
                    None
                }
                Fed::Ingest { agent, done } => {
                    let waiting = Waiting { frames: 0, done };
                    self.waiting.entry(agent).or_default().push(waiting);
                    Some(Input::Nudge { agent })
                }
                Fed::Checkpoint { done } => {
                    self.checkpoint_waiting.push(done);
                    // One in flight already covers every commit its waiter
                    // has seen: commits wait for it.
                    match self.checkpointing {
                        Some(_) => None,
                        None => Some(Input::Timer {
                            id: TimerId::Checkpoint,
                        }),
                    }
                }
                Fed::Wake | Fed::Stop | Fed::Channel { .. } | Fed::Forward { .. } => None,
            };
            self.daemon.swap_store(&mut store);
            self.lent = true;
            self.tick(&runtime, &mut out);
            let launch = runtime.launch();
            let settings = Settings {
                notify_delay_ms: launch.notify_delay_ms,
                push_retry_ms: launch.push_retry_ms.max(0) as u64,
                checkpoint_interval_ms: launch.checkpoint_interval_ms.max(0) as u64,
                tail_rows: launch.tail_rows,
                source_backoff_ms: launch.source_backoff_ms.max(0) as u64,
                source_backoff_max_ms: launch.source_backoff_max_ms.max(0) as u64,
                start_deadline_ms: launch.start_deadline_ms.max(0) as u64,
                stop_deadline_ms: launch.stop_deadline_ms.max(0) as u64,
            };
            if self.settings != Some(settings) {
                self.settings = Some(settings);
                self.step(&runtime, Input::Configure(settings), &mut out);
            }
            // Committed before this thread took the store: told before
            // anything this step reads.
            let changes = std::mem::take(&mut *self.shell.lock().unwrap());
            for change in changes {
                self.step(&runtime, Input::FromShell(change), &mut out);
            }
            // A call's request is read from the shell only now: a change
            // handed over before this read is in it, and one handed over
            // after is still queued, stepped after the call as a live
            // answer to it. Read before the drain, a change handed over in
            // between would reach the core before the call and be in
            // neither.
            let input = match call {
                Some(Arriving::Client(call, ask)) => Some(Input::ClientCall {
                    call,
                    request: runtime.client_request(self.daemon.store(), ask),
                }),
                Some(Arriving::Peer(from, call, request)) => Some(Input::PeerCall {
                    from,
                    call,
                    request: runtime.peer_request(*request),
                }),
                None => input,
            };
            if let Some(input) = input {
                self.step(&runtime, input, &mut out);
            }
            self.lent = false;
            self.daemon.swap_store(&mut store);
            drop(store);
            drop(runtime);
        }
    }

    /// Tells the core the time, when it moved.
    fn tick(&mut self, runtime: &Arc<ProfileRuntime>, out: &mut Vec<Output>) {
        let now = runtime.clock_now().max(0) as u64;
        if self.ticked != Some(now) {
            self.ticked = Some(now);
            self.step(runtime, Input::Tick { now_ms: now }, out);
        }
    }

    /// Steps one input and carries out what it asked for, with the store
    /// still held.
    fn step(&mut self, runtime: &Arc<ProfileRuntime>, input: Input, out: &mut Vec<Output>) {
        // A call's own answers, to its opening or its lag, are always
        // taken; every other answer is a live one, held to the call's room.
        let replying = match &input {
            Input::ClientCall { call, .. } | Input::AnswerLagged { call } => Some(*call),
            _ => None,
        };
        let checkpointed = match &input {
            Input::JobDone { job, result } if Some(*job) == self.checkpointing => {
                self.checkpointing = None;
                Some(match result {
                    JobResult::Done => Ok(()),
                    other => Err(format!("{other:?}")),
                })
            }
            _ => None,
        };
        if let Input::PeerAnswer { call, answer } = &input
            && answer.is_last()
        {
            self.outgoing.lock().unwrap().remove(call);
        }
        self.record(&input);
        self.daemon.step(input, out);
        // Carrying out an output may step another input, with outputs of
        // its own.
        let outputs: Vec<Output> = std::mem::take(out);
        for output in outputs {
            self.execute(runtime, output, replying);
        }
        if let Some(outcome) = checkpointed {
            for waiter in self.checkpoint_waiting.drain(..) {
                let _ = waiter.send(outcome.clone());
            }
        }
    }

    /// Records an input in the ring before the core steps it. A ring that
    /// fills before the checkpoint interval is up asks for a checkpoint,
    /// which starts the next segment.
    fn record(&mut self, input: &Input) {
        let Some(ring) = self.ring.as_mut() else {
            return;
        };
        if let Err(error) = ring.append(input) {
            tracing::warn!(%error, "recording an input in the core's ring failed");
        }
        if ring.full() && self.checkpointing.is_none() {
            self.again.push_back(Input::Timer {
                id: TimerId::Checkpoint,
            });
        }
    }

    /// Carries out one output.
    fn execute(&mut self, runtime: &Arc<ProfileRuntime>, output: Output, replying: Option<CallId>) {
        match output {
            Output::Answer { call, response } if replying == Some(call) => {
                self.calls.answer(call, response)
            }
            Output::Answer { call, response } => {
                if let Some(queue) = self.calls.get(call)
                    && queue.carry(response) == Carried::Overflowed
                {
                    self.again.push_back(Input::AnswerLagged { call });
                }
            }
            Output::EndCall { call, status } => self.calls.end(call, status),
            Output::Shell(work) => self.shell_work(runtime, work),
            Output::Ingested { agent, report } => {
                runtime.ingested(&report);
                self.ingested(agent, report);
            }
            Output::Wake(wake) => runtime.wake(wake),
            Output::ArmTimer { id, at_ms } => {
                let now = runtime.clock_now().max(0) as u64;
                // A follower or source keeps to the profile's clock, the
                // one its backoff is reckoned on; one due now is stepped
                // next, with the store.
                let sourcing = matches!(id, TimerId::Follow(_) | TimerId::Source(..));
                // An agent's start and stop deadlines keep to it too, and
                // so do notifications, whose due times it stamped.
                let registry = matches!(
                    id,
                    TimerId::Start(_) | TimerId::Exit(_) | TimerId::Notifications
                );
                if sourcing && at_ms <= now {
                    if let Some(armed) = self.timers.remove(&id) {
                        armed.abort();
                    }
                    self.again.push_back(Input::Timer { id });
                    return;
                }
                let wait = at_ms.saturating_sub(now);
                let inputs = self.inputs.clone();
                // A host's retry and a link's refresh read no store, so
                // they fire as links report, ahead of the shell's writes.
                let reports = matches!(id, TimerId::Retry(_) | TimerId::Refresh(_))
                    .then(|| self.reports.clone());
                // A credential's refresh keeps to the profile's clock, the
                // one the relay judges the credential's expiry by.
                let refresh =
                    (sourcing || registry || matches!(id, TimerId::Refresh(_))).then(|| {
                        runtime
                            .clock()
                            .sleep_until(at_ms.min(i64::MAX as u64) as i64)
                    });
                let task = tokio::spawn(async move {
                    match refresh {
                        Some(due) => due.await,
                        None => tokio::time::sleep(Duration::from_millis(wait)).await,
                    }
                    let fired = Input::Timer { id };
                    match reports {
                        Some(reports) => {
                            reports.lock().unwrap().push_back(fired);
                            if let Some(inputs) = inputs.upgrade() {
                                let _ = inputs.try_send(Fed::Wake);
                            }
                        }
                        None => feed(&inputs, fired).await,
                    }
                });
                if let Some(armed) = self.timers.insert(id, task.abort_handle()) {
                    armed.abort();
                }
            }
            Output::CancelTimer { id } => {
                if let Some(armed) = self.timers.remove(&id) {
                    armed.abort();
                }
            }
            Output::Job {
                id,
                op: Job::Checkpoint { state },
            } => {
                // The inputs after this step are the ones this checkpoint
                // does not hold.
                if let Some(ring) = self.ring.as_mut()
                    && let Err(error) = ring.rotate(&state)
                {
                    tracing::warn!(%error, "rotating the core's input ring failed");
                }
                self.checkpointing = Some(id);
                tokio::spawn(run_job(
                    Arc::downgrade(runtime),
                    id,
                    Job::Checkpoint { state },
                    self.inputs.clone(),
                ));
            }
            Output::Job { id, op } => {
                tokio::spawn(run_job(
                    Arc::downgrade(runtime),
                    id,
                    op,
                    self.inputs.clone(),
                ));
            }
            Output::Dump { reason } => {
                // The core logged why. The bundle is written off this
                // thread, once the step lets go of the store it reads.
                let runtime = Arc::downgrade(runtime);
                tokio::spawn(async move {
                    let Some(runtime) = runtime.upgrade() else {
                        return;
                    };
                    let request = wire::DumpRequest {
                        agent_ids: Vec::new(),
                        reason: format!("{CORE_DUMP}{reason}"),
                        automatic: true,
                    };
                    match runtime.dump(request).await {
                        Ok(bundle) => tracing::warn!(
                            bundle = %bundle.display(),
                            "dumped for the daemon core"
                        ),
                        Err(error) => {
                            tracing::error!(%error, "dumping for the daemon core failed")
                        }
                    }
                });
            }
            Output::OpenChannel { channel, pin, .. } => match self.channel_reply.take() {
                Some(reply) => {
                    if reply.send(Ok(Opened { channel, pin })).is_err() {
                        // Nobody waits to open it: it never opened.
                        self.again.push_back(Input::ChannelClosed {
                            channel,
                            reason: ChannelError::LinkClosed,
                        });
                    }
                }
                None => tracing::error!(
                    ?channel,
                    "the daemon core opened a channel nobody asked for"
                ),
            },
            Output::ChannelRefused { reason, .. } => {
                if let Some(reply) = self.channel_reply.take() {
                    let _ = reply.send(Err(reason));
                }
            }
            Output::Presence { host, view } => {
                if let Some(connections) = self.connections() {
                    connections.told(host, view);
                }
            }
            Output::Attempt { host, route } => match self.connections() {
                Some(connections) => connections.attempt(host, route),
                None => self.again.push_back(Input::AttemptConcluded {
                    host,
                    route,
                    tried: daemon_core::Tried::Abandoned,
                }),
            },
            Output::DropRoute { host, route } => {
                if let Some(connections) = self.connections() {
                    connections.drop_route(host, route);
                }
            }
            Output::Call {
                host,
                call,
                request,
                absorbed,
            } => {
                let runtime = Arc::downgrade(runtime);
                let reports = self.reports.clone();
                let inputs = self.inputs.clone();
                let task = match request {
                    request @ (PeerRequest::Subscribe(_)
                    | PeerRequest::SubscribeInventory { .. }) => {
                        tokio::spawn(origin::stream(runtime, host, call, request, inputs))
                    }
                    request => tokio::spawn(async move {
                        let answer = if absorbed {
                            tokio::time::timeout(PAGE_PATIENCE, call_peer(runtime, host, request))
                                .await
                                .unwrap_or(Some(Answer::Failed(PeerFailure::Uncertain)))
                        } else {
                            call_peer(runtime, host, request).await
                        };
                        let Some(answer) = answer else {
                            return;
                        };
                        let answered = Input::PeerAnswer { call, answer };
                        if absorbed {
                            return feed(&inputs, answered).await;
                        }
                        // A reply that is not absorbed reads nothing of the
                        // store: it waits on nobody's writes.
                        reports.lock().unwrap().push_back(answered);
                        if let Some(inputs) = inputs.upgrade() {
                            let _ = inputs.try_send(Fed::Wake);
                        }
                    }),
                };
                self.outgoing
                    .lock()
                    .unwrap()
                    .insert(call, task.abort_handle());
            }
            Output::HangUp { call } => {
                if let Some(task) = self.outgoing.lock().unwrap().remove(&call) {
                    task.abort();
                }
            }
            Output::Dial { host } => {
                if let Some(connections) = self.connections() {
                    connections.dial(host);
                }
            }
            Output::SpawnAgent { dir, spec } => runtime.spawn_process(dir, *spec),
            Output::DialAgent { agent } => runtime.dial_agent(uuid::Uuid::from_bytes(agent.0)),
            Output::WriteControl { agent, frame } => {
                runtime.write_control(uuid::Uuid::from_bytes(agent.0), frame)
            }
            Output::ForgetAgent { agent } => runtime.forget_agent(uuid::Uuid::from_bytes(agent.0)),
            Output::SendFile { call, path, range } => {
                tokio::spawn(send_file(
                    Arc::downgrade(runtime),
                    self.calls.clone(),
                    call,
                    path,
                    range,
                ));
            }
            output @ (Output::CloseChannel { .. } | Output::LinkControl { .. }) => {
                tracing::error!(
                    ?output,
                    "the daemon core asked for what this driver does not carry out yet"
                );
            }
        }
    }

    fn connections(&self) -> Option<Arc<ConnectionManager>> {
        self.connections.lock().unwrap().upgrade()
    }

    /// Does what the core left to the shell.
    fn shell_work(&mut self, runtime: &Arc<ProfileRuntime>, work: ShellWork) {
        match work {
            ShellWork::Wrote => runtime.wrote(),
            ShellWork::Sourcing { agent, open } => runtime.sourcing(agent, open),
            ShellWork::HostCurrent { host, current } => {
                runtime.host_current(host, current);
                self.step_handed_over(runtime);
            }
            ShellWork::HostGeneration { host, generation } => {
                runtime.host_generation_changed(&host.0, generation);
                self.step_handed_over(runtime);
            }
            ShellWork::Providers { host, providers } => {
                if runtime.peer_providers_said(&host.0, providers) {
                    let runtime = Arc::downgrade(runtime);
                    tokio::spawn(async move {
                        if let Some(runtime) = runtime.upgrade() {
                            runtime.sync_hosts().await;
                        }
                    });
                }
            }
            ShellWork::ReplicaDropped { agent, listed } => runtime.replica_dropped(&agent, listed),
            ShellWork::ForgetDraft { agent } => runtime.forget_draft(&agent.0),
        }
    }

    /// Steps at once what the shell handed over while carrying out a step's
    /// outputs, with the store still lent, so subscribers read it in the
    /// order of the outputs around it.
    fn step_handed_over(&mut self, runtime: &Arc<ProfileRuntime>) {
        if !self.lent {
            return;
        }
        let changes = std::mem::take(&mut *self.shell.lock().unwrap());
        for change in changes {
            self.step(runtime, Input::FromShell(change), &mut Vec::new());
        }
    }

    /// Tells whoever waits on the agent's journal what one ingest step did.
    fn ingested(&mut self, agent: daemon_core::AgentId, report: Ingested) {
        let Some(waiting) = self.waiting.get_mut(&agent) else {
            return;
        };
        for waiter in waiting.iter_mut() {
            waiter.frames += u64::from(report.frames);
        }
        let outcome = match report.end {
            IngestEnd::More => return,
            IngestEnd::Read => Ok(()),
            IngestEnd::Failed(why) => Err(why),
        };
        for waiter in self.waiting.remove(&agent).unwrap_or_default() {
            let _ = waiter.done.send(outcome.clone().map(|()| waiter.frames));
        }
    }
}

/// Feeds the core an input from off its thread, unless the core has
/// stopped.
async fn feed(inputs: &mpsc::WeakSender<Fed>, input: Input) {
    if let Some(inputs) = inputs.upgrade() {
        let _ = inputs.send(Fed::Input(input)).await;
    }
}

/// Runs one job off the core thread and feeds its result back.
async fn run_job(
    runtime: Weak<ProfileRuntime>,
    job: JobId,
    op: Job,
    inputs: mpsc::WeakSender<Fed>,
) {
    let result = match op {
        Job::Fsync { path, bytes } => fsync(path, bytes).await,
        Job::Checkpoint { state } => {
            let Some(dir) = runtime.upgrade().map(|runtime| runtime.dir().to_path_buf()) else {
                return;
            };
            blocking(move || checkpoint(&dir, &state)).await
        }
        Job::RefreshToken { link } => {
            let Some(edge) = runtime.upgrade().and_then(|runtime| runtime.edge()) else {
                return;
            };
            // The profile has one relay link; the one the core named is it
            // unless it went, which the core hears of as the link's end.
            match edge.refresh_entitlement().await {
                Ok(_) => JobResult::Done,
                Err(error) => {
                    tracing::debug!(?link, %error, "refreshing the relay credential failed");
                    JobResult::Failed(error)
                }
            }
        }
        Job::KillAgent { agent } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            if runtime.kill_process(uuid::Uuid::from_bytes(agent.0)) {
                JobResult::Done
            } else {
                JobResult::Failed("no process this daemon can kill".to_owned())
            }
        }
        Job::AwaitUnlocked { agent, patience_ms } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            let dir = runtime.agent_dir(uuid::Uuid::from_bytes(agent.0));
            let patience = i64::try_from(patience_ms).unwrap_or(i64::MAX);
            match runtime.wait_unlocked(&dir, patience).await {
                Ok(()) => JobResult::Done,
                Err(()) => JobResult::Failed("the directory is still locked".to_owned()),
            }
        }
        Job::RemoveDir { path } => {
            blocking(move || match std::fs::remove_dir_all(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                removed => removed,
            })
            .await
        }
        Job::MakeWorktree { repo, name } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            runtime.make_worktree(&repo, &name).await
        }
        Job::SendPush(push) => {
            let Some(sender) = runtime.upgrade().map(|runtime| runtime.push.clone()) else {
                return;
            };
            match sender.send(&push).await {
                Ok(()) => JobResult::Done,
                Err(error) => JobResult::Failed(error.to_string()),
            }
        }
        Job::WriteBlob { dir, hash, bytes } => {
            let size = bytes.len() as u64;
            let path = dir.join(crate::blobs::hex(&hash));
            let written =
                tokio::task::spawn_blocking(move || crate::blobs::write_blob(&dir, &hash, &bytes))
                    .await
                    .unwrap_or_else(|error| Err(std::io::Error::other(error)));
            match written {
                Ok(()) => {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.kept_blob(path, size);
                    }
                    JobResult::Done
                }
                // The core tells a full drive apart by the code.
                Err(error) => {
                    let message = error.to_string();
                    JobResult::Refused(wire::Error {
                        code: crate::blobs::BlobError::Write(error).to_wire().code,
                        message,
                        details: Vec::new(),
                    })
                }
            }
        }
        op @ Job::Upload(_) => JobResult::Failed(format!("this driver does not run {op:?} yet")),
    };
    feed(&inputs, Input::JobDone { job, result }).await;
}

/// Answers a blob read from its file, whole: the bytes never pass through
/// the core.
async fn send_file(
    runtime: Weak<ProfileRuntime>,
    calls: Arc<CallTable>,
    call: CallId,
    path: std::path::PathBuf,
    range: std::ops::Range<u64>,
) {
    let name = || {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if let Some(runtime) = runtime.upgrade() {
        runtime.read_blob(&path);
    }
    let failed = |error: std::io::Error| {
        Status::Failed(if error.kind() == std::io::ErrorKind::NotFound {
            CallError::NoBlob(name())
        } else {
            CallError::Blob(BlobRefusal::Read(error.to_string()))
        })
    };
    let size = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len(),
        Err(error) => return calls.end(call, failed(error)),
    };
    let wanted = range.end.min(size).saturating_sub(range.start);
    if wanted > wire::ATTACHMENT_SIZE_LIMIT as u64 {
        let too_large = BlobRefusal::TooLarge {
            name: name(),
            size: wanted,
        };
        return calls.end(call, Status::Failed(CallError::Blob(too_large)));
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let start = usize::try_from(range.start.min(size)).unwrap_or(usize::MAX);
            let end = usize::try_from(range.end.min(size)).unwrap_or(usize::MAX);
            let bytes = bytes.get(start..end).unwrap_or_default().to_vec();
            let reply = PeerReply::GetBlob(wire::GetBlobResponse { bytes });
            calls.answer(call, Response::Peer(Box::new(reply)));
            calls.end(call, Status::Ok);
        }
        Err(error) => calls.end(call, failed(error)),
    }
}

/// Makes a call the core asked for of a peer, unless the profile is gone.
async fn call_peer(
    runtime: Weak<ProfileRuntime>,
    host: HostId,
    request: PeerRequest,
) -> Option<Answer> {
    let runtime = runtime.upgrade()?;
    Some(
        runtime
            .call_peer(crate::routing::host_from_core(host), request)
            .await,
    )
}

/// The fsync job: replaces the file at `path` with `bytes` so that a crash
/// or power cut leaves the old file or the new one, and the new one is on
/// the drive when it is done. The installation runs it too, for the writes
/// it promises durable outside any profile's core.
pub(crate) async fn fsync(path: PathBuf, bytes: Vec<u8>) -> JobResult {
    blocking(move || write_durably(&path, &bytes)).await
}

/// The checkpoint job: the store's WAL into its main file, synced, and then
/// the core's state as of the same step beside it. The core held its
/// commits since that step, so the main file is the store as of it; a
/// crash between the two leaves the previous checkpoint, which the main
/// file is at or past.
fn checkpoint(dir: &Path, state: &[u8]) -> std::io::Result<()> {
    Sqlite::checkpoint_wal(&dir.join(STORE)).map_err(std::io::Error::other)?;
    write_durably(&dir.join(CORE_CHECKPOINT), state)
}

async fn blocking(work: impl FnOnce() -> std::io::Result<()> + Send + 'static) -> JobResult {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(())) => JobResult::Done,
        Ok(Err(error)) => JobResult::Failed(error.to_string()),
        Err(error) => JobResult::Failed(error.to_string()),
    }
}

impl ProfileRuntime {
    /// Completes a client call's request from what the shell owns, with the
    /// store held at the point the core answers it.
    fn client_request(&self, store: &Sqlite, ask: Ask) -> ClientRequest {
        match ask {
            Ask::Subscribe { agent_id, tail } => {
                self.used(store, &agent_id);
                self.run_join_hook();
                ClientRequest::Subscribe { agent_id, tail }
            }
            Ask::Fetch(request) => {
                self.used(store, &request.agent_id);
                ClientRequest::Fetch(request)
            }
            Ask::Get(request) => ClientRequest::Get(request),
            Ask::SubscribeInventory => ClientRequest::SubscribeInventory {
                hosts: self.opening_hosts(),
            },
            Ask::Ready(request) => request,
        }
    }

    /// Completes a paired host's call from what the shell owns, as
    /// [`ProfileRuntime::client_request`] does a client's.
    fn peer_request(&self, request: PeerRequest) -> PeerRequest {
        match request {
            PeerRequest::Subscribe(request) => {
                self.run_join_hook();
                PeerRequest::Subscribe(request)
            }
            PeerRequest::SubscribeInventory { .. } => PeerRequest::SubscribeInventory {
                hosts: self.opening_hosts(),
            },
            request => request,
        }
    }

    fn run_join_hook(&self) {
        let hook = self.join_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            Handle::current().block_on(hook());
        }
    }

    /// The host set an inventory stream opens with.
    fn opening_hosts(&self) -> Vec<wire::HostEntry> {
        let hosts = self.published_hosts();
        let hook = self.inventory_opening_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            Handle::current().block_on(hook());
        }
        hosts
    }
}
