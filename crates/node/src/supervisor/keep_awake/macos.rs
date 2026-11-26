//! Keep-awake on macOS: IOKit assertions, named for `pmset -g assertions`.

use std::ffi::{CString, c_char, c_void};

use super::REASON;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        kind: *const c_void,
        level: u32,
        name: *const c_void,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

const UTF8: u32 = 0x0800_0100;
const LEVEL_ON: u32 = 255;
/// Honoured on AC power only: the strongest a user process can hold.
const SYSTEM: &str = "PreventSystemSleep";
/// Honoured on battery too.
const IDLE: &str = "PreventUserIdleSystemSleep";

pub enum Want {
    OnPower,
    Always,
}

pub struct Held(Option<u32>);

struct CfString(*const c_void);

impl CfString {
    fn new(text: &str) -> Self {
        let text = CString::new(text).expect("no interior NUL");
        // SAFETY: a valid C string and the default allocator.
        Self(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own the one reference Create returned.
            unsafe { CFRelease(self.0) };
        }
    }
}

pub fn hold(want: Want) -> Held {
    let name = CfString::new(REASON);
    let kind = match want {
        Want::OnPower => SYSTEM,
        Want::Always => IDLE,
    };
    let kind_string = CfString::new(kind);
    let mut id = 0;
    // SAFETY: two live CFStrings and an out parameter we own.
    let result = unsafe { IOPMAssertionCreateWithName(kind_string.0, LEVEL_ON, name.0, &mut id) };
    if result != 0 {
        tracing::warn!(
            assertion = kind,
            result,
            "could not take the sleep assertion"
        );
        return Held(None);
    }
    tracing::info!(assertion = kind, "holding the sleep assertion");
    Held(Some(id))
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            // SAFETY: an assertion this process created.
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}
