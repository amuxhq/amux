//! Specifications for the shape of an ordinary session: what a turn is made
//! of, and what changes when the caller asks to watch it arrive.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::PermissionMode;
use crate::expect;

pub(super) static TEXT_TURN: SpecDef = SpecDef {
    name: "session/text_turn",
    fixture: "text_turn",
    setup: text_turn_setup,
    run: |session| Box::pin(text_turn(session)),
};

fn text_turn_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly PONG and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// The smallest complete session: one prompt in, one answer out, and enough
/// around it to know what answered.
async fn text_turn(session: &mut SpecSession) {
    let initialization = session.initialization();
    expect!(
        initialization
            .models
            .iter()
            .any(|model| model.value == "haiku"),
        "a session reports the models it can switch to before it answers \
         anything: {:?}",
        initialization
            .models
            .iter()
            .map(|model| model.value.as_str())
            .collect::<Vec<_>>()
    );

    let turn = session.turn().await;
    expect!(
        turn.saw("system.init"),
        "the session announces itself before answering, which is what carries \
         the model, tools and permission mode actually in force"
    );
    expect!(
        turn.text() == "PONG",
        "the completed text is the answer, with no framing around it: {:?}",
        turn.text()
    );
    expect!(
        turn.succeeded(),
        "an answered prompt ends in a success result, not merely in silence"
    );

    let usage = turn.usage().expect("a result accounts for its token usage");
    expect!(
        usage.input_tokens > 0 && usage.output_tokens > 0,
        "usage is the real cost of this turn in both directions: {usage:?}"
    );
    expect!(
        turn.errors().is_empty(),
        "a successful turn reports no errors: {:?}",
        turn.errors()
    );
}

pub(super) static STREAMED_TURN: SpecDef = SpecDef {
    name: "session/streamed_turn",
    fixture: "streamed_turn",
    setup: streamed_turn_setup,
    run: |session| Box::pin(streamed_turn(session)),
};

fn streamed_turn_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Think briefly about the number seven, then reply with exactly SEVEN.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.include_partial_messages = true;
    setup
}

/// Opting into partial messages adds a live view of the answer forming. It
/// does not replace the completed message, and it must not disagree with it.
///
/// This is the claim that matters for anything rendering a cursor: a caller
/// that appends deltas and then also renders the completed block would show
/// the answer twice if these were alternatives rather than two views of one
/// thing.
///
/// Nothing here claims the model reasons before answering. Whether it does is
/// its own choice and it varies between runs, so the claim is the relationship
/// between the streamed and completed content rather than the presence of any
/// particular block - which holds whether or not there was thinking to stream.
async fn streamed_turn(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "streaming does not change how a turn ends"
    );

    expect!(
        !turn.stream_events().is_empty(),
        "asking for partial messages produces incremental events"
    );
    expect!(
        turn.streamed_text() == turn.text(),
        "the deltas assemble to exactly the completed text - they are a view \
         of the same answer, not a second one: {:?} vs {:?}",
        turn.streamed_text(),
        turn.text()
    );
    expect!(
        turn.streamed_thinking() == turn.thinking(),
        "thinking streams under the same rule as text"
    );
    expect!(
        turn.text() == "SEVEN",
        "the answer survives being streamed: {:?}",
        turn.text()
    );
}

pub(super) static MULTI_TURN: SpecDef = SpecDef {
    name: "session/multi_turn",
    fixture: "multi_turn",
    setup: multi_turn_setup,
    run: |session| Box::pin(multi_turn(session)),
};

fn multi_turn_setup() -> SessionSetup {
    let mut setup = SessionSetup::conversation(
        HAIKU,
        "Remember the word ALBATROSS. Reply with exactly REMEMBERED.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A session takes more than one turn, and the second turn can see the first.
///
/// This is what separates a session from a request. The claim is that the
/// conversation is retained in the process, not merely that a second message
/// is accepted: the second turn is asked something only the first turn's
/// content can answer.
async fn multi_turn(session: &mut SpecSession) {
    let first = session.turn().await;
    expect!(
        first.text().contains("REMEMBERED"),
        "the first turn answers its prompt: {:?}",
        first.text()
    );

    session
        .say("What word did I ask you to remember? Reply with just that word.")
        .await
        .expect("a message can be sent into a session that is already open");

    let second = session.turn().await;
    expect!(
        second.succeeded(),
        "the second turn completes like the first"
    );
    expect!(
        second.text().contains("ALBATROSS"),
        "the second turn answers from the first turn's conversation, which only \
         a retained session could do: {:?}",
        second.text()
    );
    expect!(
        sent(&second) > sent(&first),
        "and it costs more to send, because the first turn is now part of what \
         is being sent: {} against {}",
        sent(&second),
        sent(&first)
    );
}

/// Everything the model had to read for a turn. Counting only `input_tokens`
/// would understate it and shrink as the conversation grows, because a resent
/// conversation is served from the prompt cache rather than read afresh.
fn sent(turn: &super::Turn) -> u64 {
    turn.usage().map_or(0, |usage| {
        usage.input_tokens
            + usage.cache_creation_input_tokens.unwrap_or_default()
            + usage.cache_read_input_tokens.unwrap_or_default()
    })
}

pub(super) static SIGN_IN_PROBLEM: SpecDef = SpecDef {
    name: "session/sign_in_problem",
    fixture: "sign_in_problem",
    setup: sign_in_problem_setup,
    run: |session| Box::pin(sign_in_problem(session)),
};

fn sign_in_problem_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly PONG and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    // A key the API rejects takes precedence over the owner's own sign-in;
    // two retries would keep the recording short, were it retried.
    setup.extra_env = vec![
        ("ANTHROPIC_API_KEY", "sk-ant-api03-amux-spec-invalid"),
        ("CLAUDE_CODE_MAX_RETRIES", "2"),
    ];
    setup
}

/// A rejected credential is not retried (Claude 2.1.283 retried it, each
/// retry naming the status and `authentication_failed`; 2.1.312 does not):
/// a synthetic assistant message carrying the error stands in place of an
/// answer.
async fn sign_in_problem(session: &mut SpecSession) {
    let turn = session.turn().await;
    let frames = turn
        .messages()
        .iter()
        .map(|message| serde_json::to_value(message).expect("a parsed frame serialises"))
        .collect::<Vec<_>>();
    let retries = frames
        .iter()
        .filter(|frame| frame["type"] == "system" && frame["subtype"] == "api_retry")
        .map(|frame| {
            (
                frame["attempt"].as_u64().unwrap_or_default(),
                frame["error_status"].as_u64().unwrap_or_default(),
                frame["error"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    expect!(
        retries.is_empty(),
        "a rejected credential is not retried: {retries:?}"
    );
    let synthetic = frames
        .iter()
        .find(|frame| frame["type"] == "assistant" && frame["message"]["model"] == "<synthetic>");
    expect!(
        synthetic.is_some_and(|frame| frame["error"] == "authentication_failed"),
        "the answer is a synthetic message carrying the sign-in error: {synthetic:?}"
    );
}
