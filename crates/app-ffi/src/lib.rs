//! The C ABI the phone calls.
//!
//! A thin C facade over [`app_bridge`]. One [`AmuxRuntime`] hosts the
//! embedded installation, with one profile per account. An [`AmuxProfile`] is
//! the fleet and chats of one of its profiles, opened for as long as it is on
//! screen; each open chat is an [`AmuxChat`]. View values cross as JSON whose
//! Swift mirrors are generated from the Rust definitions
//! (`cargo run -p ios-tools -- swift-types`), so the two sides share one
//! definition of every value.
//!
//! Every returned string is the caller's to free with `amux_string_free`,
//! and every byte buffer with `amux_bytes_free`. A null return means the
//! call failed; the reason goes to the log.

use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use app_bridge::{EdgeOverrides, Lifecycle, ProfileId, StartConfig};

mod review;

pub use review::*;

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
pub struct AmuxRuntime(app_bridge::AmuxRuntime);

/// One profile's fleet and chats.
pub struct AmuxProfile(app_bridge::AmuxProfile);

/// One open chat.
pub struct AmuxChat(app_bridge::AmuxChat);

#[cfg(test)]
impl AmuxRuntime {
    fn embedded(&self) -> Option<&std::sync::Arc<app_bridge::EmbeddedRuntime>> {
        self.0.embedded()
    }

    fn handle(&self) -> &tokio::runtime::Handle {
        self.0.handle()
    }
}

/// Starts the installation as `amux_runtime_start` does, with the overrides
/// a test chooses, such as serving links on loopback.
#[cfg(test)]
fn start(config: &StartConfig, overrides: EdgeOverrides) -> Result<Box<AmuxRuntime>, String> {
    app_bridge::start(config, overrides, app_bridge::Surface::Ios)
        .map(|runtime| Box::new(AmuxRuntime(runtime)))
}

/// Attaches as `amux_runtime_attach` does, to the daemon answering on a
/// front door the test chooses.
#[cfg(test)]
fn attach(front_door: &std::path::Path, tail: u32) -> Result<Box<AmuxRuntime>, String> {
    app_bridge::attach(front_door, tail).map(|runtime| Box::new(AmuxRuntime(runtime)))
}

// --- strings and pointers --------------------------------------------------

fn into_c_string(text: Option<String>) -> *mut c_char {
    text.and_then(|text| CString::new(text).ok())
        .map_or(std::ptr::null_mut(), CString::into_raw)
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

/// # Safety
/// `chat` is null or a live pointer from `amux_session_open`.
unsafe fn held_chat<'a>(chat: *const AmuxChat) -> Option<&'a AmuxChat> {
    // SAFETY: the caller's contract.
    unsafe { chat.as_ref() }
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

// --- the runtime -----------------------------------------------------------

/// This build's version, as a static string.
#[unsafe(no_mangle)]
pub extern "C" fn amux_version() -> *const c_char {
    static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(app_bridge::version()).unwrap())
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
    into_c_string(app_bridge::agent_name_problem(name))
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let catalogue = unsafe { text(catalogue) };
        // SAFETY: the caller's contract.
        let chosen = unsafe { text(chosen) };
        into_c_string(app_bridge::new_agent_settings(catalogue, chosen))
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let catalogue = unsafe { text(catalogue) };
        // SAFETY: the caller's contract.
        let chosen = unsafe { text(chosen) };
        // SAFETY: the caller's contract.
        let pick = unsafe { text(pick) };
        into_c_string(app_bridge::new_agent_pick(catalogue, chosen, pick))
    })
}

/// The step a run of setting up shows after `after`, a `SetupStep` as JSON
/// or null from the start, in an `OnboardingState` as JSON: the next one
/// still needed and, when the person came back to finish (`finishing`),
/// the next one skipped too. Answers the step as JSON, `null` when the run
/// is over, or null for a state that does not parse.
///
/// # Safety
/// `onboarding` is a NUL-terminated string; `after` is null or one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_onboarding_next(
    onboarding: *const c_char,
    after: *const c_char,
    finishing: bool,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let onboarding = unsafe { text(onboarding) };
        // SAFETY: the caller's contract.
        let after = unsafe { text(after) };
        into_c_string(app_bridge::onboarding_next(onboarding, after, finishing))
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let fields = unsafe { text(fields) };
        // SAFETY: the caller's contract.
        let values = unsafe { text(values) };
        into_c_string(app_bridge::form_problems(fields, values))
    })
}

