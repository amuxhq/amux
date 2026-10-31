//! Every recording in the claude-specs and codex-specs corpora plays back
//! through its fake binary, byte for byte, one test per recording.

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

/// Headless Claude's corpus: captures from live runs, driven by amux's own
/// interpreter.
mod claude_sdk {
    use super::*;

    recordings!(Kind::ClaudeSdk, "claude-specs/fixtures/live/sdk", {
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
        subagent_task => "subagent_task",
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

    recordings!(Kind::ClaudePty, "claude-specs/fixtures/live/pty", {
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
        subagent_task => "subagent_task",
        task_list => "task_list",
        terminal_allow => "terminal_allow",
        terminal_escape => "terminal_escape",
        terminal_question => "terminal_question",
        text_turn => "text_turn",
    });
}

// Unix only: these were recorded over Codex's socket, which amux uses only on
// Unix; a Windows host speaks to Codex over stdio.
/// Codex's corpus: captures from live runs, driven by amux's own
/// interpreter.
#[cfg(unix)]
mod codex {
    use super::*;

    recordings!(Kind::Codex, "codex-specs/fixtures/live", {
        access_grant => "access_grant",
        approval_allow => "approval_allow",
        approval_deny => "approval_deny",
        approval_scopes => "approval_scopes",
        attach => "attach",
        automatic_review => "automatic_review",
        background_terminal => "background_terminal",
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
