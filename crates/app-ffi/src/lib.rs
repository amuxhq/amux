//! The C ABI the phone calls.
//!
//! One [`AmuxRuntime`] hosts the embedded installation, with one profile
//! per account, on a Rust thread pool of its own. An [`AmuxProfile`] is the
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
//! Threads: every function may be called from the main thread. Reads return
//! at once. Acts that wait on the agent take a callback, called once on a
//! worker thread with a JSON result the callback borrows until it returns.
//! A profile's wake is called on a worker thread whenever its fleet (chat
//! id 0) or a chat moved, at most once until the host takes that one's
//! changes; the host schedules the take on its main thread's next turn and
//! returns. The runtime's own wake is called with 0 whenever the profile
//! list moved, and the host reads it again.
//!
//! Every returned string is the caller's to free with `amux_string_free`,
//! and every byte buffer with `amux_bytes_free`. A null return means the
//! call failed; the reason goes to the log.

use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use app_embedded::{
    EdgeOverrides, EmbeddedRuntime, PairRequest, ProfileEvent, ProfileId, StartConfig, UsageEvent,
};
use app_runtime::values::{ActOutcome, AgentAct, Draft, Found, FrozenReview, NewAgent, RowOptions};
use app_runtime::{AppRuntime, Chat, Wake};
use model::{AgentKey, Key};
use node::SourcePolicy;
use serde::Serialize;
use serde::de::DeserializeOwned;
use ui_view::{Comparison, QuestionResponse, SettingChange};

// The tests drive real agent processes, which run on Unix.
#[cfg(all(test, unix))]
mod tests;

/// Called with a chat's id when it moved, or 0 when the fleet did; for the
/// runtime, with 0 when the profile list did.
pub type AmuxWake = extern "C" fn(context: *mut c_void, chat: u64);

/// Called once with an act's JSON result, borrowed until it returns.
pub type AmuxCallback = extern "C" fn(context: *mut c_void, json: *const c_char);

/// Bytes the caller frees with `amux_bytes_free`.
#[repr(C)]
pub struct AmuxBytes {
    pub data: *mut u8,
    pub len: usize,
}

/// The embedded installation and every profile it hosts.
pub struct AmuxRuntime {
    // Dropped last: every task below runs on it.
    tokio: Option<tokio::runtime::Runtime>,
    embedded: Option<Arc<EmbeddedRuntime>>,
    watch: Option<tokio::task::JoinHandle<()>>,
    tail: u32,
}

/// One profile's fleet and chats.
pub struct AmuxProfile {
    profile: ProfileId,
    embedded: Arc<EmbeddedRuntime>,
    app: Arc<AppRuntime>,
    gate: Arc<WakeGate>,
    handle: tokio::runtime::Handle,
    tail: u32,
}

/// One open chat.
pub struct AmuxChat {
    chat: Arc<Chat>,
    gate: Arc<WakeGate>,
    handle: tokio::runtime::Handle,
}

/// Where a profile's wake passes, shut by the close that ends it. Acts
/// still in flight hold the runtime and its chats past their close, and
/// their watchers go on noticing changes; the host frees the wake's
/// context as soon as the close returns, so nothing may reach it after.
#[derive(Default)]
struct WakeGate(Mutex<Shut>);

#[derive(Default)]
struct Shut {
    profile: bool,
    chats: HashSet<u64>,
}

impl WakeGate {
    /// Calls the host's wake unless what moved is closed. The call is made
    /// under the lock, so a close waits for a wake already running.
    fn pass(&self, moved: Wake, wake: AmuxWake, context: Context) {
        let shut = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let chat = match moved {
            Wake::Fleet => 0,
            Wake::Chat(id) if shut.chats.contains(&id) => return,
            Wake::Chat(id) => id,
        };
        if !shut.profile {
            wake(context.0, chat);
        }
    }

    fn close(&self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).profile = true;
    }

    fn close_chat(&self, id: u64) {
        let mut shut = self.0.lock().unwrap_or_else(|p| p.into_inner());
        shut.chats.insert(id);
    }
}

/// A host context pointer handed back on another thread.
#[derive(Clone, Copy)]
struct Context(*mut c_void);