/// The agent's commands matching the word a draft ends in, as the shared
/// view finds them: the settings view's `[CommandView]` as JSON and the
/// draft's text give a JSON array of `CommandMatch`, each with the draft
/// it leaves once picked; null when the commands do not parse. The caller
/// frees the answer.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_command_matches(
    commands: *const c_char,
    draft: *const c_char,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let commands = unsafe { text(commands) };
        // SAFETY: the caller's contract.
        let draft = unsafe { text(draft) }.unwrap_or_default();
        into_c_string(app_bridge::command_matches(commands, draft))
    })
}

/// A fenced code block coloured by kind of token, as the shared view
/// colours it for every client: the code, the fence's info string (its
/// first word names the language) and whether this is the block still
/// being written, the last of a message still streaming whose closing
/// fence has not arrived. Answers a `HighlightedCode` as JSON, one line
/// for each of the code's lines; null only when the code is not UTF-8.
/// The caller frees the answer.
///
/// A result that is not `settled` is drawn as it is and asked for again
/// soon (within a tenth of a second): a grammar is being built, or a
/// streaming block's completed lines are being passed over, in the
/// background. Callable from any thread; finished code is kept by content,
/// so asking again for the same block costs a lookup.
///
/// # Safety
/// The strings are NUL-terminated; `info` may be null for no language.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_highlight_block(
    code: *const c_char,
    info: *const c_char,
    streaming: bool,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(code) = (unsafe { text(code) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        let info = unsafe { text(info) }.unwrap_or_default();
        into_c_string(app_bridge::highlight_block(code, info, streaming))
    })
}

/// A file's change coloured as the file at `path` is, from its hunks as
/// an edit ask's card carries them (a `FilePatch`'s `[PatchHunk]` as JSON),
/// each hunk coloured as a fragment of its own. Answers a `HighlightedCode`
/// as JSON with one line for each of the hunks' lines, hunk after hunk;
/// null when the hunks do not parse. Settles and frees as
/// `amux_highlight_block` does.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_highlight_patch(
    path: *const c_char,
    hunks: *const c_char,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let path = unsafe { text(path) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let hunks = unsafe { text(hunks) };
        into_c_string(app_bridge::highlight_patch(path, hunks))
    })
}

/// Whether `pasted`, arriving in a draft in one paste, joins it as one
/// text attachment rather than as words, by the rule every client's paste
/// follows.
///
/// # Safety
/// `pasted` is a NUL-terminated string that lives for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_pastes_as_attachment(pasted: *const c_char) -> bool {
    // SAFETY: the caller's contract.
    let pasted = unsafe { text(pasted) }.unwrap_or_default();
    app_bridge::pastes_as_attachment(pasted)
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
            app_bridge::start_from_json(unsafe { text(config) }, app_bridge::Surface::Ios)
                .map(|runtime| Box::new(AmuxRuntime(runtime)))
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

