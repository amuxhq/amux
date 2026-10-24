//! Raw attach: this terminal handed to an agent's own interface, over the
//! pty.sock in the agent's directory on this machine.
//!
//! Nothing on the wire carries terminal bytes, so only agents of this
//! machine's profile host can be attached, found by the directory
//! convention `<profile dir>/agents/<agent id>/pty.sock`. An agent on
//! another host, or headless Claude with no terminal, is a chat.
//!
//! A connection is attached for as long as it is held: the fleet is an
//! overlay over it. Leaving for the fleet keeps the connection and a screen
//! model fed by it, so coming back repaints the same process where it was
//! rather than starting a new view; picking another agent opens a second
//! connection; the process ending closes them all.

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent::attach::{Attached, AttachedInput, AttachedOutput};
use anyhow::{Context as _, Result, anyhow};
use settings::LeaderKey;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use wire::{Agent, Kind, Lifecycle, ProfileInfo};

/// How a stretch of raw attach ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// `<leader> d`: back to the shell, the agent untouched.
    Detached,
    /// `<leader> s`: to the fleet, the connection kept.
    Fleet,
    /// The agent ended the connection, saying why when it did.
    Ended(Option<String>),
}

/// This process's raw attach connections, by agent id.
pub struct Attacher {
    agents: PathBuf,
    local_host: Vec<u8>,
    leader: LeaderKey,
    held: HashMap<Vec<u8>, Held>,
}

impl Attacher {
    pub fn new(profile: &ProfileInfo, leader: LeaderKey) -> Result<Attacher> {
        let socket = Path::new(&profile.socket_path);
        let dir = socket
            .parent()
            .ok_or_else(|| anyhow!("the profile socket {} has no directory", socket.display()))?;
        // A profile's host id is its id.
        Uuid::from_slice(&profile.id).context("the profile's id")?;
        Ok(Attacher {
            agents: dir.join("agents"),
            local_host: profile.id.clone(),
            leader,
            held: HashMap::new(),
        })
    }

    /// The host this machine's agents run on, in the inventory's bytes.
    pub fn local_host(&self) -> &[u8] {
        &self.local_host
    }

    /// Why this agent's terminal cannot be attached here, pointing at its
    /// chat, which reaches every agent.
    pub fn refusal(&self, agent: &Agent) -> Option<String> {
        let name = agent.name.clone();
        if agent.host_id != self.local_host {
            Some(format!(
                "{name} runs on another host, and raw attach is only for agents on this \
                 machine; open its chat instead: run `amux` and press enter on it"
            ))
        } else if agent.lifecycle() == Lifecycle::Exited {
            Some(format!(
                "{name} is stopped, so it has no terminal to attach to; start it again with \
                 `amux resume {name}`"
            ))
        } else if agent.kind() == Kind::ClaudeSdk {
            Some(format!(
                "{name} is headless Claude and has no terminal; open its chat instead: run \
                 `amux` and press enter on it"
            ))
        } else if agent.kind() == Kind::Codex && cfg!(windows) {
            Some(format!(
                "attach is not available for Codex on Windows, where {name}'s Codex has room \
                 for amux alone; open its chat instead: run `amux` and press enter on it"
            ))
        } else {
            None
        }
    }

    /// Runs the passthrough on this terminal until a detach, the fleet
    /// chord, or the agent ending the connection. The terminal is restored
    /// whichever way it ends.
    pub async fn attach(&mut self, agent: &Agent) -> Result<Outcome> {
        if let Some(why) = self.refusal(agent) {
            return Err(anyhow!(why));
        }
        let id = agent.agent_id.clone();
        if self
            .held
            .get(&id)
            .is_some_and(|held| held.ended.borrow().is_some())
        {
            self.held.remove(&id);
        }
        let returning = self.held.contains_key(&id);
        if !returning {
            let uuid = Uuid::from_slice(&id).context("the agent's id")?;
            let dir = self.agents.join(uuid.to_string());
            let attached = Attached::connect(&dir).await.with_context(|| {
                format!(
                    "attaching to {}'s terminal at {}",
                    agent.name.clone(),
                    dir.join(agent::PTY_SOCK).display()
                )
            })?;
            let (rows, cols) = terminal_size();
            self.held
                .insert(id.clone(), Held::start(attached, rows, cols));
        }
        let held = &self.held[&id];
        let outcome = passthrough(held, &self.leader, returning).await;
        if !matches!(outcome, Ok(Outcome::Fleet)) {
            self.held.remove(&id);
        }
        outcome
    }
}

