//! The driver around a profile's daemon core.
//!
//! One thread per profile owns the core and steps it, one input at a time,
//! from a bounded channel. The driver decides nothing: it turns calls and
//! nudges into inputs, and carries out what each step asks for, answers
//! into the call table, timers and jobs on tokio, whose outcomes come back
//! as inputs.
//!
//! The core owns the profile's store: every write to it is a step's. What
//! the shell reads of it, it reads on the core thread between steps, so a
//! read sees the store as of one point in the order of steps.
//!
//! Links and channels report to the core through a queue of their own,
//! stepped ahead of everything else, so a link's word never waits behind
//! the calls and journals queued before it. A channel is
//! opened only once the core has said so and given the key its handshake
//! must find. What the core decides about each host's presence goes to the
//! profile's connection manager, which keeps it for the shell's reads and
//! makes the tries the core asks for.
//!
//! Every input the core steps is recorded in the profile's input ring
//! first, and the ring starts a new segment at each checkpoint the core
//! takes, so the newest checkpoint and the inputs since rebuild the core.

mod calls;
mod feed;
mod links;
mod origin;
pub(crate) mod pairing;
mod stepped;
mod streams;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub(crate) use calls::{Answers, Delivered};
use calls::{CallTable, Carried};
use core_ring::{DumpLimit, Ring};
use daemon_core::payload::{
    Answer, BlobRefusal, CallError, ChannelError, ClientRequest, JobResult, PeerFailure, PeerReply,
    PeerRequest, Response, Status,
};
use daemon_core::{
    CallId, ChannelId, Daemon, DrainReport, HostId, IngestEnd, Ingested, Input, Job, JobId, LinkId,
    Output, Recorded, Retain, Retained, Settings, ShellWork, StartOptions, TimerId,
};
pub(crate) use feed::{Feed, WeakFeed};
pub(crate) use links::{CoreLinks, Wanted};
pub use stepped::Cores;
use store::Sqlite;
pub use streams::{InventorySubscription, Subscription};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use wire::{GetItemRequest, ListItemsRequest};

use crate::install::{CORE_RING, STORE, write_durably};
use crate::routing::host_from_core;
use crate::runtime::ProfileRuntime;

/// How the reason of a dump the core asked for begins.
pub const CORE_DUMP: &str = "the daemon core: ";
/// How the reason of a dump the core asked for because one of its
/// invariants failed begins, so a reader tells a broken invariant from a
/// slow step without reading the sentence after it.
pub const CORE_VIOLATION_DUMP: &str = "the daemon core: an invariant failed: ";
/// How the reason of a dump an agent's core asked for begins: the daemon
/// writes it, naming the agent.
pub const AGENT_DUMP: &str = "an agent's core: ";
/// The same, for an invariant of the agent's core that failed.
pub const AGENT_VIOLATION_DUMP: &str = "an agent's core: an invariant failed: ";
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
    /// A client call forwarded to the host that owns its agent, complete
    /// as it arrives.
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
    /// A pass over the deliveries outbox, for someone who waits for its
    /// report.
    Drain {
        done: oneshot::Sender<DrainReport>,
    },
    /// A retention pass, for someone who waits for what it did.
    Retain {
        retain: Retain,
        done: oneshot::Sender<Retained>,
    },
    /// A read of the store, between steps.
    Read(Box<dyn FnOnce(&Sqlite) + Send>),
    /// The shell wants a channel, and waits for the core's word on it.
    Channel {
        want: Input,
        reply: oneshot::Sender<Result<ChannelId, ChannelError>>,
    },
    /// A link resuming with a ticket came up, and holds what its peer sent
    /// early until the core says whether it takes the identity the ticket
    /// names: true when it does.
    Resumed {
        up: Input,
        reply: oneshot::Sender<bool>,
    },
    /// A look at what the core decided, between steps.
    Look(Box<dyn FnOnce(&Daemon) + Send>),
    /// The core as a dump carries it, between steps: its newest checkpoint
    /// and the ring's lines since, with the store's main file copied to
    /// `store`.
    Capture {
        store: PathBuf,
        done: oneshot::Sender<Result<Captured, String>>,
    },
    /// Reports are queued, or a timer is due.
    Wake,
    /// The profile is gone.
    Stop,
}

/// What a link or channel reported, stepped ahead of the shell's inputs.
pub(crate) enum Report {
    Input(Input),
}

/// A client call as it arrives. What the request also carries from the
/// shell is read on the core thread, at the point the core answers.
pub(crate) enum Ask {
    SubscribeSession {
        agent_id: Vec<u8>,
        tail: u32,
    },
    ListItems(ListItemsRequest),
    GetItem(GetItemRequest),
    SubscribeInventory,
    /// A request the shell already completed.
    Ready(ClientRequest),
}

/// A profile's core thread, from the shell's side.
pub(crate) struct Driver {
    inputs: Feed,
    /// What links and channels reported, in the order they did.
    reports: Arc<Mutex<VecDeque<Report>>>,
    calls: Arc<CallTable>,
    /// The calls the core made of peers that are still out.
    outgoing: Outgoing,
    /// Where the core is held for stepping: the caller's cores, or this
    /// profile's own with a thread that steps them.
    cores: Cores,
    /// The thread stepping `cores` when the caller has none of its own.
    stepper: Option<stepped::Stepper>,
    /// The core as held for stepping, once started.
    slot: std::sync::OnceLock<Arc<stepped::Slot>>,
    /// The trust store as the core last published it.
    trust: crate::trust::SharedTrustStore,
    /// The pairing window as the core last said.
    window: Arc<pairing::PairWindow>,
    /// The pairing streams the driver carries for the core.
    pairings: Arc<pairing::Carriers>,
    /// Ends once the core is gone, its store and ring closed.
    closed: watch::Receiver<()>,
}