// SAFETY: the pointer is the host's and only ever handed back to the
// host's own callback, which the host wrote to be called from any thread.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl AmuxRuntime {
    fn handle(&self) -> tokio::runtime::Handle {
        self.tokio.as_ref().expect("running").handle().clone()
    }

    fn embedded(&self) -> &Arc<EmbeddedRuntime> {
        self.embedded.as_ref().expect("running")
    }

    /// Runs `act` on the pool and hands its JSON result to the callback.
    fn spawn<T, F>(&self, callback: AmuxCallback, context: *mut c_void, act: F)
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        spawn_on(&self.handle(), callback, context, act);
    }
}

impl AmuxProfile {
    fn spawn<T, F>(&self, callback: AmuxCallback, context: *mut c_void, act: F)
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        spawn_on(&self.handle, callback, context, act);
    }
}

fn spawn_on<T, F>(
    handle: &tokio::runtime::Handle,
    callback: AmuxCallback,
    context: *mut c_void,
    act: F,
) where
    T: Serialize,
    F: Future<Output = T> + Send + 'static,
{
    let answering = Answering {
        context: Context(context),
        callback,
        answered: false,
    };
    handle.spawn(async move {
        let result = act.await;
        let text = json(&result).unwrap_or_else(|| CString::new("null").unwrap());
        answering.answer(&text);
    });
}

/// An act's one callback, owed from the moment the act is spawned. A stop
/// shuts the pool down and drops the acts still running; the drop answers
/// for them, so whoever waits on the host side hears that the runtime has
/// stopped rather than nothing, ever.
struct Answering {
    context: Context,
    callback: AmuxCallback,
    answered: bool,
}

impl Answering {
    fn answer(mut self, text: &CStr) {
        self.answered = true;
        (self.callback)(self.context.0, text.as_ptr());
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        if !self.answered {
            (self.callback)(self.context.0, STOPPED.as_ptr());
        }
    }
}

/// What an act dropped unanswered answers: the error the host's own
/// stopped-runtime path reports.
const STOPPED: &CStr = c"{\"Err\":\"the runtime has stopped\"}";

