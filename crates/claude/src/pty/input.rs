//! The semantic input a Claude PTY accepts, before a keymap turns it into
//! bytes: prompts, interrupts, mode cycling and answers to the asks Claude
//! shows in its terminal.

use serde::{Deserialize, Serialize};

use crate::version::ClaudeVersion;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AskId(pub String);

impl std::fmt::Display for AskId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskKind {
    Permission {
        tool_name: String,
        suggestions: usize,
        is_plan: bool,
    },
    Question {
        questions: Vec<QuestionFact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionFact {
    pub options: usize,
    pub multi_select: bool,
    /// Options carry previews, which switches the form to a side-by-side
    /// layout where a digit moves the cursor instead of choosing.
    #[serde(default)]
    pub previews: bool,
}

/// Semantic input accepted by a Claude PTY session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    Prompt {
        text: String,
    },
    /// A prompt typed into the running turn, which Claude takes at its next
    /// tool boundary.
    SendNow {
        text: String,
    },
    Interrupt,
    CyclePermissionMode,
    Answer {
        ask_id: AskId,
        answer: AskAnswer,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question(QuestionResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionAnswer {
    AllowOnce,
    AllowScoped {
        suggestion: usize,
    },
    /// "Yes, and switch to auto mode", which Claude's menu offers in manual
    /// mode where its model takes auto mode.
    AllowSwitchingToAuto,
    Deny {
        feedback: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanAnswer {
    ApproveAuto,
    ApproveManual,
    RequestChanges { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionResponse {
    pub answers: Vec<QuestionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub selected: Vec<usize>,
    pub other: Option<String>,
}

impl QuestionAnswer {
    /// Nothing picked and nothing typed: the question is left unanswered.
    pub fn is_skipped(&self) -> bool {
        self.selected.is_empty() && self.other.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum PtyInput {
    Bytes(Vec<u8>),
    Delay(u32),
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("unknown Claude ask '{0}'")]
    UnknownAsk(AskId),
    #[error("unverified keymap shape for {program:?}: {reason}")]
    UnverifiedShape {
        program: crate::pty::keymap::ProgramName,
        reason: String,
    },
    #[error("unsafe PTY input text: {reason}")]
    UnsafeText { reason: String },
    #[error("answer does not fit the ask: {detail}")]
    AnswerMismatchesAsk { detail: String },
    #[error("no keymap for Claude {version} can answer {program:?}")]
    NoKeymap {
        version: ClaudeVersion,
        program: crate::pty::keymap::ProgramName,
    },
    #[error("PTY input failed: {0}")]
    Pty(#[from] pty_host::PtyError),
}

pub fn paste_program(text: &str) -> Vec<PtyInput> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text: String = text
        .chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend_from_slice(text.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    vec![
        PtyInput::Bytes(paste),
        PtyInput::Delay(400),
        PtyInput::Bytes(b"\r".to_vec()),
    ]
}
