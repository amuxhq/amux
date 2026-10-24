//! The C ABI the phone calls.
//!
//! One [`AmuxRuntime`] hosts the embedded installation, with one profile
//! per account. An [`AmuxProfile`] is the
//! fleet and chats of one of its profiles, opened for as long as it is on
//! screen; each open chat is an [`AmuxChat`]. View values cross as JSON whose Swift mirrors are generated
//! from the Rust definitions (`cargo run -p xtask -- swift-types`), so the
//! two sides share one definition of every value.
//!
//! Rows are handed out by item key. A chat's id sequence changes only at
//! its two edges, newer keys above the newest the host holds and older ones
//! below its oldest, until a change batch says `reloaded`; there is no slot
//! identity to keep in step.
//!
//! Threads: the app owns the loop. The daemon cores step on whichever
//! queue calls `amux_runtime_step`; links, channels and gRPC run on one
//! confined Rust thread, which never calls into the app. Whenever
//! something waits for a step, the descriptor `amux_runtime_wake_fd`
//! answers becomes readable, and the app steps when it sees it. Reads
//! return at once. An act that waits on the agent returns a ticket at
//! once, and its JSON result comes back from a later step under that
//! ticket; a step also names the fleets (chat 0) and chats whose changes
//! wait to be taken, and whether the profile list moved. The calls that
//! block until the cores have done something (start, stop, a profile or
//! chat opening, the profile list, a lifecycle input) step the cores on
//! the calling thread while they wait.
//!
//! Every returned string is the caller's to free with `amux_string_free`,
//! and every byte buffer with `amux_bytes_free`. A null return means the
//! call failed; the reason goes to the log.

use std::collections::{BTreeSet, HashSet};
use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use app_embedded::{
    EdgeOverrides, EmbeddedRuntime, PairRequest, Phone, ProfileEvent, ProfileId, StartConfig,
    UsageEvent,
};
use app_runtime::values::{
    ActOutcome, AgentAct, CommandOutcome, Draft, Found, FrozenReview, NewAgent, RowOptions,
};
use app_runtime::{AppRuntime, Chat, Wake};
use model::{AgentKey, Key};
use node::SourcePolicy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use ui_view::{Comparison, QuestionResponse, SettingChange};

// The tests drive real agent processes, which run on Unix.
#[cfg(all(test, unix))]
mod tests;

/// Bytes the caller frees with `amux_bytes_free`.
#[repr(C)]
pub struct AmuxBytes {
    pub data: *mut u8,
    pub len: usize,
}

impl AmuxBytes {
    /// No bytes: a null pointer the caller may still free.
    fn empty() -> AmuxBytes {
        AmuxBytes {
            data: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// A copy of `bytes` the caller owns until it frees them.
    fn leak(bytes: &[u8]) -> AmuxBytes {
        let boxed: Box<[u8]> = bytes.into();
        let len = boxed.len();
        AmuxBytes {
            data: Box::into_raw(boxed).cast(),
            len,
        }
    }
}

/// The embedded installation and every profile it hosts.
pub struct AmuxRuntime {
    shared: Arc<Shared>,
    watch: Option<tokio::task::JoinHandle<()>>,
    tail: u32,
}

/// What the runtime, its profiles and its chats share.
struct Shared {
    phone: Phone,
    outbox: Arc<Outbox>,
}

/// One profile's fleet and chats.
pub struct AmuxProfile {
    profile: ProfileId,
    /// The number redraws name this fleet by.
    fleet: u64,
    gate: Arc<Gate>,
    shared: Arc<Shared>,
    app: Arc<AppRuntime>,
    tail: u32,
}

/// One open chat.
pub struct AmuxChat {
    chat: Arc<Chat>,
    fleet: u64,
    gate: Arc<Gate>,
    shared: Arc<Shared>,
}

/// What of a fleet is closed: nothing closed is named in a redraw again,
/// however many of its acts are still in flight.
#[derive(Default)]
struct Gate(Mutex<Shut>);

#[derive(Default)]
struct Shut {
    fleet: bool,
    chats: HashSet<u64>,
}

impl AmuxRuntime {
    fn embedded(&self) -> &Arc<EmbeddedRuntime> {
        self.shared.embedded()
    }

    fn handle(&self) -> &Handle {
        self.shared.handle()
    }

    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.shared.act(act)
    }
}

impl AmuxProfile {
    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.shared.act(act)
    }
}

impl Shared {
    fn embedded(&self) -> &Arc<EmbeddedRuntime> {
        self.phone.embedded()
    }

    fn handle(&self) -> &Handle {
        self.phone.handle()
    }

    /// Starts `act` on the transport and answers the ticket its result
    /// comes back under, from a later step.
    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.outbox.act(self.handle(), act)
    }
}

/// What the transport left for the app's next step: which views to
/// redraw, and the answers to acts it started. Nothing here calls the
/// app; each change rings the wake descriptor instead.
struct Outbox {
    tickets: AtomicU64,
    fleets: AtomicU64,
    waiting: Mutex<Stepped>,
    ring: Box<dyn Fn() + Send + Sync>,
}

/// What a step hands the app, as JSON.
#[derive(Default, Serialize)]
struct Stepped {
    /// The profile list moved: read it again.
    profiles: bool,
    /// The fleets (chat 0) and chats whose changes wait to be taken.
    redraws: BTreeSet<Redraw>,
    /// The acts that ended since the last step.
    answers: Vec<Answer>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Redraw {
    fleet: u64,
    chat: u64,
}

#[derive(Serialize)]
struct Answer {
    ticket: u64,
    /// The act's result, as JSON text.
    json: String,
}

impl Outbox {
    fn post(&self, post: impl FnOnce(&mut Stepped)) {
        post(&mut self.waiting.lock().unwrap_or_else(|p| p.into_inner()));
        (self.ring)();
    }

