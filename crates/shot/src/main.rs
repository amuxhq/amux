use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use shot::{ShotError, rasterize, render_vocabulary, report_frame, write_png};
use tui::{ColorMode, Theme};

#[derive(Debug, Parser)]
#[command(about = "Render PNGs of the terminal's chat vocabulary and of reports' frames")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Render vocabulary components, by name (every one when none is
    /// named): one PNG per component and theme, `<name>.<theme>.png`.
    Render {
        names: Vec<String>,
        #[arg(long, value_enum, default_value_t = ThemeArg::Both)]
        theme: ThemeArg,
        #[arg(long, value_enum, default_value_t = ColorArg::Truecolor)]
        color: ColorArg,
        #[arg(long, default_value = "target/amux-shot/vocabulary")]
        out: PathBuf,
    },
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
        Command::Render {
            names,
            theme,
            color,
            out,
        } => {
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
            for (label, theme) in themes {
                written += render_vocabulary(&out, theme, label, &names)?;
            }
            println!("rendered {written} PNGs into {}", out.display());
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
    }
}
