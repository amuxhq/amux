//! The login item on Linux (and every Unix but macOS): a systemd user
//! unit, enabled for `default.target`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{LoginItem, SYSTEMD_UNIT, words};

/// The item's file name among the user units.
pub(super) const FILE: &str = SYSTEMD_UNIT;

/// `$XDG_CONFIG_HOME/systemd/user`, `~/.config/systemd/user` without it.
pub(super) fn items(home: &Path, xdg_config_home: Option<OsString>) -> PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(".config"),
    }
    .join("systemd/user")
}

pub(super) fn contents(item: &LoginItem) -> String {
    item.systemd_unit()
}

pub(super) fn enable_commands() -> Vec<Vec<String>> {
    vec![
        words(&["systemctl", "--user", "daemon-reload"]),
        words(&["systemctl", "--user", "enable", SYSTEMD_UNIT]),
    ]
}

pub(super) fn disable_commands() -> Vec<Vec<String>> {
    vec![words(&["systemctl", "--user", "disable", SYSTEMD_UNIT])]
}

pub(super) fn after_removal_commands() -> Vec<Vec<String>> {
    vec![words(&["systemctl", "--user", "daemon-reload"])]
}

pub(super) fn stop_commands() -> Vec<Vec<String>> {
    vec![words(&[
        "systemctl",
        "--user",
        "disable",
        "--now",
        SYSTEMD_UNIT,
    ])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_units_follow_xdg_config_home() {
        let home = Path::new("/home/someone");
        assert_eq!(
            items(home, None),
            Path::new("/home/someone/.config/systemd/user")
        );
        assert_eq!(
            items(home, Some(OsString::new())),
            Path::new("/home/someone/.config/systemd/user")
        );
        assert_eq!(
            items(home, Some("/elsewhere".into())),
            Path::new("/elsewhere/systemd/user")
        );
    }
}
