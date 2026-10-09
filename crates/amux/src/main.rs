//! The amux binary: the daemon entry, the CLI verbs, and the hidden
//! subcommands an agent's harness runs from the canonical install path —
//! the agent process itself, the tool server its provider launches, and
//! the hook command terminal Claude runs for each hook event.

mod attach;
mod connect;
mod pairing;
mod profiles;
mod relay;
mod server;
mod setup;
mod supervise;
mod ui;
mod verbs;

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tonic::Code;

use crate::verbs::{CliKind, CliStopMode};

#[derive(Debug, Parser)]
#[command(name = "amux", version = node::version(), about = "Agent multiplexer")]
struct Cli {
    /// The installation config file.
    #[arg(long, global = true, env = "AMUX_CONFIG")]
    config: Option<PathBuf>,

    /// The profile to act in, by label or id; the first one otherwise.
    #[arg(long, global = true)]
    profile: Option<String>,

    /// With none, the terminal client opens.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the agents, children beneath their parent.
    #[command(alias = "list")]
    Ls,
    /// Start a new agent.
    #[command(alias = "new")]
    Create {
        /// Which agent: claude_pty, claude_sdk or codex.
        kind: CliKind,
        #[arg(long)]
        name: Option<String>,
        /// Its working directory; the current one otherwise.
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        /// Its first prompt.
        #[arg(long)]
        prompt: Option<String>,
        /// Extra arguments for Claude, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Hand this terminal to an agent's own interface: terminal Claude's
    /// or a Codex view. Only agents on this machine; <leader> d detaches,
    /// <leader> s opens the fleet over it.
    Attach {
        /// The agent's name or id.
        agent: String,
    },
    /// Send an agent a prompt.
    Send {
        /// The agent's name or id.
        agent: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Stop an agent's process.
    Stop {
        agent: String,
        #[arg(long, value_enum, default_value = "graceful")]
        mode: CliStopMode,
    },
    /// Start an exited agent again, with an optional first prompt.
    Resume { agent: String, text: Vec<String> },
    /// Rename an agent.
    Rename { agent: String, name: String },
    /// Delete an agent, its history and its children.
    #[command(alias = "rm")]
    Delete { agent: String },
    /// Gather a dump of the named agents, or all of them, into one
    /// directory.
    Dump {
        agents: Vec<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Pair with another host: one found nearby by name, an address, or
    /// user@host over SSH. With none, this host opens pairing mode and shows
    /// a PIN, or a QR code for the phone.
    Pair {
        target: Option<String>,
        /// Show a QR code instead of a PIN.
        #[arg(long, conflicts_with_all = ["target", "link", "cancel"])]
        qr: bool,
        /// With --qr, print the QR code's link too, to paste into a simulator.
        #[arg(long = "print-link", requires = "qr")]
        print_link: bool,
        /// Pair with the host whose QR code carries this amux://pair link.
        #[arg(long, value_name = "LINK", conflicts_with_all = ["target", "cancel"])]
        link: Option<String>,
        /// Close this host's pairing mode.
        #[arg(long, conflicts_with = "target")]
        cancel: bool,
        /// Hold a PIN you choose open for an unattended demo, such as an app
        /// reviewer pairing a phone with a machine nobody watches. Every
        /// device that presents it pairs, and wrong guesses do not close it.
        /// The command returns; the window lasts until it expires,
        /// `amux pair --cancel`, or amux restarts.
        #[arg(
            long,
            requires_all = ["pin", "for"],
            conflicts_with_all = ["target", "qr", "link", "cancel"]
        )]
        demo: bool,
        /// The six-digit PIN a demo window holds.
        #[arg(long, value_name = "DIGITS", requires = "demo")]
        pin: Option<String>,
        /// How long a demo window stays open: 30d, 12h, 45m; at most 90 days.
        #[arg(
            long = "for",
            value_name = "DURATION",
            requires = "demo",
            value_parser = pairing::parse_duration
        )]
        r#for: Option<std::time::Duration>,
    },
    /// List the paired hosts and the hosts found nearby.
    Peers,
    /// Stop trusting a paired host, by name or id.
    Unpair {
        peer: String,
        /// Unpair without asking.
        #[arg(long)]
        force: bool,
    },
    /// Sign the profile in to an amux account, so its hosts reach each
    /// other through the relay.
    Login {
        /// The account service.
        #[arg(long, default_value = settings::DEFAULT_CLOUD_URL)]
        cloud_url: String,
    },
    /// Sign the profile out; its agents and paired hosts stay.
    Logout,
    /// The far end of an SSH pairing, over stdin and stdout.
    #[command(name = "pair-recv", hide = true)]
    PairRecv,
    /// A peer's link over SSH, joined to the profile's link socket.
    #[command(hide = true)]
    Relay,
    /// List the installation's profiles.
    Profiles,
    /// Manage the installation's profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Set up this install: whether amux starts at login.
    Init {
        /// Start amux at login, or not; asked when omitted.
        #[arg(long, value_enum)]
        login_item: Option<YesNo>,
        /// Write the login item and print how it would be registered,
        /// without registering it.
        #[arg(long, hide = true)]
        dry_run: bool,
    },
    /// Ask the supervisor to install the channel's release now, even one
    /// that was rolled back here.
    Update,
    /// Change a setting.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Start or stop amux.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Run the daemon in the foreground.
    #[command(hide = true)]
    Daemon,
    /// Run the daemon under a supervisor that restarts it and, under
    /// `updates: auto`, installs releases.
    Supervise {
        /// The state a supervisor hands the binary it execs after an update.
        #[arg(long, hide = true)]
        inherit: Option<String>,
    },
    /// Host one agent: the directory holds its lock, specs and sockets.
    #[command(hide = true)]
    Agent { dir: PathBuf },
    /// Ask a provider what it offers on this host and whether it is signed
    /// in, and write the answer to a file.
    #[command(hide = true)]
    Catalogue {
        /// The provider: claude or codex.
        provider: String,
        /// How the provider is started.
        #[arg(long)]
        command: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Serve amux's tools over stdio to the agent in the directory.
    #[command(hide = true)]
    Mcp { dir: PathBuf },
    /// Hand a provider's hook payload on stdin to its agent.
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        provider: Hooks,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    List,
    Create { label: Option<String> },
    Rename { profile: String, name: String },
    Delete { profile: String },
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Start amux, detached from this terminal: the supervisor where the
    /// install has one, the daemon otherwise.
    Start {
        /// Run the cloud relay instead, in the foreground, from the relay
        /// configuration `--config` names.
        #[arg(long)]
        cloud: bool,
        /// The relay always runs in the foreground; accepted for the
        /// service units that say so.
        #[arg(long, requires = "cloud")]
        foreground: bool,
    },
    /// Stop amux cleanly: the supervisor first where one runs. Agents keep
    /// running.
    Stop,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Which releases the supervisor follows.
    Channel { channel: CliChannel },
    /// Whether amux sends product analytics; with no value, says whether
    /// it does and where.
    Telemetry { state: Option<OnOff> },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum CliChannel {
    Stable,
    Preview,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum YesNo {
    Yes,
    No,
}

#[derive(Debug, Subcommand)]
enum Hooks {
    Claude,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return report(open_ui(cli.config, cli.profile));
    };
    match command {
        Command::Agent { dir } => ExitCode::from(agent::main(dir).clamp(0, 255) as u8),
        Command::Catalogue {
            provider,
            command,
            out,
        } => ExitCode::from(agent::catalogue::main(&provider, &command, &out).clamp(0, 255) as u8),
        Command::Hooks {
            provider: Hooks::Claude,
        } => {
            // Claude reads a hook's exit code as a verdict (2 blocks the
            // action), so a payload that cannot be delivered is reported
            // and the hook still succeeds.
            if let Err(error) = claude_hook() {
                eprintln!("amux hooks claude: {error}");
            }
            ExitCode::SUCCESS
        }
        Command::Daemon => {
            // SAFETY: no other thread exists yet.
            let pipe = unsafe { node::InheritedPipe::take() };
            report(
                pipe.map_err(anyhow::Error::from)
                    .and_then(|pipe| Ok((pipe, connect::load_config(cli.config.as_deref())?)))
                    .and_then(|(pipe, config)| server::run_daemon(&config, pipe)),
            )
        }
        Command::Server {
            command: ServerCommand::Start { cloud: true, .. },
        } => report(relay::run(cli.config.as_deref())),
        Command::Supervise { inherit } => report(
            connect::load_config(cli.config.as_deref()).and_then(|config| {
                supervise::run(&config, cli.config.as_deref(), inherit.as_deref())
            }),
        ),
        command => report(run(command, cli.config, cli.profile)),
    }
}

fn report(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("amux: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn open_ui(config_path: Option<PathBuf>, profile: Option<String>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let config = connect::load_config(config_path.as_deref())?;
        ui::run(&config, profile.as_deref()).await
    })
}

