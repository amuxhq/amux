//! Fleet C ABI calls: opening, family toggles, filters, cards and headers.

use std::ffi::c_char;

use app_runtime::AppRuntime;
use ui_state::AgentKey;

use crate::AmuxProfile;
use crate::helpers::{fleet_moved, fleet_read, parse};

/// The fleet whole as a `FleetDelta`: home's sections, families listed
/// under the roots `amux_fleet_toggle_family` opened, narrowed by the last
/// `amux_fleet_filter`, and every host, this device first. Steps after it
/// hand over what moved since.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_opening(profile: *const AmuxProfile) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { fleet_read(profile, AppRuntime::fleet_opening) }
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
    // SAFETY: the caller's contract.
    let Some(root) = (unsafe { parse::<Vec<u8>>(root) }) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { fleet_moved(profile, |app| app.toggle_family(&root)) };
}

/// Narrows home's list to what a `FleetFilter`, as JSON, keeps; null keeps
/// every agent. The next step hands over the list as it then stands.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `filter` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_filter(profile: *const AmuxProfile, filter: *const c_char) {
    // SAFETY: the caller's contract.
    let filter: ui_view::FleetFilter = unsafe { parse(filter) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { fleet_moved(profile, |app| app.set_filter(filter)) };
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

// --- a chat ----------------------------------------------------------------
