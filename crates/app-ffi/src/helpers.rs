//! Shared FFI pointer, C-string, JSON, panic-boundary and act helpers.

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use app_embedded::{EmbeddedRuntime, ProfileId, ProfileView};
use app_runtime::{AppRuntime, Chat};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{ATTACHED, AmuxChat, AmuxProfile, AmuxRuntime};

pub(crate) fn from_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

// --- strings and JSON ------------------------------------------------------

pub(crate) fn json<T: Serialize>(value: &T) -> Option<CString> {
    let text = serde_json::to_string(value).ok()?;
    CString::new(text).ok()
}

pub(crate) fn owned<T: Serialize>(value: &T) -> *mut c_char {
    json(value).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Reads a NUL-terminated UTF-8 string.
///
/// # Safety
/// `text` is null or points to a NUL-terminated string that lives for the
/// call.
pub(crate) unsafe fn text<'a>(text: *const c_char) -> Option<&'a str> {
    if text.is_null() {
        return None;
    }
    // SAFETY: the caller's contract above.
    unsafe { CStr::from_ptr(text) }.to_str().ok()
}

/// # Safety
/// As for [`text`].
pub(crate) unsafe fn parse<T: DeserializeOwned>(value: *const c_char) -> Option<T> {
    // SAFETY: the caller's contract.
    serde_json::from_str(unsafe { text(value) }?).ok()
}

/// Runs a call, turning a panic into the failure value rather than letting
/// it unwind into the host.
pub(crate) fn guard<T>(failed: T, call: impl FnOnce() -> T) -> T {
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
    pub(crate) static CAUGHT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// # Safety
/// `runtime` is null or a live pointer from `amux_runtime_start`.
pub(crate) unsafe fn live_runtime<'a>(runtime: *const AmuxRuntime) -> Option<&'a AmuxRuntime> {
    // SAFETY: the caller's contract.
    unsafe { runtime.as_ref() }
}

/// # Safety
/// `profile` is null or a live pointer from `amux_profile_open`.
pub(crate) unsafe fn live_profile<'a>(profile: *const AmuxProfile) -> Option<&'a AmuxProfile> {
    // SAFETY: the caller's contract.
    unsafe { profile.as_ref() }
}

/// A profile id.
///
/// # Safety
/// `profile` is null or a NUL-terminated string.
pub(crate) unsafe fn profile_id(profile: *const c_char) -> Option<ProfileId> {
    // SAFETY: the caller's contract.
    unsafe { text(profile) }?.parse().ok()
}

/// # Safety
/// `chat` is null or a live pointer from `amux_session_open`.
pub(crate) unsafe fn held_chat<'a>(chat: *const AmuxChat) -> Option<&'a AmuxChat> {
    // SAFETY: the caller's contract.
    unsafe { chat.as_ref() }
}

/// Runs a read on a chat inside its runtime, so a read that starts a fetch
/// can spawn it.
///
/// # Safety
/// As for [`held_chat`].
pub(crate) unsafe fn read<T: Serialize>(
    chat: *const AmuxChat,
    read: impl FnOnce(&Chat) -> T,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        let _entered = open.shared.handle().enter();
        owned(&read(&open.chat))
    })
}

/// Writes a failure's reason where the caller asked for it.
///
/// # Safety
/// `error` is null or writable.
pub(crate) unsafe fn fail(error: *mut *mut c_char, reason: String) {
    if !error.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *error = CString::new(reason).map_or(std::ptr::null_mut(), CString::into_raw) };
    }
}

#[derive(Serialize)]
pub(crate) enum Answered<T> {
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
pub(crate) unsafe fn profile_act<T, F, Fut>(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    act: F,
) -> u64
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
        match (unsafe { profile_id(profile) }, runtime.embedded()) {
            (Some(profile), Some(embedded)) => runtime.act(act(embedded.clone(), profile)),
            (None, _) => {
                runtime.act(async { Answered::<T>::Err("the profile is not a profile id".into()) })
            }
            (_, None) => runtime.act(async { Answered::<T>::Err(ATTACHED.into()) }),
        }
    })
}

/// As [`profile_act`], for an act on the installation's profiles, through
/// whichever installation the app has: the list it left is posted before
/// its answer.
///
/// # Safety
/// As for [`profile_act`].
pub(crate) unsafe fn profile_act_on_profiles<T, F, Fut>(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    act: F,
) -> u64
where
    T: Serialize + Send + 'static,
    F: FnOnce(Arc<dyn client::Installation>, ProfileId) -> Fut,
    Fut: Future<Output = Answered<T>> + Send + 'static,
{
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        match unsafe { profile_id(profile) } {
            Some(profile) => runtime
                .shared
                .act_on_profiles(act(runtime.shared.installation.clone(), profile)),
            None => {
                runtime.act(async { Answered::<T>::Err("the profile is not a profile id".into()) })
            }
        }
    })
}

/// A profile's description as the app holds it, from the installation's.
pub(crate) fn view(profile: Result<wire::Profile, client::RpcError>) -> Answered<ProfileView> {
    Answered::from(profile.map(|profile| ProfileView::from(&profile)))
}

// --- the profile registry ------------------------------------------------

/// # Safety
/// As for [`live_profile`].
pub(crate) unsafe fn fleet_read<T: Serialize>(
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

/// Changes what home lists, and names the fleet in the next step.
///
/// # Safety
/// As for [`live_profile`].
pub(crate) unsafe fn fleet_moved(profile: *const AmuxProfile, change: impl FnOnce(&AppRuntime)) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        change(&profile.app);
        profile.shared.outbox.redraw(profile.fleet, 0);
    })
}

/// Starts an act on the chat and answers its ticket, after the step that
/// hands over what it moved.
///
/// # Safety
/// `chat` is from `amux_session_open`.
pub(crate) unsafe fn act<T, F, Fut>(chat: *const AmuxChat, act: F) -> u64
where
    T: Serialize + Send + 'static,
    F: FnOnce(Arc<Chat>) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return 0;
        };
        let acting = act(open.chat.clone());
        let outbox = open.shared.outbox.clone();
        let (fleet, id) = (open.fleet, open.chat.id());
        open.shared.act(async move {
            let answer = acting.await;
            outbox.redraw(fleet, id);
            answer
        })
    })
}

pub(crate) fn answer(value: impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// An input id as a JSON byte array.
///
/// # Safety
/// `id` is null or a NUL-terminated string.
pub(crate) unsafe fn input_id(id: *const c_char) -> Vec<u8> {
    // SAFETY: the caller's contract.
    unsafe { parse(id) }.unwrap_or_default()
}
