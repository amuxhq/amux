//! Headless frames of the real terminal client on a served set: the same
//! `App` the `amux` binary runs, over the client host's own profile socket,
//! fed keys and mouse events and drawn to text and PNG at each size asked.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use client::SystemClock;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use settings::InstallationConfig;
use tui::{App, Theme, TuiConfig};
use ui_runtime::Fleet;

/// How long the client must have taken nothing in before a frame is drawn.
const QUIET: Duration = Duration::from_millis(150);

pub struct Request {
    pub config: PathBuf,
    /// Where new agents start: the client host's work directory.
    pub working_dir: PathBuf,
    pub open: Option<Vec<u8>>,
    /// The client host's id: what the client calls this machine.
    pub local_host: Vec<u8>,
    pub keys: Vec<Step>,
    /// The set's door, for `door:` steps.
    pub control: String,
    pub sizes: Vec<(u16, u16)>,
    pub theme: Theme,
    pub out: PathBuf,
    pub stem: String,
}

/// Connects to the host's profile as `amux` does, runs the keys and
/// writes one frame per size; returns the files' stems.
pub async fn render(request: Request) -> Result<Vec<String>> {
    tui::highlight::preload();
    let config = InstallationConfig::from_file(&request.config)
        .with_context(|| format!("reading {}", request.config.display()))?;
    let socket = profile_socket(&config.front_door_socket).await?;
    let client = client::GrpcClient::connect(&socket)
        .await
        .map_err(|error| anyhow!("connecting to {}: {error}", socket.display()))?;
    let client = Arc::new(client);
    let fleet = Fleet::connect(client.clone(), SystemClock).await?;
    let tui_config = TuiConfig {
        working_dir: request.working_dir.clone(),
        leader: char::from(config.keybinds.leader.char),
        theme: request.theme,
        initial_chat: request.open.clone(),
        attach: false,
        version: "set".into(),
        local_host: request.local_host.clone(),
        layout: None,
        reports: None,
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
    let mut app = App::new(client, fleet, tui_config);
    app.fleet_changed();
    let (width, height) = request.sizes.first().copied().unwrap_or((120, 36));
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    draw_settle(&mut app, &mut terminal).await?;
    for step in &request.keys {
        match step {
            Step::Input(event) => {
                let _ = app.input(event.clone());
            }
            Step::Wait(pause) => {
                let until = tokio::time::Instant::now() + *pause;
                while tokio::time::Instant::now() < until {
                    settle(&mut app, QUIET).await;
                }
            }
            Step::Door(door) => {
                let control = request.control.clone();
                let door = door.clone();
                tokio::task::spawn_blocking(move || crate::served::request_at(&control, &door))
                    .await??;
            }
        }
        draw_settle(&mut app, &mut terminal).await?;
    }
    let mut written = Vec::new();
    for (width, height) in &request.sizes {
        let mut terminal = Terminal::new(TestBackend::new(*width, *height))?;
        draw_settle(&mut app, &mut terminal).await?;
        let frame = terminal.draw(|frame| app.draw(frame))?.buffer.clone();
        let stem = format!("{}-{width}x{height}", request.stem);
        write_frame(&frame, request.theme, &request.out, &stem)?;
        written.push(stem);
    }
    Ok(written)
}

/// The selected profile's client socket, as the host's front door names it.
async fn profile_socket(front_door: &Path) -> Result<PathBuf> {
    let path = front_door.to_owned();
    let channel = tonic::transport::Endpoint::from_static("http://amux.local")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .with_context(|| format!("connecting to {}", front_door.display()))?;
    let profiles = wire::profile_service_client::ProfileServiceClient::new(channel)
        .list_profiles(wire::Empty {})
        .await
        .map_err(|status| anyhow!("listing profiles: {status}"))?
        .into_inner()
        .profiles;
    let profile = profiles
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the host has no profile"))?;
    Ok(PathBuf::from(profile.socket_path))
}

/// Drawing asks for older rows and opens chats, so draw and settle a few
/// times until the frame has what it asked for.
async fn draw_settle(app: &mut App, terminal: &mut Terminal<TestBackend>) -> Result<()> {
    for _ in 0..3 {
        settle(app, QUIET).await;
        terminal.draw(|frame| app.draw(frame))?;
    }
    Ok(())
}

/// Lets the app take in everything already on its way: finished tasks, the
/// fleet and the open chat. Returns once nothing has arrived for `quiet`.
async fn settle(app: &mut App, quiet: Duration) {
    let mut fleet_changed = app.fleet.changed();
    loop {
        app.housekeep();
        let mut chat = app.chat.as_ref().map(|chat| chat.session.changed());
        tokio::select! {
            Some(event) = app.receiver.recv() => {
                app.event(event);
            }
            changed = fleet_changed.changed() => {
                if changed.is_err() {
                    return;
                }
                app.fleet_changed();
            }
            _ = async {
                match chat.as_mut() {
                    Some(changed) => { let _ = changed.changed().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            _ = tokio::time::sleep(quiet) => return,
        }
    }
}

/// The screen as plain text, one line per row, trailing blanks trimmed.
fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Writes one frame as `<stem>.txt` and `<stem>.png`.
fn write_frame(buffer: &Buffer, theme: Theme, dir: &Path, stem: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{stem}.txt")), buffer_text(buffer))?;
    let raster = shot::rasterize(buffer, theme).context("rasterizing the frame")?;
    shot::write_png(&raster, &dir.join(format!("{stem}.png"))).context("writing the PNG")?;
    Ok(())
}

pub fn parse_size(text: &str) -> Result<(u16, u16)> {
    let Some((w, h)) = text.split_once('x') else {
        bail!("a size is WIDTHxHEIGHT, like 120x36: {text:?}");
    };
    Ok((w.parse()?, h.parse()?))
}

/// One step of the input: an event, time passing (`wait:MS`), so a frame
/// can show what the world does a while later, or a change to the world
/// through the set's door (`door:{"Sever": …}`).
#[derive(Clone, Debug)]
pub enum Step {
    Input(Event),
    Wait(Duration),
    Door(serde_json::Value),
}

/// Keys written like a shell line: named keys (`enter`, `esc`, `tab`,
/// `backtab`, `up`, `down`, `left`, `right`, `pgup`, `pgdn`, `home`, `end`,
/// `bs`, `del`, `space`, `f1`..`f12`), `C-x` for Ctrl+x and `C-enter`,
/// `hover:X,Y` and `click:X,Y` for the mouse at a cell (zero-based),
/// `wheelup` and `wheeldown` for one wheel event (`wheeldown:X,Y` over a
/// cell), `paste:TEXT` for a bracketed paste, `wait:MS` to let time pass,
/// `door:JSON` for a door request at that point, and anything else typed
/// as text (quote it to keep spaces: `'fix the bug' enter`).
pub fn parse_keys(text: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for word in shell_words::split(text)? {
        if let Some(ms) = word.strip_prefix("wait:") {
            steps.push(Step::Wait(Duration::from_millis(ms.parse()?)));
            continue;
        }
        if let Some(door) = word.strip_prefix("door:") {
            steps.push(Step::Door(serde_json::from_str(door)?));
            continue;
        }
        if let Some(pasted) = word.strip_prefix("paste:") {
            steps.push(Step::Input(Event::Paste(pasted.replace("\\n", "\n"))));
            continue;
        }
        steps.extend(parse_word(&word)?.into_iter().map(Step::Input));
    }
    Ok(steps)
}

fn parse_word(word: &str) -> Result<Vec<Event>> {
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let mouse = |kind, column, row| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    if let Some((what, at)) = word.split_once(':')
        && let Some((x, y)) = at.split_once(',')
        && matches!(what, "hover" | "click" | "wheelup" | "wheeldown")
    {
        let kind = match what {
            "hover" => MouseEventKind::Moved,
            "wheelup" => MouseEventKind::ScrollUp,
            "wheeldown" => MouseEventKind::ScrollDown,
            _ => MouseEventKind::Down(MouseButton::Left),
        };
        return Ok(vec![mouse(kind, x.parse()?, y.parse()?)]);
    }
    match word {
        "wheelup" => return Ok(vec![mouse(MouseEventKind::ScrollUp, 0, 0)]),
        "wheeldown" => return Ok(vec![mouse(MouseEventKind::ScrollDown, 0, 0)]),
        "C-enter" => {
            return Ok(vec![Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ))]);
        }
        _ => {}
    }
    let named = match word {
        "enter" => Some(KeyCode::Enter),
        "esc" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "pgup" => Some(KeyCode::PageUp),
        "pgdn" => Some(KeyCode::PageDown),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "bs" => Some(KeyCode::Backspace),
        "del" => Some(KeyCode::Delete),
        "space" => Some(KeyCode::Char(' ')),
        w if w.len() > 1
            && w.starts_with('f')
            && w[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            Some(KeyCode::F(w[1..].parse()?))
        }
        _ => None,
    };
    if let Some(code) = named {
        return Ok(vec![key(code)]);
    }
    if let Some(rest) = word.strip_prefix("C-")
        && let Some(c) = rest.chars().next()
        && rest.chars().count() == 1
    {
        return Ok(vec![Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        ))]);
    }
    Ok(word.chars().map(|c| key(KeyCode::Char(c))).collect())
}