    fn take(&self) -> Stepped {
        std::mem::take(&mut self.waiting.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Names a fleet (chat 0) or chat in the next step, unless it closed;
    /// the gate is held across, so a close waits for a redraw under way.
    fn redraw(&self, gate: &Gate, fleet: u64, chat: u64) {
        let shut = gate.0.lock().unwrap_or_else(|p| p.into_inner());
        if shut.fleet || shut.chats.contains(&chat) {
            return;
        }
        self.post(|waiting| {
            waiting.redraws.insert(Redraw { fleet, chat });
        });
    }

    /// Closes a fleet, or one chat of it, and forgets what waits for it.
    fn close(&self, gate: &Gate, fleet: u64, chat: Option<u64>) {
        let mut shut = gate.0.lock().unwrap_or_else(|p| p.into_inner());
        match chat {
            Some(chat) => {
                shut.chats.insert(chat);
            }
            None => shut.fleet = true,
        }
        self.waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .redraws
            .retain(|redraw| redraw.fleet != fleet || chat.is_some_and(|chat| redraw.chat != chat));
    }

    fn act<T, F>(self: &Arc<Self>, handle: &Handle, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        let ticket = self.tickets.fetch_add(1, Ordering::SeqCst) + 1;
        let owed = Owed {
            outbox: self.clone(),
            ticket,
            answered: false,
        };
        handle.spawn(async move {
            let result = act.await;
            owed.answer(serde_json::to_string(&result).unwrap_or_else(|_| "null".to_owned()));
        });
        ticket
    }
}

/// An act's one answer, owed from the moment the act is started. A stop
/// drops the acts still running; the drop answers for them, so whoever
/// waits on the app's side hears that the runtime has stopped rather than
/// nothing, ever.
struct Owed {
    outbox: Arc<Outbox>,
    ticket: u64,
    answered: bool,
}

impl Owed {
    fn answer(mut self, json: String) {
        self.answered = true;
        let ticket = self.ticket;
        self.outbox
            .post(|waiting| waiting.answers.push(Answer { ticket, json }));
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        if !self.answered {
            let ticket = self.ticket;
            self.outbox.post(|waiting| {
                waiting.answers.push(Answer {
                    ticket,
                    json: STOPPED.to_owned(),
                })
            });
        }
    }
}

/// What an act dropped unanswered answers: the error the app's own
/// stopped-runtime path reports.
const STOPPED: &str = "{\"Err\":\"the runtime has stopped\"}";

/// Starts the installation with its transport on a thread of its own and
/// its cores on the calling thread's queue; blocks, stepping them, until
/// every profile's store is open.
pub fn start(config: &StartConfig, overrides: EdgeOverrides) -> Result<Box<AmuxRuntime>, String> {
    let phone = Phone::start(config, overrides, Arc::new(client::SystemClock))
        .map_err(|error| error.to_string())?;
    let outbox = Arc::new(Outbox {
        tickets: AtomicU64::new(0),
        fleets: AtomicU64::new(0),
        waiting: Mutex::new(Stepped::default()),
        ring: Box::new(phone.bell()),
    });
    let watching = phone.embedded().clone();
    let posting = outbox.clone();
    let watch = phone.handle().spawn(async move {
        // A watch that falls behind ends; the list is read again from a
        // fresh one, and the app told once it has caught up.
        while let Ok(mut events) = watching.watch_profiles().await {
            let mut caught_up = false;
            while let Some(event) = events.next().await {
                caught_up |= event == ProfileEvent::CaughtUp;
                if caught_up {
                    posting.post(|waiting| waiting.profiles = true);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    Ok(Box::new(AmuxRuntime {
        shared: Arc::new(Shared { phone, outbox }),
        watch: Some(watch),
        tail: config.tail,
    }))
}

/// Opens a profile's fleet; blocks, stepping the cores, until it has
/// caught up with the profile's store.
pub fn open_profile(runtime: &AmuxRuntime, profile: ProfileId) -> Result<Box<AmuxProfile>, String> {
    let shared = runtime.shared.clone();
    let embedded = shared.embedded().clone();
    let outbox = shared.outbox.clone();
    let fleet = outbox.fleets.fetch_add(1, Ordering::SeqCst) + 1;
    let gate = Arc::new(Gate::default());
    let passing = gate.clone();
    let app = shared
        .phone
        .wait(async move {
            let host_wake = Arc::new(move |moved: Wake| {
                let chat = match moved {
                    Wake::Fleet => 0,
                    Wake::Chat(id) => id,
                };
                outbox.redraw(&passing, fleet, chat)
            });
            let client = embedded
                .client(profile)
                .map_err(|error| error.to_string())?;
            let host = embedded
                .host_id(profile)
                .map_err(|error| error.to_string())?;
            AppRuntime::open(client, embedded.clock(), host, host_wake)
                .await
                .map_err(|error| error.to_string())
        })
        .ok_or("the runtime has stopped")??;
    Ok(Box::new(AmuxProfile {
        profile,
        fleet,
        gate,
        shared,
        app: Arc::new(app),
        tail: runtime.tail,
    }))
}

fn overrides(config: &StartConfig) -> EdgeOverrides {
    let mut overrides = EdgeOverrides {
        // Only the app as shipped sends analytics: the bridge built
        // optimised and without the driving tools, which development,
        // simulators and test runs always carry.
        published: !cfg!(debug_assertions) && !cfg!(feature = "debug-tools"),
        ..EdgeOverrides::default()
    };
    // A driving build may keep direct links on loopback, so a simulator
    // run never listens on the machine's network.
    if cfg!(feature = "debug-tools")
        && let Some(bind) = config.lan_bind
    {
        overrides.lan_bind = bind;
    }
    // And may dial a served test relay's carriers: TCP in plaintext, QUIC
    // trusting the relay's own certificate.
    if cfg!(feature = "debug-tools") {
        overrides.cloud.relay_tcp = config.relay_tcp;
        if let (Some(addr), Some(root)) = (config.relay_quic, &config.relay_root)
            && let Some(der) = from_hex(root)
        {
            overrides.cloud.relay_quic = node::RelayQuic::trusting(addr, der).ok();
        }
    }
    overrides
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

// --- strings and JSON ------------------------------------------------------

fn json<T: Serialize>(value: &T) -> Option<CString> {
    let text = serde_json::to_string(value).ok()?;
    CString::new(text).ok()
}

fn owned<T: Serialize>(value: &T) -> *mut c_char {
    json(value).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Reads a NUL-terminated UTF-8 string.
///
/// # Safety
/// `text` is null or points to a NUL-terminated string that lives for the
/// call.
unsafe fn text<'a>(text: *const c_char) -> Option<&'a str> {
    if text.is_null() {
        return None;
    }
    // SAFETY: the caller's contract above.
    unsafe { CStr::from_ptr(text) }.to_str().ok()
}

/// # Safety
/// As for [`text`].
unsafe fn parse<T: DeserializeOwned>(value: *const c_char) -> Option<T> {
    // SAFETY: the caller's contract.
    serde_json::from_str(unsafe { text(value) }?).ok()
}

/// Runs a call, turning a panic into the failure value rather than letting
/// it unwind into the host.
fn guard<T>(failed: T, call: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => value,
        Err(_) => {
            #[cfg(test)]
            CAUGHT.with(|caught| caught.set(caught.get() + 1));
            failed
        }
    }
}

// A shipping build aborts on a panic instead of unwinding, so a panic the
// guard turns into a failure in a test is a crash on the phone: the tests
// count them on the calling thread.
#[cfg(test)]
thread_local! {
    static CAUGHT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// # Safety
/// `runtime` is null or a live pointer from `amux_runtime_start`.
unsafe fn live_runtime<'a>(runtime: *const AmuxRuntime) -> Option<&'a AmuxRuntime> {
    // SAFETY: the caller's contract.
    unsafe { runtime.as_ref() }
}

/// # Safety
/// `profile` is null or a live pointer from `amux_profile_open`.
unsafe fn live_profile<'a>(profile: *const AmuxProfile) -> Option<&'a AmuxProfile> {
    // SAFETY: the caller's contract.
    unsafe { profile.as_ref() }
}

/// A profile id.
///
/// # Safety
/// `profile` is null or a NUL-terminated string.
unsafe fn profile_id(profile: *const c_char) -> Option<ProfileId> {
    // SAFETY: the caller's contract.
    unsafe { text(profile) }?.parse().ok()
}

/// # Safety
/// `chat` is null or a live pointer from `amux_session_open`.
unsafe fn held_chat<'a>(chat: *const AmuxChat) -> Option<&'a AmuxChat> {
    // SAFETY: the caller's contract.
    unsafe { chat.as_ref() }
}

/// Runs a read on a chat inside its runtime, so a read that starts a fetch
/// can spawn it.
///
/// # Safety
/// As for [`held_chat`].
unsafe fn read<T: Serialize>(chat: *const AmuxChat, read: impl FnOnce(&Chat) -> T) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        let _entered = open.shared.handle().enter();
        owned(&read(&open.chat))
    })
}

// --- the runtime -----------------------------------------------------------

/// This build's version, as a static string.
#[unsafe(no_mangle)]
pub extern "C" fn amux_version() -> *const c_char {
    static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let mut version = node::version().to_owned();
            if cfg!(feature = "debug-tools") {
                version.push_str("+debug-tools");
            }
            CString::new(version).unwrap()
        })
        .as_ptr()
}

/// Why `name` cannot name an agent, as a JSON `AgentNameProblem`, or JSON
/// null when it can: the rule every host holds a create or a rename to, so a field can
/// say so while it is typed. The caller frees the answer.
///
/// # Safety
/// `name` is a NUL-terminated string that lives for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_agent_name_problem(name: *const c_char) -> *mut c_char {
    // SAFETY: the caller's contract above.
    let name = unsafe { text(name) }.unwrap_or_default();
    owned(&ui_view::agent_name_problem(name))
}

/// A new agent's `SettingsView` from a host's `Catalogue` and the
/// `NewAgentChoices` made so far, both JSON; null when either does not
/// parse. The caller frees the answer.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_new_agent_settings(
    catalogue: *const c_char,
    chosen: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let catalogue: Option<ui_view::Catalogue> = unsafe { parse(catalogue) };
    // SAFETY: the caller's contract.
    let chosen: Option<ui_view::NewAgentChoices> = unsafe { parse(chosen) };
    guard(std::ptr::null_mut(), || match (catalogue, chosen) {
        (Some(catalogue), Some(chosen)) => owned(&ui_view::new_agent_settings(&catalogue, &chosen)),
        _ => std::ptr::null_mut(),
    })
}

/// The `NewAgentChoices` after a `NewAgentPick`, with the rules a pick
/// carries (another model starts at its default effort, and so on); every
/// argument JSON, null when one does not parse. The caller frees the
/// answer.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_new_agent_pick(
    catalogue: *const c_char,
    chosen: *const c_char,
    pick: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let catalogue: Option<ui_view::Catalogue> = unsafe { parse(catalogue) };
    // SAFETY: the caller's contract.
    let chosen: Option<ui_view::NewAgentChoices> = unsafe { parse(chosen) };
    // SAFETY: the caller's contract.
    let pick: Option<ui_view::NewAgentPick> = unsafe { parse(pick) };
    guard(std::ptr::null_mut(), || match (catalogue, chosen, pick) {
        (Some(catalogue), Some(chosen), Some(pick)) => {
            owned(&ui_view::new_agent_pick(&catalogue, &chosen, &pick))
        }
        _ => std::ptr::null_mut(),
    })
}

/// The problems of a form's values, as the shared view checks them: the
/// form's `FormField`s and the person's `FormValue`s, both JSON arrays in
/// the form's order, give a JSON array of `FormProblem`, empty when the
/// form can be submitted; null when either does not parse. The caller
/// frees the answer.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_form_problems(
    fields: *const c_char,
    values: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let fields: Option<Vec<ui_view::FormField>> = unsafe { parse(fields) };
    // SAFETY: the caller's contract.
    let values: Option<Vec<ui_view::FormValue>> = unsafe { parse(values) };
    guard(std::ptr::null_mut(), || match (fields, values) {
        (Some(fields), Some(values)) => owned(&ui_view::form_problems(&fields, &values)),
        _ => std::ptr::null_mut(),
    })
}

/// Starts the installation from a `StartConfig` as JSON, its transport
/// on a thread of its own and its cores stepped by `amux_runtime_step`.
/// Blocks, stepping them on the calling thread, until every profile's
/// store is open. Null on failure, with the reason in `error` when it is
/// not null, which the caller frees.
///
/// # Safety
/// `config` is a NUL-terminated string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_start(
    config: *const c_char,
    error: *mut *mut c_char,
) -> *mut AmuxRuntime {
    let started = guard(
        Err("the runtime panicked while starting".to_owned()),
        || {
            // SAFETY: the caller's contract.
            let config: StartConfig = unsafe { parse(config) }
                .ok_or_else(|| "the start configuration is not a StartConfig".to_owned())?;
            start(&config, overrides(&config))
        },
    );
    match started {
        Ok(runtime) => Box::into_raw(runtime),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// A descriptor that becomes readable whenever something waits for
/// `amux_runtime_step`: inputs for the cores, views to redraw, answers to
/// acts. The app watches it from the queue it steps on; no Rust thread
/// ever calls into the app. -1 for no runtime.
///
/// # Safety
/// `runtime` is null or from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_wake_fd(runtime: *const AmuxRuntime) -> c_int {
    guard(-1, || {
        // SAFETY: the caller's contract.
        unsafe { live_runtime(runtime) }.map_or(-1, |runtime| runtime.shared.phone.wake_fd())
    })
}

/// Steps every input waiting for the cores on the calling queue, then
/// answers what the app must do as JSON: `{"profiles": bool, "redraws":
/// [{"fleet": n, "chat": n}], "answers": [{"ticket": n, "json": text}]}`.
/// `profiles` says to read the profile list again; each redraw names a
/// fleet, by the number `amux_profile_fleet_id` answers, or
/// one of its chats (chat 0 for the fleet itself) whose changes wait to be
/// taken; each answer is
/// an act's result, under the ticket the act returned. Null for no
/// runtime.
///
/// # Safety
/// `runtime` is null or from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_step(runtime: *const AmuxRuntime) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return std::ptr::null_mut();
        };
        runtime.shared.phone.step();
        owned(&runtime.shared.outbox.take())
    })
}

