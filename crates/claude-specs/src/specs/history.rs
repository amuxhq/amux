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

pub(super) static FORKED: SpecDef = SpecDef {
    name: "history/forked",
    fixture: "forked",
    setup: remembering,
    run: |session| Box::pin(forked(session)),
};

/// Forking is resuming that branches: the new session reads the old one's
/// history but writes under an identity of its own.
///
/// The difference from a plain resume is the whole point. A caller that forks
/// wants to try something without disturbing the conversation it came from, so
/// the two must not share an identity - and the fork must still know what the
/// source knew.
async fn forked(session: &mut SpecSession) {
    let first = session.turn().await;
    expect!(
        first.text().contains("REMEMBERED"),
        "the source session answers its prompt: {:?}",
        first.text()
    );
    let source = session.session_id().to_owned();

    let mut fork = session
        .open({
            let mut setup = asking_again(HAIKU);
            setup.options.resume = Some(source.clone());
            setup.options.fork_session = true;
            // Forking requires the caller to name the branch. The SDK refuses
            // to invent one, because an identity Claude Code persists under is
            // not something to guess at.
            setup.options.session_id = Some(FORK_ID.to_owned());
            setup
        })
        .await
        .expect("a persisted session can be forked");

    expect!(
        fork.session_id() == FORK_ID && fork.session_id() != source,
        "the fork runs under the identity it was given, not the one it read \
         from: {} against source {source}",
        fork.session_id()
    );

    let second = fork.turn().await;
    expect!(
        second.succeeded(),
        "the forked session takes its turn like any other"
    );
    expect!(
        second.text().contains(SECRET),
        "and still knows what the source session was told, so the branch \
         carries the history rather than starting empty: {:?}",
        second.text()
    );
    fork.close().await;
}

/// The branch identity. Fixed rather than generated: a specification's session
/// identities are part of what its recording is evidence of, and a fresh one
/// each run could not be replayed.
const FORK_ID: &str = "f0f0f0f0-1111-4222-8333-444444444444";

pub(super) static RESUMED_AT: SpecDef = SpecDef {
    name: "history/resumed_at",
    fixture: "resumed_at",
    setup: resumed_at_setup,
    run: |session| Box::pin(resumed_at(session)),
};

fn resumed_at_setup() -> SessionSetup {
    let mut setup = SessionSetup::conversation(
        HAIKU,
        format!("Remember the word {SECRET}. Reply with exactly REMEMBERED."),
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.persist_session = Some(true);
    setup
}

/// The second word the first session is told. A resume truncated before it
/// cannot know it, which is the only way to tell truncation from an ordinary
/// resume.
const LATER_SECRET: &str = "CORMORANT";

/// A resume given a point keeps the whole conversation on Claude 2.1.312:
/// the point is accepted and not honoured. (2.1.251 kept only what came
/// before it; amux never resumes at a point.)
///
/// A plain resume carries the whole conversation, so nothing about it would
/// show whether the point was honoured. This one asks for a point in the
/// middle and then asks a question only an untruncated session could answer:
/// the second word is in the transcript on disk, and a resume that ignores the
/// fork point recites it.
///
/// The point itself is an assistant message's own id, taken off the stream
/// rather than read back from disk. That is what makes the specification
/// replayable at all - the recording carries the same id it was captured
/// with, where a transcript the recording does not hold would not.
async fn resumed_at(session: &mut SpecSession) {
    let first = session.turn().await;
    expect!(
        first.text().contains("REMEMBERED"),
        "the first turn answers its prompt: {:?}",
        first.text()
    );
    let fork_point = first
        .messages()
        .iter()
        .find_map(|message| match message {
            crate::driver::sdk::Message::Assistant(assistant) => Some(assistant.uuid.clone()),
            _ => None,
        })
        .expect("an answered turn carries an assistant message to fork at");

    session
        .say(&format!(
            "Now also remember the word {LATER_SECRET}. Reply with exactly REMEMBERED."
        ))
        .await
        .expect("a second turn can be sent");
    let second = session.turn().await;
    expect!(
        second.text().contains("REMEMBERED"),
        "and the second turn answers too, so both words are in the transcript: {:?}",
        second.text()
    );
    let original = session.session_id().to_owned();

    let mut resumed = session
        .open({
            let mut setup = SessionSetup::new(
                HAIKU,
                "List every word I asked you to remember, and nothing else.",
            );
            setup.options.permission_mode = Some(PermissionMode::Default);
            setup.options.persist_session = Some(true);
            setup.options.resume = Some(original.clone());
            setup.options.resume_session_at = Some(fork_point.to_string());
            setup
        })
        .await
        .expect("a persisted session can be resumed at a chosen message");

    let answer = resumed.turn().await;
    expect!(
        answer.succeeded(),
        "the session resumed at a point takes its turn like any other"
    );
    expect!(
        answer.text().contains(SECRET),
        "and keeps what came before the fork point: {:?}",
        answer.text()
    );
    expect!(
        answer.text().contains(LATER_SECRET),
        "and what came after it too, so the point made no difference: {:?}",
        answer.text()
    );
    resumed.close().await;
}
