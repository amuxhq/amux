//! What a topology file says about the world around its net: the clients
//! that come with it, opened once the net is up, and whether everything in
//! it is real.
//!
//! A topology file for `ctl up` may carry a `clients` section beside the
//! net's own fields:
//!
//! ```json
//! "clients": {
//!   "phone": {"paired": ["studio", "laptop"], "account": "ada"},
//!   "terminal": {"host": "studio"}
//! }
//! ```
//!
//! The phone is opened and paired with each listed host through the real
//! pairing flow, then signed in to the account if one is named; the
//! terminal client is opened in a pane against its host. A world without a
//! section never touches that client.

use std::path::Path;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;

use super::phone::{self, PhoneCommand};
use super::session::{Folder, Outcome};
use super::tui::{self, Theme, TuiCommand};

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Clients {
    #[serde(default)]
    pub phone: Option<Phone>,
    #[serde(default)]
    pub terminal: Option<Terminal>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Phone {
    /// The hosts the phone pairs with as it opens; the rest of the net's
    /// hosts are left for pairing by hand.
    #[serde(default)]
    pub paired: Vec<String>,
    /// A relay account the phone signs in to once paired.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Terminal {
    pub host: String,
}

/// The clients a topology file declares, checked against its hosts and
/// accounts so a mistake is named before the net starts.
pub(crate) fn clients(path: &Path) -> Result<Clients, String> {
    let topology = read(path)?;
    let named = |what: &str| format!("{}: clients: {what}", path.display());
    let clients: Clients = match topology.get("clients") {
        None | Some(Value::Null) => return Ok(Clients::default()),
        Some(section) => {
            serde_json::from_value(section.clone()).map_err(|error| named(&error.to_string()))?
        }
    };
    let host = |name: &str| {
        topology["hosts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|host| host["name"] == name)
    };
    if let Some(phone) = &clients.phone {
        for name in &phone.paired {
            let declared = host(name)
                .ok_or_else(|| named(&format!("the phone pairs with {name}, which no host is")))?;
            // The phone finds a host on the machine's real mDNS and pairs
            // with it over its LAN listener.
            if declared["lan"] != true || declared["bonjour"] != true {
                return Err(named(&format!(
                    "the phone pairs with {name} over its LAN: give it \"lan\" and \"bonjour\""
                )));
            }
        }
        if let Some(account) = &phone.account {
            let known = topology["relay"]["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|declared| declared["name"] == account.as_str());
            if !known {
                return Err(named(&format!(
                    "the phone signs in to {account}, which the relay's accounts do not name"
                )));
            }
        }
    }
    if let Some(terminal) = &clients.terminal
        && host(&terminal.host).is_none()
    {
        return Err(named(&format!(
            "the terminal client runs against {}, which no host is",
            terminal.host
        )));
    }
    Ok(clients)
}

/// Opens the clients a world declares on its net, which is up, and says
/// what each did after what `up` said.
pub(crate) fn open(
    folder: &Folder,
    clients: &Clients,
    outcome: &mut Outcome,
) -> Result<(), String> {
    let mut said = |text: String| {
        outcome.text.push('\n');
        outcome.text.push_str(&text);
    };
    if let Some(declared) = &clients.phone {
        let began = Instant::now();
        let relay = declared.account.is_some();
        said(phone::run(folder, PhoneCommand::Open { host: None, relay })?.text);
        for host in &declared.paired {
            said(phone::run(folder, PhoneCommand::Pair { host: host.clone() })?.text);
        }
        if let Some(account) = &declared.account {
            said(
                phone::run(
                    folder,
                    PhoneCommand::Connect {
                        account: account.clone(),
                    },
                )?
                .text,
            );
        }
        said(format!(
            "the phone was ready {:.1} s after it began to open",
            began.elapsed().as_secs_f64()
        ));
    }
    if let Some(declared) = &clients.terminal {
        let opened = tui::run(
            folder,
            TuiCommand::Open {
                host: declared.host.clone(),
                size: "110x34".to_owned(),
                theme: Theme::Dark,
                name: None,
            },
        )?;
        let pane = opened.value["pane"].as_str().unwrap_or_default().to_owned();
        said(format!(
            "{} (watch it with `ctl tui watch {pane}`)",
            opened.text
        ));
    }
    Ok(())
}

/// Whether a topology file declares real hosts and agents only: every host
/// runs the real providers and no agent plays a script or a journal. A QA
/// tester stands in for a person, who only ever meets real agents.
pub(crate) fn real_only(path: &Path) -> Result<(), String> {
    let topology = read(path)?;
    let mut fake = Vec::new();
    for host in topology["hosts"].as_array().into_iter().flatten() {
        if host["provider"] != "real" || host.get("script").is_some_and(|script| !script.is_null())
        {
            fake.push(format!("host {}", host["name"].as_str().unwrap_or("?")));
        }
    }
    for agent in topology["agents"].as_array().into_iter().flatten() {
        let scripted = ["script", "script_file", "journal"]
            .iter()
            .any(|key| agent.get(*key).is_some_and(|value| !value.is_null()));
        if scripted || agent["provider"] == "fake" {
            fake.push(format!("agent {}", agent["name"].as_str().unwrap_or("?")));
        }
    }
    if fake.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} declares what is not real ({}): every host needs \"provider\": \"real\", and no agent a script or journal",
            path.display(),
            fake.join(", ")
        ))
    }
}

