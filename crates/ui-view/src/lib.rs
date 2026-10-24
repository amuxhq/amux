//! Pure content values both amux clients compose. Every view is a function
//! of a [`ui_state::SessionState`] or [`ui_state::FleetState`] and the
//! client's explicit arguments (an order range, keys, an option struct, an
//! expansion set), never a size. Views carry typed facts; wording, geometry,
//! theme and animation belong to each renderer.
//!
//! Rows are items: the row id is the item key and never moves, and a run of
//! tool steps is an attribute on its members, so paging and streaming only
//! ever add ids at the two edges.

mod ask;
mod composer;
mod fleet;
mod form;
mod overview;
mod review;
mod rows;
mod run;
mod segments;
mod settings;
mod text;

pub use ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, OptionView, QuestionResponse,
    QuestionView, Scope, answer_input, ask_card, question_answer, reply_answer,
};
/// The character that marks an attachment's place in text.
pub use attachments::PLACEHOLDER;
pub use composer::{
    CONTEXT_NEAR_FULL_PERCENT, ComposerView, ContextView, Lands, QueuedRow, RefusedPrompt,
    SentPrompt, SignInView, Underway, composer, composer_tokens, context, prompts_underway,
    queue_rows, refused_prompts, sends_to_feed, sign_in, switched_away, waiting,
};
pub use fleet::{
    ActivityLine, AskSubject, AskSummary, ExitCause, FamilyHeader, FleetCard, FleetFilter,
    FleetOnly, FleetRow, FleetSection, FleetView, LoudMember, Offline, Reach, SecondLine,
    SectionKind, SessionLine, StuckReason, family_header, fleet_card, fleet_view, kind_name,
    matches, reach, session_line,
};
pub use form::{
    FieldProblem, FormField, FormFieldKind, FormProblem, FormValue, form_answer, form_fields,
    form_problems,
};
pub use overview::{
    ChangeTotals, ChangedFile, Changes, Comparison, Folder, JobRow, Overview, ServerView, TaskLine,
    TaskMark, TasksView, UsageLabel, UsageView, UsageWindowView, changes, diff_base, overview,
    usage_windows,
};
pub use review::{DiffLine, FileStatus, Hunk, LineKind, ReviewDoc, ReviewFile, review_doc};
pub use rows::{
    AnswerView, AskRow, CallPhase, ChatOptions, ChildAction, Decision, DecisionView, EditLine,
    ExploreVerb, FileChangeView, FileRow, Granted, OUTPUT_HEAD_LINES, PatchHead, PatchLine,
    PermissionGrant, PlanVerdict, Resolution, Row, RowKind, ToolRows, chat_rows, chat_rows_for,
    patch_head,
};
pub use run::{LIVE_STEPS, Run, RunCounts, keep_open_runs, run_is_open, toggle_run};
pub use segments::{AttachmentView, Segment, segments};
pub use settings::{
    Changeability, Changeable, ClientCommand, CommandView, ControlsSummary, EffortChoice,
    ModeChoice, ModelChoice, NewAgentChoices, NewAgentPick, PermissionChoice, SettingChange,
    SettingsView, Slash, controls, new_agent_pick, new_agent_settings, setting_input, settings,
    slash, typed_setting,
};
/// Why an input was refused, which every client words.
pub use ui_state::{Refusal, RefusalReason};
/// What a host offers an agent to pick from, which a new agent's settings
/// are built from.
pub use wire::Catalogue;
/// The rule a host holds every agent name to, for a client's name field.
pub use wire::{AgentNameProblem, agent_name_problem};
