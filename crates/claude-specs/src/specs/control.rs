//! Specifications for the control protocol: requests the SDK sends alongside a
//! turn, and the acknowledgements Claude Code sends back.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::PermissionMode;
use crate::driver::sdk::control::FlagSettings;
use crate::expect;

/// The model the effort specification records against: Haiku takes no
/// effort levels.
const SONNET: &str = "claude-sonnet-5";

pub(super) static PERMISSION_MODE_AND_MODEL: SpecDef = SpecDef {
    name: "control/permission_mode_and_model",
    fixture: "controls",
    setup: modes_setup,
    run: |session| Box::pin(permission_mode_and_model(session)),
};

fn modes_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly CONTROL_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// Two controls issued while a turn is already in flight are acknowledged
/// independently, and neither disturbs the turn.
async fn permission_mode_and_model(session: &mut SpecSession) {
    // Both controls are answered on the same channel the turn is streaming
    // over, so a request id that correlated loosely would hand this
    // acknowledgement the other control's reply.
    let applied = session
        .set_permission_mode(PermissionMode::AcceptEdits)
        .await
        .expect("the mode control is acknowledged");
    expect!(
        applied == Some(PermissionMode::AcceptEdits),
        "a mode acknowledgement names the mode Claude Code actually applied"
    );

    session
        .set_model(HAIKU)
        .await
        .expect("the model control is acknowledged");

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "controls issued mid-turn leave the turn able to complete"
    );
    expect!(
        turn.text() == "CONTROL_OK",
        "the turn answers the prompt it was given, not the controls: {:?}",
        turn.text()
    );
    expect!(
        turn.saw("system.status"),
        "the mode change also arrives as an unsolicited status frame, which the \
         stream surfaces rather than swallowing"
    );
}

pub(super) static EFFORT: SpecDef = SpecDef {
    name: "control/effort",
    fixture: "effort",
    setup: effort_setup,
    run: |session| Box::pin(effort(session)),
};

fn effort_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(SONNET, "Reply with exactly EFFORT_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A running session changes its effort through the flag settings control,
/// and the settings Claude reports applied name the new effort straight
/// away, whatever the user's settings files chose for the model.
async fn effort(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(turn.succeeded(), "the session's first turn completes");
    for level in ["low", "high"] {
        session
            .apply_flag_settings(FlagSettings {
                effort_level: Some(Some(level.to_owned())),
                ..FlagSettings::default()
            })
            .await
            .expect("the flag settings control is acknowledged");
        let applied = session
            .applied_settings()
            .await
            .expect("the settings control is answered");
        expect!(
            applied.effort.as_deref() == Some(level),
            "the applied effort is the one just set: {:?}",
            applied.effort
        );
    }
    session
        .say("Reply with exactly EFFORT_AGAIN and nothing else.")
        .await
        .expect("a later prompt is accepted");
    let turn = session.turn().await;
    expect!(
        turn.succeeded() && turn.text() == "EFFORT_AGAIN",
        "a turn after the change runs as usual: {:?}",
        turn.text()
    );
}

pub(super) static SESSION_INTROSPECTION: SpecDef = SpecDef {
    name: "control/session_introspection",
    fixture: "introspection",
    setup: introspection_setup,
    run: |session| Box::pin(session_introspection(session)),
};

fn introspection_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly INTROSPECT_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A session can be asked what it is, without disturbing the turn it is
/// running.
///
/// These controls exist because a caller building an interface needs to render
/// the session's own state - which model, which commands, how much context is
/// left - rather than assume the options it asked for were the ones granted.
async fn session_introspection(session: &mut SpecSession) {
    let initialization = session.initialization();
    expect!(
        initialization.models.iter().any(|model| model
            .resolved_model
            .as_deref()
            .is_some_and(|resolved| resolved == HAIKU)),
        "initialization reports the models this session can actually switch to, \
         resolved to concrete ids rather than aliases: {:?}",
        initialization
            .models
            .iter()
            .map(|model| model.value.as_str())
            .collect::<Vec<_>>()
    );
    expect!(
        !initialization.commands.is_empty(),
        "initialization reports the slash commands Claude Code will accept"
    );

    let usage = session
        .context_usage()
        .await
        .expect("the context-usage control is answered");
    expect!(
        usage.total_tokens > 0 && usage.total_tokens < usage.max_tokens,
        "context usage accounts for a real, partly-filled window: {} of {}",
        usage.total_tokens,
        usage.max_tokens
    );
    expect!(
        usage
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<u64>()
            >= usage.total_tokens,
        "the reported categories account for the total, rather than the total \
         being an unexplained number"
    );

    // A session with no MCP servers configured reports none, rather than
    // failing or reporting something it inherited.
    let servers = session
        .mcp_server_status()
        .await
        .expect("the MCP status control is answered");
    expect!(
        !servers.iter().any(|server| server.name.trim().is_empty()),
        "every server it does report is named: {:?}",
        servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
    );

    // Nothing is backgrounded and no task exists, which is the case a caller
    // hits constantly and the one most likely to be mis-parsed: Claude Code
    // answers with an object, not the bare boolean the declaration suggests.
    expect!(
        session
            .background_tasks(None)
            .await
            .expect("the background-tasks control is answered"),
        "backgrounding with no target reports that it happened"
    );
    session
        .stop_task("task_that_does_not_exist")
        .await
        .expect("stopping an unknown task is answered rather than refused");

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "introspection controls do not disturb the turn they interleave with"
    );
    expect!(
        turn.text() == "INTROSPECT_OK",
        "the turn answers its prompt: {:?}",
        turn.text()
    );
}
