//! Every recording in the Claude, Codex and ACP corpora plays back
//! through its fake binary, byte for byte, one test per recording.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use provider_fakes::{Kind, conformance};

fn binary(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeSdk => env!("CARGO_BIN_EXE_fake-claude-sdk"),
        Kind::ClaudePty => env!("CARGO_BIN_EXE_fake-claude-pty"),
        Kind::Codex => env!("CARGO_BIN_EXE_fake-codex"),
        Kind::Acp => env!("CARGO_BIN_EXE_fake-acp"),
    }
}

fn root(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// Every directory under a corpus root that holds a recording.
fn recorded(relative: &str) -> BTreeSet<String> {
    std::fs::read_dir(root(relative))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .join("io.jsonl")
                .exists()
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

macro_rules! recordings {
    ($kind:expr, $root:literal, { $($name:ident => $dir:literal,)+ }) => {
        $(
            #[tokio::test]
            async fn $name() {
                let recording = root($root).join($dir);
                if let Err(drift) = conformance($kind, Path::new(binary($kind)), &recording).await {
                    panic!("{drift}");
                }
            }
        )+

        #[test]
        fn every_recording_has_a_test() {
            let listed: BTreeSet<String> = [$($dir),+].into_iter().map(str::to_owned).collect();
            assert_eq!(listed, recorded($root), "register each recording under {}", $root);
        }
    };
}

/// Headless Claude's corpus: captures from live runs, driven by amux's own
/// interpreter.
mod claude_sdk {
    use super::*;

    recordings!(Kind::ClaudeSdk, "interpret/fixtures/claude_sdk/live", {
        background_shell => "background_shell",
        cleared => "cleared",
        compacted => "compacted",
        configured_turn => "configured_turn",
        controls => "controls",
        effort => "effort",
        elicitation_accepted => "elicitation_accepted",
        elicitation_cancelled => "elicitation_cancelled",
        elicitation_declined => "elicitation_declined",
        elicitation_link_accepted => "elicitation_link_accepted",
        failed_tool_server => "failed_tool_server",
        failing_command => "failing_command",
        image => "image",
        interrupted => "interrupted",
        multi_turn => "multi_turn",
        permission_callback => "permission_callback",
        plan => "plan",
        plan_reviewed => "plan_reviewed",
        question_asked => "question_asked",
        question_dismissed => "question_dismissed",
        question_every_shape => "question_every_shape",
        questions => "questions",
        resumed => "resumed",
        sign_in_problem => "sign_in_problem",
        steer_folded => "steer_folded",
        subagent_amux_tools => "subagent_amux_tools",
        subagent_ask => "subagent_ask",
        subagent_continued => "subagent_continued",
        subagent_elicitation => "subagent_elicitation",
        subagent_nested => "subagent_nested",
        subagent_resumed => "subagent_resumed",
        subagent_stopped => "subagent_stopped",
        subagent_task => "subagent_task",
        subagents_parallel => "subagents_parallel",
        task_list => "task_list",
        text_turn => "text_turn",
        usage => "usage",
    });
}

/// Terminal Claude's corpus: captures from live runs, driven by amux's own
/// interpreter. Unix only: Windows does not host terminal Claude, and
/// ConPTY re-renders the child's output in its own escape sequences, so
/// byte-exact playback against these Unix recordings cannot hold there; see
/// docs/ARCHITECTURE.md, "Windows, as a stated cost".
#[cfg(unix)]
mod claude_pty {
    use super::*;

    recordings!(Kind::ClaudePty, "interpret/fixtures/claude_pty/live", {
        api_error => "api_error",
        background_shell => "background_shell",
        cleared => "cleared",
        compacted => "compacted",
        configured_turn => "configured_turn",
        failing_command => "failing_command",
        image => "image",
        interrupted => "interrupted",
        long_prompt => "long_prompt",
        mode_cycle => "mode_cycle",
        multi_turn => "multi_turn",
        permission_callback => "permission_callback",
        permission_denied => "permission_denied",
        permission_scoped => "permission_scoped",
        plan => "plan",
        plan_reviewed => "plan_reviewed",
        question_asked => "question_asked",
        question_dismissed => "question_dismissed",
        question_every_shape => "question_every_shape",
        questions => "questions",
        resumed => "resumed",
        sign_in_problem => "sign_in_problem",
        steer_folded => "steer_folded",
        subagent_amux_tools => "subagent_amux_tools",
        subagent_ask => "subagent_ask",
        subagent_continued => "subagent_continued",
        subagent_elicitation => "subagent_elicitation",
        subagent_nested => "subagent_nested",
        subagent_resumed => "subagent_resumed",
        subagent_stopped => "subagent_stopped",
        subagent_stopped_foreground => "subagent_stopped_foreground",
        subagent_task => "subagent_task",
        subagents_parallel => "subagents_parallel",
        task_list => "task_list",
        terminal_allow => "terminal_allow",
        terminal_escape => "terminal_escape",
        terminal_question => "terminal_question",
        text_turn => "text_turn",
    });

    /// What terminal Claude wrote for its subagents is among what a played
    /// recording is held to: each one's transcript and its meta.json, as
    /// they were left.
    #[test]
    fn a_recordings_subagent_files_are_played() {
        let recording = root("interpret/fixtures/claude_pty/live").join("subagents_parallel");
        let process = provider_fakes::playback::process(&recording, "pty").unwrap();
        let files = provider_fakes::playback::subagent_files(&process.events);
        let names: Vec<&str> = files.iter().map(|(_, file, _)| file.as_str()).collect();
        assert_eq!(
            names.len(),
            6,
            "three subagents, a transcript and a meta each: {names:?}"
        );
        for (_, file, contents) in &files {
            assert!(file.starts_with("subagents/agent-"), "{file}");
            if file.ends_with(".meta.json") {
                let meta: serde_json::Value = serde_json::from_slice(contents).unwrap();
                assert_eq!(meta["finished"], true, "{file} as Claude left it");
            } else {
                assert!(contents.ends_with(b"\n") && contents.len() > 1000, "{file}");
            }
        }
    }
}

// Unix only: these were recorded over Codex's socket, which amux uses only on
// Unix; a Windows host speaks to Codex over stdio.
/// Codex's corpus: captures from live runs, driven by amux's own
/// interpreter.
#[cfg(unix)]
mod codex {
    use super::*;

    recordings!(Kind::Codex, "interpret/fixtures/codex/live", {
        access_grant => "access_grant",
        approval_allow => "approval_allow",
        approval_deny => "approval_deny",
        approval_scopes => "approval_scopes",
        attach => "attach",
        automatic_review => "automatic_review",
        background_terminal => "background_terminal",
        child_approval => "child_approval",
        child_command => "child_command",
        child_continued => "child_continued",
        child_resume => "child_resume",
        child_stop => "child_stop",
        child_tools => "child_tools",
        compaction => "compaction",
        exploring => "exploring",
        failing_command => "failing_command",
        file_changes => "file_changes",
        file_moved => "file_moved",
        image => "image",
        inject_drain => "inject_drain",
        inject_idle => "inject_idle",
        interrupt => "interrupt",
        mode => "mode",
        permission => "permission",
        plan => "plan",
        questions => "questions",
        reasoning_summary => "reasoning_summary",
        resume => "resume",
        signed_out => "signed_out",
        steer_folded => "steer_folded",
        subagent => "subagent",
        tool_server_form => "tool_server_form",
        turn_error => "turn_error",
        turn_retries => "turn_retries",
        turn_round_trip => "turn_round_trip",
        two_assistant_messages => "two_assistant_messages",
        usage => "usage",
        web_search => "web_search",
    });
}

/// The ACP agents' corpus: Antigravity, OpenCode and Grok, captured live
/// by the scripted ACP client. A recording whose agent was stopped and
/// started again plays each of its processes through a fake of its own.
mod acp {
    use super::*;

    recordings!(Kind::Acp, "interpret/fixtures/acp/live", {
        agy_allow => "agy-allow",
        agy_allow_always => "agy-allow_always",
        agy_cancel => "agy-cancel",
        agy_catalogue => "agy-catalogue",
        agy_compact => "agy-compact",
        agy_config => "agy-config",
        agy_deny => "agy-deny",
        agy_edit => "agy-edit",
        agy_elicitation => "agy-elicitation",
        agy_image => "agy-image",
        agy_load => "agy-load",
        agy_mid_turn => "agy-mid_turn",
        agy_prompt => "agy-prompt",
        agy_question => "agy-question",
        agy_queued => "agy-queued",
        agy_read => "agy-read",
        agy_resume => "agy-resume",
        grok_allow => "grok-allow",
        grok_allow_always => "grok-allow_always",
        grok_cancel => "grok-cancel",
        grok_catalogue => "grok-catalogue",
        grok_compact => "grok-compact",
        grok_config => "grok-config",
        grok_deny => "grok-deny",
        grok_edit => "grok-edit",
        grok_elicitation => "grok-elicitation",
        grok_image => "grok-image",
        grok_load => "grok-load",
        grok_mid_turn => "grok-mid_turn",
        grok_prompt => "grok-prompt",
        grok_question => "grok-question",
        grok_queued => "grok-queued",
        grok_read => "grok-read",
        grok_resume => "grok-resume",
        grok_signed_out => "grok-signed_out",
        opencode_allow => "opencode-allow",
        opencode_allow_always => "opencode-allow_always",
        opencode_cancel => "opencode-cancel",
        opencode_catalogue => "opencode-catalogue",
        opencode_compact => "opencode-compact",
        opencode_config => "opencode-config",
        opencode_deny => "opencode-deny",
        opencode_edit => "opencode-edit",
        opencode_elicitation => "opencode-elicitation",
        opencode_image => "opencode-image",
        opencode_load => "opencode-load",
        opencode_mid_turn => "opencode-mid_turn",
        opencode_prompt => "opencode-prompt",
        opencode_question => "opencode-question",
        opencode_queued => "opencode-queued",
        opencode_read => "opencode-read",
        opencode_resume => "opencode-resume",
    });
}
