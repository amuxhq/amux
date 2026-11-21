//! Installation startup, attachment, stepping, lifecycle, profiles registry
//! and pure value helpers exposed over the C ABI.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use app_embedded::{EdgeOverrides, EmbeddedRuntime, Phone, ProfileId, StartConfig, UsageEvent};
use app_runtime::values::{
    Found, MachineSettings, OnboardingState, RepositorySuggestion, SettingsChange, SignInOfferView,
};
use app_runtime::{AppRuntime, Wake};
use node::SourcePolicy;
use serde::Deserialize;

use crate::helpers::{
    Answered, fail, from_hex, guard, live_runtime, owned, parse, profile_act,
    profile_act_on_profiles, profile_id, text, view,
};
use crate::outbox::{Outbox, View, Waiting};
use crate::{ATTACHED, AmuxProfile, AmuxRuntime, Shared};

/// Starts the installation with its transport on a thread of its own and
/// its cores on the calling thread's queue; blocks, stepping them, until
/// every profile's store is open.
pub fn start(config: &StartConfig, overrides: EdgeOverrides) -> Result<Box<AmuxRuntime>, String> {
    let phone = Phone::start(config, overrides, Arc::new(client::SystemClock))
        .map_err(|error| error.to_string())?;
    let installation = phone
        .embedded()
        .installation()
        .map_err(|error| error.to_string())?;
    serve(phone, installation, config.tail)
}

