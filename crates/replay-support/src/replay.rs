//! Replaying a bundle's records through their stages with the protocol's
//! properties judged as each goes: each agent's record as a provider stream
//! made up from it through its interpreter, the daemon core from its
//! checkpoint through every input since, and each chat a client showed from
//! its record's checkpoint through every message since. The properties are
//! the models' own (`crates/models`) and the interpreters' checks, the same
//! sentences the checkers explore and the live steps judge; a stage judges
//! those a run of it can be judged by alone. The first stage that fails is
//! the culprit; when none fails, the fault is in a driver or in
//! presentation, and our logs are where to look.
//!
//! On the machine that wrote them, an agent's facts ring replays exactly
//! instead ([`replay_facts`]): the real events through the interpreter,
//! each segment from its own checkpoint, against what the agent journaled.

use std::fmt;
use std::path::Path;

use daemon_core::{AgentId, Daemon, Input, Output, Recorded, StepBudget};
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Event, Interpreter};
pub use models::lifecycle::RESUME_CONTINUES;
use models::single_daemon::{REPRODUCES, judge_step, stepped};
use prost::Message as _;
use store::Sqlite;
use ui_state::{Msg, SessionState};
use wire::Step;

use crate::bundle::{Bundle, BundleError, ClientHistory, CoreDump, journal_frames, read};
use crate::synthetic;

/// The stages a bundle's records replay through, in the order a row
/// passes through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// An agent's facts ring through its interpreter, on the machine that
    /// wrote it.
    Interpreter,
    /// An agent's record as a stream made up from it, through its
    /// interpreter.
    Agent,
    /// The daemon core from its checkpoint through its inputs since.
    Core,
    /// A client's session from its record's checkpoint through its
    /// messages since.
    Session,
    /// A relay's core from its report's window start through its inputs.
    Relay,
    /// The supervisor's core from its report's window start through its
    /// inputs.
    Supervisor,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stage::Interpreter => "interpreter",
            Stage::Agent => "agent",
            Stage::Core => "daemon core",
            Stage::Session => "session",
            Stage::Relay => "relay core",
            Stage::Supervisor => "supervisor",
        })
    }
}