fn run(command: Command, config_path: Option<PathBuf>, profile: Option<String>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        if let Command::Mcp { dir } = command {
            return agent::serve_tools(dir).await.map_err(anyhow::Error::from);
        }
        if let Command::Config {
            command: ConfigCommand::Channel { channel },
        } = command
        {
            return setup::set_channel(
                config_path.as_deref(),
                match channel {
                    CliChannel::Stable => settings::Channel::Stable,
                    CliChannel::Preview => settings::Channel::Preview,
                },
            );
        }
        if let Command::Config {
            command: ConfigCommand::Telemetry { state },
        } = command
        {
            return setup::telemetry(
                config_path.as_deref(),
                state.map(|state| match state {
                    OnOff::On => settings::Switch::On,
                    OnOff::Off => settings::Switch::Off,
                }),
            );
        }
        let config = connect::load_config(config_path.as_deref())?;
        let profile = profile.as_deref();
        match command {
            Command::Init {
                login_item,
                dry_run,
            } => {
                setup::init(
                    &config,
                    config_path.as_deref(),
                    login_item.map(|answer| matches!(answer, YesNo::Yes)),
                    dry_run,
                )
                .await
            }
            Command::Update => setup::update(&config).await,
            Command::Server {
                command: ServerCommand::Start { .. },
            } => server::start(&config, config_path.as_deref()).await,
            Command::Server {
                command: ServerCommand::Stop,
            } => server::stop(&config).await,
            Command::Profiles
            | Command::Profile {
                command: ProfileCommand::List,
            } => {
                let door = connect::front_door(&config).await?;
                profiles::list(&mut connect::profiles(door)).await
            }
            Command::Profile { command } => {
                let door = connect::front_door(&config).await?;
                let mut door = connect::profiles(door);
                match command {
                    ProfileCommand::List => unreachable!("listed above"),
                    ProfileCommand::Create { label } => profiles::create(&mut door, label).await,
                    ProfileCommand::Rename { profile, name } => {
                        profiles::rename(&mut door, &profile, &name).await
                    }
                    ProfileCommand::Delete { profile } => {
                        profiles::delete(&mut door, &profile).await
                    }
                }
            }
            Command::Attach { agent } => ui::attach(&config, profile, &agent).await,
            Command::Pair {
                target,
                qr,
                print_link,
                link,
                cancel,
                demo: _,
                pin,
                r#for,
            } => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                if cancel {
                    pairing::cancel(door, &selected).await
                } else if let (Some(pin), Some(ttl)) = (pin, r#for) {
                    pairing::demo(door, &selected, pin, ttl).await
                } else if let Some(link) = link {
                    pairing::pair_with_link(door, &selected, &link).await
                } else if let Some(target) = target {
                    let mut client = connect::client_of(&selected).await?;
                    let target = pairing::Target::parse(&target)?;
                    pairing::pair(door, &mut client, &selected, target).await
                } else {
                    pairing::wait(door, &selected, qr, print_link).await
                }
            }
            Command::Peers => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                let mut client = connect::client_of(&selected).await?;
                pairing::peers(door, &mut client, &selected).await
            }
            Command::Unpair { peer, force } => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                pairing::unpair(door, &selected, &peer, force).await
            }
            Command::Login { cloud_url } => {
                let door = connect::profiles(connect::front_door(&config).await?);
                // Without --profile the daemon chooses: the profile already
                // bound to this account, or the unbound one.
                let selected = match profile {
                    Some(wanted) => Some(connect::select(&mut door.clone(), Some(wanted)).await?),
                    None => None,
                };
                pairing::login(door, selected.as_ref(), &cloud_url).await
            }
            Command::Logout => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                pairing::logout(door, &selected).await
            }
            #[cfg(unix)]
            Command::PairRecv => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                pairing::pair_recv(door, &selected).await
            }
            #[cfg(unix)]
            Command::Relay => {
                let door = connect::profiles(connect::front_door(&config).await?);
                let selected = connect::select(&mut door.clone(), profile).await?;
                pairing::relay(&selected).await
            }
            #[cfg(not(unix))]
            Command::PairRecv | Command::Relay => {
                anyhow::bail!("pairing over SSH needs a Unix host")
            }
            command => {
                let mut client = connect::client(&config, profile).await?;
                match command {
                    Command::Ls => verbs::ls(&mut client).await,
                    Command::Create {
                        kind,
                        name,
                        cwd,
                        model,
                        prompt,
                        args,
                    } => {
                        verbs::create(
                            &mut client,
                            verbs::Create {
                                kind,
                                name,
                                cwd,
                                model,
                                prompt,
                                args,
                            },
                        )
                        .await
                    }
                    Command::Send { agent, text } => {
                        verbs::send(&mut client, &agent, &text.join(" ")).await
                    }
                    Command::Stop { agent, mode } => verbs::stop(&mut client, &agent, mode).await,
                    Command::Resume { agent, text } => {
                        let text = text.join(" ");
                        let text = Some(text.as_str()).filter(|text| !text.is_empty());
                        verbs::resume(&mut client, &agent, text).await
                    }
                    Command::Rename { agent, name } => {
                        verbs::rename(&mut client, &agent, &name).await
                    }
                    Command::Delete { agent } => verbs::delete(&mut client, &agent).await,
                    Command::Dump { agents, reason } => {
                        verbs::dump(&mut client, &agents, reason).await
                    }
                    _ => unreachable!("dispatched above"),
                }
            }
        }
    })
}

