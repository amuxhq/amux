//! The terminal journey driver: starts a served topology with `testnet
//! serve`, launches the real `amux` terminal client against one host's
//! install in a private tmux server at a fixed size, waits for frames,
//! sends keys, captures text and styles, asks the door for the hosts'
//! own observations, proves the client's exit and cleans up. Stories
//! supply the acts and the assertions.

use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::frame::{mask_durations, normalize, parse_styled, respell, spell_root, style_map};
use super::{Record, Story, compare_golden, door, root};
use crate::ctl::tui::shell_word;
use crate::door::{Control, Readiness};

pub(crate) const COLS: usize = 110;
pub(crate) const ROWS: usize = 34;
/// The terminal client's ask arming window, `ASK_ARMS_MS`, with a margin.
const ASK_ARMS: Duration = Duration::from_millis(600);
/// How long a door request may take.
const DOOR: Duration = Duration::from_secs(90);
/// What an assigned agent name is drawn as: a host gives an agent created
/// without a name a random word pair, at least eight cells wide.
const ASSIGNED: &str = "<name>";

/// Where every terminal journey runs: one fixed folder, so nothing in a
/// frame is random, under a prefix of one physical width on every
/// platform. A program reading its own working directory prints the
/// physical path, and on macOS /tmp is a link to /private/tmp; elsewhere
/// the prefix is padded to the same width, so every screen is laid out
/// alike. Frames spell the prefix one way. Journeys in other checkouts
/// wait for it on the lock.
const SPELLED_PREFIX: &str = "/tmp/journey";
const SCRATCH_LOCK: &str = "/tmp/amux-terminal-journey.lock";

fn prefix() -> String {
    let tmp = std::fs::canonicalize("/tmp")
        .map_or_else(|_| "/tmp".to_owned(), |path| path.display().to_string());
    if tmp == "/tmp" {
        SPELLED_PREFIX.to_owned()
    } else {
        tmp
    }
}

fn scratch_root() -> PathBuf {
    Path::new(&prefix()).join("aj-terminal")
}

/// Home's title line: amux at the left, the fleet's facts at the right.
pub(crate) fn at_home(frame: &str) -> bool {
    frame
        .lines()
        .take(3)
        .any(|line| line == "  amux" || line.starts_with("  amux "))
}

/// The agent whose chat is open: the header's first cell.
pub(crate) fn chat_of(frame: &str) -> Option<String> {
    frame.lines().take(3).find_map(|line| {
        let rest = line.strip_prefix("  ")?;
        if rest.starts_with(char::is_whitespace) || rest.is_empty() {
            return None;
        }
        let at = rest.find(" │ ")?;
        (at > 0).then(|| rest[..at].to_owned())
    })
}

/// The line of home's row for `agent`: a mark, then its name.
pub(crate) fn agent_row(frame: &str, agent: &str) -> Option<usize> {
    frame.lines().position(|line| {
        let trimmed = line.trim_start();
        if trimmed.len() == line.len() {
            return false;
        }
        let rest = trimmed.strip_prefix("› ").unwrap_or(trimmed);
        let mut chars = rest.chars();
        match chars.next() {
            Some(mark) if !mark.is_whitespace() => {}
            _ => return false,
        }
        let Some(rest) = chars.as_str().strip_prefix(' ') else {
            return false;
        };
        let Some(after) = rest.strip_prefix(agent) else {
            return false;
        };
        after.is_empty() || after.starts_with("  ") || after.starts_with(" ▸")
    })
}

/// Whether home's selection is on `agent`'s row: a band around it in full
/// colour, a › before it in the sixteen colours.
pub(crate) fn selected_row(frame: &str, agent: &str) -> bool {
    let lines: Vec<&str> = frame.lines().collect();
    agent_row(frame, agent).is_some_and(|at| {
        lines[at].trim_start().starts_with("› ") || (at > 0 && lines[at - 1].contains("▄▄▄"))
    })
}

/// A turn is under way: the keys under the composer offer to interrupt it.
pub(crate) fn working(frame: &str) -> bool {
    frame.contains("ctrl+x interrupt")
}

/// Every term shows and no turn is under way.
pub(crate) fn at_rest(frame: &str, terms: &[&str]) -> bool {
    terms.iter().all(|term| frame.contains(term)) && !working(frame)
}