/// Attaches to amux on this machine, from an `AttachConfig` as JSON: the
/// same runtime `amux_runtime_start` answers, with the daemon's profiles
/// and their agents, reached over its sockets, and nothing hosted here.
/// Calls that only an installation hosted here can answer (pairing from
/// this device, its roster, a bearer) answer that they are the phone's.
/// Install amux first (`amux_install`). Null on failure, with the reason in
/// `error` when it is not null, which the caller frees.
///
/// # Safety
/// `config` is a NUL-terminated string; `error` is null or writable.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_attach(
    config: *const c_char,
    error: *mut *mut c_char,
) -> *mut AmuxRuntime {
    let attached = guard(
        Err("the runtime panicked while attaching".to_owned()),
        || {
            // SAFETY: the caller's contract.
            app_bridge::attach_from_json(unsafe { text(config) })
                .map(|runtime| Box::new(AmuxRuntime(runtime)))
        },
    );
    match attached {
        Ok(runtime) => Box::into_raw(runtime),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// Makes sure amux is installed and answering, from `InstallOptions` as
/// JSON, and answers what it did as an `Installed`: it returns at once when
/// amux answers, starts an installed one that is stopped, and otherwise
/// downloads the stable channel's build, checks it against its signed
/// manifest, and starts it. Blocks for as long as that
/// takes, so call it off the main thread. Null on failure, with the reason
/// in `error` when it is not null, which the caller frees.
///
/// # Safety
/// `options` is a NUL-terminated string; `error` is null or writable.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_install(
    options: *const c_char,
    error: *mut *mut c_char,
) -> *mut c_char {
    let installed = guard(Err("installing panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        app_bridge::install_from_json(unsafe { text(options) })
    });
    match installed {
        Ok(installed) => into_c_string(Some(installed)),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// Looks for a newer version of the Mac app, from an `UpdateCheck` as JSON,
/// in the feed beside amux's own, and answers an `AppUpdate` as JSON once
/// that version is downloaded, checked against its signed manifest and
/// unpacked, or `null` when the running app is current. Blocks for as long
/// as the download takes, so call it off the main thread. Null on failure,
/// with the reason in `error` when it is not null, which the caller frees.
///
/// # Safety
/// `check` is a NUL-terminated string; `error` is null or writable.
#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_check_for_update(
    check: *const c_char,
    error: *mut *mut c_char,
) -> *mut c_char {
    let checked = guard(Err("checking for an update panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        app_bridge::check_for_update_from_json(unsafe { text(check) })
    });
    match checked {
        Ok(update) => into_c_string(Some(update)),
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
        unsafe { live_runtime(runtime) }.map_or(-1, |runtime| runtime.0.wake_fd())
    })
}

/// Steps every input waiting for the cores on the calling queue, then
/// answers what moved since the last step as JSON:
///
/// - `profiles`: every profile, oldest first, when the list moved; null
///   otherwise.
/// - `fleets`: `[{"fleet": n, "delta": FleetDelta}]`, each open fleet that
///   moved, by the number `amux_profile_fleet_id` answers.
/// - `chats`: `[{"fleet": n, "delta": ChatDelta}]`, each open chat that
///   moved, with the fleet it belongs to.
/// - `sign_in_offers`: `[{"fleet": n, "offers": [..]}]`, each open
///   profile whose sign-in offers moved, with every offer it holds now.
/// - `onboarding`: `[{"fleet": n, "onboarding": OnboardingState}]`, each
///   followed profile whose setting up moved.
/// - `pairing`: `[{"fleet": n, "status": PairingStatusView}]`, each open
///   pairing window whose status moved.
/// - `answers`: `[{"ticket": n, "json": text}]`, each act that ended, its
///   result under the ticket the act returned, after the deltas of what it
///   moved.
///
/// Null for no runtime.
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
        into_c_string(runtime.0.step())
    })
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
        runtime.0.feed_json(unsafe { text(input) })
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
    let runtime = unsafe { Box::from_raw(runtime) };
    guard((), || {
        runtime.0.stop();
    });
}

// --- the profile registry ------------------------------------------------

/// Creates a profile nobody has signed in on; the ticket answers
/// `{"Ok": ProfileView}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_create_profile(runtime: *const AmuxRuntime) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        unsafe { live_runtime(runtime) }.map_or(0, |runtime| runtime.0.create_profile())
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
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.delete_profile(unsafe { text(profile) })
    })
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
        let named = unsafe { text(profile) };
        // SAFETY: the caller's contract.
        let url = unsafe { text(cloud_url) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let client = unsafe { text(client_id) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let token = unsafe { text(refresh_token) }.unwrap_or_default();
        runtime.0.bind(named, url, client, token)
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
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.sign_out(unsafe { text(profile) })
    })
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
        runtime.0.trusting_profiles(unsafe { text(host_id) })
    })
}

/// Opens pairing mode on a profile and answers the link its QR code would
/// carry; the ticket answers `{"Ok": "https://amux.sh/p#.."}` or `{"Err": ..}`. Only
/// a driving build offers one: the phone shows no pairing code of its own.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_offer_pairing(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.offer_pairing(unsafe { text(profile) })
    })
}

/// Every listed agent of the profile in front of somebody keeps a source
/// open; every other profile's, and every profile's when `profile` is null
/// (the app in the background), opens sources only for the chats that
/// open.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_list_sources(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            // SAFETY: the caller's contract.
            runtime.0.list_sources(unsafe { text(profile) });
        }
    });
}