/// A daemon error as a person reads it: the daemon's own message, or what
/// a transport failure means.
pub(crate) fn plain(status: tonic::Status) -> anyhow::Error {
    match status.code() {
        Code::Unavailable => anyhow::anyhow!("the amux daemon went away: {}", status.message()),
        _ if status.message().is_empty() => anyhow::anyhow!("{}", status.code().description()),
        _ => anyhow::anyhow!("{}", status.message()),
    }
}

/// Forwards the payload on stdin to the agent's hook socket, which the
/// agent names in the environment it gives Claude.
fn claude_hook() -> std::io::Result<()> {
    let socket = std::env::var_os(claude::hooks::HOOK_SOCKET_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::other(format!("{} is not set", claude::hooks::HOOK_SOCKET_ENV))
        })?;
    let mut payload = Vec::new();
    std::io::stdin().read_to_end(&mut payload)?;
    claude::hooks::forward_from_env(&payload, &socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_demo_window_needs_its_pin_and_its_length() {
        let parse = |args: &[&str]| Cli::try_parse_from(args);
        assert!(parse(&["amux", "pair", "--demo"]).is_err());
        assert!(parse(&["amux", "pair", "--demo", "--pin", "123456"]).is_err());
        assert!(parse(&["amux", "pair", "--pin", "123456", "--for", "1d"]).is_err());
        assert!(
            parse(&[
                "amux", "pair", "--demo", "--pin", "123456", "--for", "30d", "--qr"
            ])
            .is_err()
        );
        let cli = parse(&["amux", "pair", "--demo", "--pin", "123456", "--for", "30d"]).unwrap();
        match cli.command {
            Some(Command::Pair {
                demo, pin, r#for, ..
            }) => {
                assert!(demo);
                assert_eq!(pin.as_deref(), Some("123456"));
                assert_eq!(r#for, Some(std::time::Duration::from_secs(30 * 86_400)));
            }
            other => panic!("not a pairing: {other:?}"),
        }
    }
}
