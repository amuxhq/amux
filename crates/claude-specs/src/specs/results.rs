//! Specifications for the ways a turn ends other than by answering.
//!
//! Each of these is a distinct result subtype on the wire. A caller that
//! collapsed them into "it failed" would lose the only thing that tells it
//! whether to retry, raise a limit, or stop.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::PermissionMode;
use crate::expect;

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
