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

mod calls;
mod streams;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub(crate) use calls::{Answers, Delivered};
use calls::{CallTable, Carried};
use daemon_core::payload::{Caller, ClientRequest, JobResult, Opening, Status};
use daemon_core::{
    CallId, Daemon, FromShell, IngestEnd, Ingested, Input, Job, JobId, Output, Settings,
    StartOptions, TimerId,
};
use store::{AgentKey, Sqlite};
pub use streams::{InventorySubscription, Subscription};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use wire::{FetchRequest, GetRequest};

use crate::runtime::ProfileRuntime;

/// Inputs waiting for the core. A full channel holds its senders back
/// rather than growing.
const INPUT_ROOM: usize = 1024;

/// What reaches the core thread.
pub(crate) enum Fed {
    Input(Input),
    /// A client call, whose request the thread completes from the shell as
    /// it steps it.
    Call {
        call: CallId,
        who: Caller,
        ask: Ask,
    },
    /// A nudge from someone who waits for the journal to be read to its
    /// end: told the frames committed meanwhile, or why ingest failed.
    Ingest {
        agent: daemon_core::AgentId,
        done: oneshot::Sender<Result<u64, String>>,
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
    Subscribe { agent_id: Vec<u8>, from: Opening },
    Fetch(FetchRequest),
    Get(GetRequest),
    SubscribeInventory { only: Option<Vec<u8>> },
}

/// A profile's core thread, from the shell's side.
pub(crate) struct Driver {
    inputs: mpsc::Sender<Fed>,
    calls: Arc<CallTable>,
    /// What the shell committed, waiting for the core thread to take the
    /// store next.
    shell: Arc<Mutex<Vec<FromShell>>>,
    /// The agents a stream follows, as the core last said.
    watched: Arc<Mutex<HashSet<AgentKey>>>,
}

impl Driver {
    /// Starts the core thread for `runtime`'s profile. It ends when the
    /// driver is dropped, or the runtime is.
    pub(crate) fn start(
        runtime: Weak<ProfileRuntime>,
        own_host: Vec<u8>,
        options: StartOptions,
    ) -> Self {
        let (inputs, fed) = mpsc::channel(INPUT_ROOM);
        let calls = Arc::new(CallTable::default());
        let shell = Arc::new(Mutex::new(Vec::new()));
        let watched = Arc::new(Mutex::new(HashSet::new()));
        // The core owns a store of its own between steps; the profile's is
        // lent to it for each one.
        let store = Sqlite::open_in_memory(own_host).expect("an in-memory store opens");
        let core = CoreThread {
            daemon: Daemon::new(store, options),
            fed,
            again: VecDeque::new(),
            inputs: inputs.downgrade(),
            runtime,
            calls: calls.clone(),
            shell: shell.clone(),
            watched: watched.clone(),
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
            calls,
            shell,
            watched,
        }
    }

