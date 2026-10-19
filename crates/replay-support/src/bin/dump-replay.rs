//! `dump-replay BUNDLE`: replays a dump bundle `amux dump` wrote through
//! its four stages, judging the protocol's properties at every step, and
//! prints one line per stage. Exits non-zero when a stage fails, naming the
//! step and the property it broke.

use std::path::PathBuf;
use std::process::ExitCode;

use replay_support::bundle::Bundle;
use replay_support::replay::Verdict;

const USAGE: &str = "usage: dump-replay BUNDLE";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(bundle), None) = (args.next(), args.next()) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let bundle = PathBuf::from(bundle);
    let verdicts = match Bundle::open(&bundle).and_then(|bundle| bundle.replay_all()) {
        Ok(verdicts) => verdicts,
        Err(error) => {
            eprintln!("dump-replay: {error}");
            return ExitCode::from(2);
        }
    };
    for verdict in &verdicts {
        println!("{verdict}");
    }
    if verdicts
        .iter()
        .any(|verdict| matches!(verdict.verdict, Verdict::Fails(_)))
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