/// The tasks carrying the calls the core made of peers, by call.
type Outgoing = Arc<Mutex<HashMap<CallId, AbortHandle>>>;

impl Driver {
    /// The driver for `runtime`'s profile and the core thread it feeds,
    /// owning its `store`, to be started once the runtime is built. The
    /// thread ends when the driver is dropped, or the runtime is.
    pub(crate) fn new(
        runtime: Weak<ProfileRuntime>,
        store: Sqlite,
        dir: PathBuf,
        options: StartOptions,
        dump_interval_ms: u64,
        cores: Option<Cores>,
    ) -> (Self, CoreThread) {
        let (inputs, fed) = mpsc::channel(INPUT_ROOM);
        let (cores, stepper) = match cores {
            Some(cores) => (cores, None),
            None => {
                let (cores, stepper) = stepped::Stepper::start();
                (cores, Some(stepper))
            }
        };
        let inputs = Feed::new(inputs, Some(cores.bell()));
        let calls = Arc::new(CallTable::default());
        let reports = Arc::new(Mutex::new(VecDeque::new()));
        let outgoing = Outgoing::default();
        let window = Arc::new(pairing::PairWindow::default());
        let pairings = Arc::new(pairing::Carriers::default());
        let daemon = Daemon::new(store, options);
        let trust = Arc::new(std::sync::RwLock::new(daemon.trust().clone()));
        // The ring's first checkpoint is the core as it starts, beside a
        // main file holding every commit so far.
        let store_at_checkpoint = Sqlite::checkpoint_wal(&dir.join(STORE))
            .inspect_err(|error| tracing::warn!(%error, "moving the store's WAL at start failed"))
            .is_ok();
        let ring = Ring::open(dir.join(CORE_RING), RING_BYTES, &daemon.checkpoint())
            .inspect_err(|error| tracing::warn!(%error, "the core's input ring does not open"))
            .ok();
        let (closed, closed_seen) = watch::channel(());
        let core = CoreThread {
            daemon,
            ring,
            store_at_checkpoint,
            checkpointing: None,
            checkpoint_waiting: Vec::new(),
            checkpoint_again: false,
            drain_waiting: Vec::new(),
            retain_reply: None,
            fed,
            reports: reports.clone(),
            channel_reply: None,
            resumed_reply: None,
            // Fresh randomness, then that it has started, are the first
            // things the core is told.
            again: [Input::Entropy(entropy()), Input::Started]
                .into_iter()
                .collect(),
            inputs: inputs.downgrade(),
            runtime,
            calls: calls.clone(),
            outgoing: outgoing.clone(),
            timers: BTreeMap::new(),
            alarm: None,
            links_since: tokio::time::Instant::now(),
            link_timers: BTreeMap::new(),
            link_alarm: None,
            waiting: HashMap::new(),
            ticked: None,
            dumps: DumpLimit::new(dump_interval_ms),
            settings: None,
            trust: trust.clone(),
            window: window.clone(),
            pairings: pairings.clone(),
            _closed: closed,
        };
        let driver = Self {
            inputs,
            reports,
            calls,
            outgoing,
            cores,
            stepper,
            slot: std::sync::OnceLock::new(),
            trust,
            window,
            pairings,
            closed: closed_seen,
        };
        (driver, core)
    }

    /// Resolves once the core is gone and its store and ring are closed,
    /// wherever the last handle on the profile was let go.
    pub(crate) fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut closed = self.closed.clone();
        async move { while closed.changed().await.is_ok() {} }
    }

    /// Reports what a link or channel did. Never waits, so a stream can
    /// say it ended as it is dropped.
    pub(crate) fn report(&self, input: Input) {
        self.reports.lock().unwrap().push_back(Report::Input(input));
        // A full channel wakes the core anyway.
        let _ = self.inputs.try_send(Fed::Wake);
    }

    /// Asks the core for a channel to `peer` over `link`, and waits for
    /// its word.
    pub(crate) async fn want_channel(
        &self,
        link: LinkId,
        peer: HostId,
    ) -> Result<ChannelId, ChannelError> {
        let (reply, word) = oneshot::channel();
        let want = Input::WantChannel { link, peer };
        if self
            .inputs
            .send(Fed::Channel { want, reply })
            .await
            .is_err()
        {
            return Err(ChannelError::NoLink);
        }
        word.await.unwrap_or(Err(ChannelError::NoLink))
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

    /// The core as a dump carries it, read between steps, with the store's
    /// main file copied to `store`; or why it cannot be.
    pub(crate) async fn capture(&self, store: PathBuf) -> Result<Captured, String> {
        let (done, captured) = oneshot::channel();
        if self
            .inputs
            .send(Fed::Capture { store, done })
            .await
            .is_err()
        {
            return Err("the profile is no longer running".to_owned());
        }
        captured
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

    /// Asks the core for a pass over the deliveries outbox and waits for
    /// its report: the pass under way, else the one asking begins.
    pub(crate) async fn drain(&self) -> DrainReport {
        let (done, drained) = oneshot::channel();
        if self.inputs.send(Fed::Drain { done }).await.is_err() {
            return DrainReport::default();
        }
        drained.await.unwrap_or_default()
    }

    /// Asks the core for a retention pass and waits for what it did.
    pub(crate) async fn retain(&self, retain: Retain) -> Option<Retained> {
        let (done, retained) = oneshot::channel();
        self.inputs.send(Fed::Retain { retain, done }).await.ok()?;
        retained.await.ok()
    }

    /// Reads the store on the core thread, between steps.
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Sqlite) -> T + Send + 'static,
    ) -> Option<T> {
        let (done, value) = oneshot::channel();
        let read = Box::new(move |store: &Sqlite| {
            let _ = done.send(read(store));
        });
        self.inputs.send(Fed::Read(read)).await.ok()?;
        value.await.ok()
    }

    /// Looks at what the core decided, on its thread between steps.
    pub(crate) async fn look<T: Send + 'static>(
        &self,
        look: impl FnOnce(&Daemon) -> T + Send + 'static,
    ) -> Option<T> {
        let (done, value) = oneshot::channel();
        let look = Box::new(move |daemon: &Daemon| {
            let _ = done.send(look(daemon));
        });
        self.inputs.send(Fed::Look(look)).await.ok()?;
        value.await.ok()
    }

    /// Tells the core a link resuming with a ticket came up and waits for
    /// its word: whether it takes the identity the ticket names. Anything
    /// reported before is stepped first.
    pub(crate) async fn resumed(&self, up: Input) -> bool {
        let (reply, word) = oneshot::channel();
        if self.inputs.send(Fed::Resumed { up, reply }).await.is_err() {
            return false;
        }
        word.await.unwrap_or(false)
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
        self.pairings.end_all();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Every stream ends with the profile; the thread sees the stop or,
        // if the channel is full, the runtime gone.
        self.calls.end_all(Status::Ok);
        self.hang_up_all();
        let _ = self.inputs.try_send(Fed::Stop);
        // The profile is gone only once its store is closed: a store closed
        // later would checkpoint its WAL into a file someone may already
        // be replacing. The stepper, letting go of the profile in a read,
        // ends on its own.
        if let Some(stepper) = self.stepper.take() {
            stepper.stop();
        }
        if let Some(slot) = self.slot.get() {
            slot.end();
        }
    }
}

