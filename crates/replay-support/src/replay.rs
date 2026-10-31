//! Replaying a bundle through all four of its stages with the protocol's
//! properties judged as each goes: the facts through the interpreter, the
//! daemon core from its checkpoint through every input since, each client
//! session from its checkpoint through every message since (the store slice
//! through the session model, for an agent no client had open), and that
//! state through the views. The
//! properties are the models' own (`crates/models`), the same sentences
//! the checkers explore; a stage judges those a run of it can be judged
//! by alone. The first stage that fails is the culprit; when none fails,
//! the fault is in a driver or in presentation, and the daemon's log tail
//! is where to look.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use daemon_core::{AgentId, Checkpoint, Daemon, Input, Output, Recorded, StepBudget};
use models::single_daemon::{REPRODUCES, judge_step, stepped};
use redaction::{EMAIL_PLACEHOLDER, IDENTIFIER_PLACEHOLDER, PATH_PLACEHOLDER, SECRET_PLACEHOLDER};
use store::Sqlite;
use ui_state::{Msg, SessionState};
use wire::{Kind, Step};

use crate::bundle::{AgentDump, Bundle, BundleError, ClientHistory, CoreDump, read, segments};
use crate::show::render_steps;

/// The four stages, in the order a row passes through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The agent's facts through its interpreter.
    Interpreter,
    /// The daemon core from its checkpoint through its inputs since.
    Core,
    /// A client's session from its checkpoint through its messages since,
    /// or the store slice through the session model where no client was
    /// attached.
    Session,
    /// The session's state through the views.
    Views,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stage::Interpreter => "interpreter",
            Stage::Core => "daemon core",
            Stage::Session => "session",
            Stage::Views => "views",
        })
    }
}

/// The first step of a stage at which a property failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    /// The step, counted from one: the input to the core, the message to a
    /// client's session or the event of a store opening; for the
    /// interpreter, the facts line. Zero is a checkpoint that already
    /// breaks it.
    pub step: usize,
    /// What the step was: the input's kind, the agent it was for.
    pub what: String,
    /// The property, as the sentence its model checks.
    pub property: &'static str,
    /// What broke it.
    pub detail: String,
}

impl fmt::Display for Failing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fails at step {} ({}): \"{}\": {}",
            self.step, self.what, self.property, self.detail
        )
    }
}

/// How one stage replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every step held every property judged; what was replayed.
    Holds(String),
    Fails(Failing),
    /// The manifest names something for this stage that cannot be read, so
    /// the stage cannot be said to hold.
    Unreadable(String),
    /// The bundle carries nothing this stage replays, and why.
    Absent(String),
}

