//! The terminal client's storybook: step through its named screens in this
//! terminal, or capture every one of them to text and PNG.
//!
//! Interactively, a story fills the terminal but its last line, which is
//! the storybook's: ← → step, ↑ ↓ jump between groups, `/` finds a story by
//! name, `t` cycles the palette (this terminal's own colours, then the
//! shipped dark and light), `s` cycles the size, `q` quits. The place is
//! kept between runs. With `--watch` the storybook rebuilds itself when a
//! source file changes and comes back on the same story.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use serde::{Deserialize, Serialize};
use shot::stories::{self, Scene, Story};
use tui::{ColorMode, ColorPreference, TerminalColors, TerminalGuard, Theme};

#[derive(Debug, Parser)]
#[command(about = "Step through the terminal client's named screens, or capture them")]
struct Cli {
    #[command(subcommand)]
    command: Option<Capture>,
    /// The story to open on, by name; the last one looked at by default.
    #[arg(long)]
    story: Option<String>,
    /// Rebuild when a source file changes, and come back on the same story.
    #[arg(long)]
    watch: bool,
}

#[derive(Debug, Subcommand)]
enum Capture {
    /// Draw every story at each capture size in the dark and light
    /// palettes: text and PNG per story, size and palette, and an index.
    Capture {
        #[arg(long)]
        out: PathBuf,
        /// Only stories whose names contain this.
        #[arg(long)]
        only: Option<String>,
    },
}

/// The sizes a capture draws, and the fixed ones `s` cycles through after
/// the terminal's own.
const SIZES: [(u16, u16); 3] = [(80, 24), (110, 34), (60, 24)];

fn main() -> anyhow_free::Result {
    let cli = Cli::parse();
    tui::highlight::preload();
    match cli.command {
        Some(Capture::Capture { out, only }) => capture(&out, only.as_deref()),
        None => browse(cli.story, cli.watch),
    }
}

/// Plain error plumbing, so the binary needs no error crate.
mod anyhow_free {
    pub type Result = std::result::Result<(), Box<dyn std::error::Error>>;
}

// --- capture -----------------------------------------------------------------

fn capture(out: &Path, only: Option<&str>) -> anyhow_free::Result {
    std::fs::create_dir_all(out)?;
    let mut index = String::from("# Terminal storybook\n\n");
    let mut count = 0;
    for story in stories::all() {
        if only.is_some_and(|only| !story.id.contains(only)) {
            continue;
        }
        index.push_str(&format!("## {}\n\n{}\n\n", story.id, story.shows));
        for (width, height) in SIZES {
            for (palette, theme) in [
                ("dark", Theme::dark(ColorMode::TrueColor)),
                ("light", Theme::light(ColorMode::TrueColor)),
            ] {
                let mut scene = story.scene();
                let mut terminal = Terminal::new(TestBackend::new(width, height))?;
                terminal.draw(|frame| scene.draw(frame, frame.area(), theme))?;
                let buffer = terminal.backend().buffer().clone();
                let stem = format!("{}.{width}x{height}.{palette}", story.id);
                std::fs::write(out.join(format!("{stem}.txt")), text(&buffer))?;
                let raster = shot::rasterize(&buffer, theme)?;
                shot::write_png(&raster, &out.join(format!("{stem}.png")))?;
                index.push_str(&format!("- [{stem}]({stem}.png) ([text]({stem}.txt))\n"));
                count += 1;
            }
        }
        index.push('\n');
    }
    std::fs::write(out.join("index.md"), index)?;
    println!("{count} captures in {}", out.display());
    Ok(())
}

fn text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// --- browsing ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Palette {
    #[default]
    Terminal,
    Dark,
    Light,
}

/// Where the person was, kept between runs and across a rebuild.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Place {
    story: Option<String>,
    #[serde(default)]
    palette: Palette,
    /// 0 is the terminal's own size, then [`SIZES`] in order.
    #[serde(default)]
    size: usize,
}

struct Book {
    stories: Vec<Story>,
    at: usize,
    scene: Scene,
    place: Place,
    terminal_colors: Option<TerminalColors>,
    mode: ColorMode,
    finder: Option<Finder>,
    watch: Option<Watch>,
}

struct Finder {
    typed: String,
    chosen: usize,
}

struct Watch {
    seen: SystemTime,
    checked: Instant,
    building: Option<Child>,
    note: Option<String>,
}

