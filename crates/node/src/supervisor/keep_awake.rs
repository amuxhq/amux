//! The machine's sleep assertions under `keep_awake`: on power the machine
//! stays awake whenever amux runs, so a phone can start an agent on it
//! while nobody is at it; on battery only while an agent is working, so a
//! laptop in a bag is not drained by an idle amux.
//!
//! The two halves have two holders. The supervisor holds the one for power
//! for its lifetime, so it survives daemon restarts and the update gap; the
//! daemon holds the one that battery honours while one of its agents works,
//! since only it knows that. Each reads `keep_awake` as it changes.
//!
//! On macOS the supervisor's is the IOKit assertion PreventSystemSleep,
//! which the system honours on AC power only, and the daemon's is
//! PreventUserIdleSystemSleep, which it honours on battery too. Together
//! they keep a machine awake when it is plugged in with the lid open;
//! whether a closed lid is honoured on AC is unmeasured. On Windows both are
//! SetThreadExecutionState on a thread kept for the purpose, the
//! supervisor's set only while the machine reports AC power. Linux holds
//! none: a service-managed server does not sleep, and desktops differ in how
//! they would be asked.

/// What `pmset -g assertions` shows as the assertion's name.
pub const REASON: &str = "amux keeps this machine reachable";

/// Held for as long as it lives.
pub struct KeepAwake {
    #[allow(dead_code)] // Released on drop.
    held: imp::Held,
}

impl KeepAwake {
    /// The supervisor's: the machine stays awake while it is on power.
    /// A failure is logged and leaves the machine free to sleep, which is
    /// no worse than keep_awake off.
    pub fn on_power() -> Self {
        Self {
            held: imp::hold(imp::Want::OnPower),
        }
    }

    /// The daemon's while an agent works: the machine stays awake on
    /// battery too.
    pub fn while_working() -> Self {
        Self {
            held: imp::hold(imp::Want::Always),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
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
        let result =
            unsafe { IOPMAssertionCreateWithName(kind_string.0, LEVEL_ON, name.0, &mut id) };
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
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;
    use std::time::Duration;

    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, GetSystemPowerStatus, SYSTEM_POWER_STATUS,
        SetThreadExecutionState,
    };

    /// How often the power-only hold looks at whether the machine is on AC.
    const POWER_POLL: Duration = Duration::from_secs(30);

    pub enum Want {
        OnPower,
        Always,
    }

    /// The state belongs to the thread that set it, so one thread holds it
    /// and clears it when the sender drops.
    pub struct Held(#[allow(dead_code)] mpsc::Sender<()>);

    fn on_ac() -> bool {
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: a plain call filling a struct we own.
        unsafe { GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 1 }
    }

    pub fn hold(want: Want) -> Held {
        let (sender, receiver) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut held = false;
            loop {
                let wanted = match want {
                    Want::Always => true,
                    Want::OnPower => on_ac(),
                };
                if wanted != held {
                    let state = if wanted {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    // SAFETY: plain calls on this thread's own execution state.
                    if unsafe { SetThreadExecutionState(state) } == 0 {
                        tracing::warn!("could not change the sleep assertion");
                    } else {
                        held = wanted;
                        tracing::info!(held, "the sleep assertion");
                    }
                }
                // Returns once the sender is dropped.
                match receiver.recv_timeout(POWER_POLL) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
            // SAFETY: as above.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        });
        Held(sender)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub enum Want {
        OnPower,
        Always,
    }

    pub struct Held;

    pub fn hold(_want: Want) -> Held {
        tracing::info!("this platform holds no sleep assertion");
        Held
    }
}
