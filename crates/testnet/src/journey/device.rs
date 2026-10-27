//! The iPhone plugged into this Mac, as the phone journeys drive one in a
//! simulator's place.
//!
//! The phone and the Mac meet over its USB link, a point-to-point IPv6
//! network of their own that CoreDevice brings up for a paired phone. The app's
//! door listens on the phone's address there and the served net's gates on
//! the Mac's, so a run on a phone listens on no other network. `devicectl`
//! installs, launches and stops the app; each call costs a second or two of
//! tunnel setup, which no measurement sees, since every number is the app's
//! own, timed from its process start.

use std::net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::phone::{BUNDLE_ID, run_within};

/// The launch arguments that have the app forget everything it kept before
/// it reads any of it, as an uninstall would, keeping the permissions.
pub(crate) const FRESH_START: [&str; 2] = ["-amux-fresh-start", "YES"];

/// A phone found plugged in, paired and ready to be driven.
#[derive(Clone, Debug)]
pub(crate) struct Phone {
    pub name: String,
    /// What devicectl names it by.
    identifier: String,
    /// What xcodebuild and xctrace name it by.
    pub udid: String,
    /// The hardware model, `iPhone15,2`.
    pub model: String,
    pub os_version: String,
    /// The phone's own address on the USB link.
    pub address: Ipv6Addr,
    /// The Mac's address on the same link: what the phone dials.
    pub mac_address: Ipv6Addr,
}

fn devicectl(args: &[&str], timeout: Duration) -> Result<Value, String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let output = scratch.path().join("devicectl.json");
    let mut command = Command::new("xcrun");
    // Its own options before the subcommand: after a launch's `--` they
    // would be the app's arguments.
    command
        .arg("devicectl")
        .arg("--quiet")
        .arg("--json-output")
        .arg(&output)
        .args(args);
    run_within(command, timeout)?;
    let text = std::fs::read_to_string(&output).map_err(|error| format!("devicectl: {error}"))?;
    let reply: Value =
        serde_json::from_str(&text).map_err(|error| format!("devicectl: {error}"))?;
    Ok(reply["result"].clone())
}

/// The one phone plugged in over USB, or why none can be driven.
pub(crate) fn connected() -> Result<Phone, String> {
    let listed = devicectl(&["list", "devices"], Duration::from_secs(60))?;
    let phones: Vec<&Value> = listed["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|device| {
            device["hardwareProperties"]["reality"] == "physical"
                && device["hardwareProperties"]["platform"] == "iOS"
                && device["connectionProperties"]["transportType"] == "wired"
        })
        .collect();
    let [phone] = phones.as_slice() else {
        return Err(format!(
            "expected one phone plugged in over USB, found {}; `xcrun devicectl list devices` lists what this Mac sees",
            phones.len()
        ));
    };
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let connection = &phone["connectionProperties"];
    let properties = &phone["deviceProperties"];
    let name = text(&properties["name"]);
    if connection["pairingState"] != "paired" {
        return Err(format!(
            "{name} is not paired with this Mac: unlock it and trust the Mac"
        ));
    }
    if properties["developerModeStatus"] != "enabled" {
        return Err(format!(
            "{name} does not have Developer Mode on: Settings > Privacy & Security > Developer Mode, then restart it"
        ));
    }
    let address: Ipv6Addr = connection["tunnelIPAddress"]
        .as_str()
        .and_then(|address| address.parse().ok())
        .filter(|_| connection["tunnelState"] == "connected")
        .ok_or_else(|| format!("{name} has no USB link up yet: unlock it and try again"))?;
    Ok(Phone {
        mac_address: mac_side(address)?,
        name,
        identifier: text(&phone["identifier"]),
        udid: text(&phone["hardwareProperties"]["udid"]),
        model: text(&phone["hardwareProperties"]["productType"]),
        os_version: text(&properties["osVersionNumber"]),
        address,
    })
}

/// The Mac's own address on the link to `phone`: the source address the
/// kernel picks for a datagram to it. Connecting a UDP socket sends nothing.
fn mac_side(phone: Ipv6Addr) -> Result<Ipv6Addr, String> {
    let probe = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).map_err(|error| error.to_string())?;
    probe
        .connect((phone, 9))
        .map_err(|error| format!("no route to the phone's link address {phone}: {error}"))?;
    match probe.local_addr().map_err(|error| error.to_string())?.ip() {
        IpAddr::V6(address) => Ok(address),
        IpAddr::V4(address) => Err(format!(
            "the route to the phone left from {address}, not the USB link"
        )),
    }
}

impl Phone {
    /// Where the app's door listens on the phone, for the Mac to dial.
    pub(crate) fn door(&self, port: u16) -> SocketAddr {
        SocketAddr::from((self.address, port))
    }

    /// `app` installed over whatever build is there. Not uninstalled first:
    /// an uninstall takes the local network permission with it, and the
    /// system would ask for it again on the next launch with nobody there to
    /// answer. A run starts from nothing by launching with
    /// [`FRESH_START`] instead.
    pub(crate) fn install(&self, app: &Path) -> Result<(), String> {
        devicectl(
            &[
                "device",
                "install",
                "app",
                "--device",
                &self.identifier,
                app.to_string_lossy().as_ref(),
            ],
            Duration::from_secs(600),
        )?;
        Ok(())
    }

    /// The app launched with `arguments` and `environment`, any running
    /// instance ended first; its process id.
    pub(crate) fn launch(
        &self,
        arguments: &[String],
        environment: &[(&str, &str)],
    ) -> Result<u32, String> {
        let environment = serde_json::Map::from_iter(
            environment
                .iter()
                .map(|(key, value)| ((*key).to_owned(), Value::from(*value))),
        );
        let environment = Value::Object(environment).to_string();
        let mut args = vec![
            "device",
            "process",
            "launch",
            "--device",
            &self.identifier,
            "--terminate-existing",
            "--environment-variables",
            &environment,
            BUNDLE_ID,
            // Without it devicectl reads the app's own `-amux-…` arguments
            // as options of its own; the app ignores the separator.
            "--",
        ];
        args.extend(arguments.iter().map(String::as_str));
        let launched = devicectl(&args, Duration::from_secs(120)).map_err(|error| {
            if error.contains("Locked") || error.contains("unlocked") {
                format!(
                    "{} is locked: unlock it and turn Auto-Lock off for the run",
                    self.name
                )
            } else {
                error
            }
        })?;
        launched["process"]["processIdentifier"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| format!("devicectl launched the app but named no process: {launched}"))
    }

    /// The app's process on the phone, if it runs.
    pub(crate) fn running(&self) -> Result<Option<u32>, String> {
        let listed = devicectl(
            &["device", "info", "processes", "--device", &self.identifier],
            Duration::from_secs(60),
        )?;
        Ok(listed["runningProcesses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|process| {
                process["executable"]
                    .as_str()
                    .is_some_and(|executable| executable.ends_with("/Amux.app/Amux"))
            })
            .and_then(|process| process["processIdentifier"].as_u64())
            .and_then(|pid| u32::try_from(pid).ok()))
    }

    /// The app ended, as a person swiping it away ends it.
    pub(crate) fn terminate(&self) -> Result<(), String> {
        if let Some(pid) = self.running()? {
            devicectl(
                &[
                    "device",
                    "process",
                    "terminate",
                    "--device",
                    &self.identifier,
                    "--pid",
                    &pid.to_string(),
                ],
                Duration::from_secs(60),
            )?;
        }
        Ok(())
    }
}
