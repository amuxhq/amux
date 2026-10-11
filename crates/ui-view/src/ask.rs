//! The ask card: one anatomy for every kind. The head ask with its count,
//! the subject verbatim, the body variant, and choices stated as outcomes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui_state::{InputState, OpenAsk, Refusal, SessionState};
use wire::{ClaudeAnswer, CodexAnswer, Decision as CodexDecision, ask, claude_answer, codex_ask};

use crate::form::{FormField, form_fields};
use crate::rows::{EditLine, change_lines, counts, first_change, plan_parts, replaced_lines};

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct AskCard {
    pub kind: wire::Kind,
    /// What an answer names.
    pub key: String,
    /// The item the ask points at: the call it would let run, or the row of
    /// an ask that is the work.
    pub item_key: String,
    /// "1 of 3" when asks are queued.
    pub position: usize,
    pub count: usize,
    pub body: AskBody,
    /// The likely choice first. Only what this agent offers.
    pub choices: Vec<Choice>,
    /// A note may go out with each question's answer. Terminal Claude has
    /// no place to type one, so it takes none.
    pub question_note: bool,
    /// A question may be left unanswered. Terminal Claude's form takes one
    /// only when it asks several questions: it then ends in Claude's review
    /// screen, which submits with some unanswered, while a lone question is
    /// submitted by answering it.
    pub question_skip: bool,
    /// The person may reply in their own words instead of answering the
    /// questions. Not for the provider's own dialogs, outside any turn.
    pub question_reply: bool,
    /// Stopping interrupts a running turn. Codex proposes a plan as its
    /// turn ends, so with its plan open there is no turn to stop.
    pub stops_turn: bool,
    pub state: CardState,
}

