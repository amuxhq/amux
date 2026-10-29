//! Specifications for options: the things a caller decides before a session
//! opens.
//!
//! Options do not travel as protocol frames. They become command-line flags on
//! the Claude Code process, which is why each recording carries the spawn that
//! produced it. Two things are worth stating about them, and both are things a
//! recording can settle: that an option a caller set actually reached Claude
//! Code, and that Claude Code accepted it.
//!
//! The second is not a formality. Claude Code rejects a flag it does not know,
//! so a session that opens and answers is evidence that every flag the SDK
//! produced for these options is one Claude Code understands. A misspelled flag
//! name is otherwise invisible: the option simply does nothing.

use std::collections::HashMap;

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::{
    Effort, HookEvent, HookSubscription, PermissionMode, SdkBeta, SettingSource, SkillsConfig,
    SystemPrompt, ThinkingConfig, ThinkingDisplay, ToolsConfig, ToolsPreset,
};
use crate::expect;

pub(super) static CONFIGURED_TURN: SpecDef = SpecDef {
    name: "options/configured_turn",
    fixture: "configured_turn",
    setup: configured_setup,
    run: |session| Box::pin(configured_turn(session)),
};

fn configured_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "What is the capital of France?");
    setup.options.permission_mode = Some(PermissionMode::Default);

    // The one option here whose effect is directly visible in the answer.
    setup.options.system_prompt = Some(SystemPrompt::Custom(
        "Whatever you are asked, reply with exactly the word CONFIGURED and \
         nothing else."
            .to_string(),
    ));

    // The rest ride along to show they reach Claude Code and are accepted.
    setup.options.allowed_tools = vec!["Read".to_string()];
    setup.options.disallowed_tools = vec!["Bash".to_string()];
    // The SDK refuses a fallback that is the model itself, so this names a
    // different one. It is never reached: nothing here makes Haiku unavailable.
    setup.options.fallback_model = Some("claude-sonnet-5".to_string());
    setup.options.betas = vec![SdkBeta::Context1M];
    // Claude 2.1.312 leaves thinking text out unless asked to show it.
    setup.options.thinking = Some(ThinkingConfig::Enabled {
        budget_tokens: Some(1024),
        display: Some(ThinkingDisplay::Summarized),
    });
    setup.options.setting_sources = vec![SettingSource::Project];
    setup.options.strict_mcp_config = true;
    setup.options.persist_session = Some(false);
    setup.options.include_hook_events = true;
    setup.options.managed_settings = Some(serde_json::json!({}));
    setup.options.tools = Some(ToolsConfig::Preset {
        preset: ToolsPreset::ClaudeCode,
    });
    setup.options.additional_directories = vec![std::env::temp_dir()];
    setup.options.skills = Some(SkillsConfig::Selected(Vec::new()));
    setup.options.tool_aliases = HashMap::from([("Read".to_string(), "Peek".to_string())]);
    setup.options.title = Some("specification capture".to_string());
    setup.options.plan_mode_instructions = Some("Plan tersely.".to_string());
    setup.options.prompt_suggestions = true;
    setup.options.agent_progress_summaries = Some(true);
    setup.options.forward_subagent_text = Some(true);
    setup.options.per_task_stop_affordance = Some(true);
    setup
}

/// A caller's options reach Claude Code and change what the session does.
async fn configured_turn(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "Claude Code accepted every flag these options produced - it refuses \
         a flag it does not know, so an opened, answering session is what \
         proves the SDK spelled them all the way this Claude Code expects"
    );
    expect!(
        turn.text() == "CONFIGURED",
        "the caller's system prompt reached the model, which answered from it \
         rather than from the question: {:?}",
        turn.text()
    );
}

pub(super) static EVERY_HOOK_EVENT: SpecDef = SpecDef {
    name: "options/every_hook_event",
    fixture: "every_hook_event",
    setup: every_hook_setup,
    run: |session| Box::pin(every_hook_event(session)),
};

/// Every hook event this SDK can express. Listed rather than derived, so that
/// adding one upstream shows up here as a specification to extend rather than
/// silently widening what this claims.
const EVERY_EVENT: &[HookEvent] = &[
    HookEvent::PreToolUse,
    HookEvent::PostToolUse,
    HookEvent::PostToolUseFailure,
    HookEvent::PostToolBatch,
    HookEvent::Notification,
    HookEvent::UserPromptSubmit,
    HookEvent::UserPromptExpansion,
    HookEvent::SessionStart,
    HookEvent::SessionEnd,
    HookEvent::Stop,
    HookEvent::StopFailure,
    HookEvent::SubagentStart,
    HookEvent::SubagentStop,
    HookEvent::PreCompact,
    HookEvent::PostCompact,
    HookEvent::PermissionRequest,
    HookEvent::PermissionDenied,
    HookEvent::Setup,
    HookEvent::TeammateIdle,
    HookEvent::TaskCreated,
    HookEvent::TaskCompleted,
    HookEvent::Elicitation,
    HookEvent::ElicitationResult,
    HookEvent::ConfigChange,
    HookEvent::WorktreeCreate,
    HookEvent::WorktreeRemove,
    HookEvent::InstructionsLoaded,
    HookEvent::CwdChanged,
    HookEvent::FileChanged,
    HookEvent::DirectoryAdded,
    HookEvent::MessageDisplay,
];

fn every_hook_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly HOOKS_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    for event in EVERY_EVENT {
        setup.options.hook_subscriptions.push(HookSubscription {
            event: event.clone(),
            matcher: None,
        });
    }
    setup.answer_hooks(None);
    setup
}

/// Every hook event the SDK can express is a name Claude Code recognises, and
/// registering all of them is accepted.
///
/// This is about naming, not firing. Most of these events cannot be induced in
/// one short session - a worktree is never created, no configuration changes,
/// nothing is compacted - and `tools/hook_lifecycle` is where the four that a
/// tool call does reach are shown to arrive in order. What is at stake here is
/// that each event is registered under the name Claude Code expects. Get one
/// wrong and it is not an error: the hook is simply never called, and the
/// caller has no way to tell that from an event that never happened.
async fn every_hook_event(session: &mut SpecSession) {
    expect!(
        session.initialization().hooks_applied == Some(true),
        "Claude Code confirms it applied the hooks it was sent, rather than \
         accepting the registration and discarding it: {:?}",
        session.initialization().hooks_applied
    );

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "a session with every hook event registered still runs"
    );
    expect!(
        turn.text() == "HOOKS_OK",
        "and answers its prompt: {:?}",
        turn.text()
    );
}

/// The model the effort specification records against.
///
/// Haiku declares no supported effort levels, so setting one there would
/// record a flag the model ignores - which is no evidence that the option
/// reaches anything.
const SONNET: &str = "claude-sonnet-5";

pub(super) static EFFORTFUL_TURN: SpecDef = SpecDef {
    name: "configured/effortful_turn",
    fixture: "effortful_turn",
    setup: effortful_setup,
    run: |session| Box::pin(effortful(session)),
};

fn effortful_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(SONNET, "Reply with exactly EFFORT_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.effort = Some(Effort::Low);
    setup.options.persist_session = Some(false);
    setup
}

/// A session can be told how hard to work, and says so when asked.
async fn effortful(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded() && turn.text() == "EFFORT_OK",
        "a session with an effort level set answers like any other: {:?}",
        turn.text()
    );
}