pub(crate) struct TerminalJourney {
    name: String,
    pub output: PathBuf,
    _lock: File,
    scratch: PathBuf,
    server: String,
    process: Option<Child>,
    pub ready: Readiness,
    pub record: Record,
    /// Names hosts assigned, drawn as ASSIGNED in every frame.
    pub assigned: Vec<String>,
}

impl TerminalJourney {
    pub(crate) fn start(story: &Story) -> Result<TerminalJourney, String> {
        let root = root();
        let topology = root.join(
            story
                .topology
                .as_deref()
                .ok_or("the story names no topology")?,
        );
        let output = root.join("target/journeys").join(&story.id);
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        let lock = File::create(SCRATCH_LOCK).map_err(|error| error.to_string())?;
        lock.lock().map_err(|error| error.to_string())?;
        let scratch = scratch_root();
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
        let testnet = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(&testnet);
        command
            .args(["serve"])
            .arg(&topology)
            .arg("--root-in")
            .arg(&scratch)
            .current_dir(&root)
            // NO_COLOR in the launching shell (some sandboxes export it)
            // would strip the colour every .styles golden is approved in.
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env_remove("NO_COLOR")
            .env("TMPDIR", &scratch)
            .env("TMP", &scratch)
            .env("TEMP", &scratch)
            // `just journey terminal` builds amux and the fakes with the
            // desktop's features; left to itself, testnet would rebuild
            // them without, which on a cold runner outlasts the wait.
            .env(
                crate::PREBUILT_ENV,
                testnet.parent().unwrap_or(Path::new(".")),
            )
            .stdout(Stdio::piped())
            .stderr(File::create(output.join("testnet.log")).map_err(|error| error.to_string())?);
        let mut process = command
            .spawn()
            .map_err(|error| format!("testnet serve: {error}"))?;
        let stdout = process.stdout.take().expect("piped");
        let (sent, readiness) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sent.send(line);
        });
        let ready = match readiness.recv_timeout(Duration::from_secs(120)) {
            Ok(line) if !line.is_empty() => serde_json::from_str::<Readiness>(&line)
                .map_err(|error| format!("readiness: {error}")),
            Ok(_) => Err("testnet exited before readiness".to_owned()),
            Err(_) => Err("testnet readiness timed out".to_owned()),
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(error);
            }
        };
        let mut record = Record::default();
        record
            .actions
            .push(format!("ready control={}", ready.control));
        Ok(TerminalJourney {
            name: story.id.clone(),
            output,
            _lock: lock,
            scratch,
            server: format!("amux-journey-{}", std::process::id()),
            process: Some(process),
            ready,
            record,
            assigned: Vec::new(),
        })
    }

    // --- the door --------------------------------------------------------

    pub(crate) fn request(
        &mut self,
        request: Control,
        label: Option<&str>,
    ) -> Result<Value, String> {
        self.record.actions.push(format!(
            "door {}",
            serde_json::to_string(&request).unwrap_or_default()
        ));
        let reply = door(self.ready.control, &request, DOOR)?;
        if let Some(label) = label {
            self.record
                .observations
                .insert(label.to_owned(), reply.clone());
        }
        Ok(reply)
    }

    pub(crate) fn chat(
        &mut self,
        host: &str,
        agent: &str,
        label: Option<&str>,
    ) -> Result<Value, String> {
        self.request(
            Control::Chat {
                host: host.to_owned(),
                agent: agent.to_owned(),
            },
            label,
        )
    }

    pub(crate) fn wait_chat(
        &mut self,
        host: &str,
        agent: &str,
        ready: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value, String> {
        let control = self.ready.control;
        let request = Control::Chat {
            host: host.to_owned(),
            agent: agent.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || door(control, &request, DOOR),
            |chat| ready(chat),
        ) {
            Ok(chat) => {
                self.record
                    .observations
                    .insert(description.to_owned(), chat.clone());
                self.record
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

    pub(crate) fn inventory(
        &mut self,
        host: &str,
        label: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let reply = self.request(
            Control::Inventory {
                host: host.to_owned(),
            },
            label,
        )?;
        Ok(reply["agents"].as_array().cloned().unwrap_or_default())
    }

    pub(crate) fn wait_inventory(
        &mut self,
        host: &str,
        ready: impl Fn(&[Value]) -> bool,
        description: &str,
    ) -> Result<Vec<Value>, String> {
        let control = self.ready.control;
        let request = Control::Inventory {
            host: host.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || {
                door(control, &request, DOOR)
                    .map(|reply| reply["agents"].as_array().cloned().unwrap_or_default())
            },
            |agents| ready(agents),
        ) {
            Ok(agents) => {
                self.record
                    .observations
                    .insert(description.to_owned(), Value::Array(agents.clone()));
                self.record
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

    pub(crate) fn host_id(&self, host: &str) -> String {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.host_id.to_string())
            .unwrap_or_default()
    }

    pub(crate) fn provider_input(
        &mut self,
        agent: &str,
        label: &str,
    ) -> Result<Vec<String>, String> {
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

    fn config(&self, host: &str) -> PathBuf {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.config.clone())
            .unwrap_or_default()
    }

    /// Adds top-level settings to `host`'s installation config, before a
    /// client starts there.
    pub(crate) fn configure(&mut self, host: &str, yaml: &str) -> Result<(), String> {
        use std::io::Write as _;
        let mut config = std::fs::OpenOptions::new()
            .append(true)
            .open(self.config(host))
            .map_err(|error| error.to_string())?;
        writeln!(config, "{}", yaml.trim_end_matches('\n')).map_err(|error| error.to_string())?;
        self.record
            .actions
            .push(format!("configure {host}: {:?}", yaml.trim()));
        Ok(())
    }

    // --- the terminal ----------------------------------------------------

    fn tmux(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&self.server)
            .args(args)
            .output()
            .map_err(|error| format!("tmux: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "tmux {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The real client against `host`'s install, in a pane of its own.
    pub(crate) fn launch(
        &mut self,
        pane: &str,
        host: &str,
        args: &[&str],
    ) -> Result<String, String> {
        let amux = std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name("amux");
        let mut words = vec![
            "env".to_owned(),
            "-u".to_owned(),
            "AMUX_LOG".to_owned(),
            "-u".to_owned(),
            "AMUX_CONFIG".to_owned(),
            "-u".to_owned(),
            "NO_COLOR".to_owned(),
            "TERM=xterm-256color".to_owned(),
            // Goldens are approved in ANSI colour, which every terminal the
            // client meets can show.
            "COLORTERM=".to_owned(),
            shell_word(&amux.display().to_string()),
            "--config".to_owned(),
            shell_word(&self.config(host).display().to_string()),
        ];
        words.extend(args.iter().map(|arg| shell_word(arg)));
        let held = format!(
            "{}; status=$?; echo AMUX_EXIT_$status; sleep 600",
            words.join(" ")
        );
        // In the host's work directory, as a person starts amux in a
        // project: a new agent works where the client was started.
        let work = self.ready.root.join(host).join("work");
        let directory = if work.is_dir() { work } else { root() };
        let (cols, rows) = (COLS.to_string(), ROWS.to_string());
        let directory = directory.display().to_string();
        self.tmux(&[
            "new-session",
            "-d",
            "-x",
            &cols,
            "-y",
            &rows,
            "-s",
            pane,
            "-c",
            &directory,
            "sh",
            "-c",
            &held,
        ])?;
        self.record
            .actions
            .push(format!("launch {pane} on {host}: amux {}", args.join(" ")));
        Ok(pane.to_owned())
    }

    pub(crate) fn capture(&self, pane: &str) -> Result<String, String> {
        self.tmux(&["capture-pane", "-p", "-t", pane])
    }

    pub(crate) fn wait_for(
        &mut self,
        pane: &str,
        ready: impl Fn(&str) -> bool,
        description: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        let mut last = String::new();
        while Instant::now() < deadline {
            last = self.capture(pane)?;
            if ready(&last) {
                self.record
                    .actions
                    .push(format!("{pane}: reached {description}"));
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "timed out waiting for {description}; final frame:\n{last}"
        ))
    }

    pub(crate) fn wait(
        &mut self,
        pane: &str,
        ready: impl Fn(&str) -> bool,
        description: &str,
    ) -> Result<String, String> {
        self.wait_for(pane, ready, description, Duration::from_secs(60))
    }

    pub(crate) fn wait_terms(&mut self, pane: &str, terms: &[&str]) -> Result<String, String> {
        self.wait_terms_for(pane, terms, Duration::from_secs(60))
    }

    pub(crate) fn wait_terms_for(
        &mut self,
        pane: &str,
        terms: &[&str],
        timeout: Duration,
    ) -> Result<String, String> {
        self.wait_for(
            pane,
            |frame| terms.iter().all(|term| frame.contains(term)),
            &format!("{terms:?}"),
            timeout,
        )
    }

    /// Waits for an ask showing `terms`, then for as long as a person takes
    /// to read it: for half a second after an ask is first drawn the
    /// terminal client sends keys to the draft (`ASK_ARMS_MS`), so a key
    /// sent at once would not answer.
    pub(crate) fn read_ask(&mut self, pane: &str, terms: &[&str]) -> Result<String, String> {
        let frame = self.wait_terms(pane, terms)?;
        std::thread::sleep(ASK_ARMS);
        self.record.actions.push(format!("{pane}: read the ask"));
        Ok(frame)
    }

    pub(crate) fn keys(&mut self, pane: &str, keys: &[&str]) -> Result<(), String> {
        for key in keys {
            self.tmux(&["send-keys", "-t", pane, key])?;
            // tmux sends back-to-back Escape and a letter as Alt+letter.
            std::thread::sleep(Duration::from_millis(150));
        }
        self.record
            .actions
            .push(format!("{pane}: keys {}", keys.join(" ")));
        Ok(())
    }

    pub(crate) fn type_text(&mut self, pane: &str, value: &str) -> Result<(), String> {
        self.tmux(&["send-keys", "-t", pane, "-l", value])?;
        self.record.actions.push(format!("{pane}: type {value:?}"));
        Ok(())
    }

    /// A bracketed paste, as a terminal delivers one.
    pub(crate) fn paste(&mut self, pane: &str, value: &str) -> Result<(), String> {
        self.tmux(&["set-buffer", "-b", "journey", value])?;
        self.tmux(&["paste-buffer", "-p", "-d", "-b", "journey", "-t", pane])?;
        self.record
            .actions
            .push(format!("{pane}: paste {} lines", value.lines().count()));
        Ok(())
    }

    /// Moves home's selection onto `agent`: the row inside the band.
    pub(crate) fn select_agent(&mut self, pane: &str, agent: &str) -> Result<String, String> {
        self.wait(
            pane,
            |frame| agent_row(frame, agent).is_some(),
            &format!("{agent} on home"),
        )?;
        self.keys(pane, &["g"])?;
        for _ in 0..40 {
            let frame = self.capture(pane)?;
            if selected_row(&frame, agent) {
                self.record
                    .actions
                    .push(format!("{pane}: selected {agent}"));
                return Ok(frame);
            }
            self.keys(pane, &["Down"])?;
        }
        Err(format!(
            "could not select {agent}; final frame:\n{}",
            self.capture(pane)?
        ))
    }

    /// Unfolds home's Exited section, the last thing on home while it is
    /// folded.
    pub(crate) fn open_exited(&mut self, pane: &str) -> Result<String, String> {
        self.keys(pane, &["G"])?;
        self.wait_terms(pane, &["▸ Exited ", "enter show"])?;
        self.keys(pane, &["Enter"])?;
        self.wait_terms(pane, &["▾ Exited "])
    }

    pub(crate) fn open_chat(&mut self, pane: &str, agent: &str) -> Result<String, String> {
        self.select_agent(pane, agent)?;
        self.keys(pane, &["Enter"])?;
        self.wait(
            pane,
            |frame| chat_of(frame).as_deref() == Some(agent),
            &format!("{agent}'s chat"),
        )
    }

    /// Back to home from a chat.
    pub(crate) fn home(&mut self, pane: &str) -> Result<String, String> {
        if !at_home(&self.capture(pane)?) {
            self.keys(pane, &["C-a", "s"])?;
        }
        self.wait(pane, at_home, "home")
    }

    /// Text and styles of what the pane shows now, compared with its
    /// reviewed golden.
    pub(crate) fn frame(&mut self, pane: &str, label: &str) -> Result<(), String> {
        let dump = self.tmux(&["capture-pane", "-p", "-e", "-t", pane])?;
        let (texts, styles) = parse_styled(dump.strip_suffix('\n').unwrap_or(&dump), COLS);
        let prefix = prefix();
        let tail = scratch_root().display().to_string()[prefix.len()..].to_owned();
        let mut texts: Vec<String> = texts
            .iter()
            .map(|text| spell_root(text, &prefix, SPELLED_PREFIX, &tail))
            .collect();
        let mut styles = styles;
        for name in &self.assigned {
            (texts, styles) = respell(texts, styles, name, ASSIGNED);
        }
        let (texts, styles) = mask_durations(texts, styles);
        let text = normalize(&(texts.join("\n") + "\n"));
        let styles = style_map(&styles);
        let actual = self.output.join("actual");
        std::fs::create_dir_all(&actual).map_err(|error| error.to_string())?;
        std::fs::write(actual.join(format!("{label}.txt")), &text)
            .map_err(|error| error.to_string())?;
        std::fs::write(actual.join(format!("{label}.styles")), &styles)
            .map_err(|error| error.to_string())?;
        self.record.actions.push(format!("captured {label}"));
        let goldens = root().join("journeys/goldens/terminal").join(&self.name);
        compare_golden(&goldens.join(format!("{label}.txt")), &text)?;
        compare_golden(&goldens.join(format!("{label}.styles")), &styles)
    }

    /// Back home if a chat is open, then q; the client must say it exited
    /// cleanly.
    pub(crate) fn quit_client(&mut self, pane: &str) -> Result<(), String> {
        self.home(pane)?;
        self.keys(pane, &["q"])?;
        self.wait_terms_for(pane, &["AMUX_EXIT_0"], Duration::from_secs(30))?;
        let _ = self.tmux(&["kill-session", "-t", pane]);
        self.record.actions.push(format!("{pane}: exited 0"));
        Ok(())
    }

    // --- the end ---------------------------------------------------------

    pub(crate) fn finish(&self, proved: &[String]) {
        let lines: String = proved.iter().map(|line| format!("- {line}\n")).collect();
        self.record.write(&self.output, &format!("PASS\n{lines}"));
    }

    pub(crate) fn fail(&self, error: &str) {
        self.record
            .write(&self.output, &format!("FAIL\n- {error}\n"));
    }

    pub(crate) fn close(mut self) -> Result<(), String> {
        let result = self.shut_down();
        let _ = std::fs::write(
            self.output.join("actions.txt"),
            self.record.actions.join("\n") + "\n",
        );
        let _ = std::fs::remove_dir_all(&self.scratch);
        result
    }

    fn shut_down(&mut self) -> Result<(), String> {
        // A private tmux server: killing it touches no one else's panes.
        let _ = self.tmux(&["kill-server"]);
        if let Some(mut process) = self.process.take()
            && process.try_wait().ok().flatten().is_none()
        {
            let _ = door(
                self.ready.control,
                &Control::Shutdown,
                Duration::from_secs(10),
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while process.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    let _ = process.kill();
                    let _ = process.wait();
                    return Err("testnet did not shut down within a minute".to_owned());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let closed = door(
            self.ready.control,
            &Control::Inventory {
                host: "desk".to_owned(),
            },
            Duration::from_secs(1),
        );
        if closed.is_ok() {
            return Err("the door stayed open after teardown".to_owned());
        }
        self.record
            .actions
            .push("teardown: the door is closed".to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_is_one_width_on_every_platform() {
        assert_eq!(prefix().len(), SPELLED_PREFIX.len());
    }

    #[test]
    fn home_and_chat_rows_are_read_from_the_frame() {
        let frame = "  amux  1 working\n\n  › ● keeper  Claude\n    ○ helper ▸ 2\n";
        assert!(at_home(frame));
        assert_eq!(agent_row(frame, "keeper"), Some(2));
        assert_eq!(agent_row(frame, "helper"), Some(3));
        assert_eq!(agent_row(frame, "help"), None);
        assert!(selected_row(frame, "keeper"));
        assert!(!selected_row(frame, "helper"));
        assert_eq!(
            chat_of("  keeper │ desk │ Claude\n").as_deref(),
            Some("keeper")
        );
        assert_eq!(chat_of("   keeper │ desk\n"), None);
    }
}
