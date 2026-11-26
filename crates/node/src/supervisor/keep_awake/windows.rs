//! Keep-awake on Windows: `SetThreadExecutionState` on a thread kept for
//! the purpose, the supervisor's set only while the machine is on AC.

use std::sync::mpsc;
use std::time::Duration;

use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, GetSystemPowerStatus, SYSTEM_POWER_STATUS,
    SetThreadExecutionState,
};

/// How often the power-only hold looks at whether the machine is on AC.
const POWER_POLL: Duration = Duration::from_secs(30);

/// `ACLineStatus` when the machine is on battery; 1 is AC and 255 unknown.
const OFFLINE: u8 = 0;

pub enum Want {
    OnPower,
    Always,
}

/// The state belongs to the thread that set it, so one thread holds it
/// and clears it when the sender drops.
pub struct Held(#[allow(dead_code)] mpsc::Sender<()>);

/// Whether a line status counts as AC: anything but battery does. A
/// desktop or a VM with no battery may report 255, unknown, and is not
/// running on a battery.
fn counts_as_ac(line_status: u8) -> bool {
    line_status != OFFLINE
}

fn on_ac() -> bool {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: a plain call filling a struct we own.
    unsafe { GetSystemPowerStatus(&mut status) != 0 && counts_as_ac(status.ACLineStatus) }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Only battery is battery: an unknown status (255, as a desktop or a
    /// VM may report) keeps the machine awake as AC does.
    #[test]
    fn an_unknown_power_status_counts_as_ac() {
        assert!(!counts_as_ac(0));
        assert!(counts_as_ac(1));
        assert!(counts_as_ac(255));
    }
}
