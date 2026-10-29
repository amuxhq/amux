//! Every recording in the claude-specs and codex-specs corpora, and the live
//! captures beside them, plays back through its fake binary, byte for byte,
//! one test per recording.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use provider_fakes::{Kind, conformance};

fn binary(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeSdk => env!("CARGO_BIN_EXE_fake-claude-sdk"),
        Kind::ClaudePty => env!("CARGO_BIN_EXE_fake-claude-pty"),
        Kind::Codex => env!("CARGO_BIN_EXE_fake-codex"),
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

mod claude_sdk {
    use super::*;

    recordings!(Kind::ClaudeSdk, "claude-specs/fixtures/sdk", {
        background_shell => "background_shell",
        cleared => "cleared",
        client_id => "client_id",
        compacted => "compacted",
        configured_turn => "configured_turn",
        connected_mcp_servers => "connected_mcp_servers",
        controls => "controls",
        effort => "effort",
        effortful_turn => "effortful_turn",
        elicitation_accepted => "elicitation_accepted",
        elicitation_cancelled => "elicitation_cancelled",
        elicitation_declined => "elicitation_declined",
        elicitation_link_accepted => "elicitation_link_accepted",
        every_hook_event => "every_hook_event",
        failed_tool_server => "failed_tool_server",
        failing_command => "failing_command",
        forked => "forked",
        hook_lifecycle => "hook_lifecycle",
        image => "image",
        in_process_mcp => "in_process_mcp",
        interrupted => "interrupted",
        introspection => "introspection",
        max_budget => "max_budget",
        max_turns => "max_turns",
        multi_turn => "multi_turn",
        permission_callback => "permission_callback",
        plan_reviewed => "plan_reviewed",
        question_asked => "question_asked",
        question_dismissed => "question_dismissed",
        question_every_shape => "question_every_shape",
        resumed => "resumed",
        resumed_at => "resumed_at",
        session_maintenance => "session_maintenance",
        side_channel => "side_channel",
        sign_in_problem => "sign_in_problem",
        steer_folded => "steer_folded",
        steer_preempted => "steer_preempted",
        streamed_turn => "streamed_turn",
        subagent_task => "subagent_task",
        task_list => "task_list",
        text_turn => "text_turn",
    });
}

// Unix only: Windows does not host terminal Claude, and ConPTY re-renders the
// child's output in its own escape sequences, so byte-exact playback against
// these Unix recordings cannot hold there; see docs/ARCHITECTURE.md, "Windows,
// as a stated cost".
#[cfg(unix)]
mod claude_pty {
    use super::*;

    recordings!(Kind::ClaudePty, "claude-specs/fixtures/pty", {
        api_error => "api_error",
        background_shell => "background_shell",
        clear_relink => "clear_relink",
        compact_relink => "compact_relink",
        file_changes_and_failure => "file_changes_and_failure",
        image => "image",
        interrupt => "interrupt",
        mode_cycle => "mode_cycle",
        permission_allow_once => "permission_allow_once",
        permission_allow_scoped => "permission_allow_scoped",
        permission_deny_feedback => "permission_deny_feedback",
        plan_approve => "plan_approve",
        plan_auto => "plan_auto",
        plan_request_changes => "plan_request_changes",
        prompt => "prompt",
        prompt_multiline => "prompt_multiline",
        prompt_long => "prompt_long",
        question_cancelled => "question_cancelled",
        question_every_shape => "question_every_shape",
        question_mixed => "question_mixed",
        question_multi_other => "question_multi_other",
        question_other_single => "question_other_single",
        question_single => "question_single",
        question_skip_reply => "question_skip_reply",
        question_tabs => "question_tabs",
        sign_in_problem => "sign_in_problem",
        steer_queued => "steer_queued",
        steer_send_now => "steer_send_now",
        subagent => "subagent",
        task_list => "task_list",
        thinking => "thinking",
        tools => "tools",
    });
}

mod codex {
    use super::*;

    recordings!(Kind::Codex, "codex-specs/fixtures/runtime", {
        access_grant => "access_grant",
        approval_allow => "approval_allow",
        approval_deny => "approval_deny",
        approval_scopes => "approval_scopes",
        automatic_review => "automatic_review",
        background_terminal => "background_terminal",
        compaction => "compaction",
        dynamic_tools => "dynamic_tools",
        exploring => "exploring",
        failing_command => "failing_command",
        file_changes => "file_changes",
        file_moved => "file_moved",
        image => "image",
        initialize_and_start => "initialize_and_start",
        inject_busy => "inject_busy",
        inject_drain => "inject_drain",
        inject_idle => "inject_idle",
        interrupt => "interrupt",
        plan_mode => "plan_mode",
        reasoning_summary => "reasoning_summary",
        signed_out => "signed_out",
        subagent => "subagent",
        thread_list_and_resume => "thread_list_and_resume",
        tool_server_form => "tool_server_form",
        turn_error => "turn_error",
        turn_retries => "turn_retries",
        turn_round_trip => "turn_round_trip",
        two_assistant_messages => "two_assistant_messages",
        two_clients_approval => "two_clients_approval",
        two_clients_join_fresh => "two_clients_join_fresh",
        two_clients_prompt => "two_clients_prompt",
        two_clients_steer => "two_clients_steer",
        web_search => "web_search",
    });
}

/// Captures from a live run, driven by amux's own interpreter.
mod claude_sdk_live {
    use super::*;

    recordings!(Kind::ClaudeSdk, "claude-specs/fixtures/live/sdk", {
        plan => "plan",
        questions => "questions",
        usage => "usage",
    });
}

#[cfg(unix)]
mod claude_pty_live {
    use super::*;

    recordings!(Kind::ClaudePty, "claude-specs/fixtures/live/pty", {
        plan => "plan",
        questions => "questions",
    });
}

// Unix only: these were recorded over Codex's socket, which amux uses only on
// Unix; a Windows host speaks to Codex over stdio.
#[cfg(unix)]
mod codex_live {
    use super::*;

    recordings!(Kind::Codex, "codex-specs/fixtures/live", {
        mode => "mode",
        permission => "permission",
        plan => "plan",
        questions => "questions",
        usage => "usage",
    });
}