/// Starts the installation with a pool of its own; blocks until every
/// profile's store is open. `wake` is called with 0 whenever the profile
/// list moves.
pub fn start(
    config: &StartConfig,
    overrides: EdgeOverrides,
    wake: AmuxWake,
    context: *mut c_void,
) -> Result<Box<AmuxRuntime>, String> {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("amux")
        .enable_all()
        .build()
        .map_err(|error| format!("starting the runtime's threads: {error}"))?;
    let context = Context(context);
    let embedded = tokio.block_on(async {
        EmbeddedRuntime::start_with(config, overrides, Arc::new(client::SystemClock))
            .await
            .map(Arc::new)
            .map_err(|error| error.to_string())
    })?;
    let watching = embedded.clone();
    let watch = tokio.spawn(async move {
        // A watch that falls behind ends; the list is read again from a
        // fresh one, and the host told once it has caught up.
        while let Ok(mut events) = watching.watch_profiles().await {
            let mut caught_up = false;
            while let Some(event) = events.next().await {
                caught_up |= event == ProfileEvent::CaughtUp;
                if caught_up {
                    let context = context;
                    wake(context.0, 0);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    Ok(Box::new(AmuxRuntime {
        tokio: Some(tokio),
        embedded: Some(embedded),
        watch: Some(watch),
        tail: config.tail,
    }))
}

/// Opens a profile's fleet; blocks until it has caught up with the
/// profile's store.
pub fn open_profile(
    runtime: &AmuxRuntime,
    profile: ProfileId,
    wake: AmuxWake,
    context: *mut c_void,
) -> Result<Box<AmuxProfile>, String> {
    let context = Context(context);
    let embedded = runtime.embedded().clone();
    let handle = runtime.handle();
    let gate = Arc::new(WakeGate::default());
    let passing = gate.clone();
    let app = handle.block_on(async {
        let host_wake = Arc::new(move |moved: Wake| passing.pass(moved, wake, context));
        let client = embedded
            .client(profile)
            .map_err(|error| error.to_string())?;
        let host = embedded
            .host_id(profile)
            .map_err(|error| error.to_string())?;
        AppRuntime::open(client, embedded.clock(), host, host_wake)
            .await
            .map_err(|error| error.to_string())
    })?;
    Ok(Box::new(AmuxProfile {
        profile,
        embedded,
        app: Arc::new(app),
        gate,
        handle,
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
        let _entered = open.handle.enter();
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

/// Starts the installation from a `StartConfig` as JSON. Blocks until every
/// profile's store is open; `wake` is called with 0 whenever the profile
/// list moves. Null on failure, with the reason in `error` when it is not
/// null, which the caller frees.
///
/// # Safety
/// `config` is a NUL-terminated string; `error` is null or writable. The
/// wake may be called from any thread until `amux_runtime_stop` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_start(
    config: *const c_char,
    wake: AmuxWake,
    context: *mut c_void,
    error: *mut *mut c_char,
) -> *mut AmuxRuntime {
    let started = guard(
        Err("the runtime panicked while starting".to_owned()),
        || {
            // SAFETY: the caller's contract.
            let config: StartConfig = unsafe { parse(config) }
                .ok_or_else(|| "the start configuration is not a StartConfig".to_owned())?;
            start(&config, overrides(&config), wake, context)
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

/// Stops the installation: flushes every profile's store and joins its
/// threads. Close every chat and profile first.
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
        let tokio = runtime.tokio.take().expect("running");
        tokio.block_on(async {
            if let Some(watch) = runtime.watch.take() {
                watch.abort();
                let _ = watch.await;
            }
            // An act still in flight holds the embedded runtime; dropped
            // with it, the store is left for the next start's recovery.
            if let Some(embedded) = runtime.embedded.take().and_then(Arc::into_inner) {
                let _ = embedded.shutdown().await;
            }
        });
        tokio.shutdown_timeout(std::time::Duration::from_secs(5));
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
unsafe fn profile_act<T, F, Fut>(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
    act: F,
) where
    T: Serialize + Send + 'static,
    F: FnOnce(Arc<EmbeddedRuntime>, ProfileId) -> Fut,
    Fut: Future<Output = Answered<T>> + Send + 'static,
{
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        match unsafe { profile_id(profile) } {
            Some(profile) => {
                runtime.spawn(callback, context, act(runtime.embedded().clone(), profile));
            }
            None => runtime.spawn(callback, context, async {
                Answered::<T>::Err("the profile is not a profile id".into())
            }),
        }
    });
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
        match runtime.handle().block_on(runtime.embedded().profiles()) {
            Ok(profiles) => owned(&profiles),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Creates a profile nobody has signed in on; the callback gets
/// `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_create_profile(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.create_profile(None).await)
        });
    });
}

/// Deletes a profile, by its id: its key, the machines it trusts and its
/// store. The installation keeps at least one. Close it first. The callback
/// gets `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_delete_profile(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move { Answered::from(embedded.delete_profile(profile).await) },
        )
    }
}

/// Binds a profile to an account with the refresh token the app's sign-in
/// obtained as the OAuth client `client_id`; the relay link comes up from
/// there, and the profile alone spends the token. A named `profile` is
/// adopted even when it paired machines before; null lets the registry pick
/// the account's own profile, else one that holds nothing, else a new one.
/// The callback gets `{"Ok": ProfileView}` or `{"Err": ..}`.
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
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
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
        runtime.spawn(callback, context, async move {
            let profile = match named.map(|named| named.parse::<ProfileId>()) {
                None => None,
                Some(Ok(profile)) => Some(profile),
                Some(Err(_)) => return Answered::Err("the profile is not a profile id".into()),
            };
            Answered::from(embedded.bind(profile, &url, &client, &token).await)
        });
    });
}

/// Signs a profile out of its account; it stays tied to the account. The
/// callback gets `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_sign_out(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move { Answered::from(embedded.sign_out(profile).await) },
        )
    }
}

/// Holds a bound profile's relay link down; its direct links and store
/// stay. The callback gets `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_pause(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move { Answered::from(embedded.pause(profile).await) },
        )
    }
}

/// Lets a paused profile's relay link come up again. The callback gets
/// `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_resume(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move { Answered::from(embedded.resume(profile).await) },
        )
    }
}

