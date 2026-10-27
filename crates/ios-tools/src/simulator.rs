//! Which device a simulator kind means, decided the same way the recipes'
//! Python decides it (`apps/apple/Tools/ios_simulators.py`).
//!
//! Every manifest entry and every `--simulator` argument names a kind, `golden`
//! or `small`, never a device. Inside a wt worktree the kind means the device
//! wt leased for this command, named in `WT_LEASE_IPHONE` or
//! `WT_LEASE_IPHONE_SMALL`; without a lease the tool refuses rather than drive
//! a device another checkout may be using. Outside a worktree, which is CI or a
//! bare checkout, it means the first device of the same naming scheme. A name
//! that is not a kind is taken as a device name, for driving an ad-hoc device
//! by hand.

use crate::door::DoorError;

const KINDS: &[(&str, &str, &str, &str)] = &[
    ("golden", "WT_LEASE_IPHONE", "amux-iphone-1", "iphone"),
    (
        "small",
        "WT_LEASE_IPHONE_SMALL",
        "amux-small-1",
        "iphone-small",
    ),
];

/// The device name a kind resolves to, or the argument itself when it is not
/// a kind.
pub fn resolve(simulator: &str) -> Result<String, DoorError> {
    let Some((_, lease, fallback, pool)) = KINDS.iter().find(|(kind, ..)| *kind == simulator)
    else {
        return Ok(simulator.to_string());
    };
    if let Some(leased) = std::env::var_os(lease).filter(|value| !value.is_empty()) {
        return Ok(leased.to_string_lossy().into_owned());
    }
    if std::env::var_os("WT_TARGET").is_some_and(|value| !value.is_empty()) {
        return Err(DoorError::NoSimulator(format!(
            "{simulator} (no device is leased for this command; run it under \
             `scripts/with {pool} -- ...` so wt hands it one)"
        )));
    }
    Ok(fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn a_device_name_passes_through() {
        assert_eq!(resolve("my-device").unwrap(), "my-device");
    }
}