/// Attaches to the daemon answering on `front_door` (`<root>/amux.sock`):
/// the same runtime as [`start`], with the profiles and their agents the
/// daemon's, reached over its sockets, and nothing hosted here.
///
/// Only the Mac app attaches, so the runtime names itself the Mac app to
/// the daemon: on every profile socket it opens, which the daemon counts
/// as the Mac app opening, and in what it answers of setting up.
pub fn attach(front_door: &std::path::Path, tail: u32) -> Result<Box<AmuxRuntime>, String> {
    let phone = Phone::attached().map_err(|error| error.to_string())?;
    let door = front_door.to_owned();
    let installation = phone
        .wait(async move { client::GrpcInstallation::connect(&door).await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| format!("amux is not answering on {}: {error}", front_door.display()))?
        .naming("mac");
    serve(phone, Arc::new(installation), tail)
}

/// How soon a stream from the daemon is opened again after it ends,
/// doubling up to the most while the daemon does not answer.
const RESUBSCRIBE_FIRST: std::time::Duration = std::time::Duration::from_millis(250);
const RESUBSCRIBE_MOST: std::time::Duration = std::time::Duration::from_secs(5);

/// Hands `each` every message of the stream `open` opens, for as long as
/// the task runs. An attached daemon restarts under the app (an update, a
/// stop and start), which ends the stream; it is opened again once the
/// daemon answers, and every stream followed this way starts with the state
/// now, so nothing that moved meanwhile is missed.
pub(crate) async fn follow<T, Opened>(
    mut open: impl FnMut() -> Opened,
    mut each: impl FnMut(T),
) -> std::convert::Infallible
where
    Opened: Future<Output = Result<client::EventStream<T>, client::RpcError>>,
{
    use futures_util::StreamExt as _;
    let mut pause = RESUBSCRIBE_FIRST;
    loop {
        if let Ok(mut stream) = open().await {
            while let Some(Ok(now)) = stream.next().await {
                each(now);
                pause = RESUBSCRIBE_FIRST;
            }
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(RESUBSCRIBE_MOST);
    }
}

/// Serves the profile list to the app: posted now, for the first step, and
/// again whenever the installation's profile stream says it moved.
fn serve(
    phone: Phone,
    installation: Arc<dyn client::Installation>,
    tail: u32,
) -> Result<Box<AmuxRuntime>, String> {
    let outbox = Arc::new(Outbox {
        tickets: AtomicU64::new(0),
        fleets: AtomicU64::new(0),
        waiting: Mutex::new(Waiting::default()),
        open: Mutex::new(HashMap::new()),
        ring: Box::new(phone.bell()),
    });
    // The stream begins before the list is first read, so no move falls
    // between them; its first message is the list as it stands.
    let watching = installation.clone();
    let mut moves = phone
        .wait(async move { watching.subscribe_profiles().await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| error.to_string())?;
    // The list as the start left it waits for the first step, so the app
    // has it before anything else the runtime hands over.
    let listing = installation.clone();
    let listed = phone
        .wait(async move { listing.list_profiles().await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| error.to_string())?;
    outbox.post_listed(&listed);
    let posting = outbox.clone();
    let resubscribing = installation.clone();
    let watch = phone.handle().spawn(async move {
        use futures_util::StreamExt as _;
        // An attached daemon restarts under the app (an update, a stop and
        // start), which ends the stream; it is opened again once the front
        // door answers, and its first message is the whole list, so nothing
        // that moved meanwhile is missed.
        let mut pause = RESUBSCRIBE_FIRST;
        loop {
            while let Some(Ok(moved)) = moves.next().await {
                posting.post_listed(&moved.profiles);
                pause = RESUBSCRIBE_FIRST;
            }
            loop {
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(RESUBSCRIBE_MOST);
                if let Ok(again) = resubscribing.subscribe_profiles().await {
                    moves = again;
                    break;
                }
            }
        }
    });
    Ok(Box::new(AmuxRuntime {
        shared: Arc::new(Shared {
            phone,
            installation,
            outbox,
        }),
        watch: Some(watch),
        tail,
    }))
}

/// Opens a profile's fleet; blocks, stepping the cores, until it has
/// caught up with the profile's store.
pub fn open_profile(runtime: &AmuxRuntime, profile: ProfileId) -> Result<Box<AmuxProfile>, String> {
    let shared = runtime.shared.clone();
    let installation = shared.installation.clone();
    let clock: Arc<dyn client::Clock> = match shared.embedded() {
        Some(embedded) => embedded.clock(),
        None => Arc::new(client::SystemClock),
    };
    let outbox = shared.outbox.clone();
    let fleet = outbox.fleets.fetch_add(1, Ordering::SeqCst) + 1;
    let app = shared
        .phone
        .wait(async move {
            let host_wake = Arc::new(move |moved: Wake| {
                let chat = match moved {
                    Wake::Fleet => 0,
                    Wake::Chat(id) => id,
                };
                outbox.redraw(fleet, chat)
            });
            let listed = installation
                .list_profiles()
                .await
                .map_err(|error| error.to_string())?;
            let described = listed
                .iter()
                .find(|listed| listed.id == profile.as_bytes())
                .ok_or_else(|| format!("the installation hosts no profile {profile}"))?;
            let client = installation
                .open_profile(described)
                .await
                .map_err(|error| error.to_string())?;
            let host = client
                .get_device_identity()
                .await
                .map_err(|error| error.to_string())?
                .host_id;
            AppRuntime::open(client, clock, host, host_wake)
                .await
                .map_err(|error| error.to_string())
        })
        .ok_or("the runtime has stopped")??;
    let app = Arc::new(app);
    shared
        .outbox
        .opened(fleet, 0, View::Fleet(Arc::downgrade(&app)));
    let (watching, posting) = (app.client().clone(), shared.outbox.clone());
    let offers = shared.phone.handle().spawn(async move {
        follow(
            || watching.subscribe_sign_in_offers(),
            |now: wire::SubscribeSignInOffersResponse| {
                posting.post_offers(
                    fleet,
                    now.offers.iter().map(SignInOfferView::from).collect(),
                );
            },
        )
        .await;
    });
    Ok(Box::new(AmuxProfile {
        profile,
        fleet,
        shared,
        app,
        tail: runtime.tail,
        offers,
        onboarding: Mutex::new(None),
        pairing: Mutex::new(None),
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
        let Some(onboarding) = (unsafe { parse::<OnboardingState>(onboarding) }) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's contract.
        let after: Option<ui_view::SetupStep> = unsafe { parse(after) };
        owned(&ui_view::next_step(
            &wire::Onboarding::from(&onboarding),
            after,
            finishing,
        ))
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
    // SAFETY: the caller's contract.
    let commands: Option<Vec<ui_view::CommandView>> = unsafe { parse(commands) };
    // SAFETY: the caller's contract.
    let draft = unsafe { text(draft) }.unwrap_or_default();
    guard(std::ptr::null_mut(), || match commands {
        Some(commands) => owned(&ui_view::command_matches(&commands, draft)),
        None => std::ptr::null_mut(),
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
    ui_view::pastes_as_attachment(pasted)
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
#[cfg(not(target_os = "ios"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_attach(
    config: *const c_char,
    error: *mut *mut c_char,
) -> *mut AmuxRuntime {
    let attached = guard(
        Err("the runtime panicked while attaching".to_owned()),
        || {
            // SAFETY: the caller's contract.
            let config: app_runtime::values::AttachConfig = unsafe { parse(config) }
                .ok_or_else(|| "the attach configuration is not an AttachConfig".to_owned())?;
            let door = installer::front_door(config.config.as_deref())
                .map_err(|error| error.to_string())?;
            attach(&door, config.tail)
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
#[cfg(not(target_os = "ios"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_install(
    options: *const c_char,
    error: *mut *mut c_char,
) -> *mut c_char {
    let installed = guard(Err("installing panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let options: app_runtime::values::InstallOptions = unsafe { parse(options) }
            .ok_or_else(|| "the options are not InstallOptions".to_owned())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let options = installer::Options {
            config: options.config,
        };
        let outcome = runtime
            .block_on(installer::ensure(&options, |_| {}))
            .map_err(|error| error.to_string())?;
        Ok(match outcome {
            installer::Outcome::Running => app_runtime::values::Installed::Running,
            installer::Outcome::Started => app_runtime::values::Installed::Started,
            installer::Outcome::Installed { version } => {
                app_runtime::values::Installed::Installed { version }
            }
        })
    });
    match installed {
        Ok(installed) => owned(&installed),
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
        let check: app_runtime::values::UpdateCheck =
            unsafe { parse(check) }.ok_or_else(|| "the check is not an UpdateCheck".to_owned())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let options = installer::update::Options {
            config: check.config,
            current: check.current,
            dir: check.dir,
        };
        let staged = runtime
            .block_on(installer::update::check(&options))
            .map_err(|error| error.to_string())?;
        Ok(staged.map(|staged| app_runtime::values::AppUpdate {
            version: staged.version,
            app: staged.app,
        }))
    });
    match checked {
        Ok(update) => owned(&update),
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
        let _entered = runtime.handle().enter();
        owned(&runtime.shared.outbox.take())
    })
}

/// What the app's life feeds the runtime.
#[derive(Debug, Deserialize)]
pub enum Lifecycle {
    /// The app came to the front: counted at most once an hour, and every
    /// profile suspended dials its hosts and the relay again.
    Foreground,
    /// The system woke the app in the background, for a push: every profile
    /// suspended dials its hosts and the relay again, until the next
    /// `Suspending`.
    Woken,
    /// The app is leaving the screen: the analytics waiting are sent.
    Background,
    /// The app is about to be suspended: every profile closes its links
    /// and dials nothing until the app runs again, the analytics waiting
    /// are sent, and every profile's core is checkpointed.
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
    // A daemon keeps its own links and analytics; an attached app's life
    // is nothing to it.
    let Some(embedded) = runtime.embedded().cloned() else {
        return Ok(());
    };
    match input {
        Lifecycle::Foreground => {
            {
                let _entered = runtime.handle().enter();
                embedded.client_opened();
            }
            resume(runtime, embedded)?;
        }
        Lifecycle::Woken => resume(runtime, embedded)?,
        Lifecycle::Background => {
            runtime
                .shared
                .phone
                .wait(async move { embedded.flush_analytics().await })
                .ok_or("the runtime has stopped")?;
        }
        Lifecycle::Suspending => {
            runtime
                .shared
                .phone
                .wait(async move {
                    embedded.suspend().await;
                    embedded.flush_analytics().await;
                    embedded.checkpoint().await
                })
                .ok_or("the runtime has stopped")?
                .map_err(|error| error.to_string())?;
        }
        Lifecycle::NetworkChanged => {
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

fn resume(runtime: &AmuxRuntime, embedded: Arc<EmbeddedRuntime>) -> Result<(), String> {
    runtime
        .shared
        .phone
        .wait(async move { embedded.resume().await })
        .ok_or("the runtime has stopped")?;
    Ok(())
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
        let installation = runtime.shared.installation.clone();
        runtime.shared.act_on_profiles(async move {
            view(
                installation
                    .create_profile(wire::CreateProfileRequest::default())
                    .await,
            )
        })
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
        profile_act_on_profiles(runtime, profile, |installation, profile| async move {
            Answered::from(
                installation
                    .delete_profile(wire::DeleteProfileRequest {
                        profile_id: profile.as_bytes().to_vec(),
                    })
                    .await,
            )
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
        let installation = runtime.shared.installation.clone();
        runtime.shared.act_on_profiles(async move {
            let profile = match named.map(|named| named.parse::<ProfileId>()) {
                None => None,
                Some(Ok(profile)) => Some(profile),
                Some(Err(_)) => return Answered::Err("the profile is not a profile id".into()),
            };
            // A profile the person named is theirs to sign in, whatever it
            // already holds.
            view(
                installation
                    .bind_profile(wire::BindProfileRequest {
                        profile_id: profile.map(|profile| profile.as_bytes().to_vec()),
                        cloud_url: url,
                        staged_refresh_token: token,
                        adopt_non_pristine: profile.is_some(),
                        client_id: client,
                    })
                    .await,
            )
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
        profile_act_on_profiles(runtime, profile, |installation, profile| async move {
            let signed_out = async {
                let listed = installation.list_profiles().await?;
                let described = listed
                    .iter()
                    .find(|listed| listed.id == profile.as_bytes())
                    .ok_or_else(|| {
                        client::RpcError::Refused(wire::Error::new(
                            wire::ErrorCode::NotFound,
                            format!("no profile {profile}"),
                        ))
                    })?;
                installation
                    .open_profile(described)
                    .await?
                    .sign_out()
                    .await?;
                let listed = installation.list_profiles().await?;
                listed
                    .into_iter()
                    .find(|listed| listed.id == profile.as_bytes())
                    .ok_or_else(|| {
                        client::RpcError::Refused(wire::Error::new(
                            wire::ErrorCode::NotFound,
                            format!("no profile {profile}"),
                        ))
                    })
            };
            view(signed_out.await)
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
        let Some(embedded) = runtime.embedded().cloned() else {
            return runtime.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        runtime.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.trusting(&host_id).map(|profiles| {
                profiles
                    .iter()
                    .map(ProfileId::to_string)
                    .collect::<Vec<_>>()
            }))
        })
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
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let shown = unsafe { profile_id(profile) };
        // Switching opens and closes sources, which run on the pool.
        let _entered = runtime.handle().enter();
        if let Some(embedded) = runtime.embedded() {
            embedded.list_sources(shown);
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
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return false;
        };
        // SAFETY: the caller's contract.
        unsafe { profile_id(profile) }.is_some_and(|profile| {
            runtime.embedded().is_some_and(|embedded| {
                embedded.source_policy(profile).ok() == Some(SourcePolicy::Listed)
            })
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
            if let Some(embedded) = runtime.embedded() {
                embedded.discovered(found);
            }
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
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            Answered::from(embedded.host_id(profile))
        })
    }
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
    // SAFETY: the caller's contract.
    let key: Option<Option<Vec<u8>>> = unsafe { parse(key) };
    // SAFETY: the caller's contract.
    unsafe {
        profile_act(runtime, profile, |embedded, profile| async move {
            let Some(key) = key else {
                return Answered::Err("the key is not a byte array".into());
            };
            Answered::from(embedded.want_notifications(profile, key))
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
        if let Some(runtime) = unsafe { live_runtime(runtime) }
            && let Some(embedded) = runtime.embedded()
        {
            embedded.set_telemetry(on);
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
        if let Some(runtime) = unsafe { live_runtime(runtime) }
            && let Some(embedded) = runtime.embedded()
        {
            embedded.set_auto_reports(on);
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
        if let (Some(event), Some(embedded)) = (event, runtime.embedded()) {
            embedded.record(profile, event);
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
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        let installation = runtime.shared.installation.clone();
        runtime.act(async move {
            Answered::from(
                installation
                    .get_settings()
                    .await
                    .map(|settings| MachineSettings::from(&settings)),
            )
        })
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
        let change: Option<SettingsChange> = unsafe { parse(change) };
        let installation = runtime.shared.installation.clone();
        runtime.act(async move {
            let Some(change) = change else {
                return Answered::Err("the change is not a SettingsChange".into());
            };
            Answered::from(
                installation
                    .update_settings(change.into())
                    .await
                    .map(|settings| MachineSettings::from(&settings)),
            )
        })
    })
}

/// The folders worth offering as repositories: the Git repositories Claude
/// Code and Codex ran in lately, newest first. The ticket answers
/// `{"Ok": [RepositorySuggestion]}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_suggest_repositories(runtime: *const AmuxRuntime) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        let installation = runtime.shared.installation.clone();
        runtime.act(async move {
            Answered::from(installation.suggest_repositories().await.map(|suggested| {
                suggested
                    .into_iter()
                    .map(|entry| RepositorySuggestion {
                        repository: ui_view::Repository::of_checkout(
                            entry.checkout.as_ref(),
                            &entry.path,
                        ),
                        path: entry.path,
                        name: entry.name,
                    })
                    .collect::<Vec<_>>()
            }))
        })
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
        let (profile, path) = unsafe { (profile_id(profile), text(path).map(str::to_owned)) };
        let installation = runtime.shared.installation.clone();
        runtime.act(async move {
            let Some(profile) = profile else {
                return Answered::Err("the profile is not a profile id".into());
            };
            Answered::from(
                installation
                    .web_sign_in_link(wire::WebSignInLinkRequest {
                        profile_id: profile.as_bytes().to_vec(),
                        path: path.unwrap_or_default(),
                    })
                    .await,
            )
        })
    })
}