/// Every profile whose trust store holds a host, by its id as a JSON byte
/// array, oldest first; the callback gets `{"Ok": ["<profile id>", ..]}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `host_id` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_trusting_profiles(
    runtime: *const AmuxRuntime,
    host_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.trusting(&host_id).await.map(|profiles| {
                profiles
                    .iter()
                    .map(ProfileId::to_string)
                    .collect::<Vec<_>>()
            }))
        });
    });
}

/// Opens pairing mode on a profile and answers the link its QR code would
/// carry; the callback gets `{"Ok": "amux://pair?.."}` or `{"Err": ..}`. Only
/// a driving build offers one: the phone shows no pairing code of its own.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_offer_pairing(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move {
                if !cfg!(feature = "debug-tools") {
                    return Answered::Err("only a driving build offers pairing".into());
                }
                Answered::from(embedded.offer_pairing(profile).await)
            },
        )
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
/// callback gets `{"Ok": Bearer}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_access_token(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move { Answered::from(embedded.access_token(profile).await) },
        )
    }
}

/// Asks the account service what a profile's account buys now, over its
/// relay link, so the relay's tier follows a purchase at once; the callback
/// gets `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_refresh_entitlement(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(
            runtime,
            profile,
            callback,
            context,
            |embedded, profile| async move {
                Answered::from(embedded.refresh_entitlement(profile).await)
            },
        )
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

/// The app came to the front; counted at most once an hour.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_foreground(runtime: *const AmuxRuntime) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            let _entered = runtime.handle().enter();
            runtime.embedded().client_opened();
        }
    });
}

/// The app is leaving the screen: sends the analytics waiting, then calls
/// the callback with `null`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_background(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            callback(context, c"null".as_ptr());
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            embedded.flush_analytics().await;
        });
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

/// Opens a profile's fleet, by the profile's id; blocks until it has caught
/// up with the profile's store. Its wake is called with a chat's id when
/// that chat moved, or 0 when the fleet did. Null on failure, with the
/// reason in `error` when it is not null.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string; `error` is null or writable. The wake may be called from any
/// thread until `amux_profile_close` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_open(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    wake: AmuxWake,
    context: *mut c_void,
    error: *mut *mut c_char,
) -> *mut AmuxProfile {
    let opened = guard(Err("opening the profile panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let runtime = unsafe { live_runtime(runtime) }.ok_or("no runtime")?;
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_id(profile) }.ok_or("the profile is not a profile id")?;
        open_profile(runtime, profile, wake, context)
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

/// Closes a profile's fleet. Its wake is never called once this returns,
/// however many acts are still in flight; their callbacks still run. Close
/// its chats first.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close(profile: *mut AmuxProfile) {
    if !profile.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(profile) };
        open.gate.close();
        let handle = open.handle.clone();
        let _entered = handle.enter();
        drop(open);
    }
}

/// Writes a dump of the profile with the fleet's and every open chat's
/// part; the callback gets `{"Ok": "<bundle directory>"}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `reason` is a NUL-terminated
/// string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_dump(
    profile: *const AmuxProfile,
    reason: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let reason = unsafe { text(reason) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.spawn(callback, context, async move {
            Answered::from(app.dump(&reason).await)
        });
    });
}

/// Reaches and authenticates a machine by a `PairRequest` as JSON; the
/// callback gets `{"Ok": PendingPair}` or `{"Err": ..}`. Nothing is trusted
/// until `amux_profile_confirm_pair`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `request` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_begin_pair(
    profile: *const AmuxProfile,
    request: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let request: Option<PairRequest> = unsafe { parse(request) };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            let Some(request) = request else {
                return Answered::Err("the request is not a PairRequest".into());
            };
            Answered::from(embedded.begin_pair(id, &request).await)
        });
    });
}

/// Trusts the machine a pending pairing reached, named by its token as a
/// JSON byte array; the callback gets `{"Ok": {"host_id": [..], "name":
/// ..}}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_confirm_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    #[derive(Serialize)]
    struct Paired {
        host_id: Vec<u8>,
        name: String,
    }
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.confirm_pair(id, &token).await.map(|peer| Paired {
                host_id: peer.host_id,
                name: peer.name,
            }))
        });
    });
}

/// Turns away the machine a pending pairing reached.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_abandon_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.abandon_pair(id, &token).await)
        });
    });
}

/// Stops trusting a paired machine, by its host id as a JSON byte array.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_unpair(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.unpair(id, &host_id).await)
        });
    });
}