impl Verdict {
    /// Whether the stage did not hold: a property failed, or what it
    /// replays could not be read.
    pub fn failed(&self) -> bool {
        matches!(self, Verdict::Fails(_) | Verdict::Unreadable(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageVerdict {
    pub stage: Stage,
    pub verdict: Verdict,
}

impl fmt::Display for StageVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = self.stage.to_string();
        match &self.verdict {
            Verdict::Holds(what) => write!(f, "{stage:<12} holds   {what}"),
            Verdict::Fails(failing) => write!(f, "{stage:<12} FAILS   {failing}"),
            Verdict::Unreadable(why) => write!(f, "{stage:<12} FAILS   unreadable: {why}"),
            Verdict::Absent(why) => write!(f, "{stage:<12} absent  {why}"),
        }
    }
}

impl Bundle {
    /// All four stages in order. The first with a failing property is the
    /// culprit; no failure means the fault is in a driver or presentation.
    pub fn replay_all(&self) -> Result<Vec<StageVerdict>, BundleError> {
        let core = match self.replay_core()? {
            None => match &self.core {
                Some(core) => Verdict::Holds(format!(
                    "{} inputs from the checkpoint, each step judged",
                    inputs(core).count()
                )),
                None => Verdict::Absent("the bundle carries no part of the core".to_owned()),
            },
            Some(failing) => Verdict::Fails(failing),
        };
        Ok(vec![
            StageVerdict {
                stage: Stage::Interpreter,
                verdict: self.replay_interpreters()?,
            },
            StageVerdict {
                stage: Stage::Core,
                verdict: core,
            },
            StageVerdict {
                stage: Stage::Session,
                verdict: self.replay_sessions(),
            },
            StageVerdict {
                stage: Stage::Views,
                verdict: self.replay_views(),
            },
        ])
    }

    /// Replays the daemon core from its checkpoint, over a copy of the
    /// store as of that checkpoint, feeding every recorded input and
    /// judging the single daemon's promises after each: no dump asked
    /// for, every invariant the core checks after a write, commit before
    /// broadcast, and that each journal read finds what the live step
    /// found. Each journal is laid out as the live step read it: cut where
    /// the step's recorded read ended, and refusing the commit where the
    /// live one failed. The first failing step and its property, or None
    /// when the stage holds.
    pub fn replay_core(&self) -> Result<Option<Failing>, BundleError> {
        let Some(core) = &self.core else {
            return Ok(None);
        };
        let scratch = tempfile::tempdir().map_err(|error| BundleError::Read {
            path: std::env::temp_dir(),
            error,
        })?;
        let store_path = scratch.path().join("store.sqlite");
        std::fs::copy(&core.store, &store_path).map_err(|error| BundleError::Read {
            path: core.store.clone(),
            error,
        })?;
        let host = self.host()?;
        let store =
            Sqlite::open(&store_path, host.to_vec()).map_err(|error| BundleError::Decode {
                path: core.store.clone(),
                what: "store",
                error: error.to_string(),
            })?;
        let checkpoint_path = self.dir.join(crate::bundle::CORE).join("checkpoint");
        let agents = scratch.path().join("agents");
        let mut daemon = Checkpoint::from_bytes(&core.checkpoint)
            .and_then(|checkpoint| Daemon::restore(checkpoint, store, agents.clone()))
            .map_err(|error| BundleError::Decode {
                path: checkpoint_path,
                what: "checkpoint",
                error: error.to_string(),
            })?;
        daemon.set_budget(StepBudget::UNLIMITED);

        let journals = self.journals()?;
        for (at, (input, reads)) in steps(core).into_iter().enumerate() {
            let step = at + 1;
            let what = input.kind().to_owned();
            let mut refuse = None;
            for read in &reads {
                let journal = journals.get(&read.agent).map_or(&[][..], Vec::as_slice);
                // The reader goes first: some systems refuse to remove a
                // directory holding a file still open.
                daemon.journal_relaid(read.agent);
                lay_out(
                    &daemon_core::ingest::journal_dir(&agents, read.agent),
                    journal,
                    read.seen,
                )?;
                if let Some(why) = &read.failed {
                    refuse.get_or_insert(why.clone());
                }
            }
            let mut out = Vec::new();
            stepped(&mut daemon, input, refuse.as_deref(), &mut out);
            if let Some(broken) = judge_step(&daemon, &out) {
                return Ok(Some(Failing {
                    step,
                    what,
                    property: broken.promise,
                    detail: broken.detail,
                }));
            }
            let replayed: Vec<Read> = out
                .iter()
                .filter_map(|output| match output {
                    Output::Ingested { agent, report } => match Recorded::read(*agent, report) {
                        Recorded::Read {
                            agent,
                            seen,
                            failed,
                        } => Some(Read {
                            agent,
                            seen,
                            failed,
                        }),
                        Recorded::Input(_) => None,
                    },
                    _ => None,
                })
                .collect();
            let shape = |reads: &[Read]| -> Vec<(AgentId, u64, bool)> {
                reads
                    .iter()
                    .map(|read| (read.agent, read.seen, read.failed.is_some()))
                    .collect()
            };
            if shape(&replayed) != shape(&reads) {
                return Ok(Some(Failing {
                    step,
                    what,
                    property: REPRODUCES,
                    detail: format!(
                        "the live step read {}; the replay read {}",
                        described(&reads),
                        described(&replayed)
                    ),
                }));
            }
        }
        Ok(None)
    }

    /// Stage one for every agent whose facts the bundle carries: the
    /// interpreter, restored from the oldest checkpoint kept and fed every
    /// facts line since, writes what the agent journaled. Both end at the
    /// dump, so the replay's steps are compared with the journal's frames
    /// they overlap, rendered as the interpreter's goldens are.
    fn replay_interpreters(&self) -> Result<Verdict, BundleError> {
        let mut replayed = 0;
        let mut frames = 0;
        for agent in &self.agents {
            if agent.facts.is_empty() {
                continue;
            }
            let steps = agent.replay_facts()?;
            replayed += 1;
            let overlap = steps.len().min(agent.journal.len());
            frames += overlap;
            if let Some((frame, detail)) = disagreement(agent.row.kind(), &steps, &agent.journal) {
                return Ok(Verdict::Fails(Failing {
                    step: frame,
                    what: format!("agent {}", agent.id),
                    property: models::lifecycle::RESUME_CONTINUES,
                    detail,
                }));
            }
        }
        Ok(if replayed == 0 {
            Verdict::Absent("no agent's facts ring is in the bundle".to_owned())
        } else {
            Verdict::Holds(format!(
                "{replayed} agent{}, the facts replayed to the journal's last {frames} frames",
                plural(replayed)
            ))
        })
    }

    /// Stage three. Each session a client had open replays from its
    /// checkpoint through every message its driver applied since, the
    /// client's promises judged after each: a violation the client caught
    /// stops at the message that broke it. Every other agent has its store
    /// slice delivered to the session model as a Subscribe opening delivers
    /// it, the window checked after every event.
    fn replay_sessions(&self) -> Verdict {
        let sliced = self.agents.iter().any(|agent| agent.store.is_some());
        if !sliced && self.clients.is_empty() {
            return Verdict::Absent(
                "the bundle holds no agent's store slice or session".to_owned(),
            );
        }
        let mut messages = 0;
        for client in &self.clients {
            let history = match &client.history {
                Ok(history) => history,
                Err(why) => {
                    return Verdict::Unreadable(format!(
                        "agent {}'s client session: {why}",
                        client.agent
                    ));
                }
            };
            match replay_client(&client.agent, history) {
                Ok(Ok(count)) => messages += count,
                Ok(Err(failing)) => return Verdict::Fails(failing),
                Err(why) => {
                    return Verdict::Unreadable(format!(
                        "agent {}'s client session: {why}",
                        client.agent
                    ));
                }
            }
        }
        let opened: Vec<&AgentDump> = self
            .agents
            .iter()
            .filter(|agent| agent.store.is_some())
            .filter(|agent| !self.clients.iter().any(|client| client.agent == agent.id))
            .collect();
        let mut events = 0;
        for agent in &opened {
            let mut state = SessionState::new(agent.row.clone(), usize::MAX);
            for (at, of) in agent.opening().into_iter().enumerate() {
                state.update(Msg::Event(wire::SessionEvent { of: Some(of) }));
                events += 1;
                if let Some((property, detail)) = models::client::judge_session(&state) {
                    return Verdict::Fails(Failing {
                        step: at + 1,
                        what: format!("agent {}'s store opening", agent.id),
                        property,
                        detail,
                    });
                }
            }
        }
        let mut said = Vec::new();
        if !self.clients.is_empty() {
            said.push(format!(
                "{} client session{} from {} checkpoint, {messages} messages, each judged",
                self.clients.len(),
                plural(self.clients.len()),
                if self.clients.len() == 1 {
                    "its"
                } else {
                    "their"
                }
            ));
        }
        if !opened.is_empty() {
            said.push(format!(
                "{} agent{} with no client attached from the store's opening, {events} events, \
                 the window checked after each",
                opened.len(),
                plural(opened.len())
            ));
        }
        Verdict::Holds(said.join("; "))
    }

    /// Stage four: views are functions of state, so no property judges
    /// them; this draws every agent's rows, which is what a person compares
    /// with the screen.
    fn replay_views(&self) -> Verdict {
        let sliced: Vec<&AgentDump> = self
            .agents
            .iter()
            .filter(|agent| agent.store.is_some())
            .collect();
        if sliced.is_empty() {
            return Verdict::Absent("the bundle holds no agent's store slice".to_owned());
        }
        let rows: usize = sliced
            .iter()
            .map(|agent| AgentDump::rows(&agent.session()).len())
            .sum();
        Verdict::Holds(format!(
            "{rows} rows drawn for {} agent{}",
            sliced.len(),
            plural(sliced.len())
        ))
    }

    /// This host's id, as the manifest names it.
    fn host(&self) -> Result<[u8; 16], BundleError> {
        let path = self.dir.join("manifest.json");
        let decode = |error: String| BundleError::Decode {
            path: path.clone(),
            what: "manifest",
            error,
        };
        let manifest: serde_json::Value =
            serde_json::from_slice(&read(&path)?).map_err(|error| decode(error.to_string()))?;
        let host = manifest
            .get("host")
            .and_then(|host| host.as_str())
            .ok_or_else(|| decode("no host".to_owned()))?;
        id(host).ok_or_else(|| decode(format!("host {host} is not an id")))
    }

    /// Each own agent's journal segments as the bundle carries them, at
    /// their offsets.
    fn journals(&self) -> Result<BTreeMap<AgentId, Segments>, BundleError> {
        let mut journals = BTreeMap::new();
        for agent in &self.agents {
            let Some(id) = id(&agent.id).map(AgentId) else {
                continue;
            };
            let mut segments_held = Vec::new();
            for (start, path) in segments(&self.dir.join("agents").join(&agent.id).join("journal"))?
            {
                segments_held.push((start, read(&path)?));
            }
            if !segments_held.is_empty() {
                journals.insert(id, segments_held);
            }
        }
        Ok(journals)
    }
}

/// Replays one client session from its checkpoint, judging the client's
/// promises at the checkpoint and after every message: how many messages
/// held, or the first that broke one. Err when a value does not read back.
fn replay_client(agent: &str, history: &ClientHistory) -> Result<Result<usize, Failing>, String> {
    let mut state = SessionState::from_wire(history.start.clone())?;
    if let Some((property, detail)) = models::client::judge_session(&state) {
        return Ok(Err(Failing {
            step: 0,
            what: format!("agent {agent}'s client checkpoint"),
            property,
            detail,
        }));
    }
    let mut step = 0;
    for traced in &history.events {
        let Some(wire::dump::client_traced::Event::Msg(msg)) = &traced.event else {
            continue;
        };
        step += 1;
        let msg = Msg::from_wire(msg.clone())?;
        let what = format!(
            "agent {agent}'s client message {}, traced {}",
            message_kind(&msg),
            traced.seq
        );
        state.update(msg);
        if let Some((property, detail)) = models::client::judge_session(&state) {
            return Ok(Err(Failing {
                step,
                what,
                property,
                detail,
            }));
        }
    }
    Ok(Ok(step))
}

/// What a message is, as a person reads a failing step.
pub(crate) fn message_kind(msg: &Msg) -> String {
    use wire::session_event::Of;
    match msg {
        Msg::Event(event) => match &event.of {
            Some(Of::Snapshot(_)) => "Snapshot".to_owned(),
            Some(Of::Item(item)) => format!("Item {} order {}", item.key, item.order),
            Some(Of::Append(append)) => format!("Append {}", append.key),
            Some(Of::CaughtUp(caught_up)) => format!("CaughtUp rev {}", caught_up.revision),
            Some(Of::Lagged(_)) => "Lagged".to_owned(),
            Some(Of::Reset(reset)) => format!("Reset generation {}", reset.generation),
            Some(Of::Detached(_)) => "Detached".to_owned(),
            Some(Of::Opening(opening)) => format!("Opening generation {}", opening.generation),
            None => "an empty event".to_owned(),
        },
        Msg::Page { items, .. } => format!("Page of {}", items.len()),
        Msg::Connection(connection) => format!("Connection {connection:?}"),
        Msg::Send(_) => "Send".to_owned(),
        Msg::Sent(..) => "Sent".to_owned(),
        Msg::Discard(_) => "Discard".to_owned(),
        Msg::Entry(_) => "Entry".to_owned(),
        Msg::Host(_) => "Host".to_owned(),
        Msg::Blob { .. } => "Blob".to_owned(),
        Msg::Following(following) => format!("Following {following}"),
        Msg::Reloading => "Reloading".to_owned(),
        Msg::Window(cap) => format!("Window {cap}"),
        Msg::Catalogue { .. } => "Catalogue".to_owned(),
    }
}

/// A journal's segment files: each one's start offset and its bytes.
type Segments = Vec<(u64, Vec<u8>)>;

/// What one live step read of an agent's journal.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Read {
    agent: AgentId,
    seen: u64,
    failed: Option<String>,
}

fn inputs(core: &CoreDump) -> impl Iterator<Item = &Input> {
    core.inputs.iter().filter_map(|line| match line {
        Recorded::Input(input) => Some(input.as_ref()),
        Recorded::Read { .. } => None,
    })
}

/// Reads as a person compares them: each agent's journal, how far, and
/// whether its commit failed.
fn described(reads: &[Read]) -> String {
    if reads.is_empty() {
        return "no journal".to_owned();
    }
    reads
        .iter()
        .map(|read| {
            let agent = daemon_core::ingest::agent_dir(Path::new(""), read.agent);
            let failed = if read.failed.is_some() {
                ", failing its commit"
            } else {
                ""
            };
            format!("agent {}'s to byte {}{failed}", agent.display(), read.seen)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each recorded input with the reads its live step made, which the ring
/// records after it.
fn steps(core: &CoreDump) -> Vec<(Input, Vec<Read>)> {
    let mut steps: Vec<(Input, Vec<Read>)> = Vec::new();
    for line in &core.inputs {
        match line {
            Recorded::Input(input) => steps.push(((**input).clone(), Vec::new())),
            Recorded::Read {
                agent,
                seen,
                failed,
            } => {
                if let Some((_, reads)) = steps.last_mut() {
                    reads.push(Read {
                        agent: *agent,
                        seen: *seen,
                        failed: failed.clone(),
                    });
                }
            }
        }
    }
    steps
}

/// Lays an agent's journal out in `dir` as a live read found it: every
/// segment that starts below `seen`, the last cut at it.
fn lay_out(dir: &Path, journal: &[(u64, Vec<u8>)], seen: u64) -> Result<(), BundleError> {
    let io = |error| BundleError::Read {
        path: dir.to_owned(),
        error,
    };
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(io)?;
    }
    std::fs::create_dir_all(dir).map_err(io)?;
    for (start, bytes) in journal {
        if *start >= seen {
            break;
        }
        let kept = (seen.saturating_sub(*start) as usize).min(bytes.len());
        std::fs::write(journal::segment_path(dir, *start), &bytes[..kept]).map_err(io)?;
    }
    Ok(())
}

/// Where the steps an interpreter replayed first differ from the journal's
/// frames they overlap, both ending at the dump: the journal's frame,
/// counted from one, and the two lines. Redaction is set aside: the facts
/// and the journal are redacted apart, and a value one names a secret the
/// other may name an email.
fn disagreement(kind: Kind, replayed: &[Step], journal: &[Step]) -> Option<(usize, String)> {
    let overlap = replayed.len().min(journal.len());
    let first = journal.len() - overlap;
    let ours = render_steps(kind, first, &replayed[replayed.len() - overlap..])?;
    let theirs = render_steps(kind, first, &journal[first..])?;
    let (mut frame, mut lines) = (first, (ours.lines(), theirs.lines()));
    loop {
        match (lines.0.next(), lines.1.next()) {
            (None, None) => return None,
            (Some(a), Some(b)) if unredacted(a) == unredacted(b) => {
                if a.starts_with("## ") {
                    frame += 1;
                }
            }
            (a, b) => {
                return Some((
                    frame.max(1),
                    format!("the journal has {b:?} where the replay has {a:?}"),
                ));
            }
        }
    }
}

/// A line with every redaction placeholder the same.
fn unredacted(line: &str) -> String {
    [EMAIL_PLACEHOLDER, IDENTIFIER_PLACEHOLDER, PATH_PLACEHOLDER]
        .into_iter()
        .fold(line.to_owned(), |line, placeholder| {
            line.replace(placeholder, SECRET_PLACEHOLDER)
        })
}

/// An id from the hyphenated form a bundle writes ids in.
fn id(name: &str) -> Option<[u8; 16]> {
    let hex: String = name.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut id = [0; 16];
    for (at, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(id)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
