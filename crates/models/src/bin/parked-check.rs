//! Lists every parked property of the protocol's models and fails while
//! any remains: a parked property is a decision still waiting.

use std::process::ExitCode;

fn main() -> ExitCode {
    let parked = models::parked();
    if parked.is_empty() {
        println!("no property is parked");
        return ExitCode::SUCCESS;
    }
    for (model, sentence, parked) in &parked {
        println!("{model}: {sentence}");
        println!("  parked since {}: {}", parked.since, parked.reason);
        println!("  counterexample: {}", parked.counterexample);
    }
    println!(
        "{} parked {}",
        parked.len(),
        if parked.len() == 1 {
            "property"
        } else {
            "properties"
        }
    );
    ExitCode::FAILURE
}