/// What the app's life feeds the runtime.
#[derive(Debug, Deserialize)]
pub enum Lifecycle {
    /// The app came to the front; counted at most once an hour.
    Foreground,
    /// The app is leaving the screen: the analytics waiting are sent.
    Background,
    /// The app is about to be suspended: the analytics waiting are sent,
    /// and every profile's core is checkpointed.
    ///
    /// The checkpoint makes the store whole on the drive should the system
    /// end the app; a relaunch starts from that store, not the checkpoint,
    /// whose links, calls, jobs and timers belong to the process that ended.
    Suspending,
    /// The phone's network changed: every profile's link socket is bound
    /// afresh and every path machine probes its paths again, which the
    /// link log's rebind line records.
    NetworkChanged,
}

/// Feeds a `Lifecycle` input as JSON; blocks, stepping the cores, until
/// it is carried out. False on failure, with the reason in `error` when it
/// is not null, which the caller frees.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `input` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_feed(
    runtime: *const AmuxRuntime,
    input: *const c_char,
    error: *mut *mut c_char,
) -> bool {
    let fed = guard(Err("feeding the runtime panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let runtime = unsafe { live_runtime(runtime) }.ok_or("no runtime")?;
        // SAFETY: the caller's contract.
        let input: Lifecycle = unsafe { parse(input) }.ok_or("the input is not a Lifecycle")?;
        feed(runtime, input)
    });
    match fed {
        Ok(()) => true,
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            false
        }
    }
}