/// The first step of a stage at which a property failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    /// The step, counted from one: the event to an interpreter, the input
    /// to a core or the message to a client's session. Zero is a
    /// checkpoint that already breaks it.
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
    /// Every stage in order. The first with a failing property is the
    /// culprit.
    pub fn replay_all(&self) -> Result<Vec<StageVerdict>, BundleError> {
        let core = match self.replay_core()? {
            None => match &self.core {
                Some(core) => Verdict::Holds(format!(
                    "{} inputs from the checkpoint, each step judged",
                    inputs(core).count()
                )),
                None => Verdict::Absent("the bundle carries no record of the core".to_owned()),
            },
            Some(failing) => Verdict::Fails(failing),
        };
        Ok(vec![
            StageVerdict {
                stage: Stage::Agent,
                verdict: self.replay_agents(),
            },
            StageVerdict {
                stage: Stage::Core,
                verdict: core,
            },
            StageVerdict {
                stage: Stage::Session,
                verdict: self.replay_sessions(),
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
        let scratch = tempfile::tempdir().map_err(|error| BundleError::Read {
            path: std::env::temp_dir(),
            error,
        })?;
        self.replay_core_in(scratch.path())
    }

    /// [`Bundle::replay_core`] in `scratch`, which keeps the store the
    /// replay ends with as `store.sqlite`, to compare with the live one.
    pub fn replay_core_in(&self, scratch: &Path) -> Result<Option<Failing>, BundleError> {
        let Some(core) = &self.core else {
            return Ok(None);
        };
        let store_path = scratch.join("store.sqlite");
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
        let agents = scratch.join("agents");
        let mut daemon =
            Daemon::restore(&core.checkpoint, store, agents.clone()).map_err(|error| {
                BundleError::Decode {
                    path: checkpoint_path,
                    what: "checkpoint",
                    error: error.to_string(),
                }
            })?;
        daemon.set_budget(StepBudget::UNLIMITED);

        for (at, (input, reads)) in steps(core).into_iter().enumerate() {
            let step = at + 1;
            let what = input.kind().to_owned();
            let mut refuse = None;
            for read in &reads {
                let journal = core
                    .journals
                    .get(&read.agent)
                    .map_or(&[][..], Vec::as_slice);
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

    /// Each agent's record turned into a provider stream and stepped
    /// through the interpreter of its kind, the interpreter's checks judged
    /// at every step: the first check broken, and whether the record says
    /// the live step broke it there too. A check the record says broke but
    /// the stream does not is said beside a stage that holds: the build
    /// replaying it has changed the interpreter, or the record keeps too
    /// little of the event that broke it.
    fn replay_agents(&self) -> Verdict {
        if self.agents.is_empty() {
            return Verdict::Absent("the bundle holds no agent's record".to_owned());
        }
        let (mut events, mut unmade) = (0, 0);
        let mut said = Vec::new();
        for agent in &self.agents {
            let reproduction = match synthetic::reproduce(&agent.record) {
                Ok(reproduction) => reproduction,
                Err(why) => {
                    return Verdict::Unreadable(format!("agent {}'s record: {why}", agent.id));
                }
            };
            events += reproduction.synthetic.events.len();
            unmade += reproduction.synthetic.notes.len();
            let recorded = recorded_break(&agent.record);
            if let Some((at, violation)) = reproduction.broken.first() {
                let live = match &recorded {
                    Some((live, id)) if live == at && *id == violation.invariant.id => {
                        "; the live step broke it there too".to_owned()
                    }
                    Some((live, id)) => {
                        format!("; the live steps broke {id} at event {}", live + 1)
                    }
                    None => "; no live step broke a check".to_owned(),
                };
                return Verdict::Fails(Failing {
                    step: at + 1,
                    what: format!(
                        "agent {}'s event {}, {}",
                        agent.id,
                        at + 1,
                        event_kind(&reproduction.synthetic.events[*at])
                    ),
                    property: violation.invariant.sentence,
                    detail: format!("{}{live}", violation.detail),
                });
            }
            if let Some((at, id)) = recorded {
                said.push(format!(
                    "agent {}'s live steps broke {id} at event {}, which its stream does not",
                    agent.id,
                    at + 1
                ));
            }
        }
        if unmade > 0 {
            said.push(format!("{unmade} things the streams could not stand for"));
        }
        let said = if said.is_empty() {
            String::new()
        } else {
            format!("; {}", said.join("; "))
        };
        Verdict::Holds(format!(
            "{} agent record{} as made-up streams, {events} events, each step judged{said}",
            self.agents.len(),
            plural(self.agents.len()),
        ))
    }

    /// Each chat a client showed replays from its record's checkpoint
    /// through every message its driver applied since, the client's
    /// promises judged after each: a violation the client caught stops at
    /// the message that broke it.
    fn replay_sessions(&self) -> Verdict {
        if self.clients.is_empty() {
            return Verdict::Absent("the bundle holds no client's session".to_owned());
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
        Verdict::Holds(format!(
            "{} client session{} from {} checkpoint, {messages} messages, each judged",
            self.clients.len(),
            plural(self.clients.len()),
            if self.clients.len() == 1 {
                "its"
            } else {
                "their"
            }
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
        crate::bundle::id(host).ok_or_else(|| decode(format!("host {host} is not an id")))
    }
}

/// Replays one client session from its checkpoint, judging the client's
/// promises at the checkpoint and after every message: how many messages
/// held, or the first that broke one. Err when a value does not read back.
pub fn replay_client(
    agent: &str,
    history: &ClientHistory,
) -> Result<Result<usize, Failing>, String> {
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

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The first event a record says broke a check, by its index across the
/// record's segments, and the check's id.
fn recorded_break(record: &serde_json::Value) -> Option<(usize, String)> {
    let events = record["segments"]
        .as_array()?
        .iter()
        .flat_map(|segment| segment["events"].as_array().map_or(&[][..], Vec::as_slice));
    events.enumerate().find_map(|(at, event)| {
        let id = event["broken"].as_str()?;
        Some((at, id.to_owned()))
    })
}

/// What an interpreter's event is, as a person reads a failing step.
fn event_kind(event: &Event) -> String {
    match event {
        Event::Fact(fact) => match serde_json::from_slice::<serde_json::Value>(&fact.payload) {
            Ok(json) => {
                let tag = ["type", "method", "subtype"]
                    .iter()
                    .filter_map(|field| json[*field].as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                format!("a {:?} fact {tag}", fact.channel)
            }
            Err(_) => format!("a {:?} fact", fact.channel),
        },
        Event::Input(_) => "an input".to_owned(),
        Event::AgentMessage(_) => "an agent message".to_owned(),
        Event::Rename { .. } => "a rename".to_owned(),
        Event::Tick { at_ms } => format!("a tick at {at_ms}"),
        Event::ProviderExit { code } => format!("the provider's exit, code {code:?}"),
        Event::DaemonLost => "the daemon lost".to_owned(),
        Event::StopRequested => "a stop".to_owned(),
        Event::Exiting(_) => "the agent exiting".to_owned(),
        Event::Git(_) => "a git fact".to_owned(),
    }
}

/// Where an agent directory keeps what the facts replay reads (see the
/// agent directory in docs/ARCHITECTURE.md).
const JOURNAL: &str = "journal";
const FACTS: &str = "private/facts";

/// Replays, for every agent directory under `agents` (a profile's
/// `agents/`, or a copy of it), its facts ring through the interpreter of
/// its newest spec's kind: each segment from the checkpoint written at its
/// start, the interpreter's checks judged after every event, and the
/// newest segment's steps compared with the end of what the agent
/// journaled, which a resume from that checkpoint must reproduce.
pub fn replay_facts(agents: &Path) -> Verdict {
    let listing = match crate::bundle::listing(agents) {
        Ok(listing) => listing,
        Err(error) => return Verdict::Unreadable(error.to_string()),
    };
    let (mut replayed, mut lines, mut frames) = (0, 0, 0);
    for (id, dir) in listing {
        if !dir.join(FACTS).is_dir() {
            continue;
        }
        let kind = match newest_spec(&dir) {
            Ok(spec) => spec.kind(),
            Err(why) => return Verdict::Unreadable(format!("agent {id}'s spec: {why}")),
        };
        let replay = match kind {
            wire::Kind::ClaudeSdk => replay_agent::<ClaudeSdk>(&id, &dir),
            wire::Kind::ClaudePty => replay_agent::<ClaudePty>(&id, &dir),
            wire::Kind::Codex => replay_agent::<Codex>(&id, &dir),
            wire::Kind::Unspecified => continue,
        };
        match replay {
            Ok(Ok((read, compared))) => {
                replayed += 1;
                lines += read;
                frames += compared;
            }
            Ok(Err(failing)) => return Verdict::Fails(failing),
            Err(why) => return Verdict::Unreadable(format!("agent {id}'s facts: {why}")),
        }
    }
    if replayed == 0 {
        return Verdict::Absent("no agent here keeps a facts ring".to_owned());
    }
    Verdict::Holds(format!(
        "{replayed} agent{}, {lines} facts lines from their checkpoints, each step judged, \
         the journal's last {frames} frames reproduced",
        plural(replayed)
    ))
}

/// The newest `spec.<n>` in an agent directory.
fn newest_spec(dir: &Path) -> Result<wire::AgentSpec, String> {
    let newest = crate::bundle::listing(dir)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|(name, path)| Some((name.strip_prefix("spec.")?.parse::<u64>().ok()?, path)))
        .max();
    let (_, path) = newest.ok_or("there is none")?;
    let bytes = read(&path).map_err(|error| error.to_string())?;
    wire::AgentSpec::decode(bytes.as_slice()).map_err(|error| error.to_string())
}

/// One agent's facts replay: the lines read and the journal frames
/// compared, or the first step that broke a check or differs from what
/// was journaled. Err when the ring does not read.
fn replay_agent<I: Interpreter>(
    id: &str,
    dir: &Path,
) -> Result<Result<(usize, usize), Failing>, String> {
    let facts = dir.join(FACTS);
    let starts = journal::segments(&facts).map_err(|error| error.to_string())?;
    let mut line = 0;
    let mut newest: Vec<(usize, Step)> = Vec::new();
    for start in starts {
        let checkpoint = std::fs::read(core_ring::checkpoint_path(&facts, start))
            .map_err(|error| format!("segment {start}'s checkpoint: {error}"))?;
        let mut state: I::State = interpret::decode_checkpoint(&checkpoint)
            .map_err(|error| format!("segment {start}'s checkpoint: {error}"))?;
        let bytes = std::fs::read(journal::segment_path(&facts, start))
            .map_err(|error| format!("segment {start}: {error}"))?;
        newest.clear();
        for text in bytes
            .split(|byte| *byte == b'\n')
            .filter(|text| !text.is_empty())
        {
            line += 1;
            let event = serde_json::from_slice::<interpret::ring::Entry>(text)
                .ok()
                .and_then(interpret::ring::Entry::event)
                .ok_or_else(|| format!("line {line} does not read"))?;
            let what = format!("agent {id}'s facts line {line}, {}", event_kind(&event));
            let stepped = I::step(&mut state, event);
            if let Some(violation) = stepped.broken {
                return Ok(Err(Failing {
                    step: line,
                    what,
                    property: violation.invariant.sentence,
                    detail: violation.detail,
                }));
            }
            if stepped.step != Step::default() {
                newest.push((line, stepped.step));
            }
        }
    }
    let mut journal = Vec::new();
    let journal_dir = dir.join(JOURNAL);
    for start in journal::segments(&journal_dir).map_err(|error| error.to_string())? {
        let bytes = std::fs::read(journal::segment_path(&journal_dir, start))
            .map_err(|error| format!("journal segment {start}: {error}"))?;
        journal.extend(journal_frames(&bytes));
    }
    let overlap = newest.len().min(journal.len());
    let replayed = &newest[newest.len() - overlap..];
    let journaled = &journal[journal.len() - overlap..];
    for (at, ((line, ours), theirs)) in replayed.iter().zip(journaled).enumerate() {
        if ours != theirs {
            return Ok(Err(Failing {
                step: *line,
                what: format!(
                    "agent {id}'s facts line {line}, journal frame {}",
                    journal.len() - overlap + at + 1
                ),
                property: RESUME_CONTINUES,
                detail: "the replay's step is not the one the agent journaled".to_owned(),
            }));
        }
    }
    Ok(Ok((line, overlap)))
}