/// Whether every listed agent of a profile keeps a source open.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_source_policy_listed(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> bool {
    guard(false, || {
        // SAFETY: the caller's contract.
        unsafe { live_runtime(runtime) }.is_some_and(|runtime| {
            // SAFETY: the caller's contract.
            runtime.0.source_policy_listed(unsafe { text(profile) })
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
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            // SAFETY: the caller's contract.
            runtime.0.discovered(unsafe { text(found) });
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
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.access_token(unsafe { text(profile) })
    })
}

/// A profile's host id, which is what a notification for it is addressed
/// to; the ticket answers `{"Ok": [byte, ..]}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_host_id(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.host_id(unsafe { text(profile) })
    })
}

/// Says whether a profile wants notifications: `key` is a JSON byte array,
/// the public key they are sealed to, or `null` to take the wish back. Nothing
/// keeps it, so the app says it at every start. The ticket answers
/// `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_delete_profile`; `key` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_want_notifications(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    key: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime
            .0
            .want_notifications(unsafe { text(profile) }, unsafe { text(key) })
    })
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
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.refresh_entitlement(unsafe { text(profile) })
    })
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
            runtime.0.set_telemetry(on);
        }
    });
}

/// The app's automatic-reports setting changed.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_set_auto_reports(runtime: *const AmuxRuntime, on: bool) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            runtime.0.set_auto_reports(on);
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
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            // SAFETY: the caller's contract.
            runtime
                .0
                .record(unsafe { text(profile) }, unsafe { text(event) });
        }
    });
}

// --- settings ----------------------------------------------------------------

/// The installation's everyday settings; the ticket answers
/// `{"Ok": MachineSettings}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_settings(runtime: *const AmuxRuntime) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        unsafe { live_runtime(runtime) }.map_or(0, |runtime| runtime.0.settings())
    })
}

/// Changes the installation's settings by a `SettingsChange` as JSON, each
/// in force at once; the ticket answers `{"Ok": MachineSettings}`, the
/// settings as the change left them, or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `change` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_update_settings(
    runtime: *const AmuxRuntime,
    change: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime.0.update_settings(unsafe { text(change) })
    })
}

/// A one-time link that signs the browser in to a profile's account and
/// opens `path` on its account service (`/offer`, the page after signing
/// in). The ticket answers `{"Ok": "<url>"}`, or `{"Err": ..}` for a
/// profile signed out.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` and `path` are
/// NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_web_sign_in_link(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    path: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        runtime
            .0
            .web_sign_in_link(unsafe { text(profile) }, unsafe { text(path) })
    })
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
        runtime
            .0
            .open_profile(unsafe { text(profile) })
            .map(|profile| Box::new(AmuxProfile(profile)))
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

/// Closes a profile's fleet. No step hands over its delta, or its chats',
/// once this returns, however many acts are still in flight; they are
/// answered all the same. Close its chats first.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close(profile: *mut AmuxProfile) {
    if !profile.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(profile) };
        open.0.close();
    }
}

/// The number deltas name this fleet by.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_fleet_id(profile: *const AmuxProfile) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { live_profile(profile) }.map_or(0, |open| open.0.fleet_id())
}

/// Writes a dump of the profile with the fleet's part and the record of
/// every chat on screen: the one bundle, whether it is then saved or sent
/// with a person's report. The ticket answers `{"Ok": "<bundle
/// directory>"}` or `{"Err": ..}`.
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
        let reason = unsafe { text(reason) }.unwrap_or_default();
        profile.0.dump(reason)
    })
}

/// Writes a person's report around the dump at `dump_path` (empty when
/// none was taken; a report's own directory to send it again), with its
/// `report.json` and the frozen frame as PNG, and sends it to the account
/// the profile is signed in to. The ticket answers `{"Ok": ReportSent}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `dump_path` and `header` are
/// NUL-terminated strings or null; `frame` is null or `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_submit_report(
    profile: *const AmuxProfile,
    dump_path: *const c_char,
    header: *const c_char,
    frame: *const u8,
    len: usize,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let dump_path = unsafe { text(dump_path) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let header = unsafe { text(header) }
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        let frame = if frame.is_null() {
            Vec::new()
        } else {
            // SAFETY: the caller's contract.
            unsafe { std::slice::from_raw_parts(frame, len) }.to_vec()
        };
        profile.0.submit_report(dump_path, header, frame)
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
        profile.0.begin_pair(unsafe { text(request) })
    })
}