fn feed(runtime: &AmuxRuntime, input: Lifecycle) -> Result<(), String> {
    match input {
        Lifecycle::Foreground => {
            let _entered = runtime.handle().enter();
            runtime.embedded().client_opened();
        }
        Lifecycle::Background => {
            let embedded = runtime.embedded().clone();
            runtime
                .shared
                .phone
                .wait(async move { embedded.flush_analytics().await })
                .ok_or("the runtime has stopped")?;
        }
        Lifecycle::Suspending => {
            let embedded = runtime.embedded().clone();
            runtime
                .shared
                .phone
                .wait(async move {
                    embedded.flush_analytics().await;
                    embedded.checkpoint().await
                })
                .ok_or("the runtime has stopped")?
                .map_err(|error| error.to_string())?;
        }
        Lifecycle::NetworkChanged => {
            let embedded = runtime.embedded().clone();
            runtime
                .shared
                .phone
                .wait(async move { embedded.network_changed().await })
                .ok_or("the runtime has stopped")?
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Writes a failure's reason where the caller asked for it.
///
/// # Safety
/// `error` is null or writable.
unsafe fn fail(error: *mut *mut c_char, reason: String) {
    if !error.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *error = CString::new(reason).map_or(std::ptr::null_mut(), CString::into_raw) };
    }
}

/// Stops the installation: flushes every profile's store and ends its
/// transport. Close every chat and profile first.
///
/// # Safety
/// `runtime` is null or from `amux_runtime_start`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_stop(runtime: *mut AmuxRuntime) {
    if runtime.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    let mut runtime = unsafe { Box::from_raw(runtime) };
    guard((), || {
        // The watch holds the installation until its task is dropped on the
        // transport, and the phone shuts down only an installation nothing
        // else holds: wait for the abort to land.
        if let Some(watch) = runtime.watch.take() {
            watch.abort();
            let _ = runtime.shared.phone.wait(async move {
                let _ = watch.await;
            });
        }
        // A profile or chat left open holds the installation; it stops
        // once the last of them closes.
        if let Some(shared) = Arc::into_inner(runtime.shared) {
            shared.phone.stop();
        }
    });
}

#[derive(Serialize)]
enum Answered<T> {
    Ok(T),
    Err(String),
}

impl<T, E: std::fmt::Display> From<Result<T, E>> for Answered<T> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Answered::Ok(value),
            Err(error) => Answered::Err(error.to_string()),
        }
    }
}

/// Runs an act on the installation that names a profile, answering a
/// profile id that does not parse without running it.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string.
unsafe fn profile_act<T, F, Fut>(runtime: *const AmuxRuntime, profile: *const c_char, act: F) -> u64
where
    T: Serialize + Send + 'static,
    F: FnOnce(Arc<EmbeddedRuntime>, ProfileId) -> Fut,
    Fut: Future<Output = Answered<T>> + Send + 'static,
{
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        match unsafe { profile_id(profile) } {
            Some(profile) => runtime.act(act(runtime.embedded().clone(), profile)),
            None => {
                runtime.act(async { Answered::<T>::Err("the profile is not a profile id".into()) })
            }
        }
    })
}

// --- the profile registry ------------------------------------------------

/// Every profile, oldest first, as `[ProfileView]`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`. Not called from the runtime's
/// own threads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_profiles(runtime: *const AmuxRuntime) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return std::ptr::null_mut();
        };
        let embedded = runtime.embedded().clone();
        match runtime
            .shared
            .phone
            .wait(async move { embedded.profiles().await })
        {
            Some(Ok(profiles)) => owned(&profiles),
            _ => std::ptr::null_mut(),
        }
    })
}

/// Creates a profile nobody has signed in on; the ticket answers
/// `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_create_profile(runtime: *const AmuxRuntime) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        let embedded = runtime.embedded().clone();
        runtime.act(async move { Answered::from(embedded.create_profile(None).await) })
    })
}