/// A call as the core thread takes it, before its request is completed.
enum Arriving {
    Client(CallId, Ask),
    Peer(HostId, CallId, Box<PeerRequest>),
}

/// The core's newest checkpoint and the ring's lines since, as written.
pub(crate) struct Captured {
    pub(crate) checkpoint: Vec<u8>,
    pub(crate) lines: Vec<u8>,
}

/// Someone waiting for an agent's journal to be read to its end.
struct Waiting {
    frames: u64,
    done: oneshot::Sender<Result<u64, String>>,
}

/// A profile's core and what its thread keeps.
pub(crate) struct CoreThread {
    daemon: Daemon,
    /// Every input stepped since the last checkpoint, and what each ingest
    /// read, when the ring opened.
    ring: Option<Ring<Recorded>>,
    /// The store's main file holds what the ring's newest checkpoint was
    /// taken beside: the WAL was moved into it whole, and no move has
    /// started since.
    store_at_checkpoint: bool,
    /// The checkpoint's job in flight: its WAL job, then its write job.
    checkpointing: Option<JobId>,
    /// Whoever waits for the next checkpoint to be on the drive.
    checkpoint_waiting: Vec<oneshot::Sender<Result<(), String>>>,
    /// A waiter came after the checkpoint in flight cut the store's main
    /// file, so another is taken once it ends.
    checkpoint_again: bool,
    /// Whoever waits for a drain pass, with the first pass whose report
    /// answers them.
    drain_waiting: Vec<(u64, oneshot::Sender<DrainReport>)>,
    /// Whoever waits for the retention pass under way.
    retain_reply: Option<oneshot::Sender<Retained>>,
    fed: mpsc::Receiver<Fed>,
    reports: Arc<Mutex<VecDeque<Report>>>,
    /// Whoever waits for the core's word on the channel the step under way
    /// was asked for.
    channel_reply: Option<oneshot::Sender<Result<ChannelId, ChannelError>>>,
    /// Whoever holds a resumed link's early streams until the core decides
    /// the step under way: told false if it refuses the link.
    resumed_reply: Option<oneshot::Sender<bool>>,
    /// Inputs the driver raised itself while carrying out a step, stepped
    /// before anything newer.
    again: VecDeque<Input>,
    inputs: WeakFeed,
    runtime: Weak<ProfileRuntime>,
    calls: Arc<CallTable>,
    outgoing: Outgoing,
    /// The timers the core armed, by their deadline on the profile's clock.
    /// The map is the truth: a timer the core cancelled or re-armed is not
    /// in it, or is in it at its new deadline, when a turn looks.
    timers: BTreeMap<TimerId, u64>,
    /// The one alarm, set to the soonest deadline, which wakes the loop.
    alarm: Option<(u64, AbortHandle)>,
    /// Where the core's link time starts: it runs on the runtime's own
    /// clock, as the transports do, however the profile's clock moves.
    links_since: tokio::time::Instant,
    /// The timers armed in link time, as `timers` holds the rest.
    link_timers: BTreeMap<TimerId, u64>,
    /// The alarm for the soonest of them.
    link_alarm: Option<(u64, AbortHandle)>,
    waiting: HashMap<daemon_core::AgentId, Vec<Waiting>>,
    /// The clocks as last told to the core: policy time, then link time.
    ticked: Option<(u64, u64)>,
    /// The dumps the core asked for that were written, by kind.
    dumps: DumpLimit,
    /// The settings as last told to the core.
    settings: Option<Settings>,
    trust: crate::trust::SharedTrustStore,
    window: Arc<pairing::PairWindow>,
    pairings: Arc<pairing::Carriers>,
    /// Dropped after every other field, so whoever waits on it finds the
    /// store and the ring closed.
    _closed: watch::Sender<()>,
}