/// The profile's identity and the machines it trusts; the callback gets
/// `{"Ok": Roster}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_roster(
    profile: *const AmuxProfile,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            Answered::from(embedded.roster(id).await)
        });
    });
}

/// The account the profile is bound to; the callback gets
/// `{"Ok": AccountView}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_account(
    profile: *const AmuxProfile,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        let (embedded, id) = (profile.embedded.clone(), profile.profile);
        profile.spawn(callback, context, async move {
            Answered::from(embedded.account(id).await)
        });
    });
}

/// Starts an agent from a `NewAgent` as JSON; the callback gets
/// `{"Ok": AgentKey}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_create_agent(
    profile: *const AmuxProfile,
    agent: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let agent: Option<NewAgent> = unsafe { parse(agent) };
        let app = profile.app.clone();
        profile.spawn(callback, context, async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not a NewAgent".into());
            };
            Answered::from(app.create_agent(&agent).await)
        });
    });
}

/// Where a host, by its id as a JSON byte array, offers to start an agent,
/// matching `query` (or everything when it is null or empty), at most
/// `limit` of each; the callback gets `{"Ok": Directories}` or
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
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let query = unsafe { text(query) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.directories(&host_id, &query, limit).await)
        });
    });
}

/// Renames, stops or deletes an agent, named by its `AgentKey`, with an
/// `AgentAct`, both as JSON; the callback gets `{"Ok": null}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_agent_act(
    profile: *const AmuxProfile,
    agent: *const c_char,
    act: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let agent: Option<AgentKey> = unsafe { parse(agent) };
        // SAFETY: the caller's contract.
        let act: Option<AgentAct> = unsafe { parse(act) };
        let app = profile.app.clone();
        profile.spawn(callback, context, async move {
            let (Some(agent), Some(act)) = (agent, act) else {
                return Answered::Err("the agent or the act is not readable".into());
            };
            Answered::from(app.agent_act(&agent, &act).await)
        });
    });
}

/// What a provider, as a JSON `Provider` ("Claude" or "Codex"), offers on a
/// host, by its id as a JSON byte array, with no agent running there; the
/// callback gets `{"Ok": Catalogue}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_host_catalogue(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    provider: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let provider: Option<app_runtime::Provider> = unsafe { parse(provider) };
        let app = profile.app.clone();
        profile.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            let Some(provider) = provider else {
                return Answered::Err("the provider is not a Provider".into());
            };
            Answered::from(app.host_catalogue(&host_id, provider).await)
        });
    });
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
        let _entered = profile.handle.enter();
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
/// ids `expand` lists (a JSON array of byte arrays, or null).
///
/// # Safety
/// `profile` is from `amux_profile_open`; `expand` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_view(
    profile: *const AmuxProfile,
    expand: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let expand: Vec<Vec<u8>> = unsafe { parse(expand) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, |app| app.fleet_view(&expand)) }
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
        let handle = profile.handle.clone();
        let chat = handle
            .block_on(profile.app.open_chat(&agent, tail))
            .map_err(|error| error.to_string())?;
        Ok(AmuxChat {
            chat,
            gate: profile.gate.clone(),
            handle,
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

/// The id the wake names this chat by.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_id(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { held_chat(chat) }.map_or(0, |open| open.chat.id())
}

/// Closes a chat. No wake names it once this returns, however many of its
/// acts are still in flight; their callbacks still run.
///
/// # Safety
/// `chat` is null or from `amux_session_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_close(chat: *mut AmuxChat) {
    if !chat.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(chat) };
        open.gate.close_chat(open.chat.id());
        let _entered = open.handle.enter();
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

/// Runs an act on the chat's pool and hands its JSON result back.
///
/// # Safety
/// `chat` is from `amux_session_open`.
unsafe fn act<T, F, Fut>(
    chat: *const AmuxChat,
    callback: AmuxCallback,
    context: *mut c_void,
    act: F,
) where
    T: Serialize,
    F: FnOnce(Arc<Chat>) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return;
        };
        spawn_on(&open.handle, callback, context, act(open.chat.clone()));
    });
}

/// Sends a `Draft` as JSON; the callback gets a `SendOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send(
    chat: *const AmuxChat,
    draft: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            match draft {
                Some(draft) => Answered::Ok(chat.send(&draft).await),
                None => Answered::Err("the draft is not a Draft".into()),
            }
        })
    }
}