/// Deletes a profile, by its id: its key, the machines it trusts and its
/// store. The installation keeps at least one. Close it first. The ticket
/// answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_delete_profile(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            Answered::from(embedded.delete_profile(profile).await)
        })
    }
}

/// Binds a profile to an account with the refresh token the app's sign-in
/// obtained as the OAuth client `client_id`; the relay link comes up from
/// there, and the profile alone spends the token. A named `profile` is
/// adopted even when it paired machines before; null lets the registry pick
/// the account's own profile, else one that holds nothing, else a new one.
/// The ticket answers `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is null or a
/// NUL-terminated string; the other strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_bind(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    cloud_url: *const c_char,
    client_id: *const c_char,
    refresh_token: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let named = unsafe { text(profile) }.map(str::to_owned);
        // SAFETY: the caller's contract.
        let url = unsafe { text(cloud_url) }.unwrap_or_default().to_owned();
        // SAFETY: the caller's contract.
        let client = unsafe { text(client_id) }.unwrap_or_default().to_owned();
        // SAFETY: the caller's contract.
        let token = unsafe { text(refresh_token) }
            .unwrap_or_default()
            .to_owned();
        let embedded = runtime.embedded().clone();
        runtime.act(async move {
            let profile = match named.map(|named| named.parse::<ProfileId>()) {
                None => None,
                Some(Ok(profile)) => Some(profile),
                Some(Err(_)) => return Answered::Err("the profile is not a profile id".into()),
            };
            Answered::from(embedded.bind(profile, &url, &client, &token).await)
        })
    })
}

/// Signs a profile out of its account; it stays tied to the account. The
/// ticket answers `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_sign_out(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            Answered::from(embedded.sign_out(profile).await)
        })
    }
}

/// Every profile whose trust store holds a host, by its id as a JSON byte
/// array, oldest first; the ticket answers `{"Ok": ["<profile id>", ..]}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `host_id` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_trusting_profiles(
    runtime: *const AmuxRuntime,
    host_id: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let embedded = runtime.embedded().clone();
        runtime.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.trusting(&host_id).await.map(|profiles| {
                profiles
                    .iter()
                    .map(ProfileId::to_string)
                    .collect::<Vec<_>>()
            }))
        })
    })
}

/// Opens pairing mode on a profile and answers the link its QR code would
/// carry; the ticket answers `{"Ok": "amux://pair?.."}` or `{"Err": ..}`. Only
/// a driving build offers one: the phone shows no pairing code of its own.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_offer_pairing(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            if !cfg!(feature = "debug-tools") {
                return Answered::Err("only a driving build offers pairing".into());
            }
            Answered::from(embedded.offer_pairing(profile).await)
        })
    }
}

/// `listed` for the profile in front of somebody: every listed agent keeps
/// a source. Not `listed` for every other profile, and in the background:
/// only the chats that open keep one.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_set_source_policy(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    listed: bool,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { profile_id(profile) }) else {
            return;
        };
        // Switching opens and closes sources, which run on the pool.
        let _entered = runtime.handle().enter();
        let _ = runtime.embedded().set_source_policy(
            profile,
            if listed {
                SourcePolicy::Listed
            } else {
                SourcePolicy::OnDemand
            },
        );
    });
}

/// Whether every listed agent of a profile keeps a source open.
///
/// # Safety
/// As for `amux_runtime_set_source_policy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_source_policy_listed(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> bool {
    guard(false, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return false;
        };
        // SAFETY: the caller's contract.
        unsafe { profile_id(profile) }.is_some_and(|profile| {
            runtime.embedded().source_policy(profile).ok() == Some(SourcePolicy::Listed)
        })
    })
}

/// Hands over the whole set the phone's browser found, as `[Found]`, to
/// every profile.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `found` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_discovered(
    runtime: *const AmuxRuntime,
    found: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        if let Some(found) = unsafe { parse::<Vec<Found>>(found) } {
            let _entered = runtime.handle().enter();
            runtime.embedded().discovered(found);
        }
    });
}

/// A bearer for a profile's account, which the profile refreshes; the
/// ticket answers `{"Ok": Bearer}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_access_token(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            Answered::from(embedded.access_token(profile).await)
        })
    }
}

/// Asks the account service what a profile's account buys now, over its
/// relay link, so the relay's tier follows a purchase at once; the ticket
/// answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_refresh_entitlement(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            Answered::from(embedded.refresh_entitlement(profile).await)
        })
    }
}

// --- analytics -------------------------------------------------------------

/// The app's telemetry setting changed: off stops anything still waiting
/// from being sent.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_set_telemetry(runtime: *const AmuxRuntime, on: bool) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            runtime.embedded().set_telemetry(on);
        }
    });
}

/// Records what only the app sees, a `UsageEvent` as JSON, on the profile it
/// concerns, or on every profile when `profile` is null or not hosted.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is null or a
/// NUL-terminated string; `event` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_record(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    event: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let (profile, event) = unsafe { (profile_id(profile), parse::<UsageEvent>(event)) };
        if let Some(event) = event {
            runtime.embedded().record(profile, event);
        }
    });
}

// --- one profile ---------------------------------------------------------

/// Opens a profile's fleet, by the profile's id; blocks, stepping the
/// cores, until it has caught up with the profile's store. Null on failure, with
/// the reason in `error` when it is not null.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_open(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    error: *mut *mut c_char,
) -> *mut AmuxProfile {
    let opened = guard(Err("opening the profile panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let runtime = unsafe { live_runtime(runtime) }.ok_or("no runtime")?;
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_id(profile) }.ok_or("the profile is not a profile id")?;
        open_profile(runtime, profile)
    });
    match opened {
        Ok(profile) => Box::into_raw(profile),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// Closes a profile's fleet. No redraw names it once this returns, however
/// many acts are still in flight; they are answered all the same. Close
/// its chats first.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close(profile: *mut AmuxProfile) {
    if !profile.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(profile) };
        open.shared.outbox.close(&open.gate, open.fleet, None);
        let shared = open.shared.clone();
        let _entered = shared.handle().enter();
        drop(open);
    }
}

/// The number redraws name this fleet by.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_fleet_id(profile: *const AmuxProfile) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { live_profile(profile) }.map_or(0, |open| open.fleet)
}

/// Writes a dump of the profile with the fleet's and every open chat's
/// part; the ticket answers `{"Ok": "<bundle directory>"}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `reason` is a NUL-terminated
/// string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_dump(
    profile: *const AmuxProfile,
    reason: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let reason = unsafe { text(reason) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.act(async move { Answered::from(app.dump(&reason).await) })
    })
}