/// The bundle the core asked for at `now_ms`, or `None` when the limit
/// holds it back.
fn limited_dump_reason(
    limit: &mut DumpLimit,
    reason: &daemon_core::DumpReason,
    now_ms: u64,
) -> Option<core_ring::Admitted> {
    limit.reason(CORE_DUMP, reason, now_ms)
}

/// Fresh randomness for the core to draw from.
fn entropy() -> [u8; 32] {
    use ring::rand::SecureRandom as _;
    let mut seed = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut seed)
        .expect("the system's randomness is readable");
    seed
}

/// Whether `input` opens a pairing window or a pairing conversation: the
/// steps that draw a pairing's first secret. Every later draw is the same
/// pairing's.
fn opens_pairing(input: &Input) -> bool {
    matches!(
        input,
        Input::ClientCall {
            request: ClientRequest::StartPairing { .. },
            ..
        } | Input::PairingOpened { .. }
    )
}

impl CoreThread {
    /// Holds the core in `driver`'s cores, on the runtime it is called
    /// from, for whoever steps them.
    pub(crate) fn start(self, driver: &Driver) {
        let _ = driver.slot.set(driver.cores.hold(self));
    }

    /// Steps at most `most` of the inputs waiting, on the calling thread.
    fn step_waiting(&mut self, most: usize) -> stepped::Turn {
        let mut turn = stepped::Turn {
            stepped: 0,
            more: true,
            ended: false,
        };
        while turn.stepped < most {
            let fed = match self.again.pop_front() {
                Some(input) => Fed::Input(input),
                None => match self.fed.try_recv() {
                    Ok(fed) => fed,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        turn.more = false;
                        return turn;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        turn.ended = true;
                        return turn;
                    }
                },
            };
            turn.stepped += 1;
            if !self.turn(fed) {
                turn.ended = true;
                return turn;
            }
        }
        turn
    }

    /// Takes one thing fed to the core; false once the core has ended.
    fn turn(&mut self, fed: Fed) -> bool {
        let mut out = Vec::new();
        let fed = match fed {
            Fed::Stop => return false,
            Fed::Read(read) => {
                read(self.daemon.store());
                return true;
            }
            Fed::Look(look) => {
                look(&self.daemon);
                return true;
            }
            Fed::Capture { store, done } => {
                let _ = done.send(self.capture(&store));
                return true;
            }
            fed => fed,
        };
        let Some(runtime) = self.runtime.upgrade() else {
            return false;
        };
        // The clock first, so a timer the step arms counts from now;
        // then links and channels.
        self.tick(&runtime, &mut out);
        let reports = std::mem::take(&mut *self.reports.lock().unwrap());
        for report in reports {
            let Report::Input(input) = report;
            self.step(&runtime, input, &mut out);
        }
        let fed = match fed {
            Fed::Channel { want, reply } => {
                self.channel_reply = Some(reply);
                self.step(&runtime, want, &mut out);
                self.channel_reply = None;
                return true;
            }
            Fed::Resumed { up, reply } => {
                self.resumed_reply = Some(reply);
                self.step(&runtime, up, &mut out);
                // Not refused: taken up.
                if let Some(reply) = self.resumed_reply.take() {
                    let _ = reply.send(true);
                }
                return true;
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
                return true;
            }
            Fed::Wake => {
                self.fire_due(&runtime, &mut out);
                return true;
            }
            fed => fed,
        };
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
                match self.checkpointing {
                    // One still moving its WAL covers every commit its
                    // waiter has seen: commits wait for it.
                    Some(_) if self.daemon.checkpoints().holding() => None,
                    // One past that does not, so another follows it.
                    Some(_) => {
                        self.checkpoint_again = true;
                        None
                    }
                    None => Some(Input::Timer {
                        id: TimerId::Checkpoint,
                    }),
                }
            }
            Fed::Drain { done } => {
                // The timer begins a pass now unless one is under way.
                let pass = self.daemon.deliveries().answering_pass();
                self.drain_waiting.push((pass, done));
                Some(Input::Timer {
                    id: TimerId::Deliveries,
                })
            }
            Fed::Retain { retain, done } => {
                self.retain_reply = Some(done);
                Some(Input::Retain(retain))
            }
            Fed::Wake
            | Fed::Stop
            | Fed::Channel { .. }
            | Fed::Resumed { .. }
            | Fed::Forward { .. }
            | Fed::Look(_)
            | Fed::Read(_)
            | Fed::Capture { .. } => None,
        };
        let settings = runtime.core_settings();
        if self.settings != Some(settings) {
            self.settings = Some(settings);
            self.step(&runtime, Input::Configure(settings), &mut out);
        }
        let input = match call {
            Some(Arriving::Client(call, ask)) => Some(Input::ClientCall {
                call,
                request: runtime.client_request(self.daemon.store(), ask),
            }),
            Some(Arriving::Peer(from, call, request)) => {
                let request = *request;
                let stream = match &request {
                    PeerRequest::SubscribeInventory => Some("inventory"),
                    PeerRequest::SubscribeSession(_) => Some("session"),
                    _ => None,
                };
                if let Some(stream) = stream {
                    tracing::debug!(
                        host = %runtime.host(),
                        peer = %host_from_core(from),
                        stream,
                        "a peer opened a stream"
                    );
                }
                Some(Input::PeerCall {
                    from,
                    call,
                    request,
                })
            }
            None => input,
        };
        if let Some(input) = input {
            self.step(&runtime, input, &mut out);
        }
        drop(runtime);
        true
    }

    /// The alarm rang: steps the soonest timer due, judged against the map
    /// as it stands, and sets the alarm again for the next. One firing a
    /// wake, so a core re-arming the same timer at once (ingest's next
    /// batch) takes its turns in the channel's order with every other
    /// input.
    fn fire_due(&mut self, runtime: &Arc<ProfileRuntime>, out: &mut Vec<Output>) {
        let now = runtime.clock_now().max(0) as u64;
        let due = soonest_due(&self.timers, now);
        if let Some((_, id)) = due {
            self.timers.remove(&id);
            self.step(runtime, Input::Timer { id }, out);
        }
        let due = soonest_due(&self.link_timers, self.link_now());
        if let Some((_, id)) = due {
            self.link_timers.remove(&id);
            self.step(runtime, Input::Timer { id }, out);
        }
        self.set_alarm(runtime);
        self.set_link_alarm();
    }

    fn link_now(&self) -> u64 {
        self.links_since.elapsed().as_millis() as u64
    }

    /// Sets the link-time alarm to the soonest deadline in link time.
    fn set_link_alarm(&mut self) {
        let now = self.link_now();
        let next = self.link_timers.values().copied().min();
        let (inputs, since) = (&self.inputs, self.links_since);
        rearm(&mut self.link_alarm, now, next, |at| {
            let inputs = inputs.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(since + std::time::Duration::from_millis(at)).await;
                if let Some(inputs) = inputs.upgrade() {
                    let _ = inputs.send(Fed::Wake).await;
                }
            })
            .abort_handle()
        });
    }

    /// Sets the one alarm to the soonest deadline, on the profile's clock;
    /// it wakes the loop, which steps the soonest timer due by then. A
    /// deadline already passed rings at once, and the wake queues behind
    /// the inputs already waiting.
    fn set_alarm(&mut self, runtime: &Arc<ProfileRuntime>) {
        let now = runtime.clock_now().max(0) as u64;
        let next = self.timers.values().copied().min();
        let inputs = &self.inputs;
        rearm(&mut self.alarm, now, next, |at| {
            let due = runtime.clock().sleep_until(at.min(i64::MAX as u64) as i64);
            let inputs = inputs.clone();
            tokio::spawn(async move {
                due.await;
                if let Some(inputs) = inputs.upgrade() {
                    let _ = inputs.send(Fed::Wake).await;
                }
            })
            .abort_handle()
        });
    }

    /// Tells the core the time, when it moved.
    fn tick(&mut self, runtime: &Arc<ProfileRuntime>, out: &mut Vec<Output>) {
        let now = (runtime.clock_now().max(0) as u64, self.link_now());
        if self.ticked != Some(now) {
            self.ticked = Some(now);
            let (now_ms, link_ms) = now;
            self.step(runtime, Input::Tick { now_ms, link_ms }, out);
        }
    }

    /// Steps one input and carries out what it asked for.
    fn step(&mut self, runtime: &Arc<ProfileRuntime>, input: Input, out: &mut Vec<Output>) {
        // A call's own answers, to its opening or its lag, are always
        // taken; every other answer is a live one, held to the call's room.
        let replying = match &input {
            Input::ClientCall { call, .. } | Input::AnswerLagged { call } => Some(*call),
            _ => None,
        };
        // A checkpoint's WAL job done has the core hand over its state,
        // and the Checkpointed fed back for it ends the checkpoint; the
        // job failing ends it at once.
        let checkpointed = match &input {
            Input::JobDone { job, result } if Some(*job) == self.checkpointing => match result {
                JobResult::Done => None,
                other => {
                    self.checkpointing = None;
                    Some(Err(format!("{other:?}")))
                }
            },
            Input::Checkpointed => {
                self.checkpointing = None;
                Some(Ok(()))
            }
            _ => None,
        };
        if let Input::PeerAnswer { call, answer } = &input
            && answer.is_last()
        {
            self.outgoing.lock().unwrap().remove(call);
        }
        // A pairing draws its secrets after fresh randomness, so the seed a
        // checkpoint or the ring holds reveals no pairing opened after it.
        if opens_pairing(&input) {
            self.step(runtime, Input::Entropy(entropy()), out);
        }
        self.record(&input);
        self.daemon.step(input, out);
        // The call's own answers reach its queue together, where the first
        // of them stood, so a reader woken by one finds the step's rest: a
        // Subscribe's opening rows are there with its Snapshot.
        let mut own = Vec::new();
        let mut first = None;
        let mut outputs = Vec::with_capacity(out.len());
        // Carrying out an output may step another input, with outputs of
        // its own.
        for output in std::mem::take(out) {
            match output {
                Output::Answer { call, response } if replying == Some(call) => {
                    first.get_or_insert(outputs.len());
                    own.push(response);
                }
                output => outputs.push(output),
            }
        }
        let mut own = Some(own);
        for (at, output) in outputs.into_iter().enumerate() {
            if first == Some(at)
                && let (Some(call), Some(own)) = (replying, own.take())
            {
                self.calls.answer_all(call, own);
            }
            self.execute(runtime, output, replying);
        }
        if let (Some(call), Some(own)) = (replying, own)
            && first.is_some()
        {
            self.calls.answer_all(call, own);
        }
        if let Some(outcome) = checkpointed
            && self.checkpointing.is_none()
        {
            if std::mem::take(&mut self.checkpoint_again) {
                self.again.push_back(Input::Timer {
                    id: TimerId::Checkpoint,
                });
            } else {
                for waiter in self.checkpoint_waiting.drain(..) {
                    let _ = waiter.send(outcome.clone());
                }
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
        if let Err(error) = ring.append_as(&Recorded::input(input)) {
            tracing::warn!(%error, "recording an input in the core's ring failed");
        }
        if ring.full() && self.checkpointing.is_none() {
            self.again.push_back(Input::Timer {
                id: TimerId::Checkpoint,
            });
        }
    }

    /// The core's newest checkpoint and the ring's lines since, with the
    /// store's main file as of that checkpoint copied to `store`. Refused
    /// while the main file is not that store: a WAL move under way, or one
    /// that failed since.
    fn capture(&self, store: &Path) -> Result<Captured, String> {
        let runtime = self.runtime.upgrade().ok_or("the profile is stopping")?;
        if self.ring.is_none() {
            return Err("the core's input ring did not open".to_owned());
        }
        if !self.store_at_checkpoint {
            return Err("the store's main file is not the checkpoint's".to_owned());
        }
        let (checkpoint, lines) = core_ring::newest(&runtime.dir().join(CORE_RING))
            .map_err(|error| format!("the core's input ring: {error}"))?
            .ok_or("the core's input ring holds no checkpoint")?;
        std::fs::copy(runtime.dir().join(STORE), store)
            .map_err(|error| format!("copying the store's main file: {error}"))?;
        Ok(Captured { checkpoint, lines })
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
                if let Some(ring) = self.ring.as_mut()
                    && let Err(error) = ring.append(&Recorded::read(agent, &report))
                {
                    tracing::warn!(%error, "recording what ingest read in the core's ring failed");
                }
                runtime.ingested(&report);
                self.ingested(agent, report);
            }
            Output::Retained(retained) => {
                if let Some(done) = self.retain_reply.take() {
                    let _ = done.send(*retained);
                }
            }
            Output::Drained { pass, report } => {
                let (answered, waiting) = std::mem::take(&mut self.drain_waiting)
                    .into_iter()
                    .partition(|(first, _)| *first <= pass);
                self.drain_waiting = waiting;
                for (_, done) in answered {
                    let _ = done.send(report);
                }
            }
            Output::ArmTimer { id, at_ms } => {
                // Every timer keeps to the clock the core reckons it on:
                // link time, or the profile's clock. A follower or source
                // due now is stepped next, with the store; anything else
                // due now rings the alarm and queues behind the inputs
                // already waiting.
                let now = runtime.clock_now().max(0) as u64;
                let sourcing = matches!(id, TimerId::Follow(_) | TimerId::Source(..));
                if id.on_link_clock() {
                    self.link_timers.insert(id, at_ms);
                    self.set_link_alarm();
                } else if sourcing && at_ms <= now {
                    self.timers.remove(&id);
                    self.again.push_back(Input::Timer { id });
                } else {
                    self.timers.insert(id, at_ms);
                    self.set_alarm(runtime);
                }
            }
            Output::CancelTimer { id } => {
                if self.link_timers.remove(&id).is_some() {
                    self.set_link_alarm();
                }
                self.timers.remove(&id);
                self.set_alarm(runtime);
            }
            Output::Checkpoint { state } => {
                // The inputs after this step are the ones this checkpoint
                // does not hold; the first of them tells the core so.
                if let Some(ring) = self.ring.as_mut()
                    && let Err(error) = ring.rotate(&state)
                {
                    tracing::warn!(%error, "rotating the core's input ring failed");
                }
                // Its WAL job moved every commit into the main file.
                self.store_at_checkpoint = true;
                self.again.push_back(Input::Checkpointed);
            }
            Output::Job {
                id,
                op: Job::CheckpointWal,
            } => {
                self.store_at_checkpoint = false;
                self.checkpointing = Some(id);
                tokio::spawn(run_job(
                    Arc::downgrade(runtime),
                    id,
                    Job::CheckpointWal,
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
                // thread, reading the store between later steps, so a burst
                // of the same reason is one bundle.
                let now = runtime.clock_now().max(0) as u64;
                let check = reason.check().map_or("", |check| check.id).to_owned();
                let Some(admitted) = limited_dump_reason(&mut self.dumps, &reason, now) else {
                    return;
                };
                let core = crate::reports::Core::Daemon;
                runtime.check_failed(core, &check, admitted.held_back);
                runtime.dump_unasked(
                    wire::DumpScope::Core,
                    Vec::new(),
                    admitted.reason,
                    crate::reports::check_id(core, &check),
                    "the daemon core",
                );
            }
            Output::Channel(word) => match self.channel_reply.take() {
                Some(reply) => {
                    if let Err(Ok(channel)) = reply.send(word) {
                        // Nobody waits to open it: it never opened.
                        self.again.push_back(Input::ChannelClosed { channel });
                    }
                }
                None => {
                    tracing::error!(?word, "the daemon core answered a channel nobody asked for")
                }
            },
            Output::Abandon { host, attempt } => {
                if let Some(edge) = runtime.edge() {
                    edge.reachability().abandon(host_from_core(host), attempt);
                }
            }
            Output::Rediscover => {
                if let Some(edge) = runtime.edge() {
                    edge.reachability().requery();
                }
            }
            Output::DropRoute { host, route } => {
                if let Some(edge) = runtime.edge() {
                    edge.connections().drop_route(host, route);
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
                    request @ (PeerRequest::SubscribeSession(_)
                    | PeerRequest::SubscribeInventory) => {
                        tokio::spawn(origin::stream(runtime, host, call, request, inputs))
                    }
                    request => tokio::spawn(async move {
                        let answer = if matches!(request, PeerRequest::ListItems(_)) {
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
                        reports.lock().unwrap().push_back(Report::Input(answered));
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
            Output::Dial {
                host,
                attempt,
                plan,
                early,
            } => {
                if let Some(edge) = runtime.edge() {
                    edge.reachability()
                        .dial(host_from_core(host), attempt, plan, early);
                }
            }
            Output::DialRelay { attempt } => match runtime.edge() {
                Some(edge) => {
                    tokio::spawn(async move { edge.dial_relay(attempt).await });
                }
                None => self.again.push_back(Input::RelayEnded {
                    attempt,
                    outcome: daemon_core::RelayOutcome::Lost("the edge is not up".to_owned()),
                }),
            },
            Output::SpawnAgent { dir, spec } => runtime.spawn_process(dir, *spec),
            Output::DialAgent { agent } => runtime.dial_agent(uuid::Uuid::from_bytes(agent.0)),
            Output::WriteControl { agent, frame } => {
                let id = uuid::Uuid::from_bytes(agent.0);
                match &frame.of {
                    Some(wire::ctl_frame::Of::AgentMessage(_)) => {
                        runtime.write_message(id, frame, self.inputs.clone())
                    }
                    _ => runtime.write_control(id, frame),
                }
            }
            Output::ForgetVerdict { agent, input_id } => {
                runtime.forget_verdict(uuid::Uuid::from_bytes(agent.0), &input_id)
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
            Output::Pair { session, message } => self.pairings.send(session, message),
            Output::PairPending {
                session,
                token,
                peer,
            } => self.pairings.pending(session, token, peer),
            Output::PairEnded { session, failure } => self.pairings.end(session, failure),
            // The peer's path machine probes what it lists.
            Output::PeerAddresses { host, addrs } => {
                if let Some(edge) = runtime.edge() {
                    edge.paths()
                        .addresses(uuid::Uuid::from_bytes(host.0), &addrs);
                }
            }
            // Every link tells its peer where the relay sees this host.
            Output::ObservedAddress { addr } => {
                if let Some(edge) = runtime.edge() {
                    edge.seen_at(addr);
                }
            }
            // The link holding the early streams resets them, saying the
            // peer is revoked.
            Output::RefuseResumed { .. } => {
                if let Some(reply) = self.resumed_reply.take() {
                    let _ = reply.send(false);
                }
            }
        }
    }

    /// Does what the core left to the shell.
    fn shell_work(&mut self, runtime: &Arc<ProfileRuntime>, work: ShellWork) {
        match work {
            ShellWork::Wrote => runtime.wrote(),
            ShellWork::Sourcing { agent, open } => runtime.sourcing(agent, open),
            ShellWork::Hosts { entries } => runtime.hosts_published(entries),
            ShellWork::ReplicaDropped { agent } => runtime.replica_dropped(&agent),
            ShellWork::Trust { store, changed } => {
                *self.trust.write().unwrap() = *store;
                if let Some((host, changed)) = changed {
                    Self::trust_changed(runtime, uuid::Uuid::from_bytes(host.0), changed);
                }
            }
            ShellWork::ReplacementEnded { host } => {
                Self::replacement_ended(runtime, uuid::Uuid::from_bytes(host.0))
            }
            ShellWork::PairWindow { active, locked_out } => self.window.publish(active, locked_out),
            ShellWork::Pairing(event) => pairing::record(&event, runtime.analytics()),
        }
    }

    /// What follows a trust change on the drive: on the connections, and
    /// for a host forgotten, the tickets and tokens it gave this profile's
    /// dials, which nothing would dial it with again.
    fn trust_changed(
        runtime: &ProfileRuntime,
        host: uuid::Uuid,
        changed: daemon_core::trust::TrustChanged,
    ) {
        use daemon_core::trust::{PairingUpdate, TrustChanged};

        let profile = runtime.dir();
        let update = match changed {
            TrustChanged::Paired(update) => update,
            TrustChanged::Removed(_) => {
                for kept in [crate::CLIENT_TICKETS_DIR, crate::CLIENT_TOKENS_DIR] {
                    let path = profile.join(kept).join(host.to_string());
                    let removed = match std::fs::metadata(&path) {
                        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
                        Ok(_) => std::fs::remove_file(&path),
                        Err(_) => Ok(()),
                    };
                    if let Err(error) = removed {
                        tracing::warn!(%error, path = %path.display(), "cannot remove a forgotten host's file");
                    }
                }
                return;
            }
            _ => return,
        };
        if update == PairingUpdate::ReplacedPubkey
            && let Some(edge) = runtime.edge()
        {
            edge.connections().finish_host_replacement(host);
        }
    }

    /// A key replacement that was not written: the host's connections may
    /// open again under the key it had.
    fn replacement_ended(runtime: &ProfileRuntime, host: uuid::Uuid) {
        if let Some(edge) = runtime.edge() {
            edge.connections().finish_host_replacement(host);
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
async fn feed(inputs: &WeakFeed, input: Input) {
    if let Some(inputs) = inputs.upgrade() {
        let _ = inputs.send(Fed::Input(input)).await;
    }
}

/// Runs one job off the core thread and feeds its result back.
async fn run_job(runtime: Weak<ProfileRuntime>, job: JobId, op: Job, inputs: WeakFeed) {
    let result = match op {
        Job::Fsync { path, bytes } => fsync(path, bytes).await,
        Job::TeardownHost { host } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            if let Some(edge) = runtime.edge() {
                edge.connections()
                    .teardown_host(uuid::Uuid::from_bytes(host.0))
                    .await;
            }
            JobResult::Done
        }
        Job::CheckpointWal => {
            let Some(dir) = runtime.upgrade().map(|runtime| runtime.dir().to_path_buf()) else {
                return;
            };
            blocking(move || {
                Sqlite::checkpoint_wal(&dir.join(STORE)).map_err(std::io::Error::other)
            })
            .await
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
            blocking(move || {
                agent_dir::local_socket::forget_short_addresses(&path);
                match std::fs::remove_dir_all(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    removed => removed,
                }
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
        Job::Compare {
            cwd,
            base,
            with_patch,
        } => crate::blobs::compare(&cwd, base, with_patch).await,
        Job::NextSpec {
            agent,
            bootstrap,
            snapshot,
        } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            let agent = uuid::Uuid::from_bytes(agent.0);
            match runtime.next_spec(agent, bootstrap, snapshot.as_deref()) {
                Ok(spec) => JobResult::Spec(spec.map(Box::new)),
                Err(error) => JobResult::Failed(error.to_string()),
            }
        }
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
    Some(runtime.call_peer(host_from_core(host), request).await)
}

/// The fsync job: replaces the file at `path` with `bytes` so that a crash
/// or power cut leaves the old file or the new one, and the new one is on
/// the drive when it is done. The installation runs it too, for the writes
/// it promises durable outside any profile's core.
pub(crate) async fn fsync(path: PathBuf, bytes: Vec<u8>) -> JobResult {
    blocking(move || write_durably(&path, &bytes)).await
}

async fn blocking(work: impl FnOnce() -> std::io::Result<()> + Send + 'static) -> JobResult {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(())) => JobResult::Done,
        Ok(Err(error)) => JobResult::Failed(error.to_string()),
        Err(error) => JobResult::Failed(error.to_string()),
    }
}

impl ProfileRuntime {
    /// Completes a client call's request from what the shell owns, at the
    /// point the core answers it.
    fn client_request(&self, store: &Sqlite, ask: Ask) -> ClientRequest {
        match ask {
            Ask::SubscribeSession { agent_id, tail } => {
                self.used(store, &agent_id);
                ClientRequest::SubscribeSession { agent_id, tail }
            }
            Ask::ListItems(request) => {
                self.used(store, &request.agent_id);
                ClientRequest::ListItems(request)
            }
            Ask::GetItem(request) => ClientRequest::GetItem(request),
            Ask::SubscribeInventory => ClientRequest::SubscribeInventory,
            Ask::Ready(request) => request,
        }
    }
}

/// Keeps `alarm` when it is still to ring at `next`, and otherwise stops
/// it and has `ring_at` set one for `next`, if any. An alarm at or past
/// `now` may have rung already, so it is set afresh. One replaced is
/// stopped: wakes come from reports too, and an alarm left sleeping would
/// ring again and set one more of its own.
/// The timer due soonest by `now`, if any is.
fn soonest_due(timers: &BTreeMap<TimerId, u64>, now: u64) -> Option<(u64, TimerId)> {
    timers
        .iter()
        .filter(|(_, at)| **at <= now)
        .map(|(id, at)| (*at, *id))
        .min()
}

fn rearm(
    alarm: &mut Option<(u64, AbortHandle)>,
    now: u64,
    next: Option<u64>,
    ring_at: impl FnOnce(u64) -> AbortHandle,
) {
    if let Some((at, _)) = alarm
        && *at > now
        && Some(*at) == next
    {
        return;
    }
    if let Some((_, task)) = alarm.take() {
        task.abort();
    }
    *alarm = next.map(|at| (at, ring_at(at)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_alarm_is_kept_while_its_deadline_holds_and_stopped_when_replaced() {
        let mut alarm = None;
        let mut set = Vec::new();
        let mut ring_at = |at: u64| {
            let task = tokio::spawn(std::future::pending::<()>());
            set.push((at, task.abort_handle()));
            task.abort_handle()
        };
        rearm(&mut alarm, 0, Some(100), &mut ring_at);
        // Wakes that are not the alarm's leave it be.
        for now in [10, 50, 99] {
            rearm(&mut alarm, now, Some(100), &mut ring_at);
        }
        // A sooner deadline replaces it; one at hand is set afresh.
        rearm(&mut alarm, 99, Some(60), &mut ring_at);
        rearm(&mut alarm, 100, Some(60), &mut ring_at);
        rearm(&mut alarm, 100, None, &mut ring_at);
        tokio::task::yield_now().await;
        assert_eq!(
            set.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            [100, 60, 60]
        );
        assert!(
            set.iter().all(|(_, task)| task.is_finished()),
            "every replaced alarm is stopped"
        );
        assert!(alarm.is_none());
    }

    #[test]
    fn a_violation_the_daemon_core_repeats_is_one_bundle_per_interval() {
        let broke = daemon_core::DumpReason::Violation(daemon_core::Violation {
            invariant: daemon_core::GENERATION_FELL,
            detail: "3 then 2".into(),
        });
        let mut limit = DumpLimit::default();
        let first = limited_dump_reason(&mut limit, &broke, 0).unwrap().reason;
        assert!(first.starts_with(CORE_VIOLATION_DUMP), "{first}");
        for at in [1, DumpLimit::INTERVAL_MS - 1] {
            assert_eq!(limited_dump_reason(&mut limit, &broke, at), None);
        }
        // A step over its budget is news beside it.
        let slow = daemon_core::DumpReason::OverBudget {
            micros: 250_000,
            budget: daemon_core::StepBudget::DAEMON,
        };
        let slow = limited_dump_reason(&mut limit, &slow, 2).unwrap().reason;
        assert!(slow.contains("250000 µs"), "{slow}");
        let second = limited_dump_reason(&mut limit, &broke, DumpLimit::INTERVAL_MS).unwrap();
        assert_eq!(second.held_back, 2);
        let second = second.reason;
        assert!(
            second.starts_with(CORE_VIOLATION_DUMP)
                && second.ends_with("(2 more like it were not dumped since the last)"),
            "{second}"
        );
    }
}