/// Offers the paired machine whose host id `host_id` names, as a JSON byte
/// array, this profile's account: the phone signed in first offers the
/// computer it paired with. The ticket answers `{"Ok": null}` or
/// `{"Err": ..}`; the offer stands until that machine answers it.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `host_id` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_offer_sign_in(
    profile: *const AmuxProfile,
    host_id: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        profile.0.offer_sign_in(unsafe { text(host_id) })
    })
}

/// Answers a paired machine's offer of its account, a `SignInAnswer` as
/// JSON. The ticket answers `{"Ok": SignInOfferOutcome}`: accepting signs
/// this profile in to that account and answers it signed in, with the
/// profile list moving too; declining ends the offer; accepting while the
/// offering machine cannot be reached answers that, and the offer stays in
/// the profile's list for as long as it still stands. Any other refusal is
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `answer` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_answer_sign_in_offer(
    profile: *const AmuxProfile,
    answer: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        profile.0.answer_sign_in_offer(unsafe { text(answer) })
    })
}

/// Follows the profile's setting up: the next step hands over the steps
/// as the daemon keeps them, and every step after one that moved them
/// does, as an `OnboardingState` under the profile's fleet number, until
/// the profile closes. Following twice changes nothing.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_watch_onboarding(profile: *const AmuxProfile) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(profile) = unsafe { live_profile(profile) } {
            profile.0.watch_onboarding();
        }
    });
}

/// Tells the daemon what the person did with setting up this profile, an
/// `OnboardingAnswer` as JSON, counted for this app; the ticket answers
/// `{"Ok": OnboardingState}`, the steps after it, or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `answer` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_answer_onboarding(
    profile: *const AmuxProfile,
    answer: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        profile.0.answer_onboarding(unsafe { text(answer) })
    })
}

/// Opens a pairing window on the profile that takes the PIN or the QR
/// code's secret, for `ttl_seconds` (0 for the daemon's own), closing any
/// window open first, and follows its status: every step after it moved
/// hands over a `PairingStatusView` under the profile's fleet number until
/// `amux_profile_close_pairing`. The ticket answers
/// `{"Ok": PairingWindowView}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_open_pairing(
    profile: *const AmuxProfile,
    ttl_seconds: u64,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        unsafe { live_profile(profile) }.map_or(0, |profile| profile.0.open_pairing(ttl_seconds))
    })
}

/// Closes the profile's pairing window and stops following it; the ticket
/// answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close_pairing(profile: *const AmuxProfile) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        unsafe { live_profile(profile) }.map_or(0, |profile| profile.0.close_pairing())
    })
}

/// Trusts the machine a pending pairing reached, named by its token as a
/// JSON byte array; the ticket answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_confirm_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        profile.0.confirm_pair(unsafe { text(token) })
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
        profile.0.abandon_pair(unsafe { text(token) })
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
        profile.0.unpair(unsafe { text(host_id) })
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
        unsafe { live_profile(profile) }.map_or(0, |profile| profile.0.roster())
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
        profile.0.create_agent(unsafe { text(agent) })
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
        let host_id = unsafe { text(host_id) };
        // SAFETY: the caller's contract.
        let query = unsafe { text(query) }.unwrap_or_default();
        profile.0.directories(host_id, query, limit)
    })
}

/// Pins `path` among a host's repositories, the host by its id as a JSON
/// byte array, or unpins it; the ticket answers `{"Ok": null}` or
/// `{"Err": ..}`. The host's entry says the change once it has made it.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated
/// or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_pin_folder(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    path: *const c_char,
    pinned: bool,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id = unsafe { text(host_id) };
        // SAFETY: the caller's contract.
        let path = unsafe { text(path) }.unwrap_or_default();
        profile.0.pin_folder(host_id, path, pinned)
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
        profile
            .0
            .agent_act(unsafe { text(agent) }, unsafe { text(act) })
    })
}

/// What deleting an agent, named by its `AgentKey` as JSON, does to its
/// workspace, as the agent's host previews it: the ticket answers
/// `{"Ok": DeleteWarning}` (`Unknown` when the host could not be asked),
/// or `{"Ok": null}` when the workspace stays.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the string is NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_delete_warning(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        profile.0.delete_warning(unsafe { text(agent) })
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
        profile
            .0
            .host_catalogue(unsafe { text(host_id) }, unsafe { text(provider) })
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
        if let Some(profile) = unsafe { live_profile(profile) } {
            profile.0.set_foreground(foreground);
        }
    });
}

