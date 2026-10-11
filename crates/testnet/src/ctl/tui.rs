//! The real terminal client in tmux panes the session names, as a person
//! at a terminal uses it: keys, typing, pasting, resizing, and what the
//! screen shows, as text or as a picture.
//!
//! A shot reads the pane with its styles (`capture-pane -e`), turns the
//! escape codes into the cell format the client's own debug report writes
//! (`frame.json`), and draws that with the same renderer the client's
//! screenshots use. The parser is written for that format rather than
//! borrowed: it keeps each cell's column, the colon forms of underline,
//! and hidden text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthChar as _;

use super::session::{Folder, Outcome, binary};

#[derive(Subcommand, Debug)]
pub enum TuiCommand {
    /// Start the terminal client against a host in a new pane, and name it.
    ///
    /// The pane is a terminal of --size (default 110x34) in --theme
    /// (default dark), with 24-bit colour. Prints the pane's name, which
    /// every other tui command takes.
    Open {
        host: String,
        /// COLSxROWS.
        #[arg(long, default_value = "110x34")]
        size: String,
        #[arg(long, value_enum, default_value_t = Theme::Dark)]
        theme: Theme,
        /// The pane's name; the host's name (then host-2, ...) by default.
        #[arg(long)]
        name: Option<String>,
    },
    /// Press keys, one after another, as tmux names them.
    ///
    /// Enter, Escape, Tab, BTab (shift-tab), Space, BSpace, Up, Down, Left,
    /// Right, Home, End, PageUp (PPage), PageDown (NPage), F1..F12, C-a
    /// (control-a), S-Up (shift), and single characters (`q`, `?`).
    /// Each key is its own press, with a short pause between, so Escape
    /// then a letter stays two keys.
    Keys {
        pane: String,
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Type text as keystrokes, literally.
    Type { pane: String, text: String },
    /// Paste text as a terminal paste (bracketed), the way a person pastes:
    /// TEXT, or with --file the contents of a file in your folder.
    Paste {
        pane: String,
        #[arg(required_unless_present = "file")]
        text: Option<String>,
        #[arg(long, conflicts_with = "text")]
        file: Option<PathBuf>,
    },
    /// Copy FILE and press Ctrl+V in the pane, the way a person attaches
    /// a file they copied (or, with --image, an image: a PNG put on the
    /// clipboard as picture data, as a screenshot tool does).
    ///
    /// The clipboard is the Mac's own, shared by every pane and session:
    /// the command holds it only while the client reads it, then puts back
    /// the text it held before. Only text is put back: an image or file
    /// copied on the Mac is replaced.
    Clip {
        pane: String,
        file: PathBuf,
        #[arg(long)]
        image: bool,
    },
    /// The pane's screen as text, with the cursor's place.
    Screen { pane: String },
    /// A picture of the pane, written to shots/ and its path printed.
    ///
    /// Known limits: background colour a program paints by clearing to the
    /// end of a line is not kept, and the font has no CJK or emoji glyphs,
    /// so those draw as boxes. The text beside it (.txt) is exact.
    Shot { pane: String },
    /// Resize the pane's terminal to COLSxROWS.
    Resize { pane: String, size: String },
    /// Open the agent's native view (Claude's own interface for terminal
    /// Claude, Codex's own for Codex) with `amux attach` in a new pane,
    /// sized like PANE, against the host the agent runs on. Prints the new
    /// pane's name. Detach with C-a d.
    Attach { pane: String, agent: String },
    /// Close a pane and the client in it.
    Close { pane: String },
    /// The open panes.
    List,
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn word(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The client's own ink and ground in this theme: a detached tmux
    /// answers no colour query, so this is what the client draws on.
    fn ink(self) -> (&'static str, &'static str) {
        match self {
            Self::Dark => ("#dfe3e8", "#0f1216"),
            Self::Light => ("#2a2e38", "#fafaf8"),
        }
    }
}

/// What the session knows about a pane.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pane {
    host: String,
    theme: Theme,
    /// What runs in it: the client, or an agent's native view.
    what: String,
}

fn panes_file(folder: &Folder) -> PathBuf {
    folder.net_dir().join("panes.json")
}

fn panes(folder: &Folder) -> BTreeMap<String, Pane> {
    std::fs::read_to_string(panes_file(folder))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_panes(folder: &Folder, panes: &BTreeMap<String, Pane>) -> Result<(), String> {
    std::fs::write(
        panes_file(folder),
        serde_json::to_string_pretty(panes).unwrap_or_default(),
    )
    .map_err(|error| error.to_string())
}

/// The configuration every tmux server ctl starts reads, and only it.
const TMUX_CONF: &str = include_str!("tmux.conf");

/// tmux on the session's own server, so its panes are apart from anyone's.
fn tmux(folder: &Folder, args: &[&str]) -> Result<String, String> {
    // The server reads the net's own configuration rather than the
    // operator's: keys must reach the client the same way on every machine.
    let conf = folder.net_dir().join("tmux.conf");
    if std::fs::read_to_string(&conf).ok().as_deref() != Some(TMUX_CONF) {
        std::fs::create_dir_all(folder.net_dir()).map_err(|error| error.to_string())?;
        std::fs::write(&conf, TMUX_CONF)
            .map_err(|error| format!("writing {}: {error}", conf.display()))?;
    }
    // A tmux server of this folder's own, named rather than placed: a
    // socket path under a deep folder overruns the length a Unix socket's
    // path may have.
    let output = std::process::Command::new("tmux")
        .arg("-f")
        .arg(&conf)
        .arg("-L")
        .arg(format!(
            "testnet-{}",
            super::session::folder_hash(&folder.dir)
        ))
        .args(args)
        .output()
        .map_err(|error| format!("running tmux: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "tmux {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn size(text: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{text:?} is not a size: give COLSxROWS, as 110x34"))?;
    let parse = |part: &str| {
        part.trim()
            .parse::<u16>()
            .ok()
            .filter(|n| *n >= 10)
            .ok_or_else(|| format!("{text:?} is not a size: give COLSxROWS, each at least 10"))
    };
    Ok((parse(cols)?, parse(rows)?))
}

fn known(folder: &Folder, pane: &str) -> Result<Pane, String> {
    let all = panes(folder);
    all.get(pane).cloned().ok_or_else(|| {
        if all.is_empty() {
            format!("no pane {pane:?}: open one with `ctl tui open HOST`")
        } else {
            format!(
                "no pane {pane:?}; open panes: {}",
                all.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        }
    })
}

pub(crate) fn run(folder: &Folder, command: TuiCommand) -> Result<Outcome, String> {
    match command {
        TuiCommand::Open {
            host,
            size: wanted,
            theme,
            name,
        } => open(folder, &host, &wanted, theme, name),
        TuiCommand::Keys { pane, keys } => {
            known(folder, &pane)?;
            for key in &keys {
                tmux(folder, &["send-keys", "-t", &pane, key])?;
                std::thread::sleep(Duration::from_millis(150));
            }
            Ok(Outcome::new(
                format!("pressed {} in {pane}", keys.join(" ")),
                json!({"keys": keys}),
            ))
        }
        TuiCommand::Type { pane, text } => {
            known(folder, &pane)?;
            tmux(folder, &["send-keys", "-t", &pane, "-l", &text])?;
            Ok(Outcome::new(
                format!("typed into {pane}"),
                json!({"typed": text}),
            ))
        }
        TuiCommand::Paste { pane, text, file } => {
            known(folder, &pane)?;
            let text = match (text, file) {
                (Some(text), _) => text,
                (None, Some(file)) => {
                    let path = folder.dir.join(file);
                    std::fs::read_to_string(&path)
                        .map_err(|error| format!("reading {}: {error}", path.display()))?
                }
                (None, None) => return Err("paste TEXT, or --file FILE".to_owned()),
            };
            tmux(folder, &["set-buffer", "-b", "ctl", &text])?;
            tmux(
                folder,
                &["paste-buffer", "-p", "-d", "-b", "ctl", "-t", &pane],
            )?;
            Ok(Outcome::new(
                format!("pasted {} bytes into {pane}", text.len()),
                // A large paste is kept by its size: the session log is
                // evidence, read whole.
                if text.len() > 4096 {
                    json!({"pasted_bytes": text.len()})
                } else {
                    json!({"pasted": text})
                },
            ))
        }
        TuiCommand::Clip { pane, file, image } => clip(folder, &pane, &file, image),
        TuiCommand::Screen { pane } => {
            known(folder, &pane)?;
            let text = tmux(folder, &["capture-pane", "-p", "-t", &pane])?;
            let (x, y, shown) = cursor(folder, &pane)?;
            Ok(Outcome::new(
                format!(
                    "{}\n[cursor at column {x}, row {y}{}]",
                    text.trim_end_matches('\n'),
                    if shown { "" } else { ", hidden" }
                ),
                json!({"screen": text, "cursor": {"x": x, "y": y, "shown": shown}}),
            ))
        }
        TuiCommand::Shot { pane } => {
            let (png, text) = shot(folder, &pane)?;
            Ok(Outcome::new(
                format!("{}\n(text beside it: {})", png.display(), text.display()),
                json!({"png": png, "text": text}),
            )
            .with_files(vec![png, text]))
        }
        TuiCommand::Resize { pane, size: wanted } => {
            known(folder, &pane)?;
            let (cols, rows) = size(&wanted)?;
            tmux(
                folder,
                &[
                    "resize-window",
                    "-t",
                    &pane,
                    "-x",
                    &cols.to_string(),
                    "-y",
                    &rows.to_string(),
                ],
            )?;
            Ok(Outcome::new(
                format!("{pane} is {cols}x{rows}"),
                json!({"cols": cols, "rows": rows}),
            ))
        }
        TuiCommand::Attach { pane, agent } => attach(folder, &pane, &agent),
        TuiCommand::Close { pane } => {
            known(folder, &pane)?;
            let _ = tmux(folder, &["kill-session", "-t", &pane]);
            let mut all = panes(folder);
            all.remove(&pane);
            save_panes(folder, &all)?;
            Ok(Outcome::new(
                format!("{pane} closed"),
                json!({"closed": pane}),
            ))
        }
        TuiCommand::List => {
            let all = panes(folder);
            let text = if all.is_empty() {
                "no panes open".to_owned()
            } else {
                all.iter()
                    .map(|(name, pane)| {
                        format!(
                            "{name}: {} on {} ({})",
                            pane.what,
                            pane.host,
                            pane.theme.word()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(Outcome::new(
                text,
                serde_json::to_value(&all).unwrap_or_default(),
            ))
        }
    }
}

/// The open panes' names.
pub(crate) fn open_panes(folder: &Folder) -> Vec<String> {
    panes(folder).into_keys().collect()
}

/// Closes every pane and the session's tmux server.
pub(crate) fn close_all(folder: &Folder) {
    let _ = tmux(folder, &["kill-server"]);
    let _ = std::fs::remove_file(panes_file(folder));
}

fn fresh_name(folder: &Folder, base: &str) -> String {
    let all = panes(folder);
    if !all.contains_key(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !all.contains_key(name))
        .expect("a free name")
}

fn open(
    folder: &Folder,
    host: &str,
    wanted: &str,
    theme: Theme,
    name: Option<String>,
) -> Result<Outcome, String> {
    let entry = folder.host(host)?;
    let (cols, rows) = size(wanted)?;
    let name = match name {
        Some(name) if panes(folder).contains_key(&name) => {
            return Err(format!("a pane named {name:?} is open already"));
        }
        Some(name) => name,
        None => fresh_name(folder, host),
    };
    // The pane's own copy of the host's config, so its theme is its own.
    let config = entry["config"].as_str().ok_or("the host has no config")?;
    let mut yaml = std::fs::read_to_string(config).map_err(|error| error.to_string())?;
    yaml.push_str(&format!("ui:\n  theme: {}\n", theme.word()));
    let pane_config = folder.net_dir().join(format!("pane-{name}.yaml"));
    std::fs::write(&pane_config, yaml).map_err(|error| error.to_string())?;
    let work = PathBuf::from(entry["dir"].as_str().unwrap_or_default()).join("work");
    let command = client_command(&[
        binary("amux").to_string_lossy().as_ref(),
        "--config",
        pane_config.to_string_lossy().as_ref(),
    ]);
    start_pane(folder, &name, cols, rows, &work, &command)?;
    let mut all = panes(folder);
    all.insert(
        name.clone(),
        Pane {
            host: host.to_owned(),
            theme,
            what: "the terminal client".to_owned(),
        },
    );
    save_panes(folder, &all)?;
    Ok(Outcome::new(
        format!(
            "pane {name}: the terminal client on {host}, {cols}x{rows}, {} theme",
            theme.word()
        ),
        json!({"pane": name}),
    ))
}

/// A shell command running `args` as a person's terminal would, and
/// keeping the pane open after it exits with how it exited, so an exit is
/// seen rather than the pane vanishing.
fn client_command(args: &[&str]) -> String {
    let quoted: Vec<String> = args.iter().map(|arg| shell_word(arg)).collect();
    format!(
        "env -u AMUX_LOG -u AMUX_CONFIG -u TMUX TERM=xterm-256color COLORTERM=truecolor {}; \
         echo \"[exited with status $?]\"; sleep 86400",
        quoted.join(" ")
    )
}

fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn start_pane(
    folder: &Folder,
    name: &str,
    cols: u16,
    rows: u16,
    dir: &Path,
    command: &str,
) -> Result<(), String> {
    let dir = if dir.is_dir() {
        dir.to_owned()
    } else {
        folder.dir.clone()
    };
    tmux(
        folder,
        &[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            "-c",
            dir.to_string_lossy().as_ref(),
            "sh",
            "-c",
            command,
        ],
    )?;
    // The session's size, not whatever client attaches later, decides the
    // window's; and no status line takes a row from the client.
    let _ = tmux(folder, &["set-option", "-t", name, "status", "off"]);
    let _ = tmux(folder, &["set-option", "-t", name, "window-size", "manual"]);
    let _ = tmux(
        folder,
        &[
            "resize-window",
            "-t",
            name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ],
    );
    Ok(())
}

fn attach(folder: &Folder, pane: &str, agent: &str) -> Result<Outcome, String> {
    let from = known(folder, pane)?;
    let status = folder.status()?;
    // The host the agent runs on: attaching through a host that only holds
    // a copy of it is refused.
    let fleet = folder.call(json!({"Inventory": {"host": from.host}}))?;
    let listed = fleet["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|listed| listed["name"] == agent || listed["id"] == agent)
        .cloned()
        .ok_or_else(|| format!("{} lists no agent {agent:?}", from.host))?;
    let owner = status["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|host| host["host_id"] == listed["host_id"])
        .cloned()
        .ok_or_else(|| format!("{agent} runs on a host outside this net"))?;
    let owner_name = owner["name"].as_str().unwrap_or("?").to_owned();
    let size = tmux(
        folder,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_width}x#{pane_height}",
        ],
    )?;
    let (cols, rows) = self::size(size.trim())?;
    let name = fresh_name(folder, &format!("{pane}-attach"));
    let command = client_command(&[
        binary("amux").to_string_lossy().as_ref(),
        "--config",
        owner["config"].as_str().unwrap_or_default(),
        "attach",
        listed["id"].as_str().unwrap_or(agent),
    ]);
    let work = PathBuf::from(owner["dir"].as_str().unwrap_or_default()).join("work");
    start_pane(folder, &name, cols, rows, &work, &command)?;
    let mut all = panes(folder);
    all.insert(
        name.clone(),
        Pane {
            host: owner_name.clone(),
            theme: from.theme,
            what: format!("amux attach {agent}"),
        },
    );
    save_panes(folder, &all)?;
    Ok(Outcome::new(
        format!(
            "pane {name}: {agent}'s native view on {owner_name}, {cols}x{rows} (detach with C-a d)"
        ),
        json!({"pane": name}),
    ))
}

fn clip(folder: &Folder, pane: &str, file: &Path, image: bool) -> Result<Outcome, String> {
    known(folder, pane)?;
    let file = folder.dir.join(file);
    if !file.is_file() {
        return Err(format!("no file at {}", file.display()));
    }
    let is_png = file
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
    if image && !is_png {
        return Err(
            "--image puts a PNG on the clipboard; attach other files without it".to_owned(),
        );
    }
    // Every session on this Mac shares the clipboard; one paste at a time.
    let lock = std::fs::File::create(std::env::temp_dir().join("testnet-clipboard.lock"))
        .map_err(|error| error.to_string())?;
    lock.lock().map_err(|error| error.to_string())?;
    // Only the clipboard's text is kept and put back: an image or a file
    // someone had copied is replaced by it.
    let before_text = std::process::Command::new("pbpaste")
        .output()
        .map(|output| output.stdout)
        .unwrap_or_default();
    let pasted = paste_from_clipboard(folder, pane, &file, image);
    let _ = pb_copy(&before_text);
    drop(lock);
    pasted?;
    Ok(Outcome::new(
        format!("copied {} and pressed Ctrl+V in {pane}", file.display()),
        json!({"file": file, "image": image}),
    ))
}

/// Puts FILE on the clipboard and presses Ctrl+V in the pane, returning
/// once the client has read it.
fn paste_from_clipboard(
    folder: &Folder,
    pane: &str,
    file: &Path,
    image: bool,
) -> Result<(), String> {
    let set = if image {
        // The path goes in as an argument, never into the script's source,
        // so no file name can change what the script does.
        std::process::Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "set the clipboard to (read (POSIX file (item 1 of argv)) as «class PNGf»)",
                "-e",
                "end run",
            ])
            .arg(file)
            .output()
    } else {
        pb_copy(file.to_string_lossy().as_bytes())
    };
    match set {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return Err(format!(
                "putting {} on the clipboard: {}",
                file.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Err(error) => return Err(format!("putting it on the clipboard: {error}")),
    }
    let before = tmux(folder, &["capture-pane", "-p", "-t", pane])?;
    tmux(folder, &["send-keys", "-t", pane, "C-v"])?;
    // The client reads the clipboard as it handles the key; once the screen
    // has changed it has read it.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline
        && tmux(folder, &["capture-pane", "-p", "-t", pane])? == before
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

fn pb_copy(bytes: &[u8]) -> std::io::Result<std::process::Output> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes)?;
    }
    child.wait_with_output()
}

fn cursor(folder: &Folder, pane: &str) -> Result<(u16, u16, bool), String> {
    let said = tmux(
        folder,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{cursor_x},#{cursor_y},#{cursor_flag}",
        ],
    )?;
    let parts: Vec<&str> = said.trim().split(',').collect();
    let number = |at: usize| {
        parts
            .get(at)
            .and_then(|part| part.parse::<u16>().ok())
            .unwrap_or(0)
    };
    Ok((number(0), number(1), parts.get(2) == Some(&"1")))
}

/// Writes a shot of `pane` (PNG, its frame and its text) under shots/ and
/// answers the PNG's and the text's paths.
pub(crate) fn shot(folder: &Folder, pane: &str) -> Result<(PathBuf, PathBuf), String> {
    let known = known(folder, pane)?;
    let styled = tmux(folder, &["capture-pane", "-p", "-e", "-t", pane])?;
    let plain = tmux(folder, &["capture-pane", "-p", "-t", pane])?;
    let dims = tmux(
        folder,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_width}x#{pane_height}",
        ],
    )?;
    let (cols, rows) = size(dims.trim())?;
    let (x, y, shown) = cursor(folder, pane)?;
    let (fg, bg) = known.theme.ink();
    let frame = frame(&styled, cols, rows, fg, bg, shown.then_some((x, y)));
    let dir = folder.dir.join("shots");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let stamp = chrono::Utc::now().format("%H%M%S%3f");
    let base = dir.join(format!("{pane}-{stamp}"));
    let frame_path = base.with_extension("frame.json");
    let png = base.with_extension("png");
    let text = base.with_extension("txt");
    let json = serde_json::to_string(&frame).map_err(|error| error.to_string())?;
    std::fs::write(&frame_path, &json).map_err(|error| error.to_string())?;
    std::fs::write(&text, plain).map_err(|error| error.to_string())?;
    let buffer = shot::report_frame(&json).map_err(|error| error.to_string())?;
    let raster =
        shot::rasterize(&buffer, ::tui::Theme::default()).map_err(|error| error.to_string())?;
    shot::write_png(&raster, &png).map_err(|error| error.to_string())?;
    Ok((png, text))
}

/// One cell's style as the frame names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Style {
    fg: String,
    bg: String,
    bold: bool,
    dim: bool,
    italic: bool,
    underlined: bool,
    reversed: bool,
    hidden: bool,
    crossed_out: bool,
}

impl Style {
    fn plain() -> Self {
        Self {
            fg: "default".to_owned(),
            bg: "default".to_owned(),
            ..Self::default()
        }
    }

    fn styles(&self) -> Vec<&'static str> {
        let mut styles = Vec::new();
        for (on, name) in [
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underlined, "underlined"),
            (self.reversed, "reversed"),
            (self.crossed_out, "crossed_out"),
            (self.hidden, "hidden"),
        ] {
            if on {
                styles.push(name);
            }
        }
        styles
    }

    /// Applies one SGR sequence's parameters, as tmux writes them.
    fn apply(&mut self, params: &str) {
        let params: Vec<&str> = if params.is_empty() {
            vec!["0"]
        } else {
            params.split(';').collect()
        };
        let mut at = 0;
        while at < params.len() {
            let param = params[at];
            // A colon form carries its sub-parameters inside one parameter:
            // `4:3` is a curly underline, `38:2::r:g:b` a colour.
            let mut parts = param.split(':');
            let head: u16 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let subs: Vec<&str> = parts.collect();
            match head {
                0 => *self = Self::plain(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => {
                    self.underlined = subs.first().is_none_or(|style| *style != "0");
                }
                5 | 6 => {}
                7 => self.reversed = true,
                8 => self.hidden = true,
                9 => self.crossed_out = true,
                21 => self.underlined = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underlined = false,
                27 => self.reversed = false,
                28 => self.hidden = false,
                29 => self.crossed_out = false,
                30..=37 => self.fg = format!("ansi:{}", head - 30),
                39 => self.fg = "default".to_owned(),
                40..=47 => self.bg = format!("ansi:{}", head - 40),
                49 => self.bg = "default".to_owned(),
                90..=97 => self.fg = format!("ansi:{}", head - 90 + 8),
                100..=107 => self.bg = format!("ansi:{}", head - 100 + 8),
                38 | 48 | 58 => {
                    let (colour, used) = if subs.is_empty() {
                        extended(&params[at + 1..])
                    } else {
                        (extended_colon(&subs), 0)
                    };
                    at += used;
                    if let Some(colour) = colour {
                        match head {
                            38 => self.fg = colour,
                            48 => self.bg = colour,
                            // Underline colour: the renderer has no place
                            // for it.
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            at += 1;
        }
    }
}

/// `5;n` or `2;r;g;b` after a 38/48/58: the colour, and how many
/// parameters it took.
fn extended(rest: &[&str]) -> (Option<String>, usize) {
    match rest.first().copied() {
        Some("5") => (
            rest.get(1)
                .and_then(|n| n.parse::<u8>().ok())
                .map(|n| format!("ansi:{n}")),
            2,
        ),
        Some("2") => {
            let channel = |at: usize| rest.get(at).and_then(|n| n.parse::<u8>().ok());
            match (channel(1), channel(2), channel(3)) {
                (Some(r), Some(g), Some(b)) => (Some(format!("#{r:02x}{g:02x}{b:02x}")), 4),
                _ => (None, rest.len().min(4)),
            }
        }
        _ => (None, 0),
    }
}

/// The colon form: `5:n`, or `2:[colour space]:r:g:b`.
fn extended_colon(subs: &[&str]) -> Option<String> {
    match subs.first().copied() {
        Some("5") => subs
            .get(1)
            .and_then(|n| n.parse::<u8>().ok())
            .map(|n| format!("ansi:{n}")),
        Some("2") => {
            let numbers: Vec<u8> = subs[1..]
                .iter()
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse().ok())
                .collect();
            let rgb = &numbers[numbers.len().saturating_sub(3)..];
            (rgb.len() == 3).then(|| format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
        }
        _ => None,
    }
}

#[derive(Serialize)]
struct Frame {
    width: u16,
    height: u16,
    default: Ink,
    rows: Vec<Vec<Run>>,
}

#[derive(Serialize)]
struct Ink {
    fg: String,
    bg: String,
}

#[derive(Serialize, Clone)]
struct Run {
    x: u16,
    text: String,
    fg: String,
    bg: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    styles: Vec<&'static str>,
}

/// The cells of a styled capture as the client's report frame: one run per
/// stretch of one style, each at its column, wide characters once, the
/// cursor shown by reversing its cell.
fn frame(
    styled: &str,
    cols: u16,
    rows: u16,
    fg: &str,
    bg: &str,
    cursor: Option<(u16, u16)>,
) -> Frame {
    let mut grid: Vec<Vec<(String, Style)>> = Vec::new();
    let mut style = Style::plain();
    for line in styled.split('\n').take(usize::from(rows)) {
        let mut cells: Vec<(String, Style)> = Vec::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        let mut params = String::new();
                        let mut last = None;
                        for next in chars.by_ref() {
                            if ('\u{40}'..='\u{7e}').contains(&next) {
                                last = Some(next);
                                break;
                            }
                            params.push(next);
                        }
                        if last == Some('m') {
                            style.apply(&params);
                        }
                    }
                    Some(']') => {
                        // An operating-system command (a hyperlink): skip it
                        // to its terminator.
                        chars.next();
                        while let Some(next) = chars.next() {
                            if next == '\u{7}' {
                                break;
                            }
                            if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            let width = c.width().unwrap_or(0);
            if width == 0 {
                // A combining mark joins the cell before it.
                if let Some(last) = cells.last_mut() {
                    last.0.push(c);
                }
                continue;
            }
            cells.push((c.to_string(), style.clone()));
            if width == 2 {
                // The second column of a wide character: drawn by the first.
                cells.push((String::new(), style.clone()));
            }
        }
        // tmux leaves off the blanks it never styled at a line's end.
        while cells.len() < usize::from(cols) {
            cells.push((" ".to_owned(), Style::plain()));
        }
        cells.truncate(usize::from(cols));
        grid.push(cells);
    }
    while grid.len() < usize::from(rows) {
        grid.push(vec![(" ".to_owned(), Style::plain()); usize::from(cols)]);
    }
    if let Some((x, y)) = cursor
        && let Some(cell) = grid
            .get_mut(usize::from(y))
            .and_then(|row| row.get_mut(usize::from(x)))
    {
        cell.1.reversed = !cell.1.reversed;
    }
    let rows_out = grid
        .into_iter()
        .map(|cells| {
            let mut runs: Vec<Run> = Vec::new();
            let mut current: Option<(Run, Style)> = None;
            for (x, (text, style)) in cells.into_iter().enumerate() {
                if text.is_empty() {
                    continue;
                }
                match &mut current {
                    Some((run, held)) if *held == style && run_end(run) == x => {
                        run.text.push_str(&text);
                    }
                    _ => {
                        if let Some((run, _)) = current.take() {
                            runs.push(run);
                        }
                        current = Some((
                            Run {
                                x: x as u16,
                                text,
                                fg: style.fg.clone(),
                                bg: style.bg.clone(),
                                styles: style.styles(),
                            },
                            style,
                        ));
                    }
                }
            }
            if let Some((run, _)) = current {
                runs.push(run);
            }
            runs
        })
        .collect();
    Frame {
        width: cols,
        height: rows,
        default: Ink {
            fg: fg.to_owned(),
            bg: bg.to_owned(),
        },
        rows: rows_out,
    }
}

/// The column after a run's last cell.
fn run_end(run: &Run) -> usize {
    usize::from(run.x)
        + run
            .text
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>()
}

/// For the frame's JSON in tests.
#[allow(dead_code)]
fn frame_value(styled: &str, cols: u16, rows: u16) -> Value {
    serde_json::to_value(frame(styled, cols, rows, "#ffffff", "#000000", None)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_colours_and_columns_survive_the_capture() {
        let styled =
            "\u{1b}[1;38;2;255;0;0mab\u{1b}[0m c\u{1b}[4:3;2mde\u{1b}[0m\u{1b}[8mx\u{1b}[0m";
        let frame = frame_value(styled, 10, 2);
        let row = &frame["rows"][0];
        assert_eq!(row[0]["text"], "ab");
        assert_eq!(row[0]["fg"], "#ff0000");
        assert_eq!(row[0]["styles"], json!(["bold"]));
        assert_eq!(row[1]["text"], " c");
        assert_eq!(row[2]["x"], 4);
        assert_eq!(row[2]["styles"], json!(["dim", "underlined"]));
        assert_eq!(row[3]["styles"], json!(["hidden"]));
        assert_eq!(frame["rows"][1][0]["text"], " ".repeat(10));
    }

    #[test]
    fn a_wide_character_takes_two_columns_and_is_written_once() {
        let frame = frame_value("a界b", 6, 1);
        // Four columns of six: the wide character counts twice.
        assert_eq!(frame["rows"][0][0]["text"], "a界b  ");
    }

    #[test]
    fn indexed_and_bright_colours_name_their_ansi_number() {
        let frame = frame_value("\u{1b}[38;5;200m1\u{1b}[91m2\u{1b}[48:5:3m3", 3, 1);
        let row = &frame["rows"][0];
        assert_eq!(row[0]["fg"], "ansi:200");
        assert_eq!(row[1]["fg"], "ansi:9");
        assert_eq!(row[2]["bg"], "ansi:3");
    }

    #[test]
    fn the_cursor_reverses_its_cell() {
        let frame =
            serde_json::to_value(super::frame("abc", 3, 1, "#fff", "#000", Some((1, 0)))).unwrap();
        assert_eq!(frame["rows"][0][1]["text"], "b");
        assert_eq!(frame["rows"][0][1]["styles"], json!(["reversed"]));
    }
}