fn read(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{} is not JSON: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &tempfile::TempDir, topology: Value) -> std::path::PathBuf {
        let path = dir.path().join("world.json");
        std::fs::write(&path, topology.to_string()).unwrap();
        path
    }

    fn studio() -> Value {
        serde_json::json!({"name": "studio", "lan": true, "bonjour": true, "provider": "real"})
    }

    #[test]
    fn a_world_without_clients_opens_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, serde_json::json!({"hosts": [studio()]}));
        assert_eq!(clients(&path).unwrap(), Clients::default());
    }

    #[test]
    fn clients_are_read_and_checked_against_the_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            &dir,
            serde_json::json!({
                "hosts": [studio(), {"name": "nas", "provider": "real"}],
                "relay": {"accounts": [{"name": "ada"}]},
                "clients": {"phone": {"paired": ["studio"], "account": "ada"}, "terminal": {"host": "nas"}},
            }),
        );
        let read = clients(&path).unwrap();
        assert_eq!(read.phone.unwrap().paired, ["studio"]);
        assert_eq!(read.terminal.unwrap().host, "nas");

        for (clients_section, says) in [
            (
                serde_json::json!({"phone": {"paired": ["laptop"]}}),
                "no host is",
            ),
            (
                serde_json::json!({"phone": {"paired": ["nas"]}}),
                "\"lan\" and \"bonjour\"",
            ),
            (
                serde_json::json!({"phone": {"account": "bob"}}),
                "do not name",
            ),
            (
                serde_json::json!({"terminal": {"host": "pi"}}),
                "no host is",
            ),
            (serde_json::json!({"watch": {}}), "unknown field"),
        ] {
            let path = file(
                &dir,
                serde_json::json!({
                    "hosts": [studio(), {"name": "nas", "provider": "real"}],
                    "relay": {"accounts": [{"name": "ada"}]},
                    "clients": clients_section,
                }),
            );
            let error = clients(&path).unwrap_err();
            assert!(error.contains(says), "{error}");
        }
    }

    /// Every committed environment is a topology the net takes once its
    /// clients are set apart, opens clients its hosts can serve, and is
    /// real throughout, so a QA run may start from it too.
    #[test]
    fn every_environment_is_a_real_world_whose_clients_its_hosts_serve() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../environments");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            seen += 1;
            clients(&path).unwrap();
            real_only(&path).unwrap();
            let mut topology = read(&path).unwrap();
            topology.as_object_mut().unwrap().remove("clients");
            let topology: crate::Topology = serde_json::from_value(topology)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            topology
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        }
        assert!(seen >= 3, "found {seen} environments in {}", dir.display());
    }

    #[test]
    fn a_world_is_real_only_without_fake_hosts_scripts_or_journals() {
        let dir = tempfile::tempdir().unwrap();
        let real = serde_json::json!({
            "hosts": [studio()],
            "agents": [{"name": "a", "host": "studio", "prompt": "Hello."}],
        });
        assert_eq!(real_only(&file(&dir, real)), Ok(()));
        for (topology, names) in [
            (serde_json::json!({"hosts": [{"name": "pi"}]}), "host pi"),
            (
                serde_json::json!({"hosts": [studio()], "agents": [{"name": "a", "host": "studio", "script_file": "a.json"}]}),
                "agent a",
            ),
            (
                serde_json::json!({"hosts": [studio()], "agents": [{"name": "b", "host": "studio", "provider": "fake"}]}),
                "agent b",
            ),
        ] {
            let error = real_only(&file(&dir, topology)).unwrap_err();
            assert!(error.contains(names), "{error}");
        }
    }
}
