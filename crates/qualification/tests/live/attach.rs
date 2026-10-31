//! Codex's own app attached to a live amux agent, co-driving it with amux.
//!
//! Two real terminals: the amux client with the agent's chat open, and
//! `amux attach` running Codex's app as one more client of the agent's app
//! server. The agent has run no turn when the app attaches. A prompt typed
//! in the app must show in amux's chat, and the approval it leads to is
//! answered from amux's chat; the app sees it resolved and the command's
//! effect exists.
//!
//! Every byte each terminal was written is kept, timed, as an asciicast
//! (`app.cast`, `amux.cast`) and raw (`app.raw`, `amux.raw`); each step
//! saves the text both screens showed then under `frames/`; `steps.txt` is
//! the timeline and `verdict.txt` the result. All of it goes to
//! `target/live/codex-attach/`, emptied at the start of a run.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pty_host::{PtyProcess, PtySize, PtySpawn};
use wire::{Kind, Phase};

use super::{Install, Log, Scenario, Verdict, item_token};

const ROWS: u16 = 40;
const COLS: u16 = 120;
/// The leader key, then detach.
const DETACH: &[u8] = b"\x01d";
/// The leader key, then the fleet.
const FLEET: &[u8] = b"\x01s";
/// How long a terminal may take to draw what a step waits for.
const DRAW: Duration = Duration::from_secs(60);

/// The prompt typed into Codex's app: a command the read-only sandbox
/// makes Codex ask about, told so, or the model may answer without trying.
const PROMPT: &str = "Run exactly this shell command once, with your shell tool, and nothing \
     else: touch attached.txt. The sandbox is read-only, so request escalated permissions for \
     it; I will approve. Then reply with the single word done.";
/// A word of the prompt no terminal wraps.
const PROMPT_MARK: &str = "attached.txt";

/// Where a run's capture goes: `target/live/codex-attach`, next to the
/// test binary's own target directory.
pub fn capture_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    // target/<profile>/deps/<binary>
    let target = exe
        .ancestors()
        .nth(3)
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("target"));
    target.join("live").join("codex-attach")
}

/// Writes the verdict beside the capture, as the run's last word.
pub fn write_verdict(verdict: &Verdict) {
    let dir = capture_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("verdict.txt"), format!("{verdict}\n"));
}

/// One real terminal: the program in it, everything it wrote, timed, and
/// the screen a terminal would show.
struct Screen {
    name: &'static str,
    process: PtyProcess,
    output: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    parser: vt100::Parser,
    raw: std::fs::File,
    cast: std::fs::File,
    /// The tail of a chunk that ended inside a UTF-8 character.
    partial: Vec<u8>,
}

impl Screen {
    fn open(
        name: &'static str,
        dir: &Path,
        install: &Install,
        args: &[&str],
        cwd: &Path,
    ) -> Result<Screen, String> {
        let process = pty_host::spawn(PtySpawn {
            command: install.amux.clone(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: cwd.to_owned(),
            env: install
                .env
                .iter()
                .map(|(key, value)| (key.into(), value.clone()))
                .chain([
                    ("TERM".into(), "xterm-256color".into()),
                    ("COLORTERM".into(), "".into()),
                ])
                .collect(),
            env_remove: vec!["AMUX_LOG".into()],
            size: PtySize {
                rows: ROWS,
                cols: COLS,
            },
        })
        .map_err(|error| format!("{name}: {error}"))?;
        let output = process.handle.output();
        let raw = std::fs::File::create(dir.join(format!("{name}.raw")))
            .map_err(|error| error.to_string())?;
        let mut cast = std::fs::File::create(dir.join(format!("{name}.cast")))
            .map_err(|error| error.to_string())?;
        let header = serde_json::json!({
            "version": 2,
            "width": COLS,
            "height": ROWS,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|now| now.as_secs())
                .unwrap_or_default(),
            "title": format!("amux {}", args.join(" ")),
            "env": {"TERM": "xterm-256color"},
        });
        writeln!(cast, "{header}").map_err(|error| error.to_string())?;
        Ok(Screen {
            name,
            process,
            output,
            parser: vt100::Parser::new(ROWS, COLS, 0),
            raw,
            cast,
            partial: Vec::new(),
        })
    }

    fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    fn shows(&self, text: &str) -> bool {
        self.contents().contains(text)
    }

    fn event(&mut self, at: Duration, code: &str, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.partial) {
            Ok(_) => self.partial.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.partial.len(),
        };
        let text = String::from_utf8_lossy(&self.partial[..valid]).into_owned();
        self.partial.drain(..valid);
        if !text.is_empty() {
            let line = serde_json::json!([at.as_secs_f64(), code, text]);
            let _ = writeln!(self.cast, "{line}");
        }
    }

    /// Takes what the program wrote so far, answering the queries a
    /// terminal answers: the cursor position, the device attributes and
    /// the default colours. The keyboard-protocol query goes unanswered,
    /// as a terminal without it leaves it.
    async fn pump(&mut self, clock: Instant) -> Result<(), String> {
        loop {
            let bytes = match self.output.try_recv() {
                Ok(bytes) => bytes,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return Ok(()),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    return Err(format!(
                        "{} closed its terminal; it showed:\n{}",
                        self.name,
                        self.contents()
                    ));
                }
            };
            let _ = self.raw.write_all(&bytes);
            self.event(clock.elapsed(), "o", &bytes);
            self.parser.process(&bytes);
            // In the order asked, as a terminal answers: a program may
            // take the device attributes as the end of its questions.
            let (row, col) = self.parser.screen().cursor_position();
            let cursor = format!("\x1b[{};{}R", row + 1, col + 1).into_bytes();
            let queries: [(&[u8], &[u8]); 5] = [
                (b"\x1b[6n", &cursor),
                (b"\x1b[c", b"\x1b[?1;2c"),
                (b"\x1b[0c", b"\x1b[?1;2c"),
                (b"\x1b]10;?", b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"),
                (b"\x1b]11;?", b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\"),
            ];
            let mut answers: Vec<(usize, Vec<u8>)> = Vec::new();
            for (query, answer) in queries {
                for (at, window) in bytes.windows(query.len()).enumerate() {
                    if window == query {
                        answers.push((at, answer.to_vec()));
                    }
                }
            }
            answers.sort_by_key(|(at, _)| *at);
            for (_, answer) in answers {
                self.type_keys(clock, &answer).await?;
            }
        }
    }

    async fn type_keys(&mut self, clock: Instant, keys: &[u8]) -> Result<(), String> {
        let line = serde_json::json!([
            clock.elapsed().as_secs_f64(),
            "i",
            String::from_utf8_lossy(keys)
        ]);
        let _ = writeln!(self.cast, "{line}");
        self.process
            .handle
            .write(keys)
            .await
            .map_err(|error| format!("typing into {}: {error}", self.name))
    }

    /// Waits for the program to exit and says whether it exited cleanly.
    async fn exits(&mut self, clock: Instant) -> Result<bool, String> {
        let deadline = tokio::time::Instant::now() + DRAW;
        loop {
            let _ = self.pump(clock).await;
            if let Ok(status) =
                tokio::time::timeout(Duration::from_millis(100), self.process.exit.wait()).await
            {
                let _ = self.pump(clock).await;
                return Ok(status.success());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!(
                    "{} did not exit; it shows:\n{}",
                    self.name,
                    self.contents()
                ));
            }
        }
    }
}

/// The program is killed with the scenario, by return or by failure, and
/// its terminal read to the end on a thread of its own: a program cannot
/// finish exiting while its terminal holds output nobody reads.
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self
            .process
            .handle
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
        let (_, closed) = tokio::sync::mpsc::channel(1);
        let mut output = std::mem::replace(&mut self.output, closed);
        std::thread::spawn(move || while output.blocking_recv().is_some() {});
    }
}

/// The two terminals, the agent's committed session and the timeline.
struct Desk<'a> {
    dir: PathBuf,
    clock: Instant,
    amux: Screen,
    app: Option<Screen>,
    follow: &'a super::Follow,
    steps: std::fs::File,
    frames: usize,
}

