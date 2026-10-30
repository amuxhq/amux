//! Specifications for continuing a conversation in a session that is not the
//! one that started it.
//!
//! Both of these open two Claude Code processes. The first persists what it
//! said; the second is started against that record and has to arrive already
//! knowing it. That is the whole of the claim, and it is not observable from
//! one session: a conversation that survives inside a running process proves
//! nothing about one that survives the process ending.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::PermissionMode;
use crate::expect;

/// The word the first session is asked to keep. Anything the second session
/// says about it can only have come from the first session's transcript.
const SECRET: &str = "ALBATROSS";

fn remembering() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        format!("Remember the word {SECRET}. Reply with exactly REMEMBERED."),
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.persist_session = Some(true);
    setup
}

fn asking_again(model: &str) -> SessionSetup {
    let mut setup = SessionSetup::new(
        model,
        "What word did I ask you to remember? Reply with just that word.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.persist_session = Some(true);
    setup
}

pub(super) static RESUMED: SpecDef = SpecDef {
    name: "history/resumed",
    fixture: "resumed",
    setup: remembering,
    run: |session| Box::pin(resumed(session)),
};

/// A session can be continued after the process that held it has gone.
///
/// The second session is a different Claude Code, started only with the first
/// one's identity. Everything it knows about the conversation it has to have
/// read back from what the first one persisted.
///
/// It does not re-state that history on the stream. A caller that wants the
/// earlier turns has to read them from the persisted transcript itself, which
/// is what `list_sessions` and `get_session_messages` are for - resuming gives
/// you a session that remembers, not a session that recounts.
async fn resumed(session: &mut SpecSession) {
    let first = session.turn().await;
    expect!(
        first.text().contains("REMEMBERED"),
        "the first session answers its prompt: {:?}",
        first.text()
    );
    let original = session.session_id().to_owned();

    let mut resumed = session
        .open({
            let mut setup = asking_again(HAIKU);
            setup.options.resume = Some(original.clone());
            setup
        })
        .await
        .expect("a persisted session can be resumed");

    expect!(
        resumed.session_id() == original,
        "a resumed session continues the identity it was given rather than \
         starting a new one: {} against {original}",
        resumed.session_id()
    );

    let second = resumed.turn().await;
    expect!(
        second.succeeded(),
        "the resumed session takes its turn like any other"
    );
    expect!(
        second.text().contains(SECRET),
        "and answers from the first session's conversation, which it can only \
         have read back from what was persisted: {:?}",
        second.text()
    );
    resumed.close().await;
}
