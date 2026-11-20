//! The built-in ACP agents under their real programs and the owner's own
//! sign-in: `just live acp all all`, or one agent and scenario such as
//! `just live acp grok cancel`. See acp/mod.rs for what a run does and
//! reports.

mod acp;

fn main() -> std::process::ExitCode {
    acp::main()
}
