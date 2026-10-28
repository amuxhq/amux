//! The amux binary: the terminal app, the commands a person or a script
//! runs, and the hidden entries the rest of amux runs. Those are the
//! service manager's (`supervise`, `daemon`, `relay`) and amux's own
//! (`internal …`: the agent process, the tool server and hook command its
//! provider launches, the provider catalogue, and both ends of an SSH
//! link). Every hidden name is called by another amux build — an agent's
//! provider started by the last one, the far host of an SSH pairing, a
//! service unit — so it changes only with all of its callers.

mod attach;
mod config;
mod connect;
mod pairing;
mod profiles;
mod relay;
mod server;
mod setup;
mod status;
mod supervise;
mod ui;
mod verbs;

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tonic::Code;

use crate::verbs::{CliAgent, Interface};

#[derive(Debug, Parser)]
#[command(
    name = "amux",
    version = node::version(),
    about = "Every Claude Code and Codex agent on your machines, in one place",
    long_about = "Every Claude Code and Codex agent on your machines, in one place.\n\n\
                  With no command, the terminal app opens."
)]
struct Cli {
    /// The installation config file.
    #[arg(long, global = true, env = "AMUX_CONFIG", hide = true)]
    config: Option<PathBuf>,

    /// The profile to act in, by label or id; the first one otherwise.
    #[arg(long, global = true)]
    profile: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open an agent's chat in this terminal.
    ///
    /// It opens in the interface `ui.chat_in` names: amux's chat, or the
    /// agent's own (Claude Code's or Codex's) where it has one here. An agent
    /// with no interface of its own on this machine, such as one on another
    /// host or one started from the phone, opens in amux's chat.
    ///
    /// In an agent's own interface, <leader> d leaves it for the shell and
    /// <leader> s opens the fleet over it.
    Attach {
        /// The agent's name or id.
        agent: String,
        /// Open it in this interface instead of the configured one.
        #[arg(long = "in", value_name = "INTERFACE")]
        interface: Option<Interface>,
    },
    /// List the agents, children beneath their parent.
    Ls,
    /// Start a new agent in this directory.
    New {
        agent: CliAgent,
        #[arg(long)]
        name: Option<String>,
        /// Its working directory; the current one otherwise.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Chat with it in this interface instead of the configured one.
        #[arg(long = "in", value_name = "INTERFACE")]
        interface: Option<Interface>,
        /// Its model; `new_agent.<agent>.model` otherwise.
        #[arg(long)]
        model: Option<String>,
        /// Its reasoning effort; `new_agent.<agent>.effort` otherwise.
        #[arg(long)]
        effort: Option<String>,
        /// Its permission; `new_agent.<agent>.permission` otherwise.
        #[arg(long)]
        permission: Option<String>,
        /// Its first prompt.
        #[arg(long)]
        prompt: Option<String>,
        /// Extra arguments for Claude, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Send an agent a prompt.
    Send {
        /// The agent's name or id.
        agent: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Stop an agent now: its turn is cancelled and it exits, resumable.
    Stop { agent: String },
    /// Start a stopped agent again, with an optional first prompt.
    Resume { agent: String, text: Vec<String> },
    /// Rename an agent.
    Rename { agent: String, name: String },
    /// Delete an agent, its history and its children.
    Rm { agent: String },
    /// Gather what amux knows about the named agents, or all of them, into
    /// one directory for a bug report.
    Dump {
        agents: Vec<String>,
        /// What looks wrong, kept with the dump.
        #[arg(long)]
        reason: Option<String>,
        /// Send the dump to the amux account this profile is signed in to,
        /// as a report. It holds what agents were told and said, redacted of
        /// secrets.
        #[arg(long)]
        send: bool,
    },
    /// Pair with another host, or open this one for pairing.
    ///
    /// With a target — a host found nearby by name, an address, or
    /// user@host over SSH — this host pairs with it. With none, this host
    /// opens pairing mode and shows a PIN, or a QR code for the phone.
    Pair {
        target: Option<String>,
        /// Show a QR code for the phone instead of a PIN.
        #[arg(long, conflicts_with_all = ["target", "link", "cancel"])]
        qr: bool,
        /// With --qr, print the QR code's link too, to paste into a simulator.
        #[arg(long = "print-link", requires = "qr", hide = true)]
        print_link: bool,
        /// Pair with the host whose QR code carries this amux://pair link.
        #[arg(
            long,
            value_name = "LINK",
            conflicts_with_all = ["target", "cancel"],
            hide = true
        )]
        link: Option<String>,
        /// Close this host's pairing mode.
        #[arg(long, conflicts_with = "target", hide = true)]
        cancel: bool,
        /// Hold a PIN you choose open for an unattended demo, such as an app
        /// reviewer pairing a phone with a machine nobody watches. Every
        /// device that presents it pairs, and wrong guesses do not close it.
        /// The command returns; the window lasts until it expires,
        /// `amux pair --cancel`, or amux restarts.
        #[arg(
            long,
            requires_all = ["pin", "for"],
            conflicts_with_all = ["target", "qr", "link", "cancel"],
            hide = true
        )]
        demo: bool,
        /// The six-digit PIN a demo window holds.
        #[arg(long, value_name = "DIGITS", requires = "demo", hide = true)]
        pin: Option<String>,
        /// How long a demo window stays open: 30d, 12h, 45m; at most 90 days.
        #[arg(
            long = "for",
            value_name = "DURATION",
            requires = "demo",
            value_parser = pairing::parse_duration,
            hide = true
        )]
        r#for: Option<std::time::Duration>,
    },
    /// List the paired hosts and the hosts found nearby.
    Hosts,
    /// Stop trusting a paired host, by name or id.
    Unpair {
        host: String,
        /// Unpair without asking.
        #[arg(long)]
        force: bool,
    },
    /// Sign the profile in to an amux account, so its hosts reach each
    /// other through the relay.
    Login {
        /// The account service.
        #[arg(long, default_value = settings::DEFAULT_CLOUD_URL, hide = true)]
        cloud_url: String,
    },
    /// Sign the profile out; its agents and paired hosts stay.
    Logout,
    /// List or change the installation's profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Say whether amux is running, which version, and each profile's
    /// account.
    Status,
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
    /// Install the channel's release now.
    Update {
        /// Install it even if it was rolled back here.
        #[arg(long)]
        retry: bool,
    },
    /// Read or change a setting in the config file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Start or stop amux on this machine.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Run the daemon under a supervisor that restarts it and, under
    /// `updates = "auto"`, installs releases: what the login item runs.
    #[command(hide = true)]
    Supervise {
        /// The state a supervisor hands the binary it execs after an update.
        #[arg(long, hide = true)]
        inherit: Option<String>,
    },
    /// Run the daemon in the foreground: what a service manager runs on a
    /// host with `supervisor = false`.
    #[command(hide = true)]
    Daemon,
    /// Run the cloud relay in the foreground, from the relay configuration
    /// `--config` names.
    #[command(hide = true)]
    Relay,
    /// What amux runs on itself.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: Internal,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    Ls,
    New { label: Option<String> },
    Rename { profile: String, name: String },
    Rm { profile: String },
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Start amux, detached from this terminal: the supervisor where the
    /// install has one, the daemon otherwise.
    Start,
    /// Stop amux cleanly: the supervisor first where one runs. Agents keep
    /// running.
    Stop,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print a setting's value, the default when the file does not set it.
    Get { key: String },
    /// Set a setting, keeping the rest of the file as it is.
    Set { key: String, value: String },
    /// Remove a setting from the file, so its default applies.
    Unset { key: String },
    /// Open the config file in $VISUAL or $EDITOR, and check it after.
    Edit,
    /// Print where the config file is.
    Path,
}

