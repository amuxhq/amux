//! The driver around a profile's daemon core.
//!
//! One thread per profile owns the core and steps it, one input at a time,
//! from a bounded channel. The driver decides nothing: it turns calls into
//! inputs, and carries out what each step asks for, answers into the call
//! table, timers and jobs on tokio, whose outcomes come back as inputs.
//!
//! The profile's shell still writes the store under its own lock and
//! publishes live changes itself. Until those move into the core, the core
//! thread holds that lock across each step and the outputs that must meet
//! it, lending the store to the core for the step, so nothing commits
//! between a stream's opening and the point its live changes are joined.

mod calls;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

use calls::{AnswerQueue, CallTable, Carried};
pub(crate) use calls::{Answers, Delivered};
use daemon_core::payload::{Caller, ClientRequest, JobResult, Opening, Response, Status};
use daemon_core::{Daemon, Input, Job, JobId, Output, StartOptions, TimerId};
use store::{AgentKey, Sqlite};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;
use wire::{FetchRequest, GetRequest, InventoryEvent, SessionEvent, inventory_event};

use crate::runtime::ProfileRuntime;

/// Inputs waiting for the core. A full channel holds its senders back
/// rather than growing.
const INPUT_ROOM: usize = 1024;

/// What reaches the core thread: an input, or a call whose request the
/// thread completes from the shell as it steps it.
pub(crate) enum Fed {
    Input(Input),
    Call {
        call: daemon_core::CallId,
        who: Caller,
        ask: Ask,
    },
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
            timers: HashMap::new(),
            ticked: None,
        };
        let handle = Handle::current();
        std::thread::Builder::new()
            .name("amux-core".to_owned())
            .spawn(move || {
                let _entered = handle.enter();
                core.run();
            })
            .expect("the core thread starts");
        Self { inputs, calls }
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
    timers: HashMap<TimerId, AbortHandle>,
    /// The clock as last told to the core.
    ticked: Option<u64>,
}

impl CoreThread {
    fn run(mut self) {
        let mut out = Vec::new();
        loop {
            let fed = match self.again.pop_front() {
                Some(input) => Fed::Input(input),
                None => match self.fed.blocking_recv() {
                    Some(fed) => fed,
                    None => return,
                },
            };
            let Some(runtime) = self.runtime.upgrade() else {
                return;
            };
            let mut store = runtime.store.blocking_lock();
            let input = match fed {
                Fed::Input(input) => input,
                Fed::Call { call, who, ask } => Input::ClientCall {
                    call,
                    who,
                    request: runtime.client_request(&store, ask),
                },
            };
            self.daemon.swap_store(&mut store);
            let now = runtime.clock_now().max(0) as u64;
            if self.ticked != Some(now) {
                self.ticked = Some(now);
                self.daemon.step(Input::Tick { now_ms: now }, &mut out);
            }
            self.daemon.step(input, &mut out);
            self.daemon.swap_store(&mut store);
            for output in out.drain(..) {
                self.execute(&runtime, output);
            }
            drop(store);
            drop(runtime);
        }
    }

    /// Carries out one output, with the store still held.
    fn execute(&mut self, runtime: &Arc<ProfileRuntime>, output: Output) {
        match output {
            Output::Answer { call, response } => self.calls.answer(call, response),
            Output::EndCall { call, status } => self.calls.end(call, status),
            Output::Follow { call, agent } => {
                if let Some(queue) = self.calls.get(call) {
                    let live = runtime.fanout.join(&agent);
                    tokio::spawn(carry_session(queue, live, self.inputs.clone()));
                }
            }
            Output::FollowInventory { call, only } => {
                if let Some(queue) = self.calls.get(call) {
                    let live = runtime.fanout.join_inventory();
                    tokio::spawn(carry_inventory(queue, live, only, self.inputs.clone()));
                }
            }
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
}

/// Feeds the core an input from off its thread, unless the core has
/// stopped.
async fn feed(inputs: &mpsc::WeakSender<Fed>, input: Input) {
    if let Some(inputs) = inputs.upgrade() {
        let _ = inputs.send(Fed::Input(input)).await;
    }
}

/// Carries an agent's live events on to a Subscribe stream.
async fn carry_session(
    queue: Arc<AnswerQueue>,
    mut live: broadcast::Receiver<Arc<SessionEvent>>,
    inputs: mpsc::WeakSender<Fed>,
) {
    loop {
        let carried = tokio::select! {
            () = queue.released() => return,
            event = live.recv() => match event {
                Ok(event) => queue.carry(Response::Session(event)),
                Err(broadcast::error::RecvError::Lagged(_)) => Carried::Overflowed,
                Err(broadcast::error::RecvError::Closed) => return closed(&queue, &inputs).await,
            },
        };
        if lagged(&queue, carried, &inputs).await {
            return;
        }
    }
}

/// Carries the fleet's live changes on to a SubscribeInventory stream,
/// `only` one host's when it names one.
async fn carry_inventory(
    queue: Arc<AnswerQueue>,
    mut live: broadcast::Receiver<Arc<InventoryEvent>>,
    only: Option<Vec<u8>>,
    inputs: mpsc::WeakSender<Fed>,
) {
    loop {
        let carried = tokio::select! {
            () = queue.released() => return,
            event = live.recv() => match event {
                Ok(event) if carries(only.as_deref(), &event) => {
                    queue.carry(Response::Inventory(event))
                }
                Ok(_) => Carried::Queued,
                Err(broadcast::error::RecvError::Lagged(_)) => Carried::Overflowed,
                Err(broadcast::error::RecvError::Closed) => return closed(&queue, &inputs).await,
            },
        };
        if lagged(&queue, carried, &inputs).await {
            return;
        }
    }
}

/// Reports an overflow to the core, which answers the stream's lag; true
/// when nothing more is to be carried.
async fn lagged(queue: &AnswerQueue, carried: Carried, inputs: &mpsc::WeakSender<Fed>) -> bool {
    match carried {
        Carried::Queued => false,
        Carried::Overflowed => {
            feed(inputs, Input::AnswerLagged { call: queue.call() }).await;
            true
        }
        Carried::Refused => true,
    }
}

/// What the stream followed is gone (the agent was deleted, or the profile
/// is shutting down): the stream ends, and the core lets the call go.
async fn closed(queue: &AnswerQueue, inputs: &mpsc::WeakSender<Fed>) {
    queue.end(Status::Ok);
    feed(inputs, Input::CallCancelled { call: queue.call() }).await;
}

fn carries(only: Option<&[u8]>, event: &InventoryEvent) -> bool {
    let Some(only) = only else {
        return true;
    };
    match &event.of {
        Some(inventory_event::Of::Host(host)) => host.host_id == only,
        Some(inventory_event::Of::HostRemoved(removed)) => removed.host_id == only,
        Some(inventory_event::Of::Agent(agent)) => agent.host_id == only,
        Some(inventory_event::Of::AgentRemoved(removed)) => removed.host_id == only,
        Some(inventory_event::Of::CaughtUp(_) | inventory_event::Of::Lagged(_)) | None => true,
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