fn browse(story: Option<String>, watch: bool) -> anyhow_free::Result {
    let stories = stories::all();
    let mut place: Place = std::fs::read_to_string(place_file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if story.is_some() {
        place.story = story;
    }
    let at = place
        .story
        .as_deref()
        .and_then(|id| stories.iter().position(|story| story.id == id))
        .unwrap_or(0);
    // Asked before the screen is taken over, as the client itself asks.
    let terminal_colors = tui::query_terminal_colors(Duration::from_millis(250));
    let mode = tui::detect_color_mode(
        ColorPreference::Auto,
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("NO_COLOR").is_some(),
    );
    let scene = stories[at].scene();
    let mut book = Book {
        stories,
        at,
        scene,
        place,
        terminal_colors,
        mode,
        finder: None,
        watch: watch.then(|| Watch {
            seen: newest_source(),
            checked: Instant::now(),
            building: None,
            note: None,
        }),
    };
    book.keep();
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let rebuilt = run(&mut book, &mut terminal);
    guard.restore();
    match rebuilt? {
        Some(exe) => restart(&exe, &book),
        None => Ok(()),
    }
}

/// Draws and reads keys until the person quits, or a rebuild finishes and
/// the new binary should take over (its path is returned).
fn run(
    book: &mut Book,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> io::Result<Option<PathBuf>> {
    loop {
        terminal.draw(|frame| book.draw(frame))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && !book.key(key)
        {
            return Ok(None);
        }
        if let Some(exe) = book.check_watch() {
            return Ok(Some(exe));
        }
    }
}

impl Book {
    fn story(&self) -> &Story {
        &self.stories[self.at]
    }

    fn go(&mut self, at: usize) {
        self.at = at;
        self.scene = self.stories[at].scene();
        self.keep();
    }

    fn keep(&mut self) {
        self.place.story = Some(self.story().id.to_owned());
        if let Some(parent) = place_file().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            place_file(),
            serde_json::to_string_pretty(&self.place).unwrap_or_default(),
        );
    }

    fn theme(&self) -> Theme {
        match self.place.palette {
            Palette::Terminal => self
                .terminal_colors
                .map_or(Theme::dark(self.mode), |colors| {
                    Theme::from_terminal(colors, self.mode)
                }),
            Palette::Dark => Theme::dark(self.mode),
            Palette::Light => Theme::light(self.mode),
        }
    }

    /// Handles a key; false to quit.
    fn key(&mut self, key: KeyEvent) -> bool {
        if self.finder.is_some() {
            self.finder_key(key);
            return true;
        }
        let count = self.stories.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Right | KeyCode::Char('l') => self.go((self.at + 1) % count),
            KeyCode::Left | KeyCode::Char('h') => self.go((self.at + count - 1) % count),
            KeyCode::Down | KeyCode::Char('j') => {
                let group = self.story().group();
                let next = (self.at..count)
                    .find(|&at| self.stories[at].group() != group)
                    .unwrap_or(0);
                self.go(next);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let group = self.story().group();
                let first = self.first_of(self.at);
                // Back to the start of this group, or of the one before.
                let target = if first == self.at || self.stories[first].group() != group {
                    self.first_of((first + count - 1) % count)
                } else {
                    first
                };
                self.go(target);
            }
            KeyCode::Char('/') => {
                self.finder = Some(Finder {
                    typed: String::new(),
                    chosen: 0,
                })
            }
            KeyCode::Char('t') => {
                self.place.palette = match self.place.palette {
                    Palette::Terminal => Palette::Dark,
                    Palette::Dark => Palette::Light,
                    Palette::Light => Palette::Terminal,
                };
                self.keep();
            }
            KeyCode::Char('s') => {
                self.place.size = (self.place.size + 1) % (SIZES.len() + 1);
                self.keep();
            }
            _ => {}
        }
        true
    }

    /// The first story of the group `at` is in.
    fn first_of(&self, at: usize) -> usize {
        let group = self.stories[at].group();
        (0..=at)
            .rev()
            .take_while(|&index| self.stories[index].group() == group)
            .last()
            .unwrap_or(at)
    }

    fn matches(&self, typed: &str) -> Vec<usize> {
        let typed = typed.to_lowercase();
        (0..self.stories.len())
            .filter(|&at| {
                let story = &self.stories[at];
                story.id.contains(&typed) || story.shows.to_lowercase().contains(&typed)
            })
            .collect()
    }

    fn finder_key(&mut self, key: KeyEvent) {
        let Some(finder) = &mut self.finder else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.finder = None,
            KeyCode::Enter => {
                let (typed, chosen) = (finder.typed.clone(), finder.chosen);
                self.finder = None;
                let found = self.matches(&typed).get(chosen).copied();
                if let Some(at) = found {
                    self.go(at);
                }
            }
            KeyCode::Down => finder.chosen += 1,
            KeyCode::Up => finder.chosen = finder.chosen.saturating_sub(1),
            KeyCode::Backspace => {
                finder.typed.pop();
                finder.chosen = 0;
            }
            KeyCode::Char(c) => {
                finder.typed.push(c);
                finder.chosen = 0;
            }
            _ => {}
        }
        let found = self
            .finder
            .as_ref()
            .map(|finder| self.matches(&finder.typed).len());
        if let (Some(finder), Some(found)) = (&mut self.finder, found) {
            finder.chosen = finder.chosen.min(found.saturating_sub(1));
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let whole = frame.area();
        let theme = self.theme();
        let ground = Style::default().bg(theme.tokens.background.resolve(theme.mode));
        let stage = Rect {
            height: whole.height.saturating_sub(1),
            ..whole
        };
        let area = match self.place.size {
            0 => stage,
            size => {
                // A fixed size drawn framed in the middle, so its edges show.
                let (width, height) = SIZES[size - 1];
                let width = width.min(stage.width.saturating_sub(2));
                let height = height.min(stage.height.saturating_sub(2));
                let area = Rect {
                    x: stage.x + (stage.width.saturating_sub(width)) / 2,
                    y: stage.y + (stage.height.saturating_sub(height)) / 2,
                    width,
                    height,
                };
                let frame_area = Rect {
                    x: area.x.saturating_sub(1),
                    y: area.y.saturating_sub(1),
                    width: area.width + 2,
                    height: area.height + 2,
                };
                frame.render_widget(
                    Block::bordered().border_style(Style::default().add_modifier(Modifier::DIM)),
                    frame_area,
                );
                area
            }
        };
        frame.render_widget(Block::default().style(ground), area);
        self.scene.draw(frame, area, theme);
        let line = Rect {
            y: whole.y + whole.height.saturating_sub(1),
            height: 1.min(whole.height),
            ..whole
        };
        frame.render_widget(Paragraph::new(self.status()), line);
        if let Some(finder) = &self.finder {
            self.draw_finder(frame, stage, finder);
        }
    }

    fn status(&self) -> Line<'static> {
        let palette = match (self.place.palette, self.terminal_colors) {
            (Palette::Terminal, Some(_)) => "this terminal's colours",
            (Palette::Terminal, None) => "dark (no terminal colours)",
            (Palette::Dark, _) => "dark",
            (Palette::Light, _) => "light",
        };
        let size = match self.place.size {
            0 => "this terminal".to_owned(),
            size => {
                let (width, height) = SIZES[size - 1];
                format!("{width}x{height}")
            }
        };
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut spans = vec![
            Span::styled(self.story().id.to_owned(), bold),
            Span::raw(format!("  {}/{}  ", self.at + 1, self.stories.len())),
            Span::styled(format!("{palette} · {size}"), dim),
        ];
        if let Some(note) = self.watch.as_ref().and_then(|watch| watch.note.clone()) {
            spans.push(Span::raw(format!("  {note}")));
        }
        spans.push(Span::styled(
            "   ←→ step  ↑↓ group  / find  t palette  s size  q quit",
            dim,
        ));
        Line::from(spans)
    }

    fn draw_finder(&self, frame: &mut ratatui::Frame<'_>, stage: Rect, finder: &Finder) {
        let found = self.matches(&finder.typed);
        let width = stage.width.min(72);
        let height = stage.height.min(found.len() as u16 + 3).max(3);
        let area = Rect {
            x: stage.x + (stage.width - width) / 2,
            y: stage.y + 1,
            width,
            height,
        };
        let mut lines = vec![Line::from(format!("/{}", finder.typed))];
        let shown = usize::from(height.saturating_sub(3));
        let skip = finder.chosen.saturating_sub(shown.saturating_sub(1));
        for (row, &at) in found.iter().enumerate().skip(skip).take(shown) {
            let story = &self.stories[at];
            let style = if row == finder.chosen {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!("{:<28} {}", story.id, story.shows),
                style,
            ));
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" find a story ")),
            area,
        );
    }

    /// With `--watch`: notices a changed source, rebuilds, and hands back
    /// the new binary's path once the build has succeeded.
    fn check_watch(&mut self) -> Option<PathBuf> {
        let watch = self.watch.as_mut()?;
        if let Some(child) = &mut watch.building {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    watch.building = None;
                    return std::env::current_exe().ok();
                }
                Ok(Some(_)) | Err(_) => {
                    watch.building = None;
                    watch.note = Some("build failed, see target/storybook/build.log".to_owned());
                }
                Ok(None) => {}
            }
            return None;
        }
        if watch.checked.elapsed() < Duration::from_secs(1) {
            return None;
        }
        watch.checked = Instant::now();
        let newest = newest_source();
        if newest <= watch.seen {
            return None;
        }
        watch.seen = newest;
        let log = std::fs::File::create(build_log()).ok()?;
        watch.building = Command::new("cargo")
            .args([
                "build",
                "--locked",
                "-q",
                "-p",
                "shot",
                "--bin",
                "amux-storybook",
            ])
            .current_dir(root())
            .stdout(log.try_clone().ok()?)
            .stderr(log)
            .spawn()
            .ok();
        watch.note = Some("rebuilding…".to_owned());
        None
    }
}

/// Becomes the rebuilt binary, on the same story.
fn restart(exe: &Path, book: &Book) -> anyhow_free::Result {
    let mut command = Command::new(exe);
    command.args(["--watch", "--story", book.story().id]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn place_file() -> PathBuf {
    root().join("target/storybook/place.json")
}

fn build_log() -> PathBuf {
    root().join("target/storybook/build.log")
}

/// The newest change under the sources a story is drawn by.
fn newest_source() -> SystemTime {
    fn walk(path: &Path, newest: &mut SystemTime) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, newest);
            } else if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                *newest = (*newest).max(modified);
            }
        }
    }
    let mut newest = SystemTime::UNIX_EPOCH;
    for crate_dir in ["tui", "ui-view", "ui-state", "shot"] {
        walk(
            &root().join("crates").join(crate_dir).join("src"),
            &mut newest,
        );
    }
    newest
}
