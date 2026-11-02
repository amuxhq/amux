//! `repro WHAT [--keep DIR]`: replays a report, a sent report fetched by
//! its id, an installation on this machine, or a relay's or the
//! supervisor's report, through every stage it carries, judging the
//! protocol's properties and the cores' and interpreters' checks at every
//! step. Prints one line per stage, keeps what it replayed under DIR
//! (`target/repro/` in the repository by default) and says how to make a
//! test of it. Exits non-zero when a stage fails, naming the step and what
//! it broke. A developer's tool, never shipped.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use replay_support::fetch::Bucket;

const USAGE: &str = "usage: repro <report id | report prefix | report file | report folder | \
     installation folder | relay or supervisor report> [--keep DIR]";

fn main() -> ExitCode {
    let mut named = None;
    let mut keep = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--keep" if keep.is_none() => match args.next() {
                Some(dir) => keep = Some(PathBuf::from(dir)),
                None => return usage(),
            },
            _ if named.is_none() && !arg.starts_with("--") => named = Some(arg),
            _ => return usage(),
        }
    }
    let Some(named) = named else {
        return usage();
    };
    let keep = keep.unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/repro")
            .components()
            .collect()
    });
    let runs = match replay_support::repro::repro(&named, &keep, Bucket::from_env) {
        Ok(runs) => runs,
        Err(error) => {
            eprintln!("repro: {error}");
            return ExitCode::from(2);
        }
    };
    for run in &runs {
        println!("== {} ==", run.name);
        for verdict in &run.verdicts {
            println!("{verdict}");
        }
        println!("kept {}", run.kept.display());
        println!("to make it a test: {}", run.test);
    }
    if runs.iter().any(|run| run.failed()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