/// Reaches and authenticates a machine by a `PairRequest` as JSON; the
/// ticket answers `{"Ok": PendingPair}` or `{"Err": ..}`. Nothing is trusted
/// until `amux_profile_confirm_pair`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `request` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_begin_pair(
    profile: *const AmuxProfile,
    request: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let request: Option<PairRequest> = unsafe { parse(request) };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move {
            let Some(request) = request else {
                return Answered::Err("the request is not a PairRequest".into());
            };
            Answered::from(embedded.begin_pair(id, &request).await)
        })
    })
}

/// Trusts the machine a pending pairing reached, named by its token as a
/// JSON byte array; the ticket answers `{"Ok": {"host_id": [..], "name":
/// ..}}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_confirm_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
) -> u64 {
    #[derive(Serialize)]
    struct Paired {
        host_id: Vec<u8>,
        name: String,
    }
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.confirm_pair(id, &token).await.map(|peer| Paired {
                host_id: peer.host_id,
                name: peer.name,
            }))
        })
    })
}

/// Turns away the machine a pending pairing reached.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_abandon_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.abandon_pair(id, &token).await)
        })
    })
}

/// Stops trusting a paired machine, by its host id as a JSON byte array.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_unpair(
    profile: *const AmuxProfile,
    host_id: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.unpair(id, &host_id).await)
        })
    })
}

/// The profile's identity and the machines it trusts; the ticket answers
/// `{"Ok": Roster}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_roster(profile: *const AmuxProfile) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move { Answered::from(embedded.roster(id).await) })
    })
}

/// The account the profile is bound to; the ticket answers
/// `{"Ok": AccountView}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_account(profile: *const AmuxProfile) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        let (embedded, id) = (profile.shared.embedded().clone(), profile.profile);
        profile.act(async move { Answered::from(embedded.account(id).await) })
    })
}

/// Starts an agent from a `NewAgent` as JSON; the ticket answers
/// `{"Ok": AgentKey}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_create_agent(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let agent: Option<NewAgent> = unsafe { parse(agent) };
        let app = profile.app.clone();
        profile.act(async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not a NewAgent".into());
            };
            Answered::from(app.create_agent(&agent).await)
        })
    })
}

/// Where a host, by its id as a JSON byte array, offers to start an agent,
/// matching `query` (or everything when it is null or empty), at most
/// `limit` of each; the ticket answers `{"Ok": Directories}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated
/// or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_directories(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    query: *const c_char,
    limit: u32,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let query = unsafe { text(query) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.directories(&host_id, &query, limit).await)
        })
    })
}

/// Renames, stops or deletes an agent, named by its `AgentKey`, with an
/// `AgentAct`, both as JSON; the ticket answers `{"Ok": null}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_agent_act(
    profile: *const AmuxProfile,
    agent: *const c_char,
    act: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let agent: Option<AgentKey> = unsafe { parse(agent) };
        // SAFETY: the caller's contract.
        let act: Option<AgentAct> = unsafe { parse(act) };
        let app = profile.app.clone();
        profile.act(async move {
            let (Some(agent), Some(act)) = (agent, act) else {
                return Answered::Err("the agent or the act is not readable".into());
            };
            Answered::from(app.agent_act(&agent, &act).await)
        })
    })
}

/// What a provider, as a JSON `Provider` ("Claude" or "Codex"), offers on a
/// host, by its id as a JSON byte array, with no agent running there; the
/// ticket answers `{"Ok": Catalogue}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_host_catalogue(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    provider: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let provider: Option<app_runtime::Provider> = unsafe { parse(provider) };
        let app = profile.app.clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            let Some(provider) = provider else {
                return Answered::Err("the provider is not a Provider".into());
            };
            Answered::from(app.host_catalogue(&host_id, provider).await)
        })
    })
}

/// The app leaving (false) or returning to (true) the foreground: every
/// agent's stream closes, or reopens. Open chats keep their rows and catch
/// up when the streams reopen.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_set_foreground(
    profile: *const AmuxProfile,
    foreground: bool,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        let _entered = profile.shared.handle().enter();
        profile.app.set_foreground(foreground);
    });
}

// --- the fleet -------------------------------------------------------------

/// # Safety
/// As for [`live_profile`].
unsafe fn fleet_read<T: Serialize>(
    profile: *const AmuxProfile,
    read: impl FnOnce(&AppRuntime) -> T,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return std::ptr::null_mut();
        };
        owned(&read(&profile.app))
    })
}

/// Home's `FleetView`: its sections, expanded under the roots whose agent
/// ids `expand` lists (a JSON array of byte arrays, or null), listing the
/// agents `filter` keeps (a JSON `FleetFilter`, or null for every agent).
///
/// # Safety
/// `profile` is from `amux_profile_open`; `expand` and `filter` are null or
/// NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_view(
    profile: *const AmuxProfile,
    expand: *const c_char,
    filter: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let expand: Vec<Vec<u8>> = unsafe { parse(expand) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    let filter: ui_view::FleetFilter = unsafe { parse(filter) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, |app| app.fleet_view(&expand, &filter)) }
}

/// One agent's `FleetCard`, or null JSON when it is not listed.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is an `AgentKey` as a
/// NUL-terminated JSON string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_card(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(agent) = (unsafe { parse::<AgentKey>(agent) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, |app| app.fleet_card(&agent)) }
}

/// The `FamilyHeader` over an agent's chat, or null JSON.
///
/// # Safety
/// As for `amux_fleet_card`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_family(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(agent) = (unsafe { parse::<AgentKey>(agent) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, |app| app.family_header(&agent)) }
}

/// Every host in the fleet as `[HostView]`, this device first.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_hosts(profile: *const AmuxProfile) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, AppRuntime::hosts) }
}

/// The fleet's `FleetChanges` since the last take; the next change wakes
/// the host again.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_take_changes(profile: *const AmuxProfile) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, AppRuntime::take_fleet_changes) }
}

// --- a chat ----------------------------------------------------------------

