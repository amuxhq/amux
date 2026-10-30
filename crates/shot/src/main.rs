use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use shot::{
    ShotError, rasterize, render_vocabulary, report_frame, verify, vocabulary_index, write_png,
};
use tui::{ColorMode, Theme};

/// The one render set: every component the terminal's goldens hold.
const VOCABULARY: &str = "vocabulary";

#[derive(Debug, Parser)]
#[command(about = "Render deterministic PNGs of the terminal's chat vocabulary")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the components a render set draws, with what each shows.
    List,
    /// Render a set: one PNG per component and theme, a manifest, and an
    /// index.md naming each golden file beside its PNGs.
    Render {
        set: String,
        #[arg(long, value_enum, default_value_t = ThemeArg::Both)]
        theme: ThemeArg,
        #[arg(long, value_enum, default_value_t = ColorArg::Truecolor)]
        color: ColorArg,
        #[arg(long, default_value = "target/amux-shot/vocabulary")]
        out: PathBuf,
    },
    /// Verify PNG dimensions, hashes, decoding, and completed sets.
    Verify { dir: PathBuf },
    /// Draw a report's frozen screen: a report directory, or its
    /// `frame.json`, to a PNG (`frame.png` beside it by default).
    Frame {
        report: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum ThemeArg {
    Light,
    Dark,
    Both,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ColorArg {
    Truecolor,
    Ansi,
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("amux-shot: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), ShotError> {
    match cli.command {
        Command::List => {
            for (name, shows) in vocabulary_index() {
                println!("{name}\t{shows}");
            }
            Ok(())
        }
        Command::Render {
            set,
            theme,
            color,
            out,
        } => {
            if set != VOCABULARY {
                return Err(ShotError::UnknownSet(set));
            }
            let mode = match color {
                ColorArg::Truecolor => ColorMode::TrueColor,
                ColorArg::Ansi => ColorMode::Ansi,
            };
            let mut themes = Vec::new();
            if theme != ThemeArg::Dark {
                themes.push(("light", Theme::light(mode)));
            }
            if theme != ThemeArg::Light {
                themes.push(("dark", Theme::dark(mode)));
            }
            let mut written = 0;
            for (label, theme) in &themes {
                written += render_vocabulary(&out, *theme, label)?.len();
            }
            let labels: Vec<&str> = themes.iter().map(|(label, _)| *label).collect();
            fs::write(out.join("index.md"), index(&labels))?;
            println!(
                "rendered {written} PNGs and index.md into {}",
                out.display()
            );
            Ok(())
        }
        Command::Frame { report, out } => {
            let json = if report.is_dir() {
                report.join("frame.json")
            } else {
                report
            };
            let buffer = report_frame(&fs::read_to_string(&json)?)?;
            let out = out.unwrap_or_else(|| json.with_file_name("frame.png"));
            write_png(&rasterize(&buffer, Theme::default())?, &out)?;
            println!("drew {}", out.display());
            Ok(())
        }
        Command::Verify { dir } => {
            let manifest = verify(&dir)?;
            println!(
                "verified {} PNGs in {} sets",
                manifest.entries.len(),
                manifest.sets.len()
            );
            Ok(())
        }
    }
}

/// Every golden file under crates/tui/tests/golden with what it shows, and
/// the PNGs rendered beside this index.
fn index(themes: &[&str]) -> String {
    let mut out = String::from(
        "# Terminal vocabulary goldens\n\n\
         Each component is drawn from authored view values into the test \
         backend at 100 columns. Its golden, under `crates/tui/tests/golden`, \
         holds the text and a map of semantic style classes, once per theme; \
         the PNGs here are the same buffers rasterized.\n\n\
         | Component | What it shows | Goldens | PNGs |\n\
         | --- | --- | --- | --- |\n",
    );
    for (name, shows) in vocabulary_index() {
        let goldens: Vec<String> = ["light", "dark"]
            .iter()
            .map(|theme| format!("`{name}.{theme}.txt`"))
            .collect();
        let pngs: Vec<String> = themes
            .iter()
            .map(|theme| format!("[{theme}]({name}.{theme}.png)"))
            .collect();
        let _ = writeln!(
            out,
            "| {name} | {shows} | {} | {} |",
            goldens.join(" "),
            pngs.join(" ")
        );
    }
    out
}