#[derive(Debug, Subcommand)]
enum Internal {
    /// Host one agent: the directory holds its lock, specs and sockets.
    Agent { dir: PathBuf },
    /// Ask a provider what it offers on this host and whether it is signed
    /// in, and write the answer to a file.
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
    Mcp { dir: PathBuf },
    /// Hand a provider's hook payload on stdin to its agent.
    Hook {
        #[command(subcommand)]
        provider: Hook,
    },
    /// The far end of an SSH pairing, over stdin and stdout.
    SshPair,
    /// A peer's link over SSH, joined to the profile's link socket.
    SshLink,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum YesNo {
    Yes,
    No,
}

#[derive(Debug, Subcommand)]
enum Hook {
    Claude,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return report(open_ui(cli.config, cli.profile));
    };
    match command {
        Command::Internal {
            command: Internal::Agent { dir },
        } => ExitCode::from(agent::main(dir).clamp(0, 255) as u8),
        Command::Internal {
            command:
                Internal::Catalogue {
                    provider,
                    command,
                    out,
                },
        } => ExitCode::from(agent::catalogue::main(&provider, &command, &out).clamp(0, 255) as u8),
        Command::Internal {
            command: Internal::Hook {
                provider: Hook::Claude,
            },
        } => {
            // Claude reads a hook's exit code as a verdict (2 blocks the
            // action), so a payload that cannot be delivered is reported
            // and the hook still succeeds.
            if let Err(error) = claude_hook() {
                eprintln!("amux internal hook claude: {error}");
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
        Command::Relay => report(relay::run(cli.config.as_deref())),
        Command::Supervise { inherit } => report(
            connect::load_config(cli.config.as_deref()).and_then(|config| {
                supervise::run(&config, cli.config.as_deref(), inherit.as_deref())
            }),
        ),
        Command::Status => match block_on(status::run(cli.config.as_deref())) {
            // Not running is an answer, not a failure to give one; the exit
            // code carries it for scripts that ask, as `systemctl status`
            // does.
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(3),
            Err(error) => report(Err(error)),
        },
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

fn block_on<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

fn open_ui(config_path: Option<PathBuf>, profile: Option<String>) -> Result<()> {
    block_on(async move {
        let config = connect::load_config(config_path.as_deref())?;
        ui::run(&config, profile.as_deref()).await
    })
}

fn run(command: Command, config_path: Option<PathBuf>, profile: Option<String>) -> Result<()> {
    block_on(async move {
        match command {
            Command::Internal {
                command: Internal::Mcp { dir },
            } => return agent::serve_tools(dir).await.map_err(anyhow::Error::from),
            // The config verbs read and write the file alone: they work with
            // amux stopped, and on a file that does not read yet.
            Command::Config { command } => {
                let path = config_path
                    .clone()
                    .unwrap_or_else(settings::InstallationConfig::default_path);
                return match command {
                    ConfigCommand::Get { key } => config::get(&path, &key),
                    ConfigCommand::Set { key, value } => config::set(&path, &key, &value),
                    ConfigCommand::Unset { key } => config::unset(&path, &key),
                    ConfigCommand::Edit => config::edit(&path),
                    ConfigCommand::Path => {
                        println!("{}", path.display());
                        Ok(())
                    }
                };
            }
            _ => {}
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
            Command::Update { retry } => setup::update(&config, retry).await,
            Command::Server {
                command: ServerCommand::Start,
            } => server::start(&config, config_path.as_deref()).await,
            Command::Server {
                command: ServerCommand::Stop,
            } => server::stop(&config).await,
            Command::Profile { command } => {
                let door = connect::front_door(&config).await?;
                let mut door = connect::profiles(door);
                match command {
                    ProfileCommand::Ls => profiles::list(&mut door).await,
                    ProfileCommand::New { label } => profiles::create(&mut door, label).await,
                    ProfileCommand::Rename { profile, name } => {
                        profiles::rename(&mut door, &profile, &name).await
                    }
                    ProfileCommand::Rm { profile } => profiles::delete(&mut door, &profile).await,
                }
            }
            Command::Attach { agent, interface } => {
                ui::attach(&config, profile, &agent, interface).await
            }
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
                let selected = connect::profile(&config, profile).await?;
                let admin = connect::admin_of(&selected).await?;
                if cancel {
                    pairing::cancel(admin).await
                } else if let (Some(pin), Some(ttl)) = (pin, r#for) {
                    pairing::demo(admin, pin, ttl).await
                } else if let Some(link) = link {
                    pairing::pair_with_link(admin, &link).await
                } else if let Some(target) = target {
                    let mut client = connect::client_of(&selected).await?;
                    let target = pairing::Target::parse(&target)?;
                    pairing::pair(admin, &mut client, target, pairing::on_free(&selected)).await
                } else {
                    pairing::wait(admin, qr, print_link).await
                }
            }
            Command::Hosts => {
                let selected = connect::profile(&config, profile).await?;
                let mut client = connect::client_of(&selected).await?;
                pairing::hosts(
                    connect::admin_of(&selected).await?,
                    &mut client,
                    pairing::on_free(&selected),
                )
                .await
            }
            Command::Unpair { host, force } => {
                let selected = connect::profile(&config, profile).await?;
                pairing::unpair(connect::admin_of(&selected).await?, &host, force).await
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
                let selected = connect::profile(&config, profile).await?;
                pairing::logout(connect::admin_of(&selected).await?, &selected).await
            }
            #[cfg(unix)]
            Command::Internal {
                command: Internal::SshPair,
            } => {
                let selected = connect::profile(&config, profile).await?;
                pairing::pair_recv(connect::admin_of(&selected).await?).await
            }
            #[cfg(unix)]
            Command::Internal {
                command: Internal::SshLink,
            } => pairing::relay(&connect::profile(&config, profile).await?).await,
            #[cfg(not(unix))]
            Command::Internal {
                command: Internal::SshPair | Internal::SshLink,
            } => {
                anyhow::bail!("pairing over SSH needs a Unix host")
            }
            command => {
                let mut client = connect::client(&config, profile).await?;
                match command {
                    Command::Ls => verbs::ls(&mut client).await,
                    Command::New {
                        agent,
                        name,
                        cwd,
                        interface,
                        model,
                        effort,
                        permission,
                        prompt,
                        args,
                    } => {
                        verbs::create(
                            &mut client,
                            &config,
                            verbs::Create {
                                agent,
                                interface,
                                name,
                                cwd,
                                model,
                                effort,
                                permission,
                                prompt,
                                args,
                            },
                        )
                        .await
                    }
                    Command::Send { agent, text } => {
                        verbs::send(&mut client, &agent, &text.join(" ")).await
                    }
                    Command::Stop { agent } => verbs::stop(&mut client, &agent).await,
                    Command::Resume { agent, text } => {
                        let text = text.join(" ");
                        let text = Some(text.as_str()).filter(|text| !text.is_empty());
                        verbs::resume(&mut client, &agent, text).await
                    }
                    Command::Rename { agent, name } => {
                        verbs::rename(&mut client, &agent, &name).await
                    }
                    Command::Rm { agent } => verbs::delete(&mut client, &agent).await,
                    Command::Dump {
                        agents,
                        reason,
                        send,
                    } => {
                        let selected = connect::profile(&config, profile).await?;
                        let mut admin = connect::admin_of(&selected).await?;
                        verbs::dump(&mut client, &mut admin, &agents, reason, send).await
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
    use clap::CommandFactory as _;

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

    /// The help a person sees lists the commands they run and nothing amux
    /// runs on itself; the hidden entries still parse.
    #[test]
    fn help_lists_only_what_people_run() {
        let help = Cli::command().render_help().to_string();
        let listed: Vec<&str> = help
            .lines()
            .skip_while(|line| !line.starts_with("Commands:"))
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert_eq!(
            listed,
            [
                "attach", "ls", "new", "send", "stop", "resume", "rename", "rm", "dump", "pair",
                "hosts", "unpair", "login", "logout", "profile", "status", "init", "update",
                "config", "server", "help",
            ]
        );
        for hidden in [
            &["amux", "supervise"][..],
            &["amux", "daemon"],
            &["amux", "relay"],
            &["amux", "internal", "agent", "/dir"],
            &["amux", "internal", "mcp", "/dir"],
            &["amux", "internal", "hook", "claude"],
            &["amux", "internal", "ssh-pair"],
            &["amux", "internal", "ssh-link", "--profile", "abc"],
            &[
                "amux",
                "internal",
                "catalogue",
                "claude",
                "--command",
                "claude",
                "--out",
                "/out",
            ],
        ] {
            assert!(Cli::try_parse_from(hidden).is_ok(), "{hidden:?}");
        }
    }

    #[test]
    fn the_clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