// --- the fleet -------------------------------------------------------------

/// The fleet whole as a `FleetDelta`: home's sections, families listed
/// under the roots `amux_fleet_toggle_family` opened, narrowed by the last
/// `amux_fleet_filter`, and every host, this device first. Steps after it
/// hand over what moved since.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_opening(profile: *const AmuxProfile) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return std::ptr::null_mut();
        };
        into_c_string(profile.0.fleet_opening())
    })
}

/// Lists a family's members under its root, named by the root's agent id
/// as a JSON byte array, or folds them away again; the next step hands
/// over the list as it then stands.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `root` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_toggle_family(
    profile: *const AmuxProfile,
    root: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(profile) = unsafe { live_profile(profile) } {
            // SAFETY: the caller's contract.
            profile.0.fleet_toggle_family(unsafe { text(root) });
        }
    });
}

/// Narrows home's list to what a `FleetFilter`, as JSON, keeps; null keeps
/// every agent. The next step hands over the list as it then stands.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `filter` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_filter(profile: *const AmuxProfile, filter: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(profile) = unsafe { live_profile(profile) } {
            // SAFETY: the caller's contract.
            profile.0.fleet_filter(unsafe { text(filter) });
        }
    });
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        into_c_string(profile.0.fleet_card(unsafe { text(agent) }))
    })
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        into_c_string(profile.0.fleet_family(unsafe { text(agent) }))
    })
}

/// What settling an agent costs, as a `SettleWarning`, or null JSON for an
/// agent the fleet does not hold. Ask before settling when it cuts work
/// short or ends a conversation nothing resumes.
///
/// # Safety
/// As for `amux_fleet_card`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_settle_warning(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        into_c_string(profile.0.fleet_settle_warning(unsafe { text(agent) }))
    })
}

// --- a chat ----------------------------------------------------------------

/// Opens a chat on a fleet agent, named by its `AgentKey` as JSON, with
/// `tail` rows (the configured tail when zero). Blocks until the snapshot
/// and the rows this device holds are applied, so the opening is whole
/// even with the agent's host away. Null on failure, with the
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
        profile
            .0
            .open_chat(unsafe { text(agent) }, tail)
            .map(|chat| Box::new(AmuxChat(chat)))
    });
    match opened {
        Ok(chat) => Box::into_raw(chat),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// The number deltas name this chat by, within its fleet.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_id(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { held_chat(chat) }.map_or(0, |open| open.0.id())
}

/// Closes a chat. No step hands over its delta once this returns, however
/// many of its acts are still in flight; they are answered all the same.
///
/// # Safety
/// `chat` is null or from `amux_session_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_close(chat: *mut AmuxChat) {
    if !chat.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(chat) };
        open.0.close();
    }
}

/// The chat whole as a `ChatDelta`, for the app's model opening on it:
/// every key, the frame and the views around it. No step hands over a
/// delta of the chat before this; every one after says what moved since.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_opening(chat: *const AmuxChat) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        into_c_string(open.0.opening())
    })
}

/// Tells the chat whether the reader is at the newest row (true) or has
/// scrolled into history (false). A chat opens following.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_follow(chat: *const AmuxChat, following: bool) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            open.0.follow(following);
        }
    });
}

/// The `Draft` this device kept for the agent when the chat opened, or
/// null JSON.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_kept_draft(chat: *const AmuxChat) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        into_c_string(open.0.kept_draft())
    })
}

/// Keeps a `Draft`, as JSON, with this device's daemon as it now stands,
/// so it outlives the chat and the app; an empty one is forgotten.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_keep_draft(chat: *const AmuxChat, draft: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            open.0.keep_draft(unsafe { text(draft) });
        }
    });
}

/// `[Row]` for these keys (a JSON array of strings), in order, skipping
/// keys not held, each run folded to its newest step unless
/// `amux_session_toggle_run` opened it: for a row drawn for the first
/// time, or after a reload.
///
/// # Safety
/// `chat` is from `amux_session_open`; `keys` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_rows_for(
    chat: *const AmuxChat,
    keys: *const c_char,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        into_c_string(open.0.rows_for(unsafe { text(keys) }))
    })
}