/// Where the card is after the person acts. Stop is in the menu while a
/// turn runs: it is the interrupt, and the agent stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum CardState {
    Open,
    /// Shrunk to one line until the agent confirms.
    Sending,
    /// The card is back, with why the answer was refused.
    Rejected(Refusal),
    /// The connection dropped before a reply: resend or discard.
    NotConfirmed,
    /// The agent exited with the ask open.
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum AskBody {
    Command {
        command: String,
        cwd: String,
        reason: String,
        description: String,
    },
    /// The patch's lines, and the new file's line its first change lands
    /// on when the patch says (a Claude edit's does not: it is only the
    /// text replaced and the text replacing it).
    Edit {
        path: String,
        files: u32,
        added: u32,
        removed: u32,
        lines: Vec<EditLine>,
        line: Option<u32>,
        reason: String,
        /// Whether the file is new: true "Wants to create", false "Wants
        /// to edit". None for a write that may do either: Claude says
        /// which only once it is done ("Wants to write").
        created: Option<bool>,
    },
    Tool {
        server: String,
        tool: String,
        arguments: String,
    },
    Question(Vec<QuestionView>),
    /// The plan's title and body, as its row has them.
    Plan {
        title: Option<String>,
        body: String,
    },
    Form {
        server: String,
        message: String,
        /// The schema's fields, in the order the server wrote them.
        fields: Vec<FormField>,
    },
    Link {
        server: String,
        message: String,
        url: String,
    },
    Access {
        reason: String,
        read: Vec<String>,
        write: Vec<String>,
        network: bool,
        hosts: Vec<String>,
    },
    /// The provider asks something this build cannot read: the only ways out
    /// are Stop and attaching a terminal.
    Unanswerable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct QuestionView {
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<OptionView>,
    /// "Something else…" is always last when offered.
    pub allow_other: bool,
    /// Typed answers are hidden, and the row later says "answered (hidden)".
    pub secret: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OptionView {
    /// "(Recommended)" lifted out into `recommended`.
    pub label: String,
    pub description: String,
    pub preview: String,
    pub recommended: bool,
}

/// One choice, stated as what happens, with the answer it sends.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Choice {
    pub outcome: ChoiceOutcome,
    pub primary: bool,
    /// The person may add a note that goes back to the agent.
    pub takes_note: bool,
    #[serde(skip)]
    pub answer: Answer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ChoiceOutcome {
    AllowOnce,
    /// "Always allow cargo test in this project": what, and for how long.
    AllowAlways {
        subjects: Vec<String>,
        directories: Vec<String>,
        /// The permission it switches to, by its value; empty when none.
        mode: String,
        /// That permission by the catalogue's name; empty when the
        /// catalogue does not name it.
        mode_name: String,
        scope: Scope,
    },
    AllowForSession,
    /// Codex: the exact command prefix it would allow.
    AllowSimilar {
        prefix: Vec<String>,
    },
    AllowNetwork {
        hosts: Vec<String>,
    },
    /// Deny and let it carry on; `stops` when denying always ends the turn.
    Deny {
        stops: bool,
    },
    DenyAndStop,
    /// Approve the plan; with a `mode`, switching to that permission, which
    /// `mode_name` names as the agent's catalogue does.
    ApprovePlan {
        mode: String,
        mode_name: String,
    },
    SendBack,
    Submit,
    Decline,
    OpenLink,
    GrantForTurn,
    GrantForSession,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Scope {
    Session,
    /// This project, for this person.
    Project,
    /// This project, shared with everyone who works on it.
    ProjectShared,
    /// Every project.
    User,
    Other(String),
}

/// The answer body a choice sends; [`answer_input`] puts it in an input.
/// It stays on this side of the phone bridge: the phone names a choice by
/// its position and the answer is rebuilt here. Its ask key is filled in
/// from the card when it is sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Claude(ClaudeAnswer),
    Codex(CodexAnswer),
}

/// The head ask, or None when nothing is open or, before CaughtUp, when the
/// entry does not say needs_you.
pub fn ask_card(state: &SessionState) -> Option<AskCard> {
    let asks = state.open_asks();
    let head = asks.first()?;
    let (mut body, choices) = match head {
        OpenAsk::Claude(ask) => claude(state, ask, &state.agent_state().permissions),
        OpenAsk::Codex(ask) => codex(state, ask),
    };
    if let AskBody::Edit { path, .. } = &mut body {
        *path = within(&state.agent().cwd, path);
    }
    let card_state = if state.asks_dismissed() {
        CardState::Dismissed
    } else {
        match state.answering(head.key()).map(|sent| &sent.state) {
            Some(InputState::Sent | InputState::Settled | InputState::Queued) => CardState::Sending,
            Some(InputState::Rejected(reason)) => CardState::Rejected(reason.clone()),
            Some(InputState::Uncertain) => CardState::NotConfirmed,
            None => CardState::Open,
        }
    };
    Some(AskCard {
        kind: state.kind(),
        key: head.key().to_owned(),
        item_key: head.item_key().to_owned(),
        position: 1,
        count: asks.len(),
        body,
        choices,
        question_note: state.kind() != wire::Kind::ClaudePty,
        question_skip: state.kind() != wire::Kind::ClaudePty || reviewed_form(head),
        question_reply: !provider_dialog(head),
        stops_turn: !matches!(
            head,
            OpenAsk::Codex(wire::CodexAsk {
                body: Some(codex_ask::Body::Plan(_)),
                ..
            })
        ),
        state: card_state,
    })
}

/// `path` relative to the agent's folder `cwd` when the file is inside it,
/// else whole. A file the agent edits is nearly always in its own folder,
/// and the absolute path repeats that folder at the front, where a narrow
/// screen has to cut it to show the file's name. Paths are the host's, so
/// either separator ends the folder.
fn within(cwd: &str, path: &str) -> String {
    let folder = cwd.trim_end_matches(['/', '\\']);
    if folder.is_empty() {
        return path.to_owned();
    }
    match path.strip_prefix(folder) {
        Some(rest) if rest.starts_with(['/', '\\']) && rest.len() > 1 => rest[1..].to_owned(),
        _ => path.to_owned(),
    }
}

/// Whether terminal Claude's form for the ask ends in its review screen: a
/// question ask of more than one question.
fn reviewed_form(ask: &OpenAsk) -> bool {
    matches!(
        ask,
        OpenAsk::Claude(wire::Ask {
            body: Some(ask::Body::Question(question)),
            ..
        }) if question.questions.len() > 1
    )
}

/// Whether the ask is a question the provider asks itself, outside a turn.
fn provider_dialog(ask: &OpenAsk) -> bool {
    match ask {
        OpenAsk::Claude(wire::Ask {
            body: Some(ask::Body::Question(question)),
            ..
        })
        | OpenAsk::Codex(wire::CodexAsk {
            body: Some(codex_ask::Body::Question(question)),
            ..
        }) => question.provider_dialog,
        _ => false,
    }
}

fn choice(outcome: ChoiceOutcome, answer: Answer) -> Choice {
    // Only Claude's deny and plan send-back carry a note back to the agent.
    let takes_note = match &answer {
        Answer::Claude(ClaudeAnswer { of: Some(of), .. }) => match of {
            claude_answer::Of::Permission(permission) => {
                matches!(permission.of, Some(wire::permission_answer::Of::Deny(_)))
            }
            claude_answer::Of::Plan(plan) => plan.choice() == wire::PlanChoice::KeepPlanning,
            _ => false,
        },
        Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Plan(plan)),
            ..
        }) => plan.choice() == wire::PlanChoice::KeepPlanning,
        _ => false,
    };
    Choice {
        outcome,
        primary: false,
        takes_note,
        answer,
    }
}

