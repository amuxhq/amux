//! Specifications for the ways a turn ends other than by answering.
//!
//! Each of these is a distinct result subtype on the wire. A caller that
//! collapsed them into "it failed" would lose the only thing that tells it
//! whether to retry, raise a limit, or stop.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::PermissionMode;
use crate::expect;

pub(super) static MAX_TURNS: SpecDef = SpecDef {
    name: "results/max_turns",
    fixture: "max_turns",
    setup: max_turns_setup,
    run: |session| Box::pin(max_turns(session)),
};

fn max_turns_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Use the Write tool to create a.txt containing A, then use it again to \
         create b.txt containing B, then reply DONE.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.max_turns = Some(1);
    setup.allow_permissions();
    setup
}

/// A turn budget that runs out stops the session rather than silently
/// truncating the answer.
async fn max_turns(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        !turn.succeeded(),
        "work that could not finish inside the turn budget did not succeed"
    );
    expect!(
        turn.saw("result.error_max_turns"),
        "the result names the budget as the reason, not a generic failure: {:?}",
        turn.result().map(crate::driver::sdk::ResultMessage::kind)
    );
    expect!(
        !turn.errors().is_empty(),
        "the result carries something a caller can show the user"
    );
    expect!(
        turn.usage().is_some_and(|usage| usage.input_tokens > 0),
        "a turn that ran and then hit the budget still accounts for what it spent"
    );
}

pub(super) static MAX_BUDGET: SpecDef = SpecDef {
    name: "results/max_budget",
    fixture: "max_budget",
    setup: max_budget_setup,
    run: |session| Box::pin(max_budget(session)),
};

fn max_budget_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Write a detailed 500 word essay about the history of rope.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    // Small enough that the first request cannot fit inside it, so the
    // stop is the budget's rather than the answer happening to be short.
    setup.options.max_budget_usd = Some(0.000_01);
    setup
}

/// A spend limit is checked once a request has been paid for: on Claude
/// 2.1.312 the first answer arrives and the result still names the limit.
/// (2.1.247 stopped before the first request.)
async fn max_budget(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.saw("result.error_max_budget_usd"),
        "the result names the spend limit as the reason: {:?}",
        turn.result().map(crate::driver::sdk::ResultMessage::kind)
    );
    expect!(
        !turn.text().is_empty(),
        "the limit is checked after the request it would have stopped: {:?}",
        turn.text()
    );
    expect!(!turn.errors().is_empty(), "the result explains itself");
}

pub(super) static INTERRUPTED: SpecDef = SpecDef {
    name: "results/interrupted",
    fixture: "interrupted",
    setup: interrupted_setup,
    run: |session| Box::pin(interrupted(session)),
};

fn interrupted_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Count slowly from one to two hundred, one number per line.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A turn can be stopped from outside while it is streaming, and the session
/// stays usable enough to report how it ended.
async fn interrupted(session: &mut SpecSession) {
    // Wait for the model to have started answering, so this interrupts a turn
    // in flight rather than racing the first token.
    session.advance_to("assistant").await;
    session
        .interrupt()
        .await
        .expect("the interrupt control is acknowledged");

    let turn = session.turn().await;
    expect!(
        !turn.succeeded(),
        "an interrupted turn does not report success"
    );
    expect!(
        turn.saw("result.error_during_execution"),
        "an interrupt ends the turn through the ordinary result channel rather \
         than by dropping the stream: {:?}",
        turn.result().map(crate::driver::sdk::ResultMessage::kind)
    );
}