/// Opens or closes the run the row `member` belongs to; the next step
/// hands over its rows drawn so. The chat keeps an open run open as it
/// grows, older steps page in, or the window trims it.
///
/// # Safety
/// `chat` is from `amux_session_open`; `member` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_run(chat: *const AmuxChat, member: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            let member = unsafe { text(member) }.unwrap_or_default();
            open.0.toggle_run(member);
        }
    });
}

/// Opens or closes the group of calls that opened agents the row `member`
/// belongs to ("3 subagents"); the next step hands over its rows drawn so.
/// Opening a row's agent is a separate gesture: `amux_session_open` on the
/// agent the delta's `agents` names for the row's `opens`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `member` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_group(chat: *const AmuxChat, member: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            let member = unsafe { text(member) }.unwrap_or_default();
            open.0.toggle_group(member);
        }
    });
}

/// Folds each finished turn to its prompt, the line over its work and its
/// answer (true), or only its runs (false, as a chat opens); the next step
/// hands over every row drawn so.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_fold_turns(chat: *const AmuxChat, on: bool) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            open.0.fold_turns(on);
        }
    });
}

/// Opens or closes the finished turn whose end is the row `end`; the next
/// step hands over its rows drawn so.
///
/// # Safety
/// `chat` is from `amux_session_open`; `end` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_turn(chat: *const AmuxChat, end: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            let end = unsafe { text(end) }.unwrap_or_default();
            open.0.toggle_turn(end);
        }
    });
}

/// Starts a `ChatAct`, as JSON, on the chat; the ticket answers what the
/// act says it answers.
///
/// # Safety
/// `chat` is from `amux_session_open`; `act` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_act(chat: *const AmuxChat, act: *const c_char) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        open.0.act(unsafe { text(act) })
    })
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
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        into_c_string(open.0.draft_of(unsafe { text(input_id) }))
    })
}

/// Forgets a not-confirmed input.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_discard(chat: *const AmuxChat, input_id: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            open.0.discard(unsafe { text(input_id) });
        }
    });
}

/// Asks the agent's host which files changed for `comparison` (a JSON
/// `Comparison`), as the overview opens; the ticket answers
/// `{"Ok": Overview}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open_overview(
    chat: *const AmuxChat,
    comparison: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        open.0.open_overview(unsafe { text(comparison) })
    })
}

/// Why an attachment of `size` bytes would be refused before anything of
/// it is read or stored, as a JSON string naming the file; JSON `null` when
/// it may go.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_attachment_refusal(
    chat: *const AmuxChat,
    name: *const c_char,
    mime: *const c_char,
    size: u64,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        let name = unsafe { text(name) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let mime = unsafe { text(mime) }.unwrap_or_default();
        into_c_string(open.0.attachment_refusal(name, mime, size))
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
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return 0;
        };
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            // SAFETY: the caller's contract.
            unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
        };
        // SAFETY: the caller's contract.
        let name = unsafe { text(name) }.unwrap_or_default();
        // SAFETY: the caller's contract.
        let mime = unsafe { text(mime) }.unwrap_or_default();
        open.0.put_blob(bytes, name, mime)
    })
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
    guard(AmuxBytes::empty(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return AmuxBytes::empty();
        };
        // SAFETY: the caller's contract.
        match open.0.blob(unsafe { text(hash) }) {
            Some(bytes) => AmuxBytes::leak(&bytes),
            None => AmuxBytes::empty(),
        }
    })
}

/// Whether fetching an attachment's bytes failed, by its hash as a JSON byte
/// array.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_blob_failed(
    chat: *const AmuxChat,
    hash: *const c_char,
) -> bool {
    guard(false, || {
        // SAFETY: the caller's contract.
        unsafe { held_chat(chat) }.is_some_and(|open| {
            // SAFETY: the caller's contract.
            open.0.blob_failed(unsafe { text(hash) })
        })
    })
}

/// Fetches an attachment's bytes again, after a failure. The rows that show
/// it change when it lands.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_fetch_blob(chat: *const AmuxChat, hash: *const c_char) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            // SAFETY: the caller's contract.
            open.0.fetch_blob(unsafe { text(hash) });
        }
    });
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