/// Opens a chat on a fleet agent, named by its `AgentKey` as JSON, with
/// `tail` rows (the configured tail when zero). Blocks until the snapshot
/// and the rows this device holds are applied, so the first read is
/// correct even with the agent's host away. Null on failure, with the
/// reason in `error` when it is not null.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open(
    profile: *const AmuxProfile,
    agent: *const c_char,
    tail: u32,
    error: *mut *mut c_char,
) -> *mut AmuxChat {
    let opened = guard(Err("opening the chat panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let profile = unsafe { live_profile(profile) }.ok_or("no profile")?;
        // SAFETY: the caller's contract.
        let agent: AgentKey = unsafe { parse(agent) }.ok_or("the agent is not an AgentKey")?;
        let tail = if tail == 0 { profile.tail } else { tail };
        let app = profile.app.clone();
        let chat = profile
            .shared
            .phone
            .wait(async move { app.open_chat(&agent, tail).await })
            .ok_or("the runtime has stopped")?
            .map_err(|error| error.to_string())?;
        Ok(AmuxChat {
            chat,
            fleet: profile.fleet,
            gate: profile.gate.clone(),
            shared: profile.shared.clone(),
        })
    });
    match opened {
        Ok(chat) => Box::into_raw(Box::new(chat)),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// The number redraws name this chat by, within its fleet.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_id(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { held_chat(chat) }.map_or(0, |open| open.chat.id())
}

/// Closes a chat. No redraw names it once this returns, however many of
/// its acts are still in flight; they are answered all the same.
///
/// # Safety
/// `chat` is null or from `amux_session_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_close(chat: *mut AmuxChat) {
    if !chat.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(chat) };
        open.shared
            .outbox
            .close(&open.gate, open.fleet, Some(open.chat.id()));
        let shared = open.shared.clone();
        let _entered = shared.handle().enter();
        drop(open);
    }
}

/// The held window's keys, oldest first: `[String]`.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_keys(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::keys) }
}

/// Keys newer than `newest`, oldest first; null JSON when `newest` is not
/// held, and the host reads every key again.
///
/// # Safety
/// `chat` is from `amux_session_open`; `newest` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_new_keys_above(
    chat: *const AmuxChat,
    newest: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let newest = unsafe { text(newest) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.keys_above(&newest)) }
}

/// The oldest key the window holds, as a JSON string; null JSON when it
/// holds none. While the reader follows, the window drops its oldest rows
/// as new ones arrive, and the host drops the keys before this one.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_oldest_key(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::oldest_key) }
}

/// Tells the chat whether the reader is at the newest row (true) or has
/// scrolled into history (false). A chat opens following.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_follow(chat: *const AmuxChat, following: bool) {
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.follow(following)) };
}

/// Keys older than `oldest`, oldest first; null JSON when `oldest` is not
/// held.
///
/// # Safety
/// As for `amux_session_new_keys_above`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_new_keys_below(
    chat: *const AmuxChat,
    oldest: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let oldest = unsafe { text(oldest) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.keys_below(&oldest)) }
}

/// `[Row]` for these keys (a JSON array of strings), in order, skipping
/// keys not held; `options` is a `RowOptions` as JSON, or null.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_rows_for(
    chat: *const AmuxChat,
    keys: *const c_char,
    options: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let keys: Vec<Key> = unsafe { parse(keys) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    let options: RowOptions = unsafe { parse(options) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.rows_for(&keys, &options)) }
}

/// Opens or closes the run the row `member` belongs to: `open` is the
/// folding view's open set as a JSON array of keys, or null; answers the
/// new set.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_run(
    chat: *const AmuxChat,
    member: *const c_char,
    open: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let member = unsafe { text(member) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let open: Vec<Key> = unsafe { parse(open) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.toggle_run(&member, &open)) }
}

/// The open set, a JSON array of keys or null, re-held on each open run's
/// newest step: what to ask rows with, so an open run stays open as it
/// grows or its oldest steps leave the window.
///
/// # Safety
/// `chat` is from `amux_session_open`; `open` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_keep_open_runs(
    chat: *const AmuxChat,
    open: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let open: Vec<Key> = unsafe { parse(open) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.keep_open_runs(&open)) }
}

/// The head `AskCard`, or null JSON.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_ask_card(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::ask_card) }
}

/// The chat's `Overview`, with the changed files the last
/// `amux_session_open_overview` fetched.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_overview(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::overview) }
}

/// The `SettingsView`: what the agent offers to change, the current values
/// marked, and why a setting cannot change from here.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_settings(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::settings) }
}

/// The `ChatFrame`: phase, composer, settings in force, git and context.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_frame(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::frame) }
}

/// The chat's `ChatChanges` since the last take; the next change wakes the
/// host again.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_take_changes(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::take_changes) }
}

/// Starts an act on the chat and answers its ticket.
///
/// # Safety
/// `chat` is from `amux_session_open`.
unsafe fn act<T, F, Fut>(chat: *const AmuxChat, act: F) -> u64
where
    T: Serialize,
    F: FnOnce(Arc<Chat>) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return 0;
        };
        open.shared.act(act(open.chat.clone()))
    })
}

/// Sends a `Draft` as JSON; the ticket answers a `SendOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send(chat: *const AmuxChat, draft: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, |chat| async move {
            match draft {
                Some(draft) => Answered::Ok(chat.send(&draft).await),
                None => Answered::Err("the draft is not a Draft".into()),
            }
        })
    }
}

/// Runs a `Draft`, as JSON, that names a command the client handles itself
/// (`/model`, `/effort`, `/rename`) or one the agent does not run; the
/// ticket answers a `CommandOutcome`, whose `Send` says to send the draft as
/// typed. A draft that does not parse answers `Acted(Failed)`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_command(chat: *const AmuxChat, draft: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, |chat| async move {
            match draft {
                Some(draft) => chat.command(&draft).await,
                None => {
                    CommandOutcome::Acted(ActOutcome::Failed("the draft is not a Draft".into()))
                }
            }
        })
    }
}

/// Answers the head ask, named by its key, with the choice at `index` on
/// its card; `note` goes back with a choice that takes one. The ticket
/// answers an `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    index: u32,
    note: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let note = unsafe { text(note) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            chat.answer_choice(&ask_key, index as usize, &note).await
        })
    }
}

/// Submits the head form ask with the person's values, a JSON array of
/// `FormValue`, one per field in the form's order; the shared view checks
/// and encodes them. The ticket answers an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer_form(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    values: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let values: Option<Vec<ui_view::FormValue>> = unsafe { parse(values) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            match values {
                Some(values) => chat.answer_form(&ask_key, &values).await,
                None => ActOutcome::Failed("the values are not a list of FormValue".into()),
            }
        })
    }
}

