//! Codex under the real provider: `just live codex all`, or one scenario
//! such as `just live codex attach`.
//! See live/mod.rs for what a run judges and reports.

#[cfg(unix)]
mod live;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use live::Scenario;
    live::main(live::Driver {
        kind: wire::Kind::Codex,
        command: "codex",
        recording: |scenario| match scenario {
            Scenario::Initialize | Scenario::Respond | Scenario::Resume => {
                Some("recorded_turn_round_trip")
            }
            Scenario::Decide => Some("recorded_approval_allow"),
            Scenario::Interrupt => Some("recorded_interrupt"),
            // Judged against their own recorded traffic.
            _ => None,
        },
        replay: interpret::replay::<interpret::codex::Codex>,
    })
}

#[cfg(not(unix))]
fn main() {
    println!("live codex: not_run on this platform");
}