/// Answers the head ask, named by its key, with the choice at `index` on
/// its card; `note` goes back with a choice that takes one. The callback
/// gets an `ActOutcome`.
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
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let note = unsafe { text(note) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            chat.answer_choice(&ask_key, index as usize, &note).await
        })
    }
}

/// Submits the head form ask with the person's values, a JSON array of
/// `FormValue`, one per field in the form's order; the shared view checks
/// and encodes them. The callback gets an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer_form(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    values: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let values: Option<Vec<ui_view::FormValue>> = unsafe { parse(values) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            match values {
                Some(values) => chat.answer_form(&ask_key, &values).await,
                None => ActOutcome::Failed("the values are not a list of FormValue".into()),
            }
        })
    }
}

/// Answers the head question ask with one `QuestionResponse` per question, as a
/// JSON array: the pick (no options and no text skips the question) and
/// the person's note on it. The callback gets an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer_questions(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    responses: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let given: Option<Vec<QuestionResponse>> = unsafe { parse(responses) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
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
/// `QuestionResponse`, one per question. The callback gets an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_reply_instead(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    reply: *const c_char,
    so_far: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let reply = unsafe { text(reply) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let so_far: Option<Vec<QuestionResponse>> = unsafe { parse(so_far) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
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
/// callback gets an `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `change` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_change_setting(
    chat: *const AmuxChat,
    change: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let change: Option<SettingChange> = unsafe { parse(change) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
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

/// Takes a queued prompt back out of the queue. The callback gets an
/// `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated
/// JSON byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_withdraw(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.withdraw(&id).await
        })
    }
}

/// Steers a queued prompt into the running turn.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send_now(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.send_now(&id).await
        })
    }
}

/// Sends a not-confirmed input again under a new id; the callback gets a
/// `SendOutcome`, or null JSON when the input is not held.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resend(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.resend(&id).await
        })
    }
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
pub unsafe extern "C" fn amux_session_interrupt(
    chat: *const AmuxChat,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.interrupt().await
        })
    }
}

/// The exited composer's one tap: resumes the agent with a `Draft` as its
/// first prompt. The callback gets an `ActOutcome`; on anything but Done
/// the draft stays the composer's.
///
/// # Safety
/// As for `amux_session_send`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resume(
    chat: *const AmuxChat,
    draft: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            match draft {
                Some(draft) => chat.resume_with(&draft).await,
                None => ActOutcome::Failed("the draft is not a Draft".into()),
            }
        })
    }
}

/// Asks for up to `n` rows older than the oldest held; the callback gets a
/// `PageOutcome`, and the new keys arrive below the oldest.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_page_older(
    chat: *const AmuxChat,
    n: u32,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            chat.page_older(n).await
        })
    }
}

/// Fetches the files changed for `comparison` (a JSON `Comparison`), with
/// no patch, as the overview opens; the callback gets `{"Ok": Overview}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open_overview(
    chat: *const AmuxChat,
    comparison: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            Answered::from(chat.open_overview(comparison).await)
        })
    }
}

/// Asks the agent's host for its diff for `comparison` (a JSON
/// `Comparison`, or null for the uncommitted work) and the patch it names;
/// the callback gets `{"Ok": FrozenReview}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_review(
    chat: *const AmuxChat,
    comparison: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
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

/// Stores bytes to attach to a prompt; the callback gets
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
    callback: AmuxCallback,
    context: *mut c_void,
) {
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
        act(chat, callback, context, move |chat| async move {
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
    let empty = AmuxBytes {
        data: std::ptr::null_mut(),
        len: 0,
    };
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard(empty, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return AmuxBytes {
                data: std::ptr::null_mut(),
                len: 0,
            };
        };
        let _entered = open.handle.enter();
        match open.chat.blob(&hash) {
            Some(bytes) => {
                let boxed: Box<[u8]> = bytes.to_vec().into_boxed_slice();
                let len = boxed.len();
                AmuxBytes {
                    data: Box::into_raw(boxed).cast(),
                    len,
                }
            }
            None => AmuxBytes {
                data: std::ptr::null_mut(),
                len: 0,
            },
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
