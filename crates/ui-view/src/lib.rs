//! Pure content values both amux clients compose. Every view is a function
//! of a [`ui_state::SessionState`] or [`ui_state::FleetState`] and the
//! client's explicit arguments (an order range, keys, an option struct, an
//! expansion set), never a size. Views carry typed facts; wording, geometry,
//! theme and animation belong to each renderer.
//!
//! Rows are items: the row id is the item key and never moves, and a run of
//! tool steps is an attribute on its members, as is a group of calls that
//! opened agents, so paging and streaming only ever add ids at the two
//! edges.

mod ask;
mod changed_words;
mod code;
mod composer;
mod fleet;
mod form;
mod onboarding;
mod overview;
mod repository;
mod review;
mod review_page;
mod rows;
mod run;
mod segments;
mod settings;
mod subagents;
mod text;
mod turn;

pub use ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, OptionView, QuestionResponse,
    QuestionView, Scope, answer_input, ask_card, question_answer, reply_answer,
};
/// The character that marks an attachment's place in text.
pub use attachments::PLACEHOLDER;
pub use attachments::sent_review;
pub use changed_words::*;
pub use code::{CodeRun, HighlightedCode, TokenKind, highlight_block, highlight_patch};
pub use composer::{
    CONTEXT_NEAR_FULL_PERCENT, CommandMatch, ComposerView, ContextView, Lands, PASTE_CHARS,
    PASTE_LINES, QueuedRow, RefusedPrompt, SentPrompt, SignInView, Underway, command_matches,
    composer, context, pastes_as_attachment, prompts_underway, queue_rows, refused_prompts,
    sign_in, switched_away,
};
pub use fleet::{
    ActivityLine, ExitCause, FamilyHeader, FleetCard, FleetFilter, FleetOnly, FleetRow,
    FleetSection, FleetView, LoudMember, Offline, Reach, SecondLine, SectionKind, SessionLine,
    family_header, fleet_card, fleet_view, kind_name, matches, reach, session_line,
};
pub use form::{
    FieldProblem, FormField, FormFieldKind, FormProblem, FormValue, form_answer, form_fields,
    form_problems,
};
/// What the code functions highlight with: a client passes the one the
/// process shares, [`Highlighter::shared`], and a test one built on
/// [`HeldJobs`], whose background work it runs when it chooses.
pub use highlight::{HeldJobs, Highlighter};
pub use onboarding::{OnboardingView, SetupStep, next_step, onboarding_view, step_open};
pub use overview::{
    ChangeTotals, ChangedFile, Changes, Comparison, FileStatus, Folder, JobRow, JobStop, Overview,
    ServerView, TaskLine, TaskMark, TasksView, UsageLabel, UsageView, UsageWindowView, changes,
    lists_changes, overview, usage_windows,
};
pub use repository::{REPOSITORY_TINTS, Repository};
pub use review::{DiffLine, Hunk, LineKind, ReviewDoc, ReviewFile, review_base, review_doc};
pub use review_page::*;
pub use rows::{
    AnswerView, AskRow, CONTINUED_WORDS, CallPhase, ChildAction, Decision, DecisionView,
    ExploreVerb, FileChangeView, FilePatch, FileRow, Granted, HunkStart, OUTPUT_HEAD_LINES,
    PatchHead, PatchHunk, PatchLine, PermissionGrant, PlanVerdict, Resolution, Row, RowKind,
    ToolRows, chat_rows_for, patch_head,
};
pub use run::{Run, RunCounts, keep_open_runs, run_is_open, toggle_run};
pub use segments::{AttachmentView, Segment, SentReviewRef, THUMBNAIL_SIZE_LIMIT, segments};
pub use settings::{
    Changeability, Changeable, ClientCommand, CommandView, ControlsSummary, EffortChoice,
    ModeChoice, ModelChoice, NewAgentChoices, NewAgentPick, PermissionChoice, SettingChange,
    SettingsView, Slash, controls, new_agent_pick, new_agent_settings, setting_input, settings,
    slash, typed_setting,
};
pub use subagents::{
    ChatAgents, GroupState, GroupView, OpenedAgent, SubagentGroup, chat_agents, group_state,
    opened_agent, subagent_rows_moved, toggle_group,
};
pub use turn::{TurnPart, TurnPlace, Worked, turn_keys, turn_rows_moved};
/// Why an input was refused, which every client words.
pub use ui_state::Refusal;
/// The most an attachment may hold, which every client checks before it
/// reads or uploads one.
pub use wire::ATTACHMENT_SIZE_LIMIT;
/// What a host offers an agent to pick from, which a new agent's settings
/// are built from.
pub use wire::Catalogue;
/// The rule a host holds every agent name to, for a client's name field.
pub use wire::{AgentNameProblem, agent_name_problem};
