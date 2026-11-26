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
//! supervisor's set only while the machine reports AC power (or an unknown
//! status, as a desktop or VM without a battery may). Linux holds
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
#[path = "macos.rs"]
mod imp;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "other.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
