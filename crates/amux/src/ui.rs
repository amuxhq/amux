//! Bare `amux`: the terminal client on the selected profile; and `amux
//! attach`, whose fleet chord opens that client over the attached agent.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use settings::{ColorSetting, InstallationConfig, ThemeSetting, UiSettings};
use tui::{
    AttachFn, AttachReturn, ColorPreference, TerminalColors, Theme, ThemeError, TuiConfig,
    detect_color_mode, parse_theme_file, query_terminal_colors, theme_from_file,
};
use wire::Agent;

use crate::attach::{Attacher, Outcome, farewell};

/// How long to wait for a terminal to say what colours it paints with.
/// Terminals that answer do so in a few milliseconds; the bound is for the
/// ones that never will, over a slow remote connection.
const TERMINAL_COLOR_QUERY: Duration = Duration::from_millis(250);

pub async fn run(config: &InstallationConfig, profile: Option<&str>) -> Result<()> {
    let profile = crate::connect::profile(config, profile).await?;
    let attacher = Attacher::new(&profile, config.keybinds.leader.clone())?;
    open(config, &profile, attacher).await
}

/// `amux attach`: the agent's own interface on this terminal, and the fleet
/// over it on the fleet chord.
pub async fn attach(config: &InstallationConfig, profile: Option<&str>, agent: &str) -> Result<()> {
    let profile = crate::connect::profile(config, profile).await?;
    let mut client = crate::connect::client_of(&profile).await?;
    let agent = crate::verbs::resolve(&mut client, agent).await?;
    let mut attacher = Attacher::new(&profile, config.keybinds.leader.clone())?;
    if let Some(why) = attacher.refusal(&agent) {
        return Err(anyhow!(why));
    }
    match attacher.attach(&agent).await? {
        Outcome::Fleet => open(config, &profile, attacher).await,
        outcome => {
            println!("{}", farewell(&agent, &outcome));
            Ok(())
        }
    }
}

async fn open(
    config: &InstallationConfig,
    profile: &wire::Profile,
    attacher: Attacher,
) -> Result<()> {
    let socket = Path::new(&profile.socket_path);
    let client = client::GrpcClient::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    // Asked before the alternate screen is entered, because the answers
    // arrive on stdin and nothing else may be reading it yet.
    let terminal = match config.ui.theme {
        ThemeSetting::Terminal => query_terminal_colors(TERMINAL_COLOR_QUERY),
        _ => None,
    };
    let theme = resolve_theme(&config.ui, &ColorEnv::capture(), terminal)
        .context("failed to resolve ui.theme")?;
    let tui_config = TuiConfig {
        working_dir: std::env::current_dir()?,
        leader: char::from(config.keybinds.leader.char),
        theme,
        initial_chat: None,
        attach: false,
        version: node::version().to_owned(),
        local_host: attacher.local_host().to_vec(),
        layout: Some(config.root.join("tui").join("layout.json")),
        reports: Some(config.root.join("reports")),
        chat_in: match config.ui.chat_in {
            settings::ChatInSetting::Amux => tui::setup::ChatIn::Amux,
            settings::ChatInSetting::Terminal => tui::setup::ChatIn::Terminal,
        },
        defaults: {
            let of = |agent: &settings::NewAgentDefaults| tui::setup::AgentDefaults {
                model: agent.model.clone(),
                effort: agent.effort.clone(),
                permission: agent.permission.clone(),
            };
            tui::setup::Defaults {
                claude: of(&config.new_agent.claude),
                codex: of(&config.new_agent.codex),
            }
        },
    };
    tui::run(Arc::new(client), tui_config, Some(attach_fn(attacher))).await
}

/// The fleet's raw attach. Only the fleet chord comes back to the fleet;
/// a detach or the agent ending leaves for the shell, as `amux attach`
/// does.
fn attach_fn(attacher: Attacher) -> AttachFn {
    let attacher = Arc::new(tokio::sync::Mutex::new(attacher));
    Box::new(move |agent: Agent| {
        let attacher = attacher.clone();
        Box::pin(async move {
            match attacher.lock().await.attach(&agent).await? {
                Outcome::Fleet => Ok(AttachReturn::Fleet(None)),
                outcome => {
                    println!("{}", farewell(&agent, &outcome));
                    Ok(AttachReturn::Exit)
                }
            }
        })
    })
}

/// The colour facts the environment states, read once at the edge.
pub struct ColorEnv {
    pub colorterm: Option<String>,
    pub term: Option<String>,
    pub no_color: bool,
}

impl ColorEnv {
    fn capture() -> ColorEnv {
        ColorEnv {
            colorterm: std::env::var("COLORTERM").ok(),
            term: std::env::var("TERM").ok(),
            no_color: std::env::var_os("NO_COLOR").is_some(),
        }
    }
}

/// The palette for this session. `terminal` is what the terminal reported
/// about its own colours, when it was asked and answered; the `terminal`
/// setting derives from that and falls back to the shipped dark palette.
pub fn resolve_theme(
    settings: &UiSettings,
    env: &ColorEnv,
    terminal: Option<TerminalColors>,
) -> Result<Theme, ThemeError> {
    let preference = match settings.color {
        ColorSetting::Auto => ColorPreference::Auto,
        ColorSetting::TrueColor => ColorPreference::TrueColor,
        ColorSetting::Ansi => ColorPreference::Ansi,
    };
    let mode = detect_color_mode(
        preference,
        env.colorterm.as_deref(),
        env.term.as_deref(),
        env.no_color,
    );
    match &settings.theme {
        ThemeSetting::Terminal => Ok(match terminal {
            Some(colors) => Theme::from_terminal(colors, mode),
            None => Theme::dark(mode),
        }),
        ThemeSetting::Dark => Ok(Theme::dark(mode)),
        ThemeSetting::Light => Ok(Theme::light(mode)),
        ThemeSetting::File(path) => {
            let yaml = std::fs::read_to_string(path)?;
            theme_from_file(&parse_theme_file(&yaml)?, mode)
        }
    }
}
