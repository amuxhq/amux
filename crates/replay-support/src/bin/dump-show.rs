//! `dump-show BUNDLE [--agent A]`: prints a dump bundle `amux dump` wrote as
//! text: the manifest, and per agent its inventory row, journal (in the
//! interpreter's golden format), store slice, facts and every other file.
//! `--agent` keeps the agents whose id starts with A or whose name is A.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: dump-show BUNDLE [--agent A]";

fn main() -> ExitCode {
    let mut bundle = None;
    let mut agent = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--agent" {
            match args.next().and_then(|value| value.into_string().ok()) {
                Some(value) if agent.is_none() => agent = Some(value),
                _ => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            }
        } else if bundle.is_none() {
            bundle = Some(PathBuf::from(arg));
        } else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let Some(bundle) = bundle else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match replay_support::show::show(&bundle, agent.as_deref()) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dump-show: {error}");
            ExitCode::FAILURE
        }
    }
}