    /// Hands a call to the core and returns its answers. `room` is how many
    /// live answers the call's queue holds for a reader that has not taken
    /// them.
    pub(crate) async fn call(&self, who: Caller, ask: Ask, room: usize) -> Answers {
        let queue = self.calls.open(room);
        let call = queue.call();
        let answers = Answers::new(queue.clone(), self.calls.clone(), self.inputs.clone());
        if self
            .inputs
            .send(Fed::Call { call, who, ask })
            .await
            .is_err()
        {
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

    /// Feeds the core an input.
    pub(crate) async fn feed(&self, input: Input) {
        let _ = self.inputs.send(Fed::Input(input)).await;
    }

    /// Hands the core a change the shell committed. Called with the store
    /// still held, so the core steps it before anything that reads the
    /// store after it.
    pub(crate) fn hand_over(&self, change: FromShell) {
        self.shell.lock().unwrap().push(change);
        // A full channel wakes the core anyway.
        let _ = self.inputs.try_send(Fed::Wake);
    }

    /// Whether any stream follows the agent now.
    pub(crate) fn watched(&self, agent: &AgentKey) -> bool {
        self.watched.lock().unwrap().contains(agent)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Every stream ends with the profile; the thread sees the stop or,
        // if the channel is full, the runtime gone.
        self.calls.end_all(Status::Ok);
        let _ = self.inputs.try_send(Fed::Stop);
    }
}

/// Someone waiting for an agent's journal to be read to its end.
struct Waiting {
    frames: u64,
    done: oneshot::Sender<Result<u64, String>>,
}

struct CoreThread {
    daemon: Daemon,
    fed: mpsc::Receiver<Fed>,
    /// Inputs the driver raised itself while carrying out a step, stepped
    /// before anything newer.
    again: VecDeque<Input>,
    inputs: mpsc::WeakSender<Fed>,
    runtime: Weak<ProfileRuntime>,
    calls: Arc<CallTable>,
    shell: Arc<Mutex<Vec<FromShell>>>,
    watched: Arc<Mutex<HashSet<AgentKey>>>,
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
            let mut store = runtime.store.blocking_lock();
            let input = match fed {
                Fed::Input(input) => Some(input),
                Fed::Call { call, who, ask } => Some(Input::ClientCall {
                    call,
                    who,
                    request: runtime.client_request(&store, ask),
                }),
                Fed::Ingest { agent, done } => {
                    let waiting = Waiting { frames: 0, done };
                    self.waiting.entry(agent).or_default().push(waiting);
                    Some(Input::Nudge { agent })
                }
                Fed::Wake | Fed::Stop => None,
            };
            self.daemon.swap_store(&mut store);
            let now = runtime.clock_now().max(0) as u64;
            if self.ticked != Some(now) {
                self.ticked = Some(now);
                self.step(&runtime, Input::Tick { now_ms: now }, &mut out);
            }
            let settings = Settings {
                notify_delay_ms: runtime.launch().notify_delay_ms,
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
            if let Some(input) = input {
                self.step(&runtime, input, &mut out);
            }
            self.daemon.swap_store(&mut store);
            drop(store);
            drop(runtime);
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
        self.daemon.step(input, out);
        for output in out.drain(..) {
            self.execute(runtime, output, replying);
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
            Output::Watching { agent, watched } => {
                let mut watching = self.watched.lock().unwrap();
                if watched {
                    watching.insert(agent);
                } else {
                    watching.remove(&agent);
                }
            }
            Output::Ingested { agent, report } => {
                runtime.ingested(&report);
                self.ingested(agent, report);
            }
            Output::Wake(wake) => runtime.wake(wake),
            Output::ArmTimer { id, at_ms } => {
                let wait = at_ms.saturating_sub(runtime.clock_now().max(0) as u64);
                let inputs = self.inputs.clone();
                let task = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    feed(&inputs, Input::Timer { id }).await;
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
            Output::Job { id, op } => {
                tokio::spawn(run_job(
                    Arc::downgrade(runtime),
                    id,
                    op,
                    self.inputs.clone(),
                ));
            }
            Output::Dump { reason } => {
                tracing::error!(?reason, "the daemon core asked for a dump");
            }
            output @ (Output::OpenChannel { .. }
            | Output::CloseChannel { .. }
            | Output::Call { .. }
            | Output::SendFile { .. }
            | Output::LinkControl { .. }
            | Output::DialAgent { .. }
            | Output::WriteControl { .. }
            | Output::SpawnAgent { .. }) => {
                tracing::error!(
                    ?output,
                    "the daemon core asked for what this driver does not carry out yet"
                );
            }
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
        Job::Fsync(path) => blocking(move || std::fs::File::open(path)?.sync_all()).await,
        Job::Checkpoint { state } => {
            let Some(dir) = runtime.upgrade().map(|runtime| runtime.dir().to_path_buf()) else {
                return;
            };
            blocking(move || write_durably(&dir.join(CHECKPOINT), &state)).await
        }
        Job::FetchFromOrigin {
            agent,
            before_order,
            limit,
        } => {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            JobResult::Fetched(fetch_from_origin(&runtime, &agent, before_order, limit).await)
        }
        op @ (Job::WriteBlob { .. }
        | Job::RefreshToken { .. }
        | Job::SendPush(_)
        | Job::Upload(_)) => JobResult::Failed(format!("this driver does not run {op:?} yet")),
    };
    feed(&inputs, Input::JobDone { job, result }).await;
}

async fn fetch_from_origin(
    runtime: &ProfileRuntime,
    agent: &AgentKey,
    before_order: Option<u64>,
    limit: u32,
) -> Option<wire::FetchResponse> {
    runtime.used(agent);
    runtime
        .fetch_from_origin(agent, before_order, limit)
        .await
        .ok()
}

/// Where the profile's core checkpoint is written, in its directory.
const CHECKPOINT: &str = "core.checkpoint";

/// Writes `bytes` whole at `path`: to a sibling first, flushed, then renamed
/// over it, so a crash leaves the old file or the new one.
fn write_durably(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let partial = path.with_extension("partial");
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
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
            Ask::Subscribe { agent_id, from } => {
                let followed = self.follow_replica(store, &agent_id);
                let hook = self.join_hook.lock().unwrap().clone();
                if let Some(hook) = hook {
                    Handle::current().block_on(hook());
                }
                ClientRequest::Subscribe {
                    agent_id,
                    from,
                    tail_rows: self.launch().tail_rows,
                    followed,
                }
            }
            Ask::Fetch(request) => ClientRequest::Fetch(request),
            Ask::Get(request) => ClientRequest::Get(request),
            Ask::SubscribeInventory { only } => ClientRequest::SubscribeInventory {
                hosts: self.published_hosts(),
                only,
            },
        }
    }
}