fn claude_answer(of: claude_answer::Of) -> Answer {
    Answer::Claude(ClaudeAnswer {
        of: Some(of),
        ..Default::default()
    })
}

fn permission(of: wire::permission_answer::Of) -> Answer {
    claude_answer(claude_answer::Of::Permission(wire::PermissionAnswer {
        of: Some(of),
    }))
}

fn claude(
    state: &SessionState,
    ask: &wire::Ask,
    offered: &[wire::OfferedPermission],
) -> (AskBody, Vec<Choice>) {
    use wire::permission_answer::Of as P;
    let (body, mut choices) = match &ask.body {
        Some(ask::Body::Permission(p)) => {
            let body = permission_body(p);
            let mut choices = vec![choice(
                ChoiceOutcome::AllowOnce,
                permission(P::Allow(wire::PermissionAllow { scope: None })),
            )];
            for scope in &p.scopes {
                let grant = scope.grant.clone().unwrap_or_default();
                let mode = grant.mode.clone().unwrap_or_default();
                choices.push(choice(
                    ChoiceOutcome::AllowAlways {
                        subjects: grant.rules.iter().map(|rule| lift_rule(rule)).collect(),
                        directories: grant.directories.clone(),
                        mode_name: offered
                            .iter()
                            .find(|permission| permission.value == mode)
                            .map(|permission| permission.display_name.clone())
                            .unwrap_or_default(),
                        mode,
                        scope: scope_of(&grant),
                    },
                    permission(P::Allow(wire::PermissionAllow {
                        scope: Some(scope.index),
                    })),
                ));
            }
            let deny = |stop| {
                permission(P::Deny(wire::PermissionDeny {
                    note: String::new(),
                    stop,
                }))
            };
            if p.deny_stops {
                choices.push(choice(ChoiceOutcome::Deny { stops: true }, deny(true)));
            } else {
                choices.push(choice(ChoiceOutcome::Deny { stops: false }, deny(false)));
                if p.deny_can_stop {
                    choices.push(choice(ChoiceOutcome::DenyAndStop, deny(true)));
                }
            }
            (body, choices)
        }
        Some(ask::Body::Question(q)) => (
            AskBody::Question(q.questions.iter().map(question).collect()),
            vec![],
        ),
        Some(ask::Body::Plan(plan)) => plan_card(state, &ask.item_key, plan, |plan| {
            claude_answer(claude_answer::Of::Plan(plan))
        }),
        Some(ask::Body::Form(form)) => {
            let action = |action: wire::FormAction| {
                claude_answer(claude_answer::Of::Form(wire::FormAnswer {
                    action: action as i32,
                    content_json: vec![],
                }))
            };
            (
                form_body(form),
                vec![
                    choice(ChoiceOutcome::Submit, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(ask::Body::Link(link)) => {
            let action = |action: wire::FormAction| {
                claude_answer(claude_answer::Of::Link(wire::LinkAnswer {
                    action: action as i32,
                }))
            };
            (
                link_body(link),
                vec![
                    choice(ChoiceOutcome::OpenLink, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(ask::Body::Unanswerable(u)) => (
            AskBody::Unanswerable {
                reason: u.reason.clone(),
            },
            vec![],
        ),
        None => (
            AskBody::Unanswerable {
                reason: String::new(),
            },
            vec![],
        ),
    };
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    (body, choices)
}

fn permission_body(p: &wire::PermissionAsk) -> AskBody {
    let input: Value = serde_json::from_slice(&p.input_json).unwrap_or(Value::Null);
    let text = |name: &str| {
        input
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if !p.server.is_empty() {
        return AskBody::Tool {
            server: p.server.clone(),
            tool: p.tool_name.clone(),
            arguments: pretty(&input),
        };
    }
    match p.tool_name.as_str() {
        "Bash" => AskBody::Command {
            command: text("command"),
            cwd: String::new(),
            reason: p.reason.clone(),
            description: if p.description.is_empty() {
                text("description")
            } else {
                p.description.clone()
            },
        },
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let path = [text("file_path"), text("notebook_path")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            let lines = replaced_lines(&input);
            let (added, removed) = counts(&lines);
            AskBody::Edit {
                path,
                files: 1,
                added,
                removed,
                line: first_change(&lines),
                lines,
                reason: p.reason.clone(),
                created: (p.tool_name != "Write").then_some(false),
            }
        }
        _ => AskBody::Tool {
            server: String::new(),
            tool: p.tool_name.clone(),
            arguments: pretty(&input),
        },
    }
}

/// A tool called with no arguments shows no arguments block: null, `{}`
/// and `[]` all read as nothing.
fn pretty(value: &Value) -> String {
    let empty = match value {
        Value::Null => true,
        Value::Object(fields) => fields.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    };
    if empty {
        String::new()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }
}

fn form_body(form: &wire::FormAsk) -> AskBody {
    AskBody::Form {
        server: form.server.clone(),
        message: form.message.clone(),
        fields: form_fields(&form.schema_json),
    }
}

fn link_body(link: &wire::LinkAsk) -> AskBody {
    AskBody::Link {
        server: link.server.clone(),
        message: link.message.clone(),
        url: link.url.clone(),
    }
}

/// "Bash(cargo test:*)" reads "cargo test"; a bare tool name stays.
pub(crate) fn lift_rule(rule: &str) -> String {
    let Some((_, inner)) = rule.split_once('(') else {
        return rule.to_owned();
    };
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    let inner = inner
        .strip_suffix(":*")
        .or_else(|| inner.strip_suffix(" *"))
        .unwrap_or(inner);
    let inner = inner.strip_prefix("domain:").unwrap_or(inner);
    inner.to_owned()
}

/// Where a grant is saved, as the person reads it; a destination this
/// build does not know is shown in Claude's own word.
pub(crate) fn scope_of(grant: &wire::ClaudeGrant) -> Scope {
    match grant.destination() {
        wire::ScopeDestination::Session => Scope::Session,
        wire::ScopeDestination::LocalSettings => Scope::Project,
        wire::ScopeDestination::ProjectSettings => Scope::ProjectShared,
        wire::ScopeDestination::UserSettings => Scope::User,
        wire::ScopeDestination::Unspecified => Scope::Other(grant.destination_word.clone()),
    }
}

pub(crate) fn question(q: &wire::Question) -> QuestionView {
    QuestionView {
        header: q.header.clone(),
        question: q.question.clone(),
        multi_select: q.multi_select,
        options: q
            .options
            .iter()
            .map(|option| {
                lifted(
                    &option.label,
                    &option.description,
                    &option.preview,
                    option.recommended,
                )
            })
            .collect(),
        allow_other: q.allow_other,
        secret: q.secret,
    }
}

pub(crate) fn lifted(
    label: &str,
    description: &str,
    preview: &str,
    recommended: bool,
) -> OptionView {
    let trimmed = label.trim_end();
    let (label, tagged) = match trimmed.strip_suffix("(Recommended)") {
        Some(rest) => (rest.trim_end().to_owned(), true),
        None => (trimmed.to_owned(), false),
    };
    OptionView {
        label,
        description: description.to_owned(),
        preview: preview.to_owned(),
        recommended: recommended || tagged,
    }
}

/// A plan's decision: the plan is the text of the item the ask points at,
/// and the choices are the ones the ask lists.
fn plan_card(
    state: &SessionState,
    item_key: &str,
    plan: &wire::PlanAsk,
    answer: impl Fn(wire::PlanAnswer) -> Answer,
) -> (AskBody, Vec<Choice>) {
    let choices = plan
        .choices()
        .filter_map(|offered| {
            let (outcome, permission) = match offered {
                wire::PlanChoice::Start => (
                    ChoiceOutcome::ApprovePlan {
                        mode: String::new(),
                        mode_name: String::new(),
                    },
                    String::new(),
                ),
                wire::PlanChoice::StartSwitching if !plan.switch_permission.is_empty() => {
                    let mode = plan.switch_permission.clone();
                    (
                        ChoiceOutcome::ApprovePlan {
                            mode_name: crate::rows::permission_name(
                                &state.agent_state().permissions,
                                &mode,
                            ),
                            mode: mode.clone(),
                        },
                        mode,
                    )
                }
                wire::PlanChoice::KeepPlanning => (ChoiceOutcome::SendBack, String::new()),
                wire::PlanChoice::StartSwitching | wire::PlanChoice::Unspecified => return None,
            };
            let answer = answer(wire::PlanAnswer {
                choice: offered as i32,
                note: None,
                permission,
            });
            Some(choice(outcome, answer))
        })
        .collect();
    let (title, body) = state
        .transcript()
        .get(item_key)
        .map(|held| {
            let writing = matches!(
                &held.body,
                ui_state::ItemBody::ClaudePty(wire::claude_pty_item::Kind::Plan(plan))
                | ui_state::ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Plan(plan))
                | ui_state::ItemBody::Codex(wire::codex_item::Kind::Plan(plan))
                    if !plan.complete
            );
            plan_parts(&held.item.text, writing)
        })
        .unwrap_or_default();
    (AskBody::Plan { title, body }, choices)
}

fn codex(state: &SessionState, ask: &wire::CodexAsk) -> (AskBody, Vec<Choice>) {
    let decisions: Vec<CodexDecision> = ask
        .decisions
        .iter()
        .filter_map(|d| CodexDecision::try_from(*d).ok())
        .collect();
    let decision_choices = |similar: Vec<String>, hosts: Vec<String>| {
        decisions
            .iter()
            .filter_map(|decision| {
                let outcome = match decision {
                    CodexDecision::Approve => ChoiceOutcome::AllowOnce,
                    CodexDecision::ApproveSession => ChoiceOutcome::AllowForSession,
                    CodexDecision::ApproveSimilar => ChoiceOutcome::AllowSimilar {
                        prefix: similar.clone(),
                    },
                    CodexDecision::ApproveNetwork => ChoiceOutcome::AllowNetwork {
                        hosts: hosts.clone(),
                    },
                    CodexDecision::Deny => ChoiceOutcome::Deny { stops: false },
                    CodexDecision::Abort => ChoiceOutcome::DenyAndStop,
                    CodexDecision::Unspecified => return None,
                };
                Some(choice(
                    outcome,
                    Answer::Codex(CodexAnswer {
                        of: Some(wire::codex_answer::Of::Approve(*decision as i32)),
                        ..Default::default()
                    }),
                ))
            })
            .collect::<Vec<_>>()
    };
    let codex_answer = |of: wire::codex_answer::Of| {
        Answer::Codex(CodexAnswer {
            of: Some(of),
            ..Default::default()
        })
    };
    let (body, mut choices) = match &ask.body {
        Some(codex_ask::Body::Plan(plan)) => plan_card(state, &ask.item_key, plan, |plan| {
            codex_answer(wire::codex_answer::Of::Plan(plan))
        }),
        Some(codex_ask::Body::Command(c)) => (
            AskBody::Command {
                command: c.command.clone(),
                cwd: c.cwd.clone(),
                reason: c.reason.clone(),
                description: String::new(),
            },
            decision_choices(c.allow_prefix.clone(), c.network_hosts.clone()),
        ),
        Some(codex_ask::Body::FileChange(f)) => {
            let lines: Vec<EditLine> = f.changes.iter().flat_map(change_lines).collect();
            let (added, removed) = counts(&lines);
            (
                AskBody::Edit {
                    path: f
                        .changes
                        .first()
                        .map(|change| change.path.clone())
                        .unwrap_or_else(|| f.grant_root.clone()),
                    files: f.changes.len() as u32,
                    added,
                    removed,
                    line: first_change(&lines),
                    lines,
                    reason: f.reason.clone(),
                    created: Some(
                        f.changes.len() == 1 && f.changes[0].kind() == wire::FileChangeKind::Add,
                    ),
                },
                decision_choices(vec![], vec![]),
            )
        }
        Some(codex_ask::Body::McpTool(t)) => (
            AskBody::Tool {
                server: t.server.clone(),
                tool: t.tool.clone(),
                arguments: pretty(
                    &serde_json::from_slice(&t.arguments_json).unwrap_or(Value::Null),
                ),
            },
            decision_choices(vec![], vec![]),
        ),
        Some(codex_ask::Body::McpForm(form)) => {
            let action = |action: wire::FormAction| {
                codex_answer(wire::codex_answer::Of::Form(wire::FormAnswer {
                    action: action as i32,
                    content_json: vec![],
                }))
            };
            (
                form_body(form),
                vec![
                    choice(ChoiceOutcome::Submit, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(codex_ask::Body::McpLink(link)) => {
            let action = |action: wire::FormAction| {
                codex_answer(wire::codex_answer::Of::Link(wire::LinkAnswer {
                    action: action as i32,
                }))
            };
            (
                link_body(link),
                vec![
                    choice(ChoiceOutcome::OpenLink, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(codex_ask::Body::Access(grant)) => {
            let answer = |for_session: bool, all: bool| {
                codex_answer(wire::codex_answer::Of::Grant(wire::GrantAnswer {
                    read: if all { grant.read.clone() } else { vec![] },
                    write: if all { grant.write.clone() } else { vec![] },
                    network: all && grant.network,
                    for_session,
                }))
            };
            (
                AskBody::Access {
                    reason: grant.reason.clone(),
                    read: grant.read.clone(),
                    write: grant.write.clone(),
                    network: grant.network,
                    hosts: grant.network_hosts.clone(),
                },
                vec![
                    choice(ChoiceOutcome::GrantForTurn, answer(false, true)),
                    choice(ChoiceOutcome::GrantForSession, answer(true, true)),
                    choice(ChoiceOutcome::Deny { stops: false }, answer(false, false)),
                ],
            )
        }
        Some(codex_ask::Body::Question(q)) => (
            AskBody::Question(q.questions.iter().map(question).collect()),
            vec![],
        ),
        None => (
            AskBody::Unanswerable {
                reason: String::new(),
            },
            vec![],
        ),
    };
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    (body, choices)
}

/// One question's answer as the person gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Pick {
    /// The chosen options; none, with no text, skips the question.
    Options(Vec<u32>),
    Other(String),
}

/// One question's pick and the person's note on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionResponse {
    pub pick: Pick,
    /// Empty when there is none.
    #[serde(default)]
    pub note: String,
}

impl QuestionResponse {
    /// The question left unanswered.
    pub fn skip() -> QuestionResponse {
        QuestionResponse {
            pick: Pick::Options(Vec::new()),
            note: String::new(),
        }
    }
}

fn responses(given: &[QuestionResponse]) -> Vec<wire::QuestionResponse> {
    given
        .iter()
        .map(|response| {
            let (selected, other) = match &response.pick {
                Pick::Options(selected) => (selected.clone(), None),
                Pick::Other(text) => (vec![], Some(text.clone())),
            };
            wire::QuestionResponse {
                selected,
                other,
                note: (!response.note.is_empty()).then(|| response.note.clone()),
            }
        })
        .collect()
}

/// The answer to a question ask: one response per question, in order.
pub fn question_answer(card: &AskCard, given: &[QuestionResponse]) -> Answer {
    let answers = wire::QuestionAnswer {
        answers: responses(given),
    };
    match card.kind {
        wire::Kind::Codex => Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Question(answers)),
            ..Default::default()
        }),
        _ => Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Question(answers)),
            ..Default::default()
        }),
    }
}

/// The person's own words instead of answering a question ask, with what
/// they had answered so far: one response per question, a skip where
/// nothing was.
pub fn reply_answer(card: &AskCard, text: &str, so_far: &[QuestionResponse]) -> Answer {
    let reply = wire::ReplyInstead {
        text: text.to_owned(),
        answers_so_far: responses(so_far),
    };
    match card.kind {
        wire::Kind::Codex => Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Reply(reply)),
            ..Default::default()
        }),
        _ => Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Reply(reply)),
            ..Default::default()
        }),
    }
}

/// The input that sends `answer` to the card's ask, in the card's kind's
/// arm, under the ask's key. `note` goes back to the agent with a choice
/// that takes one; other answers ignore it. The input has no id: the
/// session gives it a fresh one when it sends it. None when the kind does
/// not take this answer.
pub fn answer_input(card: &AskCard, answer: &Answer, note: &str) -> Option<wire::Input> {
    use wire::{KindInput, claude_pty_input, claude_sdk_input, codex_input};
    let input = match (card.kind, with_note(answer, note)) {
        (wire::Kind::ClaudePty, Answer::Claude(answer)) => {
            KindInput::ClaudePty(wire::ClaudePtyInput {
                of: Some(claude_pty_input::Of::Answer(ClaudeAnswer {
                    ask_key: card.key.clone(),
                    ..answer
                })),
            })
        }
        (wire::Kind::ClaudeSdk, Answer::Claude(answer)) => {
            KindInput::ClaudeSdk(wire::ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Answer(ClaudeAnswer {
                    ask_key: card.key.clone(),
                    ..answer
                })),
            })
        }
        (wire::Kind::Codex, Answer::Codex(answer)) => KindInput::Codex(wire::CodexInput {
            of: Some(codex_input::Of::Answer(CodexAnswer {
                ask_key: card.key.clone(),
                ..answer
            })),
        }),
        _ => return None,
    };
    Some(wire::Input::new(Vec::new(), input))
}

/// The answer with the person's note in the place its kind carries one.
fn with_note(answer: &Answer, note: &str) -> Answer {
    let mut answer = answer.clone();
    if note.is_empty() {
        return answer;
    }
    match &mut answer {
        Answer::Claude(ClaudeAnswer { of: Some(of), .. }) => match of {
            claude_answer::Of::Permission(wire::PermissionAnswer {
                of: Some(wire::permission_answer::Of::Deny(deny)),
            }) => deny.note = note.to_owned(),
            claude_answer::Of::Plan(plan) if plan.choice() == wire::PlanChoice::KeepPlanning => {
                plan.note = Some(note.to_owned())
            }
            _ => {}
        },
        Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Plan(plan)),
            ..
        }) if plan.choice() == wire::PlanChoice::KeepPlanning => plan.note = Some(note.to_owned()),
        _ => {}
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_called_with_no_arguments_shows_no_arguments_block() {
        assert_eq!(pretty(&Value::Null), "");
        assert_eq!(pretty(&serde_json::json!({})), "");
        assert_eq!(pretty(&serde_json::json!([])), "");
        assert_eq!(
            pretty(&serde_json::json!({"path": "a.txt"})),
            "{\n  \"path\": \"a.txt\"\n}"
        );
    }
}