/// Answers the head question ask with one `QuestionResponse` per question, as a
/// JSON array: the pick (no options and no text skips the question) and
/// the person's note on it. The ticket answers an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer_questions(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    responses: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let given: Option<Vec<QuestionResponse>> = unsafe { parse(responses) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            match given {
                Some(given) => chat.answer_questions(&ask_key, &given).await,
                None => {
                    ActOutcome::Failed("the responses are not a list of QuestionResponse".into())
                }
            }
        })
    }
}

/// Replies to the head question ask in the person's own words instead of
/// answering, with what they had answered so far as a JSON array of
/// `QuestionResponse`, one per question. The ticket answers an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_reply_instead(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    reply: *const c_char,
    so_far: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let reply = unsafe { text(reply) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let so_far: Option<Vec<QuestionResponse>> = unsafe { parse(so_far) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            match so_far {
                Some(so_far) => chat.reply_instead(&ask_key, &reply, &so_far).await,
                None => ActOutcome::Failed(
                    "the answers so far are not a list of QuestionResponse".into(),
                ),
            }
        })
    }
}

/// Sends a pick from the settings view, a `SettingChange` as JSON. The
/// ticket answers an `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `change` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_change_setting(
    chat: *const AmuxChat,
    change: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let change: Option<SettingChange> = unsafe { parse(change) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            match change {
                Some(change) => chat.change_setting(&change).await,
                None => ActOutcome::Failed("the change is not a SettingChange".into()),
            }
        })
    }
}

/// An input id as a JSON byte array.
///
/// # Safety
/// `id` is null or a NUL-terminated string.
unsafe fn input_id(id: *const c_char) -> Vec<u8> {
    // SAFETY: the caller's contract.
    unsafe { parse(id) }.unwrap_or_default()
}

/// Takes a queued prompt back out of the queue. The ticket answers an
/// `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated
/// JSON byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_withdraw(
    chat: *const AmuxChat,
    input_id: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe { act(chat, |chat| async move { chat.withdraw(&id).await }) }
}

/// Steers a queued prompt into the running turn.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send_now(
    chat: *const AmuxChat,
    input_id: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe { act(chat, |chat| async move { chat.send_now(&id).await }) }
}

/// Sends a not-confirmed input again under a new id; the ticket answers a
/// `SendOutcome`, or null JSON when the input is not held.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resend(
    chat: *const AmuxChat,
    input_id: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe { act(chat, |chat| async move { chat.resend(&id).await }) }
}

/// A queued or sent prompt as the `Draft` it came from, its words and every
/// attachment whole, or null JSON: what a withdraw or an edit puts back in
/// the composer.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_draft_of(
    chat: *const AmuxChat,
    input_id: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let id: Vec<u8> = unsafe { parse(input_id) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.draft_of(&id)) }
}

/// Forgets a not-confirmed input.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_discard(chat: *const AmuxChat, input_id: *const c_char) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.discard(&id)) };
}

/// Stops the agent's turn; the agent stays.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_interrupt(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { act(chat, |chat| async move { chat.interrupt().await }) }
}

/// Takes terminal Claude's own view back to the agent's conversation
/// after it switched to another. The ticket answers an `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_switch_back(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { act(chat, |chat| async move { chat.switch_back().await }) }
}

/// The exited composer's one tap: resumes the agent with a `Draft` as its
/// first prompt. The ticket answers an `ActOutcome`; on anything but Done
/// the draft stays the composer's.
///
/// # Safety
/// As for `amux_session_send`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resume(chat: *const AmuxChat, draft: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, |chat| async move {
            match draft {
                Some(draft) => chat.resume_with(&draft).await,
                None => ActOutcome::Failed("the draft is not a Draft".into()),
            }
        })
    }
}

/// Asks for up to `n` rows older than the oldest held; the ticket answers a
/// `PageOutcome`, and the new keys arrive below the oldest.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_page_older(chat: *const AmuxChat, n: u32) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { act(chat, move |chat| async move { chat.page_older(n).await }) }
}

/// Fetches the files changed for `comparison` (a JSON `Comparison`), with
/// no patch, as the overview opens; the ticket answers `{"Ok": Overview}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open_overview(
    chat: *const AmuxChat,
    comparison: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.open_overview(comparison).await)
        })
    }
}

/// Asks the agent's host for its diff for `comparison` (a JSON
/// `Comparison`, or null for the uncommitted work) and the patch it names;
/// the ticket answers `{"Ok": FrozenReview}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_review(
    chat: *const AmuxChat,
    comparison: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.review(comparison).await)
        })
    }
}

/// The review page's document: a `FrozenReview`'s patch parsed into files
/// and hunks, with `comments` (a JSON array of `ReviewComment`) placed on
/// their lines. Null when either argument does not parse.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_doc(
    review: *const c_char,
    comments: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let review: Option<FrozenReview> = unsafe { parse(review) };
    // SAFETY: the caller's contract.
    let comments: Option<Vec<_>> = unsafe { parse(comments) };
    guard(std::ptr::null_mut(), || match (review, comments) {
        (Some(review), Some(comments)) => owned(&review.doc(&comments)),
        _ => std::ptr::null_mut(),
    })
}

/// Stores bytes to attach to a prompt; the ticket answers
/// `{"Ok": BlobRef}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `data` points to `len` bytes; the
/// strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_put_blob(
    chat: *const AmuxChat,
    data: *const u8,
    len: usize,
    name: *const c_char,
    mime: *const c_char,
) -> u64 {
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    // SAFETY: the caller's contract.
    let name = unsafe { text(name) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let mime = unsafe { text(mime) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.put_blob(&name, &mime, bytes).await)
        })
    }
}

/// An attachment's bytes, by its hash as a JSON byte array, once fetched.
/// Empty until then: asking starts the fetch, and the rows that show it
/// change when it lands.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_blob(
    chat: *const AmuxChat,
    hash: *const c_char,
) -> AmuxBytes {
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard(AmuxBytes::empty(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return AmuxBytes::empty();
        };
        let _entered = open.shared.handle().enter();
        match open.chat.blob(&hash) {
            Some(bytes) => AmuxBytes::leak(&bytes),
            None => AmuxBytes::empty(),
        }
    })
}

// --- freeing ---------------------------------------------------------------

/// # Safety
/// `text` is null or a string this library returned, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_string_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { CString::from_raw(text) });
    }
}

/// # Safety
/// `bytes` came from this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_bytes_free(bytes: AmuxBytes) {
    if !bytes.data.is_null() {
        // SAFETY: the caller's contract: this is the boxed slice we leaked.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes.data, bytes.len)) });
    }
}
