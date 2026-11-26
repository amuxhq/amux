//! The login item on macOS: a LaunchAgent, which launchd loads at every
//! login without being told.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{LAUNCH_AGENT_LABEL, LoginItem, words};

/// The item's file name in `~/Library/LaunchAgents`.
pub(super) const FILE: &str = "sh.amux.supervise.plist";

pub(super) fn items(home: &Path, _xdg_config_home: Option<OsString>) -> PathBuf {
    home.join("Library/LaunchAgents")
}

pub(super) fn contents(item: &LoginItem) -> String {
    item.launch_agent()
}

/// launchd loads every LaunchAgent at login: the file alone is the item.
pub(super) fn enable_commands() -> Vec<Vec<String>> {
    Vec::new()
}

pub(super) fn disable_commands() -> Vec<Vec<String>> {
    Vec::new()
}

pub(super) fn after_removal_commands() -> Vec<Vec<String>> {
    Vec::new()
}

pub(super) fn stop_commands() -> Vec<Vec<String>> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    vec![words(&[
        "launchctl",
        "bootout",
        &format!("gui/{uid}/{LAUNCH_AGENT_LABEL}"),
    ])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_named_for_the_label() {
        assert_eq!(FILE, format!("{LAUNCH_AGENT_LABEL}.plist"));
    }
}
