//! The JNI facade the Android app calls.
//!
//! A thin Rust JNI layer over [`app_bridge`] for Kotlin class
//! `sh.amux.core.runtime.NativeBridge`. Handles are stored in synchronized
//! tables keyed by monotonic 64-bit ids so closed or invalid handles never
//! dereference freed memory. Every entrypoint catches Rust panics and
//! translates recoverable failures into `sh.amux.core.runtime.BridgeException`.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use app_bridge::{AmuxChat, AmuxProfile, AmuxRuntime};
use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jbyteArray, jint, jlong, jstring};

const BRIDGE_EXCEPTION: &str = "sh/amux/core/runtime/BridgeException";

// --- handle tables & wake pipe ---------------------------------------------

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn next_handle() -> i64 {
    NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
}

#[cfg(unix)]
struct ShutdownPipe {
    read_fd: i32,
    write_fd: i32,
}

#[cfg(unix)]
impl ShutdownPipe {
    fn new() -> Result<Self, String> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element array for pipe(2).
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "failed to create wake shutdown pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        for fd in fds {
            // SAFETY: `fd` was just returned by pipe(2).
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
                let status = libc::fcntl(fd, libc::F_GETFL);
                if status >= 0 {
                    libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK);
                }
            }
        }
        Ok(Self {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }

    fn signal(&self) {
        let byte = 1u8;
        // SAFETY: `write_fd` is an open non-blocking pipe write end owned by `self`.
        unsafe {
            libc::write(self.write_fd, (&raw const byte).cast(), 1);
        }
    }

    fn wait_wake(&self, wake_fd: i32, timeout_ms: i32) -> i32 {
        if wake_fd < 0 {
            return -1;
        }
        let mut fds = [
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` points to 2 valid `pollfd` entries.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            return if err.raw_os_error() == Some(libc::EINTR) {
                0
            } else {
                -1
            };
        }
        if rc == 0 {
            return 0;
        }
        if fds[1].revents != 0 {
            return -1;
        }
        if (fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0 {
            1
        } else {
            0
        }
    }
}

#[cfg(unix)]
impl Drop for ShutdownPipe {
    fn drop(&mut self) {
        // SAFETY: both descriptors belong exclusively to `self`.
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

struct RuntimeSlot {
    inner: RwLock<Option<Box<AmuxRuntime>>>,
    /// Open profile handles owned by this runtime; `None` once stopping begins.
    profiles: Mutex<Option<HashSet<i64>>>,
    #[cfg(unix)]
    shutdown: ShutdownPipe,
}

struct ProfileSlot {
    runtime_handle: i64,
    inner: RwLock<Option<Box<AmuxProfile>>>,
    /// Open chat handles owned by this profile; `None` once closing begins.
    chats: Mutex<Option<HashSet<i64>>>,
}

struct ChatSlot {
    profile_handle: i64,
    inner: RwLock<Option<Box<AmuxChat>>>,
}

type HandleMap<T> = Mutex<HashMap<i64, Arc<T>>>;

fn runtimes() -> &'static HandleMap<RuntimeSlot> {
    static TABLE: OnceLock<HandleMap<RuntimeSlot>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn profiles() -> &'static HandleMap<ProfileSlot> {
    static TABLE: OnceLock<HandleMap<ProfileSlot>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn chats() -> &'static HandleMap<ChatSlot> {
    static TABLE: OnceLock<HandleMap<ChatSlot>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn insert_runtime(runtime: Box<AmuxRuntime>, #[cfg(unix)] shutdown: ShutdownPipe) -> i64 {
    let id = next_handle();
    let slot = Arc::new(RuntimeSlot {
        inner: RwLock::new(Some(runtime)),
        profiles: Mutex::new(Some(HashSet::new())),
        #[cfg(unix)]
        shutdown,
    });
    runtimes()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, slot);
    id
}

fn start_runtime_from_json(config: &str) -> Result<i64, String> {
    #[cfg(unix)]
    let shutdown = ShutdownPipe::new()?;
    let runtime = app_bridge::start_from_json(Some(config))?;
    Ok(insert_runtime(
        runtime,
        #[cfg(unix)]
        shutdown,
    ))
}

fn stop_runtime_handle(handle: i64) {
    let slot = runtimes()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&handle);
    if let Some(slot) = slot {
        #[cfg(unix)]
        slot.shutdown.signal();
        let child_profiles = slot
            .profiles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(child_profiles) = child_profiles {
            for profile_handle in child_profiles {
                close_profile_handle(profile_handle);
            }
        }
        let runtime = slot.inner.write().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(runtime) = runtime {
            runtime.stop();
        }
    }
}

fn get_runtime(handle: i64) -> Option<Arc<RuntimeSlot>> {
    runtimes()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&handle)
        .cloned()
}

fn with_runtime<T>(handle: i64, f: impl FnOnce(&AmuxRuntime) -> T) -> Option<T> {
    let slot = get_runtime(handle)?;
    let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
    guard.as_deref().map(f)
}

fn get_profile(handle: i64) -> Option<Arc<ProfileSlot>> {
    profiles()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&handle)
        .cloned()
}

fn open_profile_handle(runtime_handle: i64, profile_id: &str) -> Result<i64, String> {
    let slot = get_runtime(runtime_handle).ok_or_else(|| "the runtime has stopped".to_owned())?;
    let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
    let Some(runtime) = guard.as_deref() else {
        return Err("the runtime has stopped".to_owned());
    };
    if slot
        .profiles
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_none()
    {
        return Err("the runtime has stopped".to_owned());
    }
    let opened = runtime.open_profile(Some(profile_id))?;
    let mut children = slot.profiles.lock().unwrap_or_else(|p| p.into_inner());
    let Some(children) = children.as_mut() else {
        drop(children);
        opened.close();
        return Err("the runtime has stopped".to_owned());
    };
    let id = next_handle();
    profiles().lock().unwrap_or_else(|p| p.into_inner()).insert(
        id,
        Arc::new(ProfileSlot {
            runtime_handle,
            inner: RwLock::new(Some(opened)),
            chats: Mutex::new(Some(HashSet::new())),
        }),
    );
    children.insert(id);
    Ok(id)
}

fn close_profile_handle(handle: i64) {
    let Some(slot) = get_profile(handle) else {
        return;
    };
    let child_chats = slot.chats.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(child_chats) = child_chats {
        for chat_handle in child_chats {
            close_chat_handle(chat_handle);
        }
    }
    {
        let mut guard = slot.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(profile) = guard.take() {
            profile.close();
        }
    }
    profiles()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&handle);
    if let Some(runtime_slot) = get_runtime(slot.runtime_handle)
        && let Some(children) = runtime_slot
            .profiles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
    {
        children.remove(&handle);
    }
}

fn with_profile<T>(handle: i64, f: impl FnOnce(&AmuxProfile) -> T) -> Option<T> {
    let slot = get_profile(handle)?;
    let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
    guard.as_deref().map(f)
}

fn open_chat_handle(profile_handle: i64, agent_json: &str, tail: u32) -> Result<i64, String> {
    let slot = get_profile(profile_handle).ok_or_else(|| "the profile is closed".to_owned())?;
    let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
    let Some(profile) = guard.as_deref() else {
        return Err("the profile is closed".to_owned());
    };
    if slot
        .chats
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_none()
    {
        return Err("the profile is closed".to_owned());
    }
    let opened = profile.open_chat(Some(agent_json), tail)?;
    let mut children = slot.chats.lock().unwrap_or_else(|p| p.into_inner());
    let Some(children) = children.as_mut() else {
        drop(children);
        opened.close();
        return Err("the profile is closed".to_owned());
    };
    let id = next_handle();
    chats().lock().unwrap_or_else(|p| p.into_inner()).insert(
        id,
        Arc::new(ChatSlot {
            profile_handle,
            inner: RwLock::new(Some(opened)),
        }),
    );
    children.insert(id);
    Ok(id)
}

fn close_chat_handle(handle: i64) {
    let slot = chats()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&handle)
        .cloned();
    let Some(slot) = slot else {
        return;
    };
    {
        let mut guard = slot.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(chat) = guard.take() {
            chat.close();
        }
    }
    chats()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&handle);
    if let Some(profile_slot) = get_profile(slot.profile_handle)
        && let Some(children) = profile_slot
            .chats
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
    {
        children.remove(&handle);
    }
}

fn with_chat<T>(handle: i64, f: impl FnOnce(&AmuxChat) -> T) -> Option<T> {
    let slot = chats()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&handle)
        .cloned()?;
    let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
    guard.as_deref().map(f)
}

// --- JNI helpers -----------------------------------------------------------

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

fn throw_bridge_error(env: &mut JNIEnv, message: &str) {
    if env.exception_check().unwrap_or(false) {
        return;
    }
    let _ = env.throw_new(BRIDGE_EXCEPTION, message);
}

fn guard_jni<T>(
    env: &mut JNIEnv,
    fallback: T,
    call: impl FnOnce(&mut JNIEnv) -> Result<T, String>,
) -> T {
    match catch_unwind(AssertUnwindSafe(|| call(env))) {
        Ok(Ok(value)) => value,
        Ok(Err(message)) => {
            throw_bridge_error(env, &message);
            fallback
        }
        Err(payload) => {
            let message = panic_message(payload);
            throw_bridge_error(env, &format!("native bridge panicked: {message}"));
            fallback
        }
    }
}

fn opt_string(env: &mut JNIEnv, value: JString) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    env.get_string(&value)
        .map(|text| Some(String::from(text)))
        .map_err(|error| format!("failed to read JNI string: {error}"))
}

fn req_string(env: &mut JNIEnv, value: JString, name: &str) -> Result<String, String> {
    opt_string(env, value)?.ok_or_else(|| format!("{name} must not be null"))
}

fn into_jstring(env: &mut JNIEnv, value: Option<String>) -> Result<jstring, String> {
    let Some(text) = value else {
        return Ok(std::ptr::null_mut());
    };
    env.new_string(text)
        .map(JString::into_raw)
        .map_err(|error| format!("failed to allocate JNI string: {error}"))
}

fn opt_bytes(env: &JNIEnv, value: JByteArray) -> Result<Vec<u8>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    env.convert_byte_array(&value)
        .map_err(|error| format!("failed to read JNI byte array: {error}"))
}

fn into_jbyte_array(env: &mut JNIEnv, value: Option<Arc<[u8]>>) -> Result<jbyteArray, String> {
    let Some(bytes) = value else {
        return Ok(std::ptr::null_mut());
    };
    env.byte_array_from_slice(&bytes)
        .map(JByteArray::into_raw)
        .map_err(|error| format!("failed to allocate JNI byte array: {error}"))
}

fn to_jbool(value: bool) -> jboolean {
    if value { JNI_TRUE } else { JNI_FALSE }
}

// --- stateless helpers -----------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_version(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        into_jstring(env, Some(app_bridge::version()))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_agentNameProblem(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let name = opt_string(env, name)?.unwrap_or_default();
        into_jstring(env, app_bridge::agent_name_problem(&name))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_newAgentSettings(
    mut env: JNIEnv,
    _class: JClass,
    catalogue_json: JString,
    chosen_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let catalogue = opt_string(env, catalogue_json)?;
        let chosen = opt_string(env, chosen_json)?;
        into_jstring(
            env,
            app_bridge::new_agent_settings(catalogue.as_deref(), chosen.as_deref()),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_newAgentPick(
    mut env: JNIEnv,
    _class: JClass,
    catalogue_json: JString,
    chosen_json: JString,
    pick_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let catalogue = opt_string(env, catalogue_json)?;
        let chosen = opt_string(env, chosen_json)?;
        let pick = opt_string(env, pick_json)?;
        into_jstring(
            env,
            app_bridge::new_agent_pick(catalogue.as_deref(), chosen.as_deref(), pick.as_deref()),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_onboardingNext(
    mut env: JNIEnv,
    _class: JClass,
    onboarding_json: JString,
    after_json: JString,
    finishing: jboolean,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let onboarding = opt_string(env, onboarding_json)?;
        let after = opt_string(env, after_json)?;
        into_jstring(
            env,
            app_bridge::onboarding_next(onboarding.as_deref(), after.as_deref(), finishing != 0),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_formProblems(
    mut env: JNIEnv,
    _class: JClass,
    fields_json: JString,
    values_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let fields = opt_string(env, fields_json)?;
        let values = opt_string(env, values_json)?;
        into_jstring(
            env,
            app_bridge::form_problems(fields.as_deref(), values.as_deref()),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_commandMatches(
    mut env: JNIEnv,
    _class: JClass,
    commands_json: JString,
    draft: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let commands = opt_string(env, commands_json)?;
        let draft = opt_string(env, draft)?.unwrap_or_default();
        into_jstring(
            env,
            app_bridge::command_matches(commands.as_deref(), &draft),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_pastesAsAttachment(
    mut env: JNIEnv,
    _class: JClass,
    pasted: JString,
) -> jboolean {
    guard_jni(&mut env, JNI_FALSE, |env| {
        let pasted = opt_string(env, pasted)?.unwrap_or_default();
        Ok(to_jbool(app_bridge::pastes_as_attachment(&pasted)))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_attachmentLimit(
    mut env: JNIEnv,
    _class: JClass,
) -> jlong {
    guard_jni(&mut env, 0, |_| Ok(app_bridge::attachment_limit() as jlong))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_reviewDoc(
    mut env: JNIEnv,
    _class: JClass,
    review_json: JString,
    comments_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let review = opt_string(env, review_json)?;
        let comments = opt_string(env, comments_json)?;
        into_jstring(
            env,
            app_bridge::review_doc(review.as_deref(), comments.as_deref()),
        )
    })
}

// --- runtime lifecycle & stepping ------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeStart(
    mut env: JNIEnv,
    _class: JClass,
    config_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let config = req_string(env, config_json, "configJson")?;
        start_runtime_from_json(&config)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeStop(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    guard_jni(&mut env, (), |_| {
        stop_runtime_handle(handle);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeWakeFd(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    guard_jni(&mut env, -1, |_| {
        Ok(with_runtime(handle, AmuxRuntime::wake_fd).unwrap_or(-1))
    })
}

/// Waits for the runtime's wake descriptor to become readable or until
/// `timeout_ms` elapses (-1 for indefinite). Returns:
/// - `1` when `wake_fd` is readable (caller should run `runtimeStep`),
/// - `0` on timeout or signal interruption,
/// - `-1` when the runtime is stopped or wake was interrupted for shutdown.
#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeWaitWake(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    timeout_ms: jint,
) -> jint {
    guard_jni(&mut env, -1, |_| {
        let Some(slot) = get_runtime(handle) else {
            return Ok(-1);
        };
        let guard = slot.inner.read().unwrap_or_else(|p| p.into_inner());
        let Some(runtime) = guard.as_deref() else {
            return Ok(-1);
        };
        #[cfg(unix)]
        {
            Ok(slot.shutdown.wait_wake(runtime.wake_fd(), timeout_ms))
        }
        #[cfg(not(unix))]
        {
            let _ = (runtime, timeout_ms);
            Ok(-1)
        }
    })
}

/// Wakes any thread blocked in `runtimeWaitWake` for shutdown.
#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeInterruptWake(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    guard_jni(&mut env, (), |_| {
        #[cfg(unix)]
        if let Some(slot) = get_runtime(handle) {
            slot.shutdown.signal();
        }
        #[cfg(not(unix))]
        let _ = handle;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeStep(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let stepped = with_runtime(handle, AmuxRuntime::step).flatten();
        into_jstring(env, stepped)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeFeed(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    lifecycle_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let input = req_string(env, lifecycle_json, "lifecycleJson")?;
        with_runtime(handle, |runtime| runtime.feed_json(Some(&input)))
            .ok_or_else(|| "the runtime has stopped".to_owned())?
    });
}

// --- profile registry & installation acts ----------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeCreateProfile(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_runtime(handle, AmuxRuntime::create_profile).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeDeleteProfile(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(
            with_runtime(handle, |runtime| runtime.delete_profile(profile.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeBind(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
    cloud_url: JString,
    client_id: JString,
    refresh_token: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        let cloud_url = opt_string(env, cloud_url)?.unwrap_or_default();
        let client_id = opt_string(env, client_id)?.unwrap_or_default();
        let refresh_token = opt_string(env, refresh_token)?.unwrap_or_default();
        Ok(with_runtime(handle, |runtime| {
            runtime.bind(profile.as_deref(), &cloud_url, &client_id, &refresh_token)
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSignOut(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(
            with_runtime(handle, |runtime| runtime.sign_out(profile.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeTrustingProfiles(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        Ok(with_runtime(handle, |runtime| {
            runtime.trusting_profiles(host_id.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeOfferPairing(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(
            with_runtime(handle, |runtime| runtime.offer_pairing(profile.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeListSources(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) {
    guard_jni(&mut env, (), |env| {
        let profile = opt_string(env, profile_id)?;
        let _ = with_runtime(handle, |runtime| runtime.list_sources(profile.as_deref()));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSourcePolicyListed(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jboolean {
    guard_jni(&mut env, JNI_FALSE, |env| {
        let profile = opt_string(env, profile_id)?;
        let listed = with_runtime(handle, |runtime| {
            runtime.source_policy_listed(profile.as_deref())
        })
        .unwrap_or(false);
        Ok(to_jbool(listed))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeDiscovered(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    found_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let found = opt_string(env, found_json)?;
        let _ = with_runtime(handle, |runtime| runtime.discovered(found.as_deref()));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeAccessToken(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(
            with_runtime(handle, |runtime| runtime.access_token(profile.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeHostId(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(
            with_runtime(handle, |runtime| runtime.host_id(profile.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeWantNotifications(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
    key_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        let key = opt_string(env, key_json)?;
        Ok(with_runtime(handle, |runtime| {
            runtime.want_notifications(profile.as_deref(), key.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeRefreshEntitlement(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        Ok(with_runtime(handle, |runtime| {
            runtime.refresh_entitlement(profile.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSetTelemetry(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    on: jboolean,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_runtime(handle, |runtime| runtime.set_telemetry(on != 0));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSetAutoReports(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    on: jboolean,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_runtime(handle, |runtime| runtime.set_auto_reports(on != 0));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeRecord(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
    event_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let profile = opt_string(env, profile_id)?;
        let event = opt_string(env, event_json)?;
        let _ = with_runtime(handle, |runtime| {
            runtime.record(profile.as_deref(), event.as_deref())
        });
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSettings(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_runtime(handle, AmuxRuntime::settings).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeUpdateSettings(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    change_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let change = opt_string(env, change_json)?;
        Ok(
            with_runtime(handle, |runtime| runtime.update_settings(change.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeSuggestRepositories(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_runtime(handle, AmuxRuntime::suggest_repositories).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_runtimeWebSignInLink(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    profile_id: JString,
    path: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = opt_string(env, profile_id)?;
        let path = opt_string(env, path)?;
        Ok(with_runtime(handle, |runtime| {
            runtime.web_sign_in_link(profile.as_deref(), path.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

// --- profiles & fleet ------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileOpen(
    mut env: JNIEnv,
    _class: JClass,
    runtime_handle: jlong,
    profile_id: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let profile = req_string(env, profile_id, "profileId")?;
        open_profile_handle(runtime_handle, &profile)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileClose(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    guard_jni(&mut env, (), |_| {
        close_profile_handle(handle);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileFleetId(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_profile(handle, AmuxProfile::fleet_id).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileDump(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    reason: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let reason = opt_string(env, reason)?.unwrap_or_default();
        Ok(with_profile(handle, |profile| profile.dump(&reason)).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileSubmitReport(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    dump_path: JString,
    header: JByteArray,
    frame: JByteArray,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let dump_path = opt_string(env, dump_path)?.unwrap_or_default();
        let header = opt_bytes(env, header)?;
        let frame = opt_bytes(env, frame)?;
        Ok(with_profile(handle, |profile| {
            profile.submit_report(&dump_path, header, frame)
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileBeginPair(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    request_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let request = opt_string(env, request_json)?;
        Ok(
            with_profile(handle, |profile| profile.begin_pair(request.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileOfferSignIn(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        Ok(
            with_profile(handle, |profile| profile.offer_sign_in(host_id.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileAnswerSignInOffer(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    answer_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let answer = opt_string(env, answer_json)?;
        Ok(with_profile(handle, |profile| {
            profile.answer_sign_in_offer(answer.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileWatchOnboarding(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_profile(handle, AmuxProfile::watch_onboarding);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileAnswerOnboarding(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    answer_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let answer = opt_string(env, answer_json)?;
        Ok(with_profile(handle, |profile| {
            profile.answer_onboarding(answer.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileOpenPairing(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    ttl_seconds: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        let ttl = u64::try_from(ttl_seconds).unwrap_or(0);
        Ok(with_profile(handle, |profile| profile.open_pairing(ttl)).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileClosePairing(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_profile(handle, AmuxProfile::close_pairing).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileConfirmPair(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    token_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let token = opt_string(env, token_json)?;
        Ok(
            with_profile(handle, |profile| profile.confirm_pair(token.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileAbandonPair(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    token_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let token = opt_string(env, token_json)?;
        Ok(
            with_profile(handle, |profile| profile.abandon_pair(token.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileUnpair(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        Ok(
            with_profile(handle, |profile| profile.unpair(host_id.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileRoster(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_profile(handle, AmuxProfile::roster).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileCreateAgent(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    agent_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let agent = opt_string(env, agent_json)?;
        Ok(
            with_profile(handle, |profile| profile.create_agent(agent.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileDirectories(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
    query: JString,
    limit: jint,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        let query = opt_string(env, query)?.unwrap_or_default();
        let limit = u32::try_from(limit).unwrap_or(0);
        Ok(with_profile(handle, |profile| {
            profile.directories(host_id.as_deref(), &query, limit)
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profilePinFolder(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
    path: JString,
    pinned: jboolean,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        let path = opt_string(env, path)?.unwrap_or_default();
        Ok(with_profile(handle, |profile| {
            profile.pin_folder(host_id.as_deref(), &path, pinned != 0)
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileAgentAct(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    agent_json: JString,
    act_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let agent = opt_string(env, agent_json)?;
        let act = opt_string(env, act_json)?;
        Ok(with_profile(handle, |profile| {
            profile.agent_act(agent.as_deref(), act.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileDeleteWarning(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    agent_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let agent = opt_string(env, agent_json)?;
        Ok(
            with_profile(handle, |profile| profile.delete_warning(agent.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileHostCatalogue(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    host_id_json: JString,
    provider_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let host_id = opt_string(env, host_id_json)?;
        let provider = opt_string(env, provider_json)?;
        Ok(with_profile(handle, |profile| {
            profile.host_catalogue(host_id.as_deref(), provider.as_deref())
        })
        .unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_profileSetForeground(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    foreground: jboolean,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_profile(handle, |profile| profile.set_foreground(foreground != 0));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_fleetOpening(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let opening = with_profile(handle, AmuxProfile::fleet_opening).flatten();
        into_jstring(env, opening)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_fleetToggleFamily(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    root_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let root = opt_string(env, root_json)?;
        let _ = with_profile(handle, |profile| {
            profile.fleet_toggle_family(root.as_deref())
        });
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_fleetFilter(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    filter_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let filter = opt_string(env, filter_json)?;
        let _ = with_profile(handle, |profile| profile.fleet_filter(filter.as_deref()));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_fleetCard(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    agent_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let agent = opt_string(env, agent_json)?;
        let card = with_profile(handle, |profile| profile.fleet_card(agent.as_deref())).flatten();
        into_jstring(env, card)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_fleetFamily(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    agent_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let agent = opt_string(env, agent_json)?;
        let family =
            with_profile(handle, |profile| profile.fleet_family(agent.as_deref())).flatten();
        into_jstring(env, family)
    })
}

// --- sessions / chats ------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionOpen(
    mut env: JNIEnv,
    _class: JClass,
    profile_handle: jlong,
    agent_json: JString,
    tail: jint,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let agent = req_string(env, agent_json, "agentJson")?;
        let tail = u32::try_from(tail).unwrap_or(0);
        open_chat_handle(profile_handle, &agent, tail)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionId(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    guard_jni(&mut env, 0, |_| {
        Ok(with_chat(handle, AmuxChat::id).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionClose(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    guard_jni(&mut env, (), |_| {
        close_chat_handle(handle);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionOpening(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let opening = with_chat(handle, AmuxChat::opening).flatten();
        into_jstring(env, opening)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionFollow(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    following: jboolean,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_chat(handle, |chat| chat.follow(following != 0));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionKeptDraft(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let draft = with_chat(handle, AmuxChat::kept_draft).flatten();
        into_jstring(env, draft)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionKeepDraft(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    draft_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let draft = opt_string(env, draft_json)?;
        let _ = with_chat(handle, |chat| chat.keep_draft(draft.as_deref()));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionRowsFor(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    keys_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let keys = opt_string(env, keys_json)?;
        let rows = with_chat(handle, |chat| chat.rows_for(keys.as_deref())).flatten();
        into_jstring(env, rows)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionToggleRun(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    member: JString,
) {
    guard_jni(&mut env, (), |env| {
        let member = opt_string(env, member)?.unwrap_or_default();
        let _ = with_chat(handle, |chat| chat.toggle_run(&member));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionToggleGroup(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    member: JString,
) {
    guard_jni(&mut env, (), |env| {
        let member = opt_string(env, member)?.unwrap_or_default();
        let _ = with_chat(handle, |chat| chat.toggle_group(&member));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionFoldTurns(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    on: jboolean,
) {
    guard_jni(&mut env, (), |_| {
        let _ = with_chat(handle, |chat| chat.fold_turns(on != 0));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionToggleTurn(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    end: JString,
) {
    guard_jni(&mut env, (), |env| {
        let end = opt_string(env, end)?.unwrap_or_default();
        let _ = with_chat(handle, |chat| chat.toggle_turn(&end));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionAct(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    act_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let act = opt_string(env, act_json)?;
        Ok(with_chat(handle, |chat| chat.act(act.as_deref())).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionDraftOf(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    input_id_json: JString,
) -> jstring {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let input_id = opt_string(env, input_id_json)?;
        let draft = with_chat(handle, |chat| chat.draft_of(input_id.as_deref())).flatten();
        into_jstring(env, draft)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionDiscard(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    input_id_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let input_id = opt_string(env, input_id_json)?;
        let _ = with_chat(handle, |chat| chat.discard(input_id.as_deref()));
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionOpenOverview(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    comparison_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let comparison = opt_string(env, comparison_json)?;
        Ok(
            with_chat(handle, |chat| chat.open_overview(comparison.as_deref())).unwrap_or(0)
                as jlong,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionReview(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    comparison_json: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let comparison = opt_string(env, comparison_json)?;
        Ok(with_chat(handle, |chat| chat.review(comparison.as_deref())).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionPutBlob(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    bytes: JByteArray,
    name: JString,
    mime: JString,
) -> jlong {
    guard_jni(&mut env, 0, |env| {
        let bytes = opt_bytes(env, bytes)?;
        let name = opt_string(env, name)?.unwrap_or_default();
        let mime = opt_string(env, mime)?.unwrap_or_default();
        Ok(with_chat(handle, |chat| chat.put_blob(bytes, &name, &mime)).unwrap_or(0) as jlong)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionBlob(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    hash_json: JString,
) -> jbyteArray {
    guard_jni(&mut env, std::ptr::null_mut(), |env| {
        let hash = opt_string(env, hash_json)?;
        let bytes = with_chat(handle, |chat| chat.blob(hash.as_deref())).flatten();
        into_jbyte_array(env, bytes)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionBlobFailed(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    hash_json: JString,
) -> jboolean {
    guard_jni(&mut env, JNI_FALSE, |env| {
        let hash = opt_string(env, hash_json)?;
        let failed = with_chat(handle, |chat| chat.blob_failed(hash.as_deref())).unwrap_or(false);
        Ok(to_jbool(failed))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_sh_amux_core_runtime_NativeBridge_sessionFetchBlob(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    hash_json: JString,
) {
    guard_jni(&mut env, (), |env| {
        let hash = opt_string(env, hash_json)?;
        let _ = with_chat(handle, |chat| chat.fetch_blob(hash.as_deref()));
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "amux-{prefix}-{}-{}",
                std::process::id(),
                next_handle()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn first_profile_id(stepped_json: &str) -> String {
        let needle = "\"id\":\"";
        let start = stepped_json
            .find(needle)
            .expect("stepped JSON should include a profile id")
            + needle.len();
        let rest = &stepped_json[start..];
        let end = rest.find('"').expect("closing quote for profile id");
        rest[..end].to_owned()
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_pipe_wakes_poll_and_handles_timeout() {
        let wake = ShutdownPipe::new().expect("wake pipe");
        let shutdown = ShutdownPipe::new().expect("shutdown pipe");

        // Neither descriptor is readable -> immediate timeout (0).
        assert_eq!(shutdown.wait_wake(wake.read_fd, 0), 0);

        // Signaling wake makes wait_wake return 1.
        wake.signal();
        assert_eq!(shutdown.wait_wake(wake.read_fd, 100), 1);

        // Signaling shutdown takes precedence and returns -1.
        shutdown.signal();
        assert_eq!(shutdown.wait_wake(wake.read_fd, 100), -1);

        // Invalid wake descriptor returns -1.
        assert_eq!(shutdown.wait_wake(-1, 0), -1);
    }

    #[test]
    fn handle_lookups_return_none_for_unknown_ids() {
        assert!(with_runtime(0, AmuxRuntime::wake_fd).is_none());
        assert!(with_profile(0, AmuxProfile::fleet_id).is_none());
        assert!(with_chat(0, AmuxChat::id).is_none());
    }

    #[test]
    fn runtime_stop_closes_unclosed_child_profiles_and_guards_closed_parents() {
        let dir = TempDir::new("app-jni-teardown");
        let data_dir = dir.0.to_str().expect("utf-8 temp dir");
        let escaped_dir = data_dir.replace('\\', "\\\\").replace('"', "\\\"");
        let config = format!(
            r#"{{"data_dir":"{escaped_dir}","device_name":"JNI Test","lan":false,"telemetry":false,"auto_reports":false}}"#
        );

        let runtime_handle = start_runtime_from_json(&config).expect("start runtime");
        let stepped = with_runtime(runtime_handle, AmuxRuntime::step)
            .flatten()
            .expect("initial step");
        let profile_id = first_profile_id(&stepped);

        let profile_handle =
            open_profile_handle(runtime_handle, &profile_id).expect("open profile");
        assert!(with_profile(profile_handle, AmuxProfile::fleet_id).is_some());

        // Verify the parent runtime tracks the open child profile.
        {
            let rt_slot = get_runtime(runtime_handle).expect("runtime slot");
            let children = rt_slot.profiles.lock().unwrap_or_else(|p| p.into_inner());
            assert!(
                children
                    .as_ref()
                    .is_some_and(|set| set.contains(&profile_handle)),
                "runtime should track open profile handle"
            );
        }

        // Stop runtime WITHOUT closing the profile first: child must be closed
        // and removed before `AmuxRuntime::stop` flushes the installation.
        stop_runtime_handle(runtime_handle);

        assert!(with_runtime(runtime_handle, AmuxRuntime::wake_fd).is_none());
        assert!(with_profile(profile_handle, AmuxProfile::fleet_id).is_none());
        assert!(open_profile_handle(runtime_handle, &profile_id).is_err());
        assert!(open_chat_handle(profile_handle, "{}", 50).is_err());

        // Verify explicit profileClose unlinks from a live parent runtime too.
        let second_runtime = start_runtime_from_json(&config).expect("restart runtime");
        let second_profile =
            open_profile_handle(second_runtime, &profile_id).expect("reopen profile");
        close_profile_handle(second_profile);
        assert!(with_profile(second_profile, AmuxProfile::fleet_id).is_none());
        assert!(open_chat_handle(second_profile, "{}", 50).is_err());
        {
            let rt_slot = get_runtime(second_runtime).expect("second runtime slot");
            let children = rt_slot.profiles.lock().unwrap_or_else(|p| p.into_inner());
            assert!(
                children
                    .as_ref()
                    .is_some_and(|set| !set.contains(&second_profile)),
                "explicit profile close should unlink from parent runtime"
            );
        }
        stop_runtime_handle(second_runtime);
    }
}
