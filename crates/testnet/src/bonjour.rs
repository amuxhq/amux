//! A served host's advertisement on the machine's real local network, for a
//! client outside the net: the iPhone app on a simulator browses with the
//! system's own Bonjour browser and can find nothing on the scripted bus.
//!
//! The record goes through the system's mDNS responder (`dns-sd -P`) rather
//! than this process's own multicast socket, so no test binary rebuilt on
//! every run asks for local-network access, and it names the host's loopback
//! listener, so nothing listens on the Mac's network. Withdrawing ends the
//! registration, which the responder answers with a goodbye.
//!
//! A `dns-sd` left running keeps its record on the network after the net
//! is gone, and a later run then finds a renamed duplicate ("desk (2)").
//! Destructors cannot stop it when the owner is SIGKILLed or dies of an
//! unhandled signal, so `dns-sd` runs under a small shell that holds a pipe
//! from the owner and kills it the moment that pipe closes, which the
//! kernel does however the owner ends.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use node::discovery::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent, txt_properties};
use tokio::sync::broadcast;

/// Starts `dns-sd` with the script's arguments and kills it once standard
/// input reaches its end: when the owner withdraws, or the owner is gone.
/// The shell ignores interrupt, terminate and hangup, so a signal sent to
/// the whole process group cannot end the shell first and leave `dns-sd`
/// behind; the owner's own end closes the pipe all the same.
const TIED_TO_OWNER: &str = r#"dns-sd "$@" &
advertiser=$!
trap '' INT TERM HUP
read -r _
kill -KILL "$advertiser"
wait "$advertiser"
"#;

/// Advertises through `dns-sd`; browses nothing, since the net's own hosts
/// find each other on the scripted bus or by their known addresses.
pub struct SystemBonjour {
    registration: Mutex<Option<Child>>,
    events: broadcast::Sender<DiscoveryEvent>,
}

impl SystemBonjour {
    pub fn new() -> Self {
        Self {
            registration: Mutex::new(None),
            events: broadcast::channel(1).0,
        }
    }
}

impl Default for SystemBonjour {
    fn default() -> Self {
        Self::new()
    }
}

/// The `dns-sd -P` arguments that register `advert` as its own host name at
/// its first address, with the TXT record every amux advertiser writes.
pub fn registration_arguments(advert: &Advertisement) -> Result<Vec<String>, String> {
    let address = advert
        .addrs
        .first()
        .ok_or("an advertisement needs a listener address")?;
    let mut arguments = vec![
        "-P".to_owned(),
        advert.name.clone(),
        "_amux._udp".to_owned(),
        "local".to_owned(),
        address.port().to_string(),
        format!("amux-{}.local", advert.host_id.simple()),
        address.ip().to_string(),
    ];
    arguments.extend(
        txt_properties(advert)
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    Ok(arguments)
}

impl Discovery for SystemBonjour {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        let arguments = registration_arguments(&advert).map_err(DiscoveryError::Register)?;
        self.withdraw();
        let child = Command::new("/bin/sh")
            .args(["-c", TIED_TO_OWNER, "testnet-bonjour"])
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| DiscoveryError::Register(format!("starting dns-sd: {error}")))?;
        *self.registration.lock().unwrap() = Some(child);
        Ok(())
    }

    fn withdraw(&self) {
        if let Some(mut child) = self.registration.lock().unwrap().take() {
            // Closing the pipe is the shell's cue; it returns once `dns-sd`
            // has exited, so a following registration of the same name
            // does not collide with this one.
            drop(child.stdin.take());
            let _ = child.wait();
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    fn requery(&self) {}
}

impl Drop for SystemBonjour {
    fn drop(&mut self) {
        self.withdraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set in a re-run of this test binary that advertises and then waits
    /// to be killed, standing in for a `testnet serve` that is SIGKILLed.
    #[cfg(target_os = "macos")]
    const OWNER: &str = "TESTNET_BONJOUR_OWNER";

    #[cfg(target_os = "macos")]
    fn advert(name: &str) -> Advertisement {
        Advertisement {
            host_id: uuid::Uuid::new_v4(),
            name: name.into(),
            version: 4,
            addrs: vec!["127.0.0.1:9".parse().unwrap()],
            scope: "testnet-bonjour-test".into(),
        }
    }

    /// Whether a `dns-sd` still registers `name`.
    #[cfg(target_os = "macos")]
    fn registered(name: &str) -> bool {
        Command::new("pgrep")
            .args(["-f", &format!("^dns-sd -P {name} ")])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn withdrawing_ends_the_registration_before_it_returns() {
        let name = format!("testnet-withdraw-{}", uuid::Uuid::new_v4().simple());
        let bonjour = SystemBonjour::new();
        bonjour.advertise(advert(&name)).unwrap();
        patience::until("dns-sd to register the advertisement", || {
            std::future::ready(registered(&name).then_some(()).ok_or("no dns-sd yet"))
        })
        .await
        .unwrap();
        drop(bonjour);
        assert!(!registered(&name), "dns-sd outlived the withdrawal");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_killed_owner_takes_its_registration_with_it() {
        if let Some(name) = std::env::var_os(OWNER) {
            let bonjour = SystemBonjour::new();
            bonjour.advertise(advert(name.to_str().unwrap())).unwrap();
            // Held until the parent SIGKILLs this process, so no
            // destructor of ours withdraws it.
            match std::future::pending::<std::convert::Infallible>().await {}
        }
        let name = format!("testnet-owner-{}", uuid::Uuid::new_v4().simple());
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "bonjour::tests::a_killed_owner_takes_its_registration_with_it",
                "--exact",
                "--nocapture",
            ])
            .env(OWNER, &name)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let advertised = patience::until("the owner's dns-sd to register", || {
            std::future::ready(registered(&name).then_some(()).ok_or("no dns-sd yet"))
        })
        .await;
        owner.kill().unwrap();
        owner.wait().unwrap();
        advertised.unwrap();
        patience::until("the owner's dns-sd to go with it", || {
            std::future::ready(
                (!registered(&name))
                    .then_some(())
                    .ok_or("dns-sd still running"),
            )
        })
        .await
        .unwrap();
    }

    #[test]
    fn the_registration_names_the_loopback_listener_and_the_amux_record() {
        let advert = Advertisement {
            host_id: "d6e6b93c-114e-4d20-b29a-6fac10bfb78d".parse().unwrap(),
            name: "desk".into(),
            version: 4,
            addrs: vec!["127.0.0.1:52648".parse().unwrap()],
            scope: "journey".into(),
        };
        assert_eq!(
            registration_arguments(&advert).unwrap(),
            [
                "-P",
                "desk",
                "_amux._udp",
                "local",
                "52648",
                "amux-d6e6b93c114e4d20b29a6fac10bfb78d.local",
                "127.0.0.1",
                "v=4",
                "hid=d6e6b93c-114e-4d20-b29a-6fac10bfb78d",
                "name=desk",
                "scope=journey",
            ]
        );
        let bare = Advertisement {
            addrs: vec![],
            ..advert
        };
        assert!(registration_arguments(&bare).is_err());
    }
}
