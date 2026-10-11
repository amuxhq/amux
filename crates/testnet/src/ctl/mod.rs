//! `testnet ctl`: one command line for a person or an agent driving a
//! served net by hand, step by step, from a folder of their own.
//!
//! The folder is the session. `ctl up` starts `testnet serve` in the
//! background with the net's root under `.testnet/` there, and every later
//! command finds the net through the record it wrote. Each command prints
//! one readable answer (or the raw value with `--json`), exits non-zero
//! with the reason when it fails, and appends a line to the folder's
//! `session.jsonl`, so what was done, what came back and what was drawn is
//! kept without anyone asking.

mod door;
mod evidence;
mod files;
mod phone;
mod session;
mod tui;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use session::Folder;
pub use session::Outcome;

/// Drive a served test network by hand: grow it, break it, drive its
/// clients, and keep the evidence.
///
/// Run every command from the same folder: `ctl up` starts the net there
/// and later commands find it. Every command is logged to session.jsonl in
/// that folder. Agents, hosts and panes are named as you named them.
#[derive(Parser, Debug)]
#[command(name = "testnet ctl", verbatim_doc_comment)]
pub struct Ctl {
    /// Print the door's raw answer as JSON instead of readable text.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start a net in this folder: the topology file's, or an empty one.
    ///
    /// The net gets a relay with no accounts (add them with `account add`),
    /// its own discovery scope, facts rings large enough to keep each
    /// agent's whole life, and its LAN routed through gates so any pair of
    /// hosts can be cut. Real agents' provider traffic is recorded under
    /// providers/ here. Prints the hosts once the net is ready.
    Up {
        /// A topology JSON file (see the TestNet docs); none starts empty.
        topology: Option<PathBuf>,
    },
    /// Shut the net down: every agent and daemon stopped, panes closed.
    /// The net's own files go; your session log, screenshots, findings and
    /// recordings stay.
    Down,
    /// The hosts, agents and links as they stand.
    Status,
    /// Add or remove a host.
    #[command(subcommand)]
    Host(HostCommand),
    /// Make two hosts trust each other and link them; returns once the
    /// link is up. Two hosts in the net's process link directly; a process
    /// host pairs for real through the net's gates (both need --lan).
    Link { a: String, b: String },
    /// Make two hosts forget each other, as unpairing does.
    Unlink { a: String, b: String },
    /// Add an account to the relay.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Cut the link between two hosts the way a dead connection goes, however
    /// they came to trust each other: a link made by `link`, or a pairing
    /// you made with `ctl amux` (pairing by name or through `ctl amux` by
    /// address makes it run through the net's gates).
    Sever { a: String, b: String },
    /// Bring a severed link back.
    Restore { a: String, b: String },
    /// Cut and restore a link on a period, until `flap stop`.
    ///
    /// `flap desk laptop --down 3s --up 10s`; `flap stop` stops every flap
    /// and leaves the links restored.
    Flap(FlapArgs),
    /// Make a host's network bad, or the relay's (`net relay ...`).
    ///
    /// Shapes the datagrams on every path through the net's gates: the
    /// host's way to the relay over QUIC, its LAN pairings, and the front
    /// gate a phone dials. A link made in process by `link` carries no
    /// datagrams and is untouched (sever or flap it instead); a host's TCP
    /// way to the relay is untouched too. Give no options to make it clean
    /// again. Durations take ms or s (`200ms`, `1.5s`); rate is bytes a
    /// second with k or m (`64k`).
    Net(NetArgs),
    /// Drop every UDP datagram between a host and the relay (on), so it
    /// falls back to TCP, or stop (off).
    BlockUdp { host: String, state: OnOff },
    /// Make `a` fail every call to `b` before it goes out (on), while the
    /// link and its chats stay up, or stop (off).
    RefuseCalls { a: String, b: String, state: OnOff },
    /// Crash a host's daemon (SIGKILL for a process host). Its agents keep
    /// running; a supervised host's supervisor starts the daemon again.
    Kill { host: String },
    /// Stop a host's daemon cleanly (`amux server stop` on a process host).
    /// Its agents keep running.
    Stop { host: String },
    /// Start a stopped or killed daemon again (killing it first when it is
    /// up, on a host in the net's process).
    Restart { host: String },
    /// Stop an agent's process where it stands, as an overloaded machine
    /// would: it holds its lock and connection and answers nothing.
    Freeze { agent: String },
    /// Let a frozen agent run again.
    Thaw { agent: String },
    /// Sign a host in to an account on the relay; returns once its relay
    /// link is up.
    SignIn { host: String, account: String },
    /// Change what an account has bought.
    Tier { account: String, tier: Tier },
    /// The release feed supervised hosts update from.
    #[command(subcommand)]
    Release(ReleaseCommand),
    /// Start an agent the way a person's create does, on any host.
    ///
    /// On a real host (or with --provider real) it is the real Claude or
    /// Codex, on Haiku or gpt-5.6-luna unless --model says otherwise. With
    /// a fake, --script FILE is what it plays (the script language is in
    /// the TestNet docs). In a QA run agents are real only. --wait returns
    /// once the agent has come to rest.
    Spawn(SpawnArgs),
    /// Send an agent a prompt through its own host. --wait returns once
    /// that prompt has been taken and the agent is at rest again.
    Send(SendArgs),
    /// Wait until an agent is at rest: idle or needing you, nothing queued.
    Wait(WaitArgs),
    /// Stop an agent as a person does; it can be resumed.
    StopAgent { agent: String },
    /// Start a stopped agent again, optionally with a prompt.
    Resume {
        agent: String,
        /// A first prompt for the new incarnation.
        text: Vec<String>,
    },
    /// Release every fake script step waiting on a gate file.
    #[command(subcommand)]
    Gate(GateCommand),
    /// A host's fleet as its own inventory has it: every agent it knows,
    /// its own and its peers'.
    Fleet { host: String },
    /// Everything a host holds of an agent's chat, in order, with its phase
    /// and model: the host's own record, to compare with what clients show.
    /// The agent is a name or, for one a client created, its id.
    Chat { host: String, agent: String },
    /// Check that a host's copy of an agent's rows is one whole block ending
    /// at its origin's newest row.
    Block { host: String, agent: String },
    /// Every line a fake provider read from its agent.
    ProviderInput { agent: String },
    /// Run the real `amux` CLI against a host: `ctl amux desk -- ls`.
    ///
    /// Standard input passes through, so `echo 123456 | ctl amux laptop --
    /// pair desk` joins a pairing. Bare `pair` (the waiting side) runs in
    /// the background and prints its PIN and address at once; its window
    /// stays open up to five minutes. `pair <host>:<port>` naming a host
    /// of this net is sent through the net's gate for the pair, so `sever`
    /// can cut it later.
    Amux {
        host: String,
        #[arg(last = true, required = true)]
        args: Vec<String>,
    },
    /// Drive the real terminal client in a named pane, and see it.
    #[command(subcommand)]
    Tui(tui::TuiCommand),
    /// Drive the phone app on the simulator this session holds, and see it.
    #[command(subcommand)]
    Phone(phone::PhoneCommand),
    /// Keep everything about what is wrong right now: copies (never moves)
    /// the topology, the session log, a dump of every host, recorded
    /// provider traffic, a fresh shot of every pane and the phone, and the
    /// logs, into findings/NAME/ with a finding.md to fill in.
    Capture {
        name: String,
        /// What the capture is for; also the dumps' reason.
        #[arg(long)]
        note: Option<String>,
    },
    /// Make a file in your folder to test with: a large attachment, a long
    /// paste. NAME is below your folder (no '..'); SIZE is 50m, 1.5g, 300k
    /// or a count of bytes, up to 2g.
    MakeFile {
        name: PathBuf,
        size: String,
        /// What it holds.
        #[arg(long, value_enum, default_value = "zero")]
        fill: files::Fill,
    },
    /// This session's log, readably.
    Log {
        /// Only the last N commands.
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Read a dump bundle a capture holds.
    #[command(subcommand)]
    Dump(DumpCommand),
    /// The run's shared ledger of who covers what, and what was found.
    #[command(subcommand)]
    Ledger(evidence::LedgerCommand),
}

#[derive(Subcommand, Debug)]
pub enum HostCommand {
    /// Add a host and start it.
    ///
    /// --lan gives it a LAN listener (needed to pair, and for the phone);
    /// --discovery lets it find and be found by the net's other hosts by
    /// name; --bonjour advertises it on the machine's real mDNS so the
    /// phone finds it (needs --lan, not --discovery). --process runs it as
    /// a real `amux daemon` process; --supervised under `amux supervise`,
    /// taking updates from `release publish`. --provider real makes every
    /// agent created on it, from any client, the real Claude or Codex.
    Add(HostArgs),
    /// Take a host out: its agents killed and its daemon gone.
    Remove { name: String },
}

#[derive(Args, Debug)]
pub struct HostArgs {
    /// The host's name: letters, digits and '-'.
    pub name: String,
    /// A LAN listener, which pairing, real links and the phone need.
    #[arg(long)]
    pub lan: bool,
    /// Find and be found by the net's other hosts by name. A process host
    /// is found by the hosts in the net's process but does not browse
    /// itself.
    #[arg(long)]
    pub discovery: bool,
    /// Advertise on the machine's real mDNS, so the phone finds it (not
    /// with --discovery).
    #[arg(long)]
    pub bonjour: bool,
    /// The relay account it signs in to at start.
    #[arg(long)]
    pub account: Option<String>,
    /// Run it as a real `amux` daemon process.
    #[arg(long)]
    pub process: bool,
    /// Run it under `amux supervise`, taking releases from `release
    /// publish` (implies --process).
    #[arg(long)]
    pub supervised: bool,
    /// What every agent made on it runs: the real Claude or Codex, or the
    /// scripted fakes. Fakes unless given; real, and only real, in a QA run.
    #[arg(long, value_enum)]
    pub provider: Option<ProviderArg>,
    /// A script file every agent a client creates on it plays (fake hosts).
    #[arg(long)]
    pub script: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum AccountCommand {
    /// Add an account the relay's cloud knows; `refresh-NAME` is its login.
    Add {
        name: String,
        #[arg(long, value_enum, default_value_t = Tier::Pro)]
        tier: Tier,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReleaseCommand {
    /// Publish the build under test as VERSION and have every supervised
    /// host check for it now.
    ///
    /// --broken publishes a build that exits at once, so a supervisor that
    /// takes it rolls back after its failed starts. Supervised hosts start
    /// on 0.0.1, so any ordinary version is newer.
    Publish {
        version: String,
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Percent of hosts that take it, by their host id.
        #[arg(long)]
        rollout: Option<u8>,
        #[arg(long)]
        broken: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum GateCommand {
    /// Create the gate file NAME.
    Open { name: String },
}

#[derive(Subcommand, Debug)]
pub enum DumpCommand {
    /// Print a dump bundle (a folder under findings/*/dumps/) as text: each
    /// agent's row, journal, store rows, facts and every other file.
    Show {
        bundle: PathBuf,
        /// Only this agent: an id prefix or a name.
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct FlapArgs {
    /// Two hosts, or `stop`.
    #[arg(num_args = 1..=2, required = true)]
    pub hosts: Vec<String>,
    /// How long each cut lasts.
    #[arg(long)]
    pub down: Option<String>,
    /// How long the link stays up between cuts.
    #[arg(long)]
    pub up: Option<String>,
}

#[derive(Args, Debug)]
pub struct NetArgs {
    /// A host's name, or `relay`.
    pub target: String,
    /// Percent of datagrams lost, each way.
    #[arg(long, default_value_t = 0)]
    pub loss: u32,
    #[arg(long)]
    pub delay: Option<String>,
    /// Up to this much more delay, drawn for each datagram.
    #[arg(long)]
    pub jitter: Option<String>,
    /// Percent of datagrams overtaken by the ones behind them.
    #[arg(long, default_value_t = 0)]
    pub reorder: u32,
    /// The most bytes a second that get through.
    #[arg(long)]
    pub rate: Option<String>,
}

#[derive(Args, Debug)]
pub struct SpawnArgs {
    /// The agent's name, which later commands take.
    pub name: String,
    /// The host it runs on.
    #[arg(long)]
    pub host: String,
    /// Headless Claude (claude-sdk), terminal Claude (claude-pty) or Codex.
    #[arg(long, value_enum, default_value_t = KindArg::ClaudeSdk)]
    pub kind: KindArg,
    /// The host's provider when not given.
    #[arg(long, value_enum)]
    pub provider: Option<ProviderArg>,
    /// What a fake plays: a script file.
    #[arg(long)]
    pub script: Option<PathBuf>,
    /// The first prompt.
    #[arg(long)]
    pub prompt: Option<String>,
    /// The model it starts on: haiku for Claude, gpt-5.6-luna for Codex.
    #[arg(long)]
    pub model: Option<String>,
    /// The permission it starts with, as the provider names it
    /// (`plan` starts Claude planning).
    #[arg(long)]
    pub permission: Option<String>,
    /// Codex's collaboration mode to start in (`plan` starts Codex
    /// planning).
    #[arg(long)]
    pub mode: Option<String>,
    /// A parent agent, for a family.
    #[arg(long)]
    pub parent: Option<String>,
    /// Its working folder: relative is below the host's work folder.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Return once it has come to rest.
    #[arg(long)]
    pub wait: bool,
    /// How long --wait waits, in seconds: by default 100 for a fake, 240
    /// for a real provider. Give your shell call longer than this.
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Args, Debug)]
pub struct SendArgs {
    pub agent: String,
    #[arg(required = true)]
    pub text: Vec<String>,
    #[arg(long)]
    pub wait: bool,
    /// How long --wait waits, in seconds: by default 100 for a fake, 240
    /// for a real provider. Give your shell call longer than this.
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    pub agent: String,
    /// In seconds: by default 100 for a fake, 240 for a real provider.
    /// Give your shell call longer than this.
    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Tier {
    Pro,
    Free,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ProviderArg {
    Real,
    Fake,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum KindArg {
    ClaudeSdk,
    ClaudePty,
    Codex,
}

impl ProviderArg {
    fn word(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Fake => "fake",
        }
    }
}

impl KindArg {
    fn word(self) -> &'static str {
        match self {
            Self::ClaudeSdk => "claude_sdk",
            Self::ClaudePty => "claude_pty",
            Self::Codex => "codex",
        }
    }
}

/// Runs one `ctl` command from the current folder and answers the exit
/// code.
pub fn main(arguments: Vec<String>) -> std::process::ExitCode {
    let ctl = match Ctl::try_parse_from(
        std::iter::once("testnet ctl".to_owned()).chain(arguments.iter().cloned()),
    ) {
        Ok(ctl) => ctl,
        Err(error) => {
            let _ = error.print();
            return std::process::ExitCode::from(if error.use_stderr() { 2 } else { 0 });
        }
    };
    let folder = match Folder::here() {
        Ok(folder) => folder,
        Err(error) => {
            eprintln!("testnet ctl: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let json = ctl.json;
    let outcome = run(&folder, ctl.command);
    folder.log(&arguments, &outcome);
    match outcome {
        Ok(done) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&done.value).unwrap_or_default()
                );
            } else if !done.text.is_empty() {
                println!("{}", done.text.trim_end());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("testnet ctl: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(folder: &Folder, command: Command) -> Result<Outcome, String> {
    match command {
        Command::Up { topology } => session::up(folder, topology.as_deref()),
        Command::Down => session::down(folder),
        Command::Status => {
            let status = folder.call(json!({"Status": {}}))?;
            Ok(Outcome::new(describe_status(&status), status))
        }
        Command::Host(HostCommand::Add(host)) => {
            let provider = match (host.provider, in_qa_run()) {
                (Some(ProviderArg::Fake), true) => return Err(REAL_ONLY.to_owned()),
                (_, true) if host.script.is_some() => return Err(REAL_ONLY.to_owned()),
                (None, true) => ProviderArg::Real,
                (Some(provider), _) => provider,
                (None, false) => ProviderArg::Fake,
            };
            let mut decl = json!({
                "name": host.name,
                "lan": host.lan,
                "discovery": host.discovery,
                "bonjour": host.bonjour,
                "process": host.process,
                "supervised": host.supervised,
                "provider": provider.word(),
            });
            if let Some(account) = host.account {
                decl["account"] = json!(account);
            }
            if let Some(script) = host.script {
                decl["script"] = read_json(&script)?;
            }
            ack(folder.call(json!({"AddHost": {"host": decl}}))?)
        }
        Command::Host(HostCommand::Remove { name }) => {
            ack(folder.call(json!({"RemoveHost": {"host": name}}))?)
        }
        Command::Link { a, b } => ack(folder.call(json!({"AddLink": {"a": a, "b": b}}))?),
        Command::Unlink { a, b } => ack(folder.call(json!({"RemoveLink": {"a": a, "b": b}}))?),
        Command::Account(AccountCommand::Add { name, tier }) => ack(folder.call(
            json!({"AddAccount": {"account": {"name": name, "tier": tier_word(tier)}}}),
        )?),
        Command::Sever { a, b } => ack(folder.call(json!({"Sever": {"a": a, "b": b}}))?),
        Command::Restore { a, b } => ack(folder.call(json!({"Restore": {"a": a, "b": b}}))?),
        Command::Flap(flap) => match flap.hosts.as_slice() {
            [stop] if stop == "stop" => ack(folder.call(json!({"FlapStop": {}}))?),
            [a, b] => {
                let down = duration_ms(flap.down.as_deref().ok_or("flap needs --down")?)?;
                let up = duration_ms(flap.up.as_deref().ok_or("flap needs --up")?)?;
                ack(folder.call(
                    json!({"Flap": {"a": a, "b": b, "down_ms": down, "up_ms": up}}),
                )?)
            }
            _ => Err("flap takes two hosts, or `stop`".to_owned()),
        },
        Command::Net(net) => ack(folder.call(json!({"NetFaults": {
            "target": net.target,
            "loss_percent": net.loss,
            "delay_ms": net.delay.as_deref().map(duration_ms).transpose()?.unwrap_or(0),
            "jitter_ms": net.jitter.as_deref().map(duration_ms).transpose()?.unwrap_or(0),
            "reorder_percent": net.reorder,
            "rate_bytes_per_sec": net.rate.as_deref().map(rate).transpose()?.unwrap_or(0),
        }}))?),
        Command::BlockUdp { host, state } => ack(folder.call(
            json!({"BlockUdp": {"host": host, "blocked": matches!(state, OnOff::On)}}),
        )?),
        Command::RefuseCalls { a, b, state } => ack(folder.call(
            json!({"RefuseCalls": {"a": a, "b": b, "refuse": matches!(state, OnOff::On)}}),
        )?),
        Command::Kill { host } => ack(folder.call(json!({"KillDaemon": {"host": host}}))?),
        Command::Stop { host } => ack(folder.call(json!({"StopDaemon": {"host": host}}))?),
        Command::Restart { host } => ack(folder.call(json!({"RestartDaemon": {"host": host}}))?),
        Command::Freeze { agent } => ack(folder.call(json!({"Freeze": {"agent": agent}}))?),
        Command::Thaw { agent } => ack(folder.call(json!({"Thaw": {"agent": agent}}))?),
        Command::SignIn { host, account } => {
            ack(folder.call(json!({"SignIn": {"host": host, "account": account}}))?)
        }
        Command::Tier { account, tier } => ack(folder.call(
            json!({"SetTier": {"account": account, "tier": tier_word(tier)}}),
        )?),
        Command::Release(ReleaseCommand::Publish {
            version,
            channel,
            rollout,
            broken,
        }) => ack(folder.call_long(
            json!({"Release": {"version": version, "channel": channel, "rollout": rollout, "broken": broken}}),
        )?),
        Command::Spawn(spawn) => spawn_agent(folder, spawn),
        Command::Send(send) => {
            let text = send.text.join(" ");
            let sent = folder.call(json!({"Send": {"agent": send.agent, "text": text}}))?;
            let verdict = sent["verdict"].as_str().unwrap_or("").to_owned();
            if !verdict.contains("Accepted") {
                return Err(format!("{} refused the prompt: {verdict}", send.agent));
            }
            let mut said = format!(
                "{} took the prompt{}",
                send.agent,
                if verdict.contains("queued: true") {
                    " (queued behind what it is doing)"
                } else {
                    ""
                }
            );
            if send.wait {
                folder.call_long(json!({"Settle": {
                    "agent": send.agent,
                    "input": sent["input_id"],
                    "timeout_ms": send.timeout.map(|seconds| seconds * 1000),
                }}))?;
                said.push_str(" and is at rest again");
            }
            Ok(Outcome::new(said, sent))
        }
        Command::Wait(wait) => ack(folder.call_long(json!({"Settle": {
            "agent": wait.agent,
            "timeout_ms": wait.timeout.map(|seconds| seconds * 1000),
        }}))?),
        Command::StopAgent { agent } => ack(folder.call(json!({"Stop": {"agent": agent}}))?),
        Command::Resume { agent, text } => {
            let text = (!text.is_empty()).then(|| text.join(" "));
            let resumed = folder.call(json!({"Resume": {"agent": agent, "text": text}}))?;
            Ok(Outcome::new(
                format!("{agent} resumed as incarnation {}", resumed["incarnation"]),
                resumed,
            ))
        }
        Command::Gate(GateCommand::Open { name }) => {
            ack(folder.call(json!({"OpenGate": {"name": name}}))?)
        }
        Command::Fleet { host } => {
            let fleet = folder.call(json!({"Inventory": {"host": host}}))?;
            Ok(Outcome::new(describe_fleet(folder, &fleet), fleet))
        }
        Command::Chat { host, agent } => {
            let chat = folder.call(json!({"Chat": {"host": host, "agent": agent}}))?;
            Ok(Outcome::new(describe_chat(&chat), chat))
        }
        Command::Block { host, agent } => {
            let held = folder.call(json!({"Block": {"host": host, "agent": agent}}))?;
            Ok(Outcome::new(
                format!("{host}'s copy of {agent} is one whole block"),
                held,
            ))
        }
        Command::ProviderInput { agent } => {
            let lines = folder.call(json!({"ProviderInput": {"agent": agent}}))?;
            let text = lines["lines"]
                .as_array()
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Ok(Outcome::new(
                if text.is_empty() {
                    format!("{agent}'s provider has read nothing (only the headless Claude and Codex fakes log)")
                } else {
                    text
                },
                lines,
            ))
        }
        Command::Amux { host, args } => session::amux(folder, &host, &args),
        Command::Tui(command) => tui::run(folder, command),
        Command::Phone(command) => phone::run(folder, command),
        Command::Capture { name, note } => evidence::capture(folder, &name, note.as_deref()),
        Command::MakeFile { name, size, fill } => files::make(folder, &name, &size, fill),
        Command::Log { tail } => evidence::log(folder, tail),
        Command::Dump(DumpCommand::Show { bundle, agent }) => {
            evidence::dump_show(folder, &bundle, agent.as_deref())
        }
        Command::Ledger(command) => evidence::ledger(folder, command),
    }
}

/// Whether this session is a tester's in a `just qa` run.
fn in_qa_run() -> bool {
    std::env::var_os(evidence::RUN_ENV).is_some_and(|run| !run.is_empty())
}

/// A tester stands in for a person, and a person only ever meets real
/// agents; a fake script that reproduces a finding is built in triage.
const REAL_ONLY: &str = "a QA tester uses real agents only, as a person meets them: \
     leave out --provider fake and --script (a fake-script repro is triage's work)";

fn spawn_agent(folder: &Folder, spawn: SpawnArgs) -> Result<Outcome, String> {
    if in_qa_run() && (matches!(spawn.provider, Some(ProviderArg::Fake)) || spawn.script.is_some())
    {
        return Err(REAL_ONLY.to_owned());
    }
    let mut decl = json!({
        "name": spawn.name,
        "host": spawn.host,
        "kind": spawn.kind.word(),
    });
    for (key, value) in [
        ("prompt", spawn.prompt),
        ("model", spawn.model),
        ("permission", spawn.permission),
        ("mode", spawn.mode),
        ("parent", spawn.parent),
        ("cwd", spawn.cwd),
    ] {
        if let Some(value) = value {
            decl[key] = json!(value);
        }
    }
    if let Some(provider) = spawn.provider {
        decl["provider"] = json!(provider.word());
    }
    if let Some(script) = &spawn.script {
        decl["script"] = read_json(script)?;
    }
    let spawned = folder.call_long(json!({"Spawn": {"agent": decl}}))?;
    let mut said = format!(
        "{} started ({})",
        spawn.name,
        spawned["id"].as_str().unwrap_or("?")
    );
    if spawn.wait {
        folder.call_long(json!({"Settle": {
            "agent": spawn.name,
            "timeout_ms": spawn.timeout.map(|seconds| seconds * 1000),
        }}))?;
        said.push_str(" and is at rest");
    }
    Ok(Outcome::new(said, spawned))
}

fn ack(value: Value) -> Result<Outcome, String> {
    let text = match &value {
        Value::Object(object) => object
            .get("installed")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    Ok(Outcome::new(text, value))
}

fn tier_word(tier: Tier) -> &'static str {
    match tier {
        Tier::Pro => "pro",
        Tier::Free => "free",
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{} is not JSON: {error}", path.display()))
}

/// `250ms`, `2s`, `1.5s` or a bare number of milliseconds.
pub(crate) fn duration_ms(text: &str) -> Result<u64, String> {
    let bad = || format!("{text:?} is not a duration: give it as 250ms or 2s");
    let text = text.trim();
    let (number, scale) = if let Some(ms) = text.strip_suffix("ms") {
        (ms, 1.0)
    } else if let Some(s) = text.strip_suffix('s') {
        (s, 1000.0)
    } else if let Some(m) = text.strip_suffix('m') {
        (m, 60_000.0)
    } else {
        (text, 1.0)
    };
    let value: f64 = number.trim().parse().map_err(|_| bad())?;
    if value.is_nan() || value < 0.0 {
        return Err(bad());
    }
    Ok((value * scale).round() as u64)
}

/// `64k`, `1.5m` or a bare number of bytes a second.
fn rate(text: &str) -> Result<u64, String> {
    let bad = || format!("{text:?} is not a rate: give bytes a second, as 64k or 2m");
    let lower = text.trim().to_ascii_lowercase();
    let (number, scale) = if let Some(k) = lower.strip_suffix('k') {
        (k.to_owned(), 1024.0)
    } else if let Some(m) = lower.strip_suffix('m') {
        (m.to_owned(), 1024.0 * 1024.0)
    } else {
        (lower, 1.0)
    };
    let value: f64 = number.trim().parse().map_err(|_| bad())?;
    Ok((value * scale).round() as u64)
}

fn describe_status(status: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "net at {}\n",
        status["root"].as_str().unwrap_or("?")
    ));
    if let Some(scope) = status["topology"]["scope"].as_str() {
        out.push_str(&format!("discovery scope {scope:?}\n"));
    }
    out.push_str("hosts:\n");
    for host in status["hosts"].as_array().into_iter().flatten() {
        let mut traits = Vec::new();
        traits.push(if host["up"].as_bool() == Some(true) {
            "up"
        } else {
            "DOWN"
        });
        if host["supervised"].as_bool() == Some(true) {
            traits.push("supervised process");
        } else if host["process"].as_bool() == Some(true) {
            traits.push("process");
        } else {
            traits.push("in-process");
        }
        traits.push(if host["provider"] == "real" {
            "real providers"
        } else {
            "fake providers"
        });
        let lan = host["lan_port"]
            .as_u64()
            .map(|port| format!(", LAN 127.0.0.1:{port}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {} ({}{lan}) id {}\n    config {}\n",
            host["name"].as_str().unwrap_or("?"),
            traits.join(", "),
            host["host_id"].as_str().unwrap_or("?"),
            host["config"].as_str().unwrap_or("?"),
        ));
    }
    let links = status["topology"]["links"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str(&format!(
        "links: {}\n",
        if links.is_empty() {
            "none".to_owned()
        } else {
            links
                .iter()
                .map(|link| {
                    format!(
                        "{} - {}",
                        link["a"].as_str().unwrap_or("?"),
                        link["b"].as_str().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    let accounts: Vec<String> = status["topology"]["relay"]["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|account| {
            format!(
                "{} ({})",
                account["name"].as_str().unwrap_or("?"),
                account["tier"].as_str().unwrap_or("pro")
            )
        })
        .collect();
    out.push_str(&format!(
        "relay accounts: {}\n",
        if accounts.is_empty() {
            "none".to_owned()
        } else {
            accounts.join(", ")
        }
    ));
    out.push_str("agents the net started (agents clients made are in `fleet HOST`):\n");
    let agents = status["agents"].as_array().cloned().unwrap_or_default();
    if agents.is_empty() {
        out.push_str("  none\n");
    }
    for agent in agents {
        out.push_str(&format!(
            "  {} on {} ({}, {}) id {}\n",
            agent["name"].as_str().unwrap_or("?"),
            agent["host"].as_str().unwrap_or("?"),
            agent["kind"].as_str().unwrap_or("?"),
            agent["provider"].as_str().unwrap_or("fake"),
            agent["id"].as_str().unwrap_or("?"),
        ));
    }
    out
}

fn describe_fleet(folder: &Folder, fleet: &Value) -> String {
    let hosts = folder
        .call(json!({"Status": {}}))
        .ok()
        .and_then(|status| status["hosts"].as_array().cloned())
        .unwrap_or_default();
    let host_name = |id: &str| {
        hosts
            .iter()
            .find(|host| host["host_id"] == id)
            .and_then(|host| host["name"].as_str())
            .unwrap_or(id)
            .to_owned()
    };
    let agents = fleet["agents"].as_array().cloned().unwrap_or_default();
    if agents.is_empty() {
        return "no agents".to_owned();
    }
    let mut out = String::new();
    for agent in agents {
        let phase = agent["phase_name"].as_str().unwrap_or("?");
        out.push_str(&format!(
            "{}  on {}  {}  {}{}  incarnation {}  id {}\n",
            agent["name"].as_str().unwrap_or("?"),
            host_name(agent["host_id"].as_str().unwrap_or("?")),
            agent["kind"].as_str().unwrap_or("?"),
            agent["lifecycle_name"].as_str().unwrap_or("?"),
            if phase.is_empty() || phase == "UNSPECIFIED" {
                String::new()
            } else {
                format!(" {phase}")
            },
            agent["incarnation"],
            agent["id"].as_str().unwrap_or("?"),
        ));
        if let Some(working) = agent["working_on"].as_str() {
            out.push_str(&format!("    working on: {working}\n"));
        }
        if let Some(cause) = agent["exit_cause"].as_str() {
            let detail = agent["exit_detail"]
                .as_str()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default();
            out.push_str(&format!("    ended: {cause}{detail}\n"));
        }
    }
    out
}

fn describe_chat(chat: &Value) -> String {
    let mut out = format!(
        "phase {}  model {}\n",
        chat["phase"].as_str().unwrap_or("?"),
        chat["model"].as_str().unwrap_or("?")
    );
    for item in chat["items"].as_array().into_iter().flatten() {
        let text = item["text"].as_str().unwrap_or("");
        out.push_str(&format!(
            "#{} {}{}: {}\n",
            item["order"],
            item["key"].as_str().unwrap_or("?"),
            item["input_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(|id| format!(" (input {id})"))
                .unwrap_or_default(),
            text.replace('\n', "\n    ")
        ));
        if let Some(body) = item["body"].as_str().filter(|body| !body.is_empty()) {
            out.push_str(&format!("    = {body}\n"));
        }
    }
    let asks: Vec<&str> = chat["asks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !asks.is_empty() {
        out.push_str("waiting on you:\n");
        for ask in asks {
            out.push_str(&format!("  {ask}\n"));
        }
    }
    out
}

/// How long a command waits for the door by default; the long ones (a
/// spawn on a real provider, a wait) give their own.
pub(crate) const DOOR_PATIENCE: Duration = Duration::from_secs(120);
