mod door;
/// The component manifest's check against the snapshot baselines, which
/// only the tests run.
#[cfg(test)]
mod golden;
mod simulator;

use ios_tools::{ios_verify, swift_types};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("door") => door::main(),
        Some("verify") => ios_verify::run(),
        Some("swift-types") => swift_types::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        _ => {
            eprintln!(
                "usage: ios-tools <verify [--gate|--captures|--shipping|--measured]|swift-types [--check]|door [--simulator NAME] [--bundle-id ID] [--install APP] [--timeout SECS] [--requests FILE] [JSON...]>"
            );
            std::process::exit(2);
        }
    }
}