/// What the passthrough says on the way back to the shell.
pub fn farewell(agent: &Agent, outcome: &Outcome) -> String {
    let name = agent.name.clone();
    match outcome {
        Outcome::Detached | Outcome::Fleet => format!("[detached from {name}]"),
        Outcome::Ended(Some(why)) => format!("[{name}: {why}]"),
        Outcome::Ended(None) => format!("[{name} ended]"),
    }
}

fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size()
        .map(|(cols, rows)| (rows.max(1), cols.max(1)))
        .unwrap_or((24, 80))
}

/// The agent's screen as this client last drew it, and whether it is on
/// the terminal now.
struct Screen {
    parser: vt100::Parser,
    live: bool,
    /// What the agent drew before the terminal first went live, written
    /// as-is when it does. The connection is read from the moment it is
    /// held, and its first read is the agent's history.
    unshown: Option<Vec<u8>>,
}

impl Screen {
    fn draw(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        if self.live {
            let mut out = io::stdout().lock();
            let _ = out.write_all(bytes);
            let _ = out.flush();
        } else if let Some(unshown) = &mut self.unshown {
            unshown.extend_from_slice(bytes);
        }
    }
}

enum Typed {
    Keys(Vec<u8>),
    Resize(u16, u16),
}

/// One held connection: a task reads what the agent draws into the screen
/// and sends what is typed, until the agent ends it or the hold is dropped.
struct Held {
    screen: Arc<Mutex<Screen>>,
    typed: mpsc::UnboundedSender<Typed>,
    /// Set once the connection is over: why, when the agent said.
    ended: watch::Receiver<Option<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Held {
    fn start(attached: Attached, rows: u16, cols: u16) -> Held {
        let screen = Arc::new(Mutex::new(Screen {
            parser: vt100::Parser::new(rows, cols, 0),
            live: false,
            unshown: Some(Vec::new()),
        }));
        let (typed, receiver) = mpsc::unbounded_channel();
        let (ended_tx, ended) = watch::channel(None);
        let (output, input) = attached.split();
        let task = tokio::spawn(pump(output, input, screen.clone(), receiver, ended_tx));
        Held {
            screen,
            typed,
            ended,
            task,
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.screen.lock().unwrap().parser.set_size(rows, cols);
        let _ = self.typed.send(Typed::Resize(rows, cols));
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pump(
    mut output: AttachedOutput,
    mut input: AttachedInput,
    screen: Arc<Mutex<Screen>>,
    mut typed: mpsc::UnboundedReceiver<Typed>,
    ended: watch::Sender<Option<Option<String>>>,
) {
    let reading = async {
        while let Ok(Some(bytes)) = output.next().await {
            screen.lock().unwrap().draw(&bytes);
        }
    };
    let typing = async {
        while let Some(typed) = typed.recv().await {
            let sent = match typed {
                Typed::Keys(keys) => input.keys(&keys).await,
                Typed::Resize(rows, cols) => input.resize(rows, cols).await,
            };
            if sent.is_err() {
                return;
            }
        }
        std::future::pending::<()>().await;
    };
    tokio::select! {
        () = reading => {}
        () = typing => {}
    }
    let _ = ended.send(Some(output.closed().map(str::to_owned)));
}

async fn passthrough(held: &Held, leader: &LeaderKey, returning: bool) -> Result<Outcome> {
    let raw = RawMode::enter()?;
    // Listen for window changes before reading the size and drawing: a
    // terminal resized once it shows the agent's screen must reach the
    // agent, and a signal with no listener yet is lost.
    #[cfg(unix)]
    let mut resized =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let (rows, cols) = terminal_size();
    {
        let mut screen = held.screen.lock().unwrap();
        screen.parser.set_size(rows, cols);
        let unshown = screen.unshown.take();
        let mut out = io::stdout().lock();
        // The agent draws on the alternate screen, left on the way out
        // (RESTORE_BYTES), so leaving gives the shell its own screen back
        // instead of the agent's last frame in its scrollback.
        let _ = out.write_all(ENTER_ALTERNATE_SCREEN);
        if returning {
            // The fleet drew over the terminal: put back the agent's screen
            // as the model holds it, with the modes it had switched on.
            let _ = out.write_all(b"\x1b[H\x1b[2J");
            let _ = out.write_all(&screen.parser.screen().state_formatted());
        } else if let Some(unshown) = unshown {
            let _ = out.write_all(&unshown);
        }
        let _ = out.flush();
        screen.live = true;
    }
    let _ = held.typed.send(Typed::Resize(rows, cols));
    let mut keys = read_keys(Chords::new(leader));
    let mut ended = held.ended.clone();
    let outcome = loop {
        #[cfg(unix)]
        let window = resized.recv();
        #[cfg(not(unix))]
        let window = std::future::pending::<Option<()>>();
        tokio::select! {
            key = keys.recv() => match key {
                Some(Scanned::Keys(bytes)) => {
                    let _ = held.typed.send(Typed::Keys(bytes));
                }
                Some(Scanned::Detach) | None => break Outcome::Detached,
                Some(Scanned::Fleet) => break Outcome::Fleet,
            },
            Some(()) = window => {
                let (rows, cols) = terminal_size();
                held.resize(rows, cols);
            }
            why = ended.wait_for(Option::is_some) => {
                break Outcome::Ended(why.ok().and_then(|why| why.clone()).flatten());
            }
        }
    };
    held.screen.lock().unwrap().live = false;
    drop(raw);
    // The agent's bytes went to the terminal verbatim, so it may have left
    // mouse reporting, bracketed paste, a hidden cursor or the alternate
    // screen switched on; its own restore arrives only while attached.
    let mut out = io::stdout();
    let _ = out.write_all(tui::RESTORE_BYTES);
    let _ = out.flush();
    Ok(outcome)
}

const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";

struct RawMode;

impl RawMode {
    fn enter() -> io::Result<RawMode> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Reads stdin on its own thread until a chord or the end of input. The
/// thread stops reading after a chord, so the fleet's own reader gets the
/// next key; after the agent ends the connection it stays parked in a read
/// until the process exits, which is what attach does next.
fn read_keys(mut chords: Chords) -> mpsc::UnboundedReceiver<Scanned> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        loop {
            let read = match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            for scanned in chords.feed(&buffer[..read]) {
                let chord = !matches!(scanned, Scanned::Keys(_));
                if tx.send(scanned).is_err() || chord {
                    return;
                }
            }
        }
    });
    rx
}

/// What the person typed, with the leader's chords taken out.
#[derive(Debug, PartialEq)]
enum Scanned {
    Keys(Vec<u8>),
    Detach,
    Fleet,
}

/// Finds `<leader> d` and `<leader> s` in typed bytes. The leader arrives
/// as its control byte, as its CSI u form when the agent has switched on
/// the kitty keyboard protocol, or as `CSI 27 ; 5 ; code ~` when it has
/// switched on xterm's modifyOtherKeys (terminal Claude turns on both, and
/// a terminal that honours the latter, tmux with extended keys among them,
/// sends that form); a leader followed by anything else goes to the agent
/// as typed.
struct Chords {
    raw: u8,
    csi_u: Vec<u8>,
    modify_other: Vec<u8>,
    /// A leader seen at the end of the last read, as it was typed.
    pending: Option<Vec<u8>>,
}

impl Chords {
    fn new(leader: &LeaderKey) -> Chords {
        Chords {
            raw: leader.raw_byte(),
            csi_u: leader.csi_u_sequence(),
            modify_other: leader.modify_other_keys_sequence(),
            pending: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Scanned> {
        let mut out = Vec::new();
        let mut keys = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if let Some(leader) = self.pending.take() {
                match bytes[at] {
                    b'd' | b's' => {
                        if !keys.is_empty() {
                            out.push(Scanned::Keys(std::mem::take(&mut keys)));
                        }
                        out.push(if bytes[at] == b'd' {
                            Scanned::Detach
                        } else {
                            Scanned::Fleet
                        });
                        return out;
                    }
                    _ => keys.extend_from_slice(&leader),
                }
                continue;
            }
            if bytes[at] == self.raw {
                self.pending = Some(vec![self.raw]);
                at += 1;
            } else if bytes[at..].starts_with(&self.csi_u) {
                self.pending = Some(self.csi_u.clone());
                at += self.csi_u.len();
            } else if bytes[at..].starts_with(&self.modify_other) {
                self.pending = Some(self.modify_other.clone());
                at += self.modify_other.len();
            } else {
                keys.push(bytes[at]);
                at += 1;
            }
        }
        if !keys.is_empty() {
            out.push(Scanned::Keys(keys));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chords() -> Chords {
        Chords::new(&LeaderKey::default())
    }

    #[test]
    fn a_leader_chord_ends_the_keys_and_says_where_to_go() {
        let mut scan = chords();
        assert_eq!(
            scan.feed(b"ls\r\x01d more"),
            vec![Scanned::Keys(b"ls\r".to_vec()), Scanned::Detach]
        );
        let mut scan = chords();
        assert_eq!(scan.feed(b"\x01s"), vec![Scanned::Fleet]);
    }

    #[test]
    fn a_leader_split_across_reads_still_makes_a_chord() {
        let mut scan = chords();
        assert_eq!(scan.feed(b"a\x01"), vec![Scanned::Keys(b"a".to_vec())]);
        assert_eq!(scan.feed(b"s"), vec![Scanned::Fleet]);
    }

    #[test]
    fn the_kitty_form_of_the_leader_is_a_leader_too() {
        let mut scan = chords();
        assert_eq!(scan.feed(b"\x1b[97;5ud"), vec![Scanned::Detach]);
    }

    #[test]
    fn a_leader_before_any_other_key_reaches_the_agent_as_typed() {
        let mut scan = chords();
        assert_eq!(scan.feed(b"\x01x"), vec![Scanned::Keys(b"\x01x".to_vec())]);
        assert_eq!(
            scan.feed(b"\x1b[97;5uq"),
            vec![Scanned::Keys(b"\x1b[97;5uq".to_vec())]
        );
    }

    /// Terminal Claude (2.1.296, recorded in a QA run) switches on xterm's
    /// modifyOtherKeys at level 2 (`CSI > 4 ; 2 m`) beside the kitty
    /// protocol. A terminal that honours it, such as xterm or tmux with
    /// `extended-keys on`, then sends Ctrl+A as `CSI 27 ; 5 ; 97 ~`: the
    /// recorded attach passed exactly those bytes on to Claude, and the
    /// person could not leave.
    #[test]
    fn the_modify_other_keys_form_of_the_leader_is_a_leader_too() {
        let mut scan = chords();
        assert_eq!(scan.feed(b"\x1b[27;5;97~d"), vec![Scanned::Detach]);
        let mut scan = chords();
        assert_eq!(scan.feed(b"\x1b[27;5;97~s"), vec![Scanned::Fleet]);
    }

    /// The modes terminal Claude switches on as it draws, from the same
    /// recording: bracketed paste, the kitty flags replaced by its own,
    /// and modifyOtherKeys at level 2.
    const CLAUDE_KEYBOARD_MODES: &[u8] = b"\x1b[?2004h\x1b[<u\x1b[>5u\x1b[>4;2m";

    /// The modifyOtherKeys level a terminal is left at by `bytes`: the
    /// last `CSI > 4 ; n m` sets it, `CSI > 4 m` resets it to 0.
    fn modify_other_keys_level(bytes: &[u8]) -> u8 {
        let mut level = 0;
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(4).position(|w| w == b"\x1b[>4") {
            let start = at + found + 4;
            let end = start
                + bytes[start..]
                    .iter()
                    .position(|&b| b == b'm')
                    .expect("a terminated sequence");
            level = match &bytes[start..end] {
                [] => 0,
                [b';', digits @ ..] => std::str::from_utf8(digits).unwrap().parse().unwrap(),
                _ => level,
            };
            at = end;
        }
        level
    }

    /// Leaving raw attach must leave the terminal as amux's own client
    /// reads it. With modifyOtherKeys still at 2, every Ctrl chord arrives
    /// as `CSI 27 ; 5 ; <key> ~`, which the client's key reader drops, so
    /// Ctrl+U, Ctrl+C and the leader do nothing after a visit to Claude.
    #[test]
    fn leaving_raw_attach_switches_off_modify_other_keys() {
        let mut terminal = CLAUDE_KEYBOARD_MODES.to_vec();
        assert_eq!(modify_other_keys_level(&terminal), 2);
        terminal.extend_from_slice(tui::RESTORE_BYTES);
        assert_eq!(
            modify_other_keys_level(&terminal),
            0,
            "the bytes written on leaving attach leave modifyOtherKeys on: {:?}",
            String::from_utf8_lossy(tui::RESTORE_BYTES)
        );
    }

    fn profile() -> ProfileInfo {
        ProfileInfo {
            socket_path: "/data/profiles/p/client.sock".into(),
            id: vec![7; 16],
            ..ProfileInfo::default()
        }
    }

    #[test]
    fn only_this_machines_terminals_attach_and_the_rest_point_at_the_chat() {
        let attacher = Attacher::new(&profile(), LeaderKey::default()).unwrap();
        assert_eq!(attacher.agents, Path::new("/data/profiles/p/agents"));
        let agent = |host: [u8; 16], kind: Kind| Agent {
            agent_id: vec![1; 16],
            host_id: host.to_vec(),
            kind: kind as i32,
            name: "scout".into(),
            ..Agent::default()
        };
        assert_eq!(attacher.refusal(&agent([7; 16], Kind::ClaudePty)), None);
        let codex = attacher.refusal(&agent([7; 16], Kind::Codex));
        if cfg!(windows) {
            let codex = codex.unwrap();
            assert!(
                codex.contains("not available for Codex on Windows"),
                "{codex}"
            );
            assert!(codex.contains("open its chat"), "{codex}");
        } else {
            assert_eq!(codex, None);
        }
        let remote = attacher.refusal(&agent([8; 16], Kind::ClaudePty)).unwrap();
        assert!(remote.contains("another host"), "{remote}");
        assert!(remote.contains("open its chat"), "{remote}");
        let headless = attacher.refusal(&agent([7; 16], Kind::ClaudeSdk)).unwrap();
        assert!(headless.contains("open its chat"), "{headless}");
    }

    /// An exited agent has no terminal to attach to: the refusal says it
    /// is stopped in words, as the other refusals do, before any socket is
    /// tried. Attaching to one printed the socket's path and "No such file
    /// or directory (os error 2)".
    #[test]
    fn a_stopped_agents_terminal_is_refused_in_words_before_any_socket() {
        let attacher = Attacher::new(&profile(), LeaderKey::default()).unwrap();
        for kind in [Kind::ClaudePty, Kind::Codex] {
            let stopped = Agent {
                agent_id: vec![1; 16],
                host_id: vec![7; 16],
                kind: kind as i32,
                name: "pt".into(),
                lifecycle: wire::Lifecycle::Exited as i32,
                ..Agent::default()
            };
            let refusal = attacher.refusal(&stopped);
            assert!(
                refusal
                    .as_deref()
                    .is_some_and(|why| why.contains("stopped")),
                "attaching to the stopped {kind:?} agent pt is not refused as stopped: {refusal:?}"
            );
        }
    }
}
