//! What a phone story needs from the app it drives, whichever phone runs
//! it: the iPhone app on its simulator (`phone.rs`) or the Android app on
//! its emulator (`android.rs`). Both apps answer the same debug door verbs
//! and name what they draw by the same element identifiers, with the same
//! words and values, so one story reads the same on either; what the
//! platforms genuinely do differently (how a tab is reached, how Settings
//! opens, how the phone finds a machine on its network, how large text is
//! asked for) is a method here that each driver answers its own way.
//!
//! The served net (`testnet serve`) is the same for both, so what a story
//! asks of it (a chat, an inventory, what a provider was sent, `amux` at a
//! machine) is written once here on the readiness each driver keeps.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::phone::{AppError, Drawn, Launch, Printing};
use super::{Record, door};
use crate::Binaries;
use crate::door::{Control, Readiness};

/// Which phone a driver drives, for the few acts one platform has and the
/// other does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Client {
    Iphone,
    Android,
}

/// The app's two tabs, by what a person reads on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Agents,
    Devices,
}

impl Tab {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::Agents => "Agents",
            Tab::Devices => "Devices",
        }
    }
}

/// A phone app driven through its debug door against a served net.
pub(crate) trait Driver {
    /// Which phone this is.
    const CLIENT: Client;

    // --- what the run is ----------------------------------------------------

    /// What the served net said when it was ready.
    fn ready(&self) -> &Readiness;
    /// Every act and observation the run keeps beside its result.
    fn record(&mut self) -> &mut Record;
    /// Where this run writes what it found.
    fn output(&self) -> &Path;
    /// The binaries the served net runs, which a person at one of its
    /// machines runs too.
    fn binaries(&self) -> &Binaries;
    /// The run's one scratch directory, which every served machine's
    /// temporary files live under.
    fn scratch(&self) -> &Path;
    /// The account service as the phone reaches it: an emulator reaches this
    /// Mac's loopback by another address.
    fn cloud_url(&self) -> String {
        self.ready().cloud_url.clone().unwrap_or_default()
    }

    // --- the app ------------------------------------------------------------

    /// The app installed fresh, so no earlier run's identity or trust is on
    /// the phone, and launched against this net.
    fn launch(&mut self, launch: Launch) -> Result<(), String>;
    /// The same installation opened again, as a person reopens it.
    fn relaunch(&mut self, launch: Launch) -> Result<(), String>;
    /// The app closed, as a person swipes it away.
    fn quit(&mut self) -> Result<(), String>;
    /// One request to the app's door and its reply.
    fn app_within(&self, request: Value, timeout: Duration) -> Result<Value, AppError>;
    fn app(&self, request: Value) -> Result<Value, AppError> {
        self.app_within(request, Duration::from_secs(90))
    }
    /// What the app's door says it draws, and how.
    fn query(&self) -> Result<Value, String> {
        Ok(self.app(json!({"kind": "query"}))?["state"].take())
    }
    fn elements(&self) -> Result<Drawn, String> {
        let state = self.query()?;
        Ok(Drawn::on(
            state["elements"].as_array().map_or(&[], Vec::as_slice),
            state["screen"].as_str(),
        ))
    }
    /// What the app shows now, compared with its reviewed golden.
    fn screen(&mut self, label: &str, volatile: &[&str]) -> Result<(), String>;
    /// A tap as a person makes one, on a control that is there and enabled.
    fn tap(&mut self, name: &str) -> Result<(), String>;
    /// Text typed into the field `name`, after what it holds.
    fn type_text(&mut self, name: &str, text: &str) -> Result<(), String>;
    /// The field `name` emptied, once it is drawn.
    fn clear(&mut self, name: &str) -> Result<(), String> {
        self.wait_for(&[name])?;
        self.app(json!({"kind": "clear", "identifier": name}))?;
        self.record().actions.push(format!("clear {name}"));
        Ok(())
    }
    fn paste(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.app(json!({"kind": "paste", "identifier": name, "text": text}))?;
        self.record()
            .actions
            .push(format!("paste {} lines into {name}", text.lines().count()));
        Ok(())
    }
    /// Back from a pushed page, as the system's back does.
    fn back(&mut self) -> Result<(), String> {
        self.app(json!({"kind": "back"}))?;
        self.record().actions.push("back".to_owned());
        Ok(())
    }
    /// The item a person reads as `label` in a menu, sheet or dialog the app
    /// presented, once it is drawn.
    fn choose(&mut self, label: &str) -> Result<(), String> {
        self.wait(
            |drawn| drawn.iter().any(|(_, element)| element["label"] == label),
            &format!("{label:?} presented"),
        )?;
        self.app(json!({"kind": "choose", "label": label}))?;
        self.record().actions.push(format!("choose {label:?}"));
        Ok(())
    }
    /// An element's named accessibility action, as VoiceOver and TalkBack
    /// offer it.
    fn perform(&mut self, name: &str, action: &str) -> Result<(), String> {
        self.app(json!({"kind": "perform", "identifier": name, "action": action}))?;
        self.record().actions.push(format!("{action} on {name}"));
        Ok(())
    }
    /// The link a machine printed, as a scanned code hands it over; the host
    /// that answered.
    fn pair(&mut self, link: &str) -> Result<String, AppError> {
        let reply = self.app(json!({"kind": "pair", "qr": link}))?;
        let host = reply["host"].as_str().unwrap_or_default().to_owned();
        self.record()
            .actions
            .push(format!("pair by the link, answered by {host}"));
        Ok(host)
    }
    /// The system photographs the app, as a person's screenshot does.
    fn system_screenshot(&mut self) -> Result<(), String> {
        self.app(json!({"kind": "screenshot"}))?;
        self.record()
            .actions
            .push("the system took a screenshot".to_owned());
        Ok(())
    }