impl Desk<'_> {
    async fn pump(&mut self) -> Result<(), String> {
        self.amux.pump(self.clock).await?;
        if let Some(app) = &mut self.app {
            app.pump(self.clock).await?;
        }
        Ok(())
    }

    fn app(&mut self) -> &mut Screen {
        self.app.as_mut().expect("the app is attached")
    }

    /// Reads both terminals until `done` holds of them and the session.
    async fn until(
        &mut self,
        what: &str,
        patience: Duration,
        done: impl Fn(&Desk, &Log) -> bool,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            self.pump().await?;
            if done(self, &self.follow.snapshot()) {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                self.frame(&format!("timed out waiting for {what}"));
                return Err(format!(
                    "timed out after {patience:?} waiting for {what}; amux shows:\n{}\nthe app shows:\n{}",
                    self.amux.contents(),
                    self.app.as_ref().map(Screen::contents).unwrap_or_default()
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Reads both terminals for a while, keeping what they draw.
    async fn settle(&mut self, quiet: Duration) -> Result<(), String> {
        let until = tokio::time::Instant::now() + quiet;
        while tokio::time::Instant::now() < until {
            self.pump().await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Saves what both screens show now as the step's frame.
    fn frame(&mut self, step: &str) {
        self.frames += 1;
        let at = self.clock.elapsed().as_secs_f64();
        let slug: String = step
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let frames = self.dir.join("frames");
        let _ = std::fs::create_dir_all(&frames);
        let mut screens = vec![("amux", self.amux.contents())];
        if let Some(app) = &self.app {
            screens.push(("app", app.contents()));
        }
        for (name, contents) in screens {
            let _ = std::fs::write(
                frames.join(format!("{:02}-{slug}.{name}.txt", self.frames)),
                format!("{at:.3}s  {step}  ({name})\n{}\n", rule(&contents)),
            );
        }
        let _ = writeln!(self.steps, "{at:>8.3}s  {:02}  {step}", self.frames);
    }

    async fn type_into_app(&mut self, keys: &[u8]) -> Result<(), String> {
        let clock = self.clock;
        self.app().type_keys(clock, keys).await
    }

    async fn type_into_amux(&mut self, keys: &[u8]) -> Result<(), String> {
        self.amux.type_keys(self.clock, keys).await
    }
}

/// A screen boxed in a rule so its edges read in a text file.
fn rule(contents: &str) -> String {
    let edge = "-".repeat(COLS as usize);
    let mut lines: Vec<String> = contents.lines().map(str::to_owned).collect();
    lines.resize(ROWS as usize, String::new());
    format!("{edge}\n{}\n{edge}", lines.join("\n"))
}

/// Whether home's selection is on the agent's row: `›`, a mark, its name.
fn selected(contents: &str, agent: &str) -> bool {
    contents.lines().any(|line| {
        line.trim_start()
            .strip_prefix("› ")
            .and_then(|rest| rest.split_whitespace().nth(1))
            == Some(agent)
    })
}

/// The agent whose chat amux's client has open: its header's first cell.
fn chat_open(contents: &str, agent: &str) -> bool {
    contents
        .lines()
        .take(3)
        .any(|line| line.starts_with(&format!("  {agent} │ ")))
}

fn prompts(log: &Log) -> Vec<String> {
    log.ordered()
        .into_iter()
        .filter(|item| item_token(Kind::Codex, item).as_deref() == Some("prompt"))
        .map(|item| item.text.clone())
        .collect()
}

impl Install {
    pub(super) async fn attach(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Attach;
        let name = scenario.name();
        let dir = capture_dir();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let agent = self.create_asking_codex(scenario).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        if !follow.snapshot().turns(Kind::Codex).is_empty() {
            return Err("the agent ran a turn before the app attached".into());
        }
        let project = self.project(scenario)?;
        let clock = Instant::now();
        let mut desk = Desk {
            dir: dir.clone(),
            clock,
            amux: Screen::open("amux", &dir, self, &[], &project)?,
            app: None,
            follow: &follow,
            steps: std::fs::File::create(dir.join("steps.txt"))
                .map_err(|error| error.to_string())?,
            frames: 0,
        };

        // amux's client, the agent's chat open.
        desk.until("amux to list the agent", DRAW, |desk, _| {
            desk.amux.shows(name)
        })
        .await?;
        for _ in 0..20 {
            if selected(&desk.amux.contents(), name) {
                break;
            }
            desk.type_into_amux(b"\x1b[B").await?;
            desk.settle(Duration::from_millis(200)).await?;
        }
        desk.type_into_amux(b"\r").await?;
        desk.until("amux to open the agent's chat", DRAW, |desk, _| {
            chat_open(&desk.amux.contents(), name)
        })
        .await?;
        desk.frame("amux has the chat open; the agent has run no turn");

        // Codex's own app, attached before any turn.
        desk.app = Some(Screen::open(
            "app",
            &dir,
            self,
            &["attach", name, "--in", "native"],
            &project,
        )?);
        // The app is ready for a prompt once its composer is drawn and it
        // has stopped drawing.
        let mut last = String::new();
        let mut still = 0;
        desk.until("Codex's app to draw its composer", DRAW, |desk, _| {
            desk.app
                .as_ref()
                .is_some_and(|app| !app.contents().trim().is_empty())
        })
        .await?;
        let deadline = tokio::time::Instant::now() + DRAW;
        while still < 20 {
            desk.settle(Duration::from_millis(100)).await?;
            let now = desk.app().contents();
            still = if now == last { still + 1 } else { 0 };
            last = now;
            if tokio::time::Instant::now() > deadline {
                break;
            }
        }
        desk.frame("Codex's app attached");

        // A prompt typed in the app: pasted, as a person's typing would be
        // taken for a paste and its Enter for a new line, then submitted.
        let paste = format!("\x1b[200~{PROMPT}\x1b[201~");
        desk.type_into_app(paste.as_bytes()).await?;
        desk.until("the app to show the typed prompt", DRAW, |desk, _| {
            desk.app.as_ref().is_some_and(|app| app.shows(PROMPT_MARK))
        })
        .await?;
        desk.settle(Duration::from_millis(500)).await?;
        desk.frame("the prompt typed in Codex's app");
        desk.type_into_app(b"\r").await?;

        // amux's chat shows the app's prompt, once.
        desk.until(
            "the app's prompt in amux's chat",
            super::TURN,
            |desk, log| {
                prompts(log).iter().any(|text| text.contains(PROMPT_MARK))
                    && desk.amux.shows(PROMPT_MARK)
            },
        )
        .await?;
        desk.frame("amux's chat shows the app's prompt");

        // The approval it leads to, answered from amux's chat.
        desk.until("amux to ask for the approval", super::TURN, |desk, log| {
            !log.state(Kind::Codex).asks.is_empty() && desk.amux.shows("1. Yes")
        })
        .await?;
        desk.settle(Duration::from_millis(500)).await?;
        desk.frame("amux and the app both show the approval");
        desk.type_into_amux(b"1").await?;
        desk.until("the turn to end", super::TURN, |_, log| {
            !log.turns(Kind::Codex).is_empty()
        })
        .await?;
        desk.until("the agent to take input again", DRAW, |_, log| {
            log.phase() == Phase::Idle
        })
        .await?;
        desk.settle(Duration::from_secs(2)).await?;
        desk.frame("the approved command ran and the turn ended");
        self.note_model(&follow);
        let live = follow.snapshot();
        super::expect_outcome(live.turns(Kind::Codex)[0], wire::TurnOutcome::Completed)?;
        let effect = project.join("attached.txt");
        if !effect.exists() {
            return Err(format!(
                "approved from amux, but {} does not exist",
                effect.display()
            ));
        }
        let sent = prompts(&live);
        if sent.len() != 1 || !sent[0].contains(PROMPT_MARK) {
            return Err(format!("amux's chat holds prompts {sent:?}, not the one"));
        }
        super::judge(
            &self.captured(scenario)?.shape(Kind::Codex),
            &live.shape(Kind::Codex),
        )?;

        // The app detaches and the agent lives on; amux's client quits.
        desk.type_into_app(DETACH).await?;
        let clean = desk.app().exits(clock).await?;
        desk.frame("the app detached");
        desk.app = None;
        if !clean {
            return Err("the app's terminal exited with a failure".into());
        }
        self.listed(name, "idle").await?;
        desk.type_into_amux(FLEET).await?;
        desk.settle(Duration::from_millis(500)).await?;
        desk.type_into_amux(b"q").await?;
        let quit = desk.amux.exits(clock).await?;
        if !quit {
            return Err("amux's client exited with a failure".into());
        }
        Ok(Verdict::Pass)
    }
}