    // --- what the platforms do differently ---------------------------------

    /// The tab a person reads as `tab`.
    fn tab(&mut self, tab: Tab) -> Result<(), String>;
    /// Settings, as a person reaches them, open.
    fn settings(&mut self) -> Result<(), String>;
    /// Settings put away.
    fn close_settings(&mut self) -> Result<(), String>;
    /// The phone's own look for machines on its network finds `host`, its
    /// pairing window open or not. The iPhone's browser reports what the
    /// launch let it, by itself; the emulator has no network a machine
    /// advertises on, so its driver says what was found.
    fn found(&mut self, _host: &str, _pairing_open: bool) -> Result<(), String> {
        Ok(())
    }
    /// Whether the activity bar's motion is reduced, so a running turn can
    /// be photographed standing still.
    fn reduce_motion(&mut self, _on: bool) -> Result<(), String> {
        Ok(())
    }
    /// The app asked to draw at the platform's large accessibility text
    /// size; the size it says it draws at.
    fn large_text(&mut self) -> Result<String, String>;
    /// Checks only this platform makes on its first launch, each a line of
    /// what it proved.
    fn self_checks(&mut self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    // --- waiting on the app -------------------------------------------------

    /// Until what is drawn satisfies `ready`, by element name.
    fn wait_within(
        &mut self,
        ready: impl Fn(&Drawn) -> bool,
        description: &str,
        timeout: Duration,
    ) -> Result<Drawn, String> {
        let deadline = Instant::now() + timeout;
        let mut last = Drawn::default();
        while Instant::now() < deadline {
            last = self.elements()?;
            if ready(&last) {
                self.record().actions.push(format!("reached {description}"));
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let drawn: Vec<String> = last
            .iter()
            .map(|(name, element)| format!("{name} | {} | {}", element["label"], element["value"]))
            .collect();
        Err(format!(
            "timed out waiting for {description}; the app draws:\n{}",
            drawn.join("\n")
        ))
    }
    fn wait(&mut self, ready: impl Fn(&Drawn) -> bool, description: &str) -> Result<Drawn, String> {
        self.wait_within(ready, description, Duration::from_secs(60))
    }
    fn wait_for(&mut self, names: &[&str]) -> Result<Drawn, String> {
        self.wait(
            |drawn| names.iter().all(|name| drawn.has(name)),
            &format!("{names:?}"),
        )
    }

    // --- the served net -----------------------------------------------------

    fn request(&mut self, request: Control, label: Option<&str>) -> Result<Value, String> {
        self.record().actions.push(format!(
            "net {}",
            serde_json::to_string(&request).unwrap_or_default()
        ));
        let reply = door(self.ready().control, &request, Duration::from_secs(90))?;
        if let Some(label) = label {
            self.record()
                .observations
                .insert(label.to_owned(), reply.clone());
        }
        Ok(reply)
    }
    fn chat(&mut self, host: &str, agent: &str, label: &str) -> Result<Value, String> {
        self.request(
            Control::Chat {
                host: host.to_owned(),
                agent: agent.to_owned(),
            },
            Some(label),
        )
    }
    /// Until `host`'s chat with `agent` satisfies `ready`.
    fn wait_chat(
        &mut self,
        host: &str,
        agent: &str,
        ready: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value, String> {
        let control = self.ready().control;
        let request = Control::Chat {
            host: host.to_owned(),
            agent: agent.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || door(control, &request, Duration::from_secs(90)),
            |chat| ready(chat),
        ) {
            Ok(chat) => {
                self.record()
                    .observations
                    .insert(description.to_owned(), chat.clone());
                self.record()
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(chat)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} holds {}",
                last.unwrap_or_default()
            )),
        }
    }
    /// Until `host`'s agents satisfy `ready`.
    fn wait_inventory(
        &mut self,
        host: &str,
        ready: impl Fn(&[Value]) -> bool,
        description: &str,
    ) -> Result<Vec<Value>, String> {
        let control = self.ready().control;
        let request = Control::Inventory {
            host: host.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || {
                door(control, &request, Duration::from_secs(90))
                    .map(|reply| reply["agents"].as_array().cloned().unwrap_or_default())
            },
            |agents| ready(agents),
        ) {
            Ok(agents) => {
                self.record()
                    .observations
                    .insert(description.to_owned(), Value::Array(agents.clone()));
                self.record()
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(agents)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} lists {}",
                Value::Array(last.unwrap_or_default())
            )),
        }
    }
    /// What the agent's provider was sent, line by line.
    fn provider_input(&mut self, agent: &str, label: &str) -> Result<Vec<String>, String> {
        let reply = self.request(
            Control::ProviderInput {
                agent: agent.to_owned(),
            },
            Some(label),
        )?;
        Ok(reply["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }
    /// The id of the host the topology declared as `host`.
    fn host_id(&self, host: &str) -> String {
        self.ready()
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.host_id.to_string())
            .unwrap_or_default()
    }
    /// The id of the agent the topology declared as `name`.
    fn agent_id(&self, name: &str) -> String {
        self.ready()
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .map(|agent| agent.id.to_string())
            .unwrap_or_default()
    }
    fn config(&self, host: &str) -> PathBuf {
        self.ready()
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.config.clone())
            .unwrap_or_default()
    }
    /// `amux` on a served machine, as a person at that machine types it;
    /// what it prints, line by line.
    fn amux(&mut self, host: &str, args: &[&str]) -> Result<Printing, String> {
        self.record()
            .actions
            .push(format!("at {host}: amux {}", args.join(" ")));
        let scratch = self.scratch().to_path_buf();
        let mut child = Command::new(self.binaries().amux())
            .arg("--config")
            .arg(self.config(host))
            .args(args)
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env("TMPDIR", &scratch)
            .env("TMP", &scratch)
            .env("TEMP", &scratch)
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env("RUST_LOG", super::phone::rust_log())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("amux: {error}"))?;
        let (sent, lines) = mpsc::channel();
        for pipe in [
            child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sent = sent.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if sent.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(Printing::new(child, lines))
    }
    /// The link `amux pair --qr --print-link` prints on `host`; the process
    /// stays open, as the machine keeps offering it.
    fn pairing_link(&mut self, host: &str) -> Result<(Printing, String), String> {
        let mut process = self.amux(host, &["pair", "--qr", "--print-link"])?;
        match process.find(Duration::from_secs(60), |line| {
            line.strip_prefix("Pairing link: ")
                .map(|link| link.trim().to_owned())
        }) {
            Some(link) => Ok((process, link)),
            None => {
                process.kill();
                Err(format!("{host} printed no pairing link"))
            }
        }
    }
}
