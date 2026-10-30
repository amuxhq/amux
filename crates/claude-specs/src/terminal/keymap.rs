//! The data that parameterizes Claude Code PTY input programs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{AskAnswer, AskKind, InputError, Intent, PermissionAnswer, PlanAnswer, QuestionAnswer};

const BAKED: &str = include_str!("../../keymaps/claude-2.1.toml");

/// The keymap the PTY driver types Claude's input with.
pub fn baked() -> Keymap {
    load_str(BAKED, "keymaps/claude-2.1.toml").expect("the baked keymap is valid")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub keys: BTreeMap<KeyName, Vec<u8>>,
    pub delays: BTreeMap<DelayName, u32>,
    pub menus: BTreeMap<MenuName, MenuLayout>,
    pub verified_shapes: BTreeMap<ProgramName, ShapeSet>,
    pub programs: BTreeMap<ProgramName, Program>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuLayout {
    pub entries: BTreeMap<MenuEntryName, u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSet {
    /// Suggestion counts whose whole permission menu is verified: one
    /// scoped entry per suggestion between allow-once and deny.
    #[serde(default)]
    pub permission_suggestions: Vec<usize>,
    /// Suggestion counts whose menu is verified to fold every suggestion
    /// into one scoped entry right after allow-once: its only scoped answer
    /// is suggestion 0, and choosing it applies them all. Claude 2.1.283
    /// shows a directory and a mode suggestion this way.
    #[serde(default)]
    pub permission_folded_suggestions: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Key {
        key: KeyName,
    },
    Paste {
        text: TextSource,
        /// Long text goes as several pastes, each short enough that Claude
        /// keeps it in the prompt rather than collapsing it into pasted
        /// content the model treats as quoted material.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pieces: Option<PastePieces>,
    },
    Type {
        text: TextSource,
    },
    Digit {
        digit: DigitSource,
    },
    Delay {
        delay: DelayName,
    },
    Repeat {
        count: CountSource,
        steps: Vec<Step>,
    },
    ForEach {
        over: Iter,
        steps: Vec<Step>,
    },
    If {
        cond: Cond,
        #[serde(rename = "then")]
        then_steps: Vec<Step>,
        #[serde(default)]
        otherwise: Vec<Step>,
    },
    MoveTo {
        row: RowSource,
    },
    Call {
        program: ProgramName,
    },
}

/// How a long paste is split: at most `chars` characters per paste, with
/// the `between` pause after each so Claude reads them as separate pastes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PastePieces {
    pub chars: usize,
    pub between: DelayName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DigitSource {
    MenuEntry {
        menu: MenuName,
        entry: MenuEntryName,
    },
    SelectedOption,
    OtherRow,
    Suggestion,
    /// The entry after the scope entries: "Yes, and switch to auto mode".
    AutoSwitch,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

string_enum!(KeyName {
    Escape,
    Enter,
    Tab,
    Space,
    Down,
    ShiftTab,
    PasteBegin,
    PasteEnd,
});

string_enum!(DelayName {
    AfterPaste,
    BetweenPastes,
    AfterToggle,
    AfterMove,
    AfterTab,
    AfterEditorOpen,
    BeforeSubmit,
    AfterReviewOpen,
    AfterDeny,
    BeforePlanApproval,
    AfterMenuTextOpen,
    AfterOtherSave,
});

string_enum!(MenuName { Permission, Plan });

string_enum!(MenuEntryName {
    AllowOnce,
    AllowScoped,
    ApproveAuto,
    ApproveManual,
    RequestChanges,
});

string_enum!(ProgramName {
    Prompt,
    SendNow,
    Interrupt,
    ModeCycle,
    PermissionMenu,
    PlanMenu,
    QuestionForm,
});

impl ProgramName {
    /// What the program does, as a refusal names it to a person.
    pub fn doing(self) -> &'static str {
        match self {
            Self::Prompt => "Typing a prompt",
            Self::SendNow => "Send now",
            Self::Interrupt => "Interrupting",
            Self::ModeCycle => "Changing the permission mode",
            Self::PermissionMenu => "Answering a permission menu",
            Self::PlanMenu => "Answering a plan review",
            Self::QuestionForm => "Answering a question",
        }
    }
}

string_enum!(TextSource {
    PromptText,
    DenyFeedback,
    PlanFeedback,
    OtherText,
});

string_enum!(CountSource {
    Questions,
    SelectedOptions,
});

string_enum!(Iter {
    Questions,
    SelectedOptions,
});

string_enum!(Cond {
    Denies,
    SendsBack,
    HasFeedback,
    HasOther,
    MultiSelect,
    IsFirst,
    LastQuestionMulti,
    SingleQuestionSingleSelect,
    HasPreviews,
    Skipped,
});

string_enum!(RowSource {
    SelectedOption,
    OtherRow,
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStep {
    Write(Vec<u8>),
    Delay(Duration),
}

pub struct Environment<'a> {
    pub ask: Option<&'a AskKind>,
    pub answer: Option<&'a AskAnswer>,
    pub prompt: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IntentName {
    Prompt,
    SendNow,
    Interrupt,
    ModeCycle,
    Permission,
    Plan,
    Question,
}

const PROGRAM_TABLE: &[(IntentName, ProgramName)] = &[
    (IntentName::Prompt, ProgramName::Prompt),
    (IntentName::SendNow, ProgramName::SendNow),
    (IntentName::Interrupt, ProgramName::Interrupt),
    (IntentName::ModeCycle, ProgramName::ModeCycle),
    (IntentName::Permission, ProgramName::PermissionMenu),
    (IntentName::Plan, ProgramName::PlanMenu),
    (IntentName::Question, ProgramName::QuestionForm),
];

#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    #[error("could not parse keymap '{origin}': {reason}")]
    Parse { origin: String, reason: String },
    #[error("program table violates the fixed intent mapping at '{program}'")]
    ProgramTableViolation { program: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeymap {
    keys: BTreeMap<KeyName, Vec<u8>>,
    delays: BTreeMap<DelayName, u32>,
    menus: BTreeMap<MenuName, MenuLayout>,
    verified_shapes: BTreeMap<ProgramName, ShapeSet>,
    intent_programs: BTreeMap<IntentName, ProgramName>,
    programs: BTreeMap<ProgramName, Program>,
}

pub fn load_str(source: &str, origin: &str) -> Result<Keymap, KeymapError> {
    let raw: RawKeymap = toml::from_str(source).map_err(|error| KeymapError::Parse {
        origin: origin.to_owned(),
        reason: error.to_string(),
    })?;

    validate_program_table(&raw)?;
    validate_references(&raw, origin)?;
    validate_call_graph(&raw)?;

    Ok(Keymap {
        keys: raw.keys,
        delays: raw.delays,
        menus: raw.menus,
        verified_shapes: raw.verified_shapes,
        programs: raw.programs,
    })
}

fn validate_program_table(raw: &RawKeymap) -> Result<(), KeymapError> {
    for (intent, expected) in PROGRAM_TABLE {
        if raw.intent_programs.get(intent) != Some(expected) {
            let actual = raw
                .intent_programs
                .get(intent)
                .map_or_else(|| "missing".to_owned(), |program| format!("{program:?}"));
            return Err(KeymapError::ProgramTableViolation {
                program: format!("{intent:?}: expected {expected:?}, got {actual}"),
            });
        }
        if !raw.programs.contains_key(expected) {
            return Err(KeymapError::ProgramTableViolation {
                program: format!("{intent:?}: missing root {expected:?}"),
            });
        }
    }
    if raw.intent_programs.len() != PROGRAM_TABLE.len() || raw.programs.len() != PROGRAM_TABLE.len()
    {
        return Err(KeymapError::ProgramTableViolation {
            program: "the keymap must contain exactly the fixed intent roots".to_owned(),
        });
    }
    Ok(())
}

fn validate_references(raw: &RawKeymap, origin: &str) -> Result<(), KeymapError> {
    for (program_name, program) in &raw.programs {
        validate_steps(
            &program.steps,
            raw,
            origin,
            &format!("programs.{program_name:?}.steps"),
        )?;
    }
    Ok(())
}

fn validate_steps(
    steps: &[Step],
    raw: &RawKeymap,
    origin: &str,
    field: &str,
) -> Result<(), KeymapError> {
    for (index, step) in steps.iter().enumerate() {
        let field = format!("{field}[{index}]");
        match step {
            Step::Key { key } if !raw.keys.contains_key(key) => {
                return reference_error(origin, &format!("{field}.key"), key);
            }
            Step::Delay { delay } if !raw.delays.contains_key(delay) => {
                return reference_error(origin, &format!("{field}.delay"), delay);
            }
            Step::Paste {
                pieces: Some(pieces),
                ..
            } => {
                if !raw.delays.contains_key(&pieces.between) {
                    return reference_error(
                        origin,
                        &format!("{field}.pieces.between"),
                        &pieces.between,
                    );
                }
                if pieces.chars < 2 {
                    return Err(KeymapError::Parse {
                        origin: origin.to_owned(),
                        reason: format!("{field}.pieces.chars must be at least 2"),
                    });
                }
            }
            Step::Digit {
                digit: DigitSource::MenuEntry { menu, entry },
            } => {
                let Some(layout) = raw.menus.get(menu) else {
                    return reference_error(origin, &format!("{field}.digit.menu"), menu);
                };
                if !layout.entries.contains_key(entry) {
                    return reference_error(origin, &format!("{field}.digit.entry"), entry);
                }
            }
            Step::Call { program } if !raw.programs.contains_key(program) => {
                return reference_error(origin, &format!("{field}.program"), program);
            }
            Step::Repeat { steps, .. } | Step::ForEach { steps, .. } => {
                validate_steps(steps, raw, origin, &format!("{field}.steps"))?;
            }
            Step::If {
                then_steps,
                otherwise,
                ..
            } => {
                validate_steps(then_steps, raw, origin, &format!("{field}.then"))?;
                validate_steps(otherwise, raw, origin, &format!("{field}.otherwise"))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_call_graph(raw: &RawKeymap) -> Result<(), KeymapError> {
    fn visit(
        program: ProgramName,
        raw: &RawKeymap,
        visiting: &mut BTreeSet<ProgramName>,
        visited: &mut BTreeSet<ProgramName>,
    ) -> Result<(), KeymapError> {
        if visited.contains(&program) {
            return Ok(());
        }
        if !visiting.insert(program) {
            return Err(KeymapError::ProgramTableViolation {
                program: format!("recursive call through {program:?}"),
            });
        }
        let mut calls = Vec::new();
        collect_calls(&raw.programs[&program].steps, &mut calls);
        for called in calls {
            visit(called, raw, visiting, visited)?;
        }
        visiting.remove(&program);
        visited.insert(program);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for (_, program) in PROGRAM_TABLE {
        visit(*program, raw, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn collect_calls(steps: &[Step], calls: &mut Vec<ProgramName>) {
    for step in steps {
        match step {
            Step::Call { program } => calls.push(*program),
            Step::Repeat { steps, .. } | Step::ForEach { steps, .. } => {
                collect_calls(steps, calls);
            }
            Step::If {
                then_steps,
                otherwise,
                ..
            } => {
                collect_calls(then_steps, calls);
                collect_calls(otherwise, calls);
            }
            _ => {}
        }
    }
}

fn reference_error<T: std::fmt::Debug>(
    origin: &str,
    field: &str,
    value: &T,
) -> Result<(), KeymapError> {
    Err(KeymapError::Parse {
        origin: origin.to_owned(),
        reason: format!("field '{field}' references missing value '{value:?}'"),
    })
}

/// Chooses the binary-owned root program for a semantic intent.
pub fn program_for(intent: &Intent, ask: Option<&AskKind>) -> Result<ProgramName, InputError> {
    let intent_name = match intent {
        Intent::Prompt { .. } => IntentName::Prompt,
        Intent::SendNow { .. } => IntentName::SendNow,
        Intent::Interrupt => IntentName::Interrupt,
        Intent::CyclePermissionMode => IntentName::ModeCycle,
        Intent::Answer { ask_id, answer } => {
            let ask = ask.ok_or_else(|| InputError::UnknownAsk(ask_id.clone()))?;
            match (ask, answer) {
                (AskKind::Permission { is_plan: false, .. }, AskAnswer::Permission(_)) => {
                    IntentName::Permission
                }
                (AskKind::Permission { is_plan: true, .. }, AskAnswer::Plan(_)) => IntentName::Plan,
                (AskKind::Question { .. }, AskAnswer::Question(_)) => IntentName::Question,
                (AskKind::Permission { is_plan: true, .. }, _) => {
                    return mismatch("a plan-review ask takes a plan answer");
                }
                (AskKind::Permission { is_plan: false, .. }, _) => {
                    return mismatch("a permission ask takes a permission answer");
                }
                (AskKind::Question { .. }, _) => {
                    return mismatch("a question ask takes a question answer");
                }
            }
        }
    };
    Ok(PROGRAM_TABLE
        .iter()
        .find_map(|(name, program)| (*name == intent_name).then_some(*program))
        .expect("the closed intent table covers every intent"))
}

/// Interprets one fixed root against typed ask and answer facts.
pub fn encode(
    keymap: &Keymap,
    program: ProgramName,
    env: &Environment<'_>,
) -> Result<Vec<KeyStep>, InputError> {
    validate_environment(keymap, program, env)?;
    let Some(root) = keymap.programs.get(&program) else {
        return mismatch(format!("keymap has no {program:?} program"));
    };
    let question_count = match env.ask {
        Some(AskKind::Question { questions }) => questions.len(),
        _ => 0,
    };
    let mut interpreter = Interpreter {
        keymap,
        env,
        output: Vec::new(),
        question: None,
        selected: None,
        cursors: vec![0; question_count],
    };
    interpreter.run(&root.steps)?;
    Ok(interpreter.output)
}

fn validate_environment(
    keymap: &Keymap,
    program: ProgramName,
    env: &Environment<'_>,
) -> Result<(), InputError> {
    match program {
        ProgramName::Prompt | ProgramName::SendNow => {
            let text = env
                .prompt
                .ok_or_else(|| unsafe_text("prompt text is missing"))?;
            if text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .trim()
                .is_empty()
            {
                return Err(unsafe_text("prompt must not be empty"));
            }
        }
        ProgramName::Interrupt | ProgramName::ModeCycle => {}
        ProgramName::PermissionMenu => {
            let (suggestions, answer) = match (env.ask, env.answer) {
                (
                    Some(AskKind::Permission {
                        suggestions,
                        is_plan: false,
                        ..
                    }),
                    Some(AskAnswer::Permission(answer)),
                ) => (*suggestions, answer),
                _ => return mismatch("permission program requires a non-plan permission answer"),
            };
            let shapes = keymap.verified_shapes.get(&ProgramName::PermissionMenu);
            let whole =
                shapes.is_some_and(|shapes| shapes.permission_suggestions.contains(&suggestions));
            let folded = shapes
                .is_some_and(|shapes| shapes.permission_folded_suggestions.contains(&suggestions));
            // Yes is the first entry of every menu and Escape denies
            // whatever entries it holds; a scope, and the switch to auto
            // mode after the scopes, need a known menu.
            let scoped = matches!(answer, PermissionAnswer::AllowScoped { .. });
            let auto = matches!(answer, PermissionAnswer::AllowSwitchingToAuto);
            if (scoped || auto) && !whole && !folded {
                return Err(InputError::UnverifiedShape {
                    program,
                    reason: format!(
                        "{} on a permission menu with {suggestions} suggestions is not verified",
                        if auto {
                            "the switch to auto mode"
                        } else {
                            "a scoped allowance"
                        }
                    ),
                });
            }
            let offered = if folded { 1 } else { suggestions };
            if let PermissionAnswer::AllowScoped { suggestion } = answer
                && *suggestion >= offered
            {
                return mismatch(if folded {
                    format!(
                        "suggestion {suggestion} selected on a permission menu that folds its {suggestions} suggestions into one entry"
                    )
                } else {
                    format!(
                        "suggestion {suggestion} selected on a permission menu with {suggestions} suggestions"
                    )
                });
            }
        }
        ProgramName::PlanMenu => match (env.ask, env.answer) {
            (Some(AskKind::Permission { is_plan: true, .. }), Some(AskAnswer::Plan(_))) => {}
            _ => return mismatch("plan program requires a plan-review answer"),
        },
        ProgramName::QuestionForm => {
            let (questions, response) = match (env.ask, env.answer) {
                (Some(AskKind::Question { questions }), Some(AskAnswer::Question(response))) => {
                    (questions, response)
                }
                _ => return mismatch("question program requires a question answer"),
            };
            if questions.is_empty() {
                return mismatch("the ask carries no questions");
            }
            if questions.len() != response.answers.len() {
                return mismatch(format!(
                    "{} questions, {} answers; each question takes one, empty when skipped",
                    questions.len(),
                    response.answers.len()
                ));
            }
            for (index, (question, answer)) in questions.iter().zip(&response.answers).enumerate() {
                if answer.is_skipped() {
                    // Only a form of several questions ends in Claude's
                    // review screen, which submits with some unanswered; a
                    // lone question is submitted by answering it.
                    if questions.len() == 1 {
                        return mismatch("a lone question cannot be skipped");
                    }
                    continue;
                }
                validate_question_answer(index, question.options, question.multi_select, answer)?;
            }
        }
    }
    Ok(())
}

fn validate_question_answer(
    question: usize,
    options: usize,
    multi_select: bool,
    answer: &QuestionAnswer,
) -> Result<(), InputError> {
    if let Some(selected) = answer
        .selected
        .iter()
        .find(|selected| **selected >= options)
    {
        return mismatch(format!(
            "option {selected} selected on question {question} with {options} options"
        ));
    }
    if multi_select {
        if answer.selected.is_empty() && answer.other.is_none() {
            return mismatch(format!("multi-select question {question} has no answer"));
        }
    } else {
        match (answer.selected.as_slice(), answer.other.as_ref()) {
            ([_], None) | ([], Some(_)) => {}
            ([], None) => {
                return mismatch(format!("single-select question {question} has no answer"));
            }
            _ => {
                return mismatch(format!(
                    "single-select question {question} takes one selection or Other"
                ));
            }
        }
    }
    Ok(())
}

/// `text` in runs of at most `limit` characters. Each cut falls between
/// two characters that are not whitespace where the run has such a place,
/// so no piece starts or ends with a space or a line break for Claude to
/// trim from a paste.
fn paste_pieces(text: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    loop {
        let head: Vec<(usize, char)> = rest.char_indices().take(limit + 1).collect();
        if head.len() <= limit {
            break;
        }
        let cut = (1..=limit)
            .rev()
            .find(|&at| !head[at - 1].1.is_whitespace() && !head[at].1.is_whitespace())
            .unwrap_or(limit);
        let (piece, after) = rest.split_at(head[cut].0);
        pieces.push(piece);
        rest = after;
    }
    pieces.push(rest);
    pieces
}

fn mismatch<T>(detail: impl Into<String>) -> Result<T, InputError> {
    Err(InputError::AnswerMismatchesAsk {
        detail: detail.into(),
    })
}

fn unsafe_text(reason: impl Into<String>) -> InputError {
    InputError::UnsafeText {
        reason: reason.into(),
    }
}

struct Interpreter<'a, 'e> {
    keymap: &'a Keymap,
    env: &'a Environment<'e>,
    output: Vec<KeyStep>,
    question: Option<usize>,
    selected: Option<usize>,
    cursors: Vec<usize>,
}

impl Interpreter<'_, '_> {
    fn run(&mut self, steps: &[Step]) -> Result<(), InputError> {
        for step in steps {
            match step {
                Step::Key { key } => self.write_key(*key)?,
                Step::Paste { text, pieces } => {
                    let text = self.text(*text, false)?;
                    let parts = match pieces {
                        Some(pieces) => paste_pieces(&text, pieces.chars),
                        None => vec![text.as_str()],
                    };
                    for (index, part) in parts.iter().enumerate() {
                        if let (true, Some(pieces)) = (index > 0, pieces) {
                            self.delay(pieces.between)?;
                        }
                        let mut bytes = self.key(KeyName::PasteBegin)?.to_vec();
                        bytes.extend_from_slice(part.as_bytes());
                        bytes.extend_from_slice(self.key(KeyName::PasteEnd)?);
                        self.output.push(KeyStep::Write(bytes));
                    }
                }
                Step::Type { text } => {
                    let text = self.text(*text, true)?;
                    self.output.push(KeyStep::Write(text.into_bytes()));
                }
                Step::Digit { digit } => {
                    if let Some(position) = self.digit(digit)? {
                        if !(1..=9).contains(&position) {
                            return Err(InputError::UnverifiedShape {
                                program: ProgramName::QuestionForm,
                                reason: format!("menu position {position} is beyond the digit row"),
                            });
                        }
                        self.output
                            .push(KeyStep::Write(position.to_string().into_bytes()));
                    }
                }
                Step::Delay { delay } => self.delay(*delay)?,
                Step::Repeat { count, steps } => {
                    for _ in 0..self.count(*count)? {
                        self.run(steps)?;
                    }
                }
                Step::ForEach { over, steps } => self.for_each(*over, steps)?,
                Step::If {
                    cond,
                    then_steps,
                    otherwise,
                } => {
                    if self.condition(*cond)? {
                        self.run(then_steps)?;
                    } else {
                        self.run(otherwise)?;
                    }
                }
                Step::MoveTo { row } => self.move_to(*row)?,
                Step::Call { program } => {
                    let called = self.keymap.programs.get(program).ok_or_else(|| {
                        InputError::AnswerMismatchesAsk {
                            detail: format!("keymap has no called program {program:?}"),
                        }
                    })?;
                    self.run(&called.steps)?;
                }
            }
        }
        Ok(())
    }

    fn key(&self, name: KeyName) -> Result<&[u8], InputError> {
        self.keymap
            .keys
            .get(&name)
            .map(Vec::as_slice)
            .ok_or_else(|| unsafe_text(format!("keymap is missing key {name:?}")))
    }

    fn write_key(&mut self, name: KeyName) -> Result<(), InputError> {
        let bytes = self.key(name)?.to_vec();
        self.output.push(KeyStep::Write(bytes));
        Ok(())
    }

    fn delay(&mut self, name: DelayName) -> Result<(), InputError> {
        let milliseconds = self
            .keymap
            .delays
            .get(&name)
            .copied()
            .ok_or_else(|| unsafe_text(format!("keymap is missing delay {name:?}")))?;
        self.output
            .push(KeyStep::Delay(Duration::from_millis(u64::from(
                milliseconds,
            ))));
        Ok(())
    }

    fn text(&self, source: TextSource, typed: bool) -> Result<String, InputError> {
        let raw = match source {
            TextSource::PromptText => self
                .env
                .prompt
                .ok_or_else(|| unsafe_text("prompt text is missing"))?
                .replace("\r\n", "\n")
                .replace('\r', "\n"),
            TextSource::DenyFeedback => match self.env.answer {
                Some(AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some(feedback),
                })) => feedback.trim().replace("\r\n", "\n").replace('\r', "\n"),
                _ => return mismatch("deny feedback text is missing"),
            },
            TextSource::PlanFeedback => match self.env.answer {
                Some(AskAnswer::Plan(PlanAnswer::RequestChanges { feedback })) => {
                    feedback.trim().to_owned()
                }
                _ => return mismatch("plan feedback text is missing"),
            },
            TextSource::OtherText => self
                .question_answer()?
                .other
                .as_deref()
                .ok_or_else(|| unsafe_text("Other text is missing"))?
                .trim()
                .to_owned(),
        };
        if raw.trim().is_empty() {
            return Err(unsafe_text(format!("{source:?} must not be empty")));
        }
        if typed && raw.contains('\n') {
            return Err(unsafe_text(format!("{source:?} must be a single line")));
        }
        if raw
            .chars()
            .any(|character| character.is_control() && character != '\n')
        {
            return Err(unsafe_text(format!(
                "{source:?} must not contain control characters"
            )));
        }
        Ok(raw)
    }

    fn digit(&self, source: &DigitSource) -> Result<Option<usize>, InputError> {
        match source {
            DigitSource::MenuEntry { menu, entry } => {
                let matches = matches!(
                    (menu, entry, self.env.answer),
                    (
                        MenuName::Permission,
                        MenuEntryName::AllowOnce,
                        Some(AskAnswer::Permission(PermissionAnswer::AllowOnce)),
                    ) | (
                        MenuName::Permission,
                        MenuEntryName::AllowScoped,
                        Some(AskAnswer::Permission(PermissionAnswer::AllowScoped { .. })),
                    ) | (
                        MenuName::Plan,
                        MenuEntryName::ApproveAuto,
                        Some(AskAnswer::Plan(PlanAnswer::ApproveAuto)),
                    ) | (
                        MenuName::Plan,
                        MenuEntryName::ApproveManual,
                        Some(AskAnswer::Plan(PlanAnswer::ApproveManual)),
                    ) | (
                        MenuName::Plan,
                        MenuEntryName::RequestChanges,
                        Some(AskAnswer::Plan(PlanAnswer::RequestChanges { .. })),
                    )
                );
                if !matches {
                    return Ok(None);
                }
                let position = self
                    .keymap
                    .menus
                    .get(menu)
                    .and_then(|layout| layout.entries.get(entry))
                    .copied()
                    .ok_or_else(|| unsafe_text(format!("keymap is missing {menu:?}.{entry:?}")))?;
                Ok(Some(usize::from(position)))
            }
            DigitSource::SelectedOption => Ok(Some(
                self.selected
                    .ok_or_else(|| unsafe_text("selected option is out of scope"))?
                    + 1,
            )),
            DigitSource::OtherRow => {
                let question = self.question_fact()?;
                Ok(Some(question.options + 1))
            }
            DigitSource::Suggestion => match self.env.answer {
                Some(AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion })) => {
                    let first = self.keymap.menus[&MenuName::Permission].entries
                        [&MenuEntryName::AllowScoped];
                    Ok(Some(usize::from(first) + suggestion))
                }
                _ => Ok(None),
            },
            DigitSource::AutoSwitch => match (self.env.ask, self.env.answer) {
                (
                    Some(AskKind::Permission { suggestions, .. }),
                    Some(AskAnswer::Permission(PermissionAnswer::AllowSwitchingToAuto)),
                ) => {
                    let first = self.keymap.menus[&MenuName::Permission].entries
                        [&MenuEntryName::AllowScoped];
                    let folded = self
                        .keymap
                        .verified_shapes
                        .get(&ProgramName::PermissionMenu)
                        .is_some_and(|shapes| {
                            shapes.permission_folded_suggestions.contains(suggestions)
                        });
                    let entries = if folded { 1 } else { *suggestions };
                    Ok(Some(usize::from(first) + entries))
                }
                _ => Ok(None),
            },
        }
    }

    fn count(&self, source: CountSource) -> Result<usize, InputError> {
        match source {
            CountSource::Questions => match self.env.ask {
                Some(AskKind::Question { questions }) => Ok(questions.len()),
                _ => mismatch("question count is out of scope"),
            },
            CountSource::SelectedOptions => Ok(self.selected_options()?.len()),
        }
    }

    fn for_each(&mut self, iter: Iter, steps: &[Step]) -> Result<(), InputError> {
        match iter {
            Iter::Questions => {
                let count = match self.env.ask {
                    Some(AskKind::Question { questions }) => questions.len(),
                    _ => return mismatch("questions are out of scope"),
                };
                for question in 0..count {
                    self.question = Some(question);
                    self.selected = None;
                    self.cursors[question] = 0;
                    self.run(steps)?;
                }
                self.question = None;
                self.selected = None;
            }
            Iter::SelectedOptions => {
                for selected in self.selected_options()? {
                    self.selected = Some(selected);
                    self.run(steps)?;
                }
                self.selected = None;
            }
        }
        Ok(())
    }

    fn condition(&self, cond: Cond) -> Result<bool, InputError> {
        match cond {
            Cond::Denies => Ok(matches!(
                self.env.answer,
                Some(AskAnswer::Permission(PermissionAnswer::Deny { .. }))
            )),
            Cond::SendsBack => Ok(matches!(
                self.env.answer,
                Some(AskAnswer::Plan(PlanAnswer::RequestChanges { .. }))
            )),
            Cond::HasFeedback => Ok(match self.env.answer {
                Some(AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some(feedback),
                })) => !feedback.trim().is_empty(),
                Some(AskAnswer::Plan(PlanAnswer::RequestChanges { feedback })) => {
                    !feedback.trim().is_empty()
                }
                _ => false,
            }),
            Cond::HasOther => Ok(self.question_answer()?.other.is_some()),
            Cond::Skipped => Ok(self.question_answer()?.is_skipped()),
            Cond::MultiSelect => Ok(self.question_fact()?.multi_select),
            Cond::HasPreviews => Ok(self.question_fact()?.previews),
            Cond::IsFirst => Ok(self.question == Some(0)),
            Cond::LastQuestionMulti => match self.env.ask {
                Some(AskKind::Question { questions }) => Ok(questions
                    .last()
                    .is_some_and(|question| question.multi_select)),
                _ => mismatch("last question is out of scope"),
            },
            Cond::SingleQuestionSingleSelect => match self.env.ask {
                Some(AskKind::Question { questions }) => Ok(questions.len() == 1
                    && questions
                        .first()
                        .is_some_and(|question| !question.multi_select)),
                _ => mismatch("question form shape is out of scope"),
            },
        }
    }

    fn move_to(&mut self, row: RowSource) -> Result<(), InputError> {
        let question = self
            .question
            .ok_or_else(|| unsafe_text("question cursor is out of scope"))?;
        let target = match row {
            RowSource::SelectedOption => self
                .selected
                .ok_or_else(|| unsafe_text("selected option is out of scope"))?,
            RowSource::OtherRow => self.question_fact()?.options,
        };
        if target < self.cursors[question] {
            return mismatch(format!(
                "question cursor cannot move from row {} back to {target}",
                self.cursors[question]
            ));
        }
        while self.cursors[question] < target {
            self.write_key(KeyName::Down)?;
            self.delay(DelayName::AfterMove)?;
            self.cursors[question] += 1;
        }
        Ok(())
    }

    fn question_fact(&self) -> Result<&super::QuestionFact, InputError> {
        let index = self
            .question
            .ok_or_else(|| unsafe_text("question is out of scope"))?;
        match self.env.ask {
            Some(AskKind::Question { questions }) => questions
                .get(index)
                .ok_or_else(|| unsafe_text("question index is out of range")),
            _ => mismatch("question is out of scope"),
        }
    }

    fn question_answer(&self) -> Result<&QuestionAnswer, InputError> {
        let index = self
            .question
            .ok_or_else(|| unsafe_text("question answer is out of scope"))?;
        match self.env.answer {
            Some(AskAnswer::Question(response)) => response
                .answers
                .get(index)
                .ok_or_else(|| unsafe_text("question answer index is out of range")),
            _ => mismatch("question answer is out of scope"),
        }
    }

    fn selected_options(&self) -> Result<Vec<usize>, InputError> {
        let mut selected = self.question_answer()?.selected.clone();
        selected.sort_unstable();
        selected.dedup();
        Ok(selected)
    }
}

#[cfg(test)]
mod format {
    use super::*;

    fn assert_parse_mentions(source: &str, needle: &str) {
        let error = load_str(source, "broken.toml").expect_err("malformed keymap must fail");
        let rendered = error.to_string();
        assert!(rendered.contains("broken.toml"), "{rendered}");
        assert!(rendered.contains(needle), "{rendered}");
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let source = format!("surprise = true\n{BAKED}");
        assert_parse_mentions(&source, "surprise");
    }

    #[test]
    fn unknown_program_is_rejected() {
        let source = BAKED.replacen("[programs.prompt]", "[programs.launch]", 1);
        assert_parse_mentions(&source, "launch");
    }

    #[test]
    fn unknown_step_is_rejected() {
        let source = BAKED.replacen(
            "{ step = \"paste\", text = \"prompt_text\", pieces = { chars = 700, between = \"between_pastes\" } }",
            "{ step = \"shell\", command = \"stty\" }",
            1,
        );
        assert_parse_mentions(&source, "shell");
    }

    #[test]
    fn unknown_key_is_rejected() {
        let source = BAKED.replace("enter = [13]", "return = [13]");
        assert_parse_mentions(&source, "return");
    }

    #[test]
    fn unknown_condition_is_rejected() {
        let source = BAKED.replacen("cond = \"has_feedback\"", "cond = \"pane_visible\"", 1);
        assert_parse_mentions(&source, "pane_visible");
    }

    #[test]
    fn malformed_toml_names_its_origin_and_field() {
        let source = BAKED.replacen("after_paste = 400", "after_paste = \"slow\"", 1);
        assert_parse_mentions(&source, "after_paste");
    }

    #[test]
    fn references_must_name_values_present_in_the_same_file() {
        let source = BAKED.replace(
            "programs = {}",
            "[programs.prompt]\nsteps = [{ step = \"key\", key = \"enter\" }]",
        );
        let source = source.replace("enter = [13]\n", "");
        assert_parse_mentions(&source, "programs.Prompt.steps[2].key");
    }
}

#[cfg(test)]
mod interpret {
    use super::*;
    use crate::terminal::{AskId, QuestionFact, QuestionResponse};

    fn permission(suggestions: usize) -> AskKind {
        AskKind::Permission {
            tool_name: "Bash".to_owned(),
            suggestions,
            is_plan: false,
        }
    }

    fn plan() -> AskKind {
        AskKind::Permission {
            tool_name: "ExitPlanMode".to_owned(),
            suggestions: 0,
            is_plan: true,
        }
    }

    fn questions(shapes: &[(usize, bool)]) -> AskKind {
        AskKind::Question {
            questions: shapes
                .iter()
                .map(|(options, multi_select)| QuestionFact {
                    options: *options,
                    multi_select: *multi_select,
                    previews: false,
                })
                .collect(),
        }
    }

    fn question_answer(answers: Vec<(Vec<usize>, Option<&str>)>) -> AskAnswer {
        AskAnswer::Question(QuestionResponse {
            answers: answers
                .into_iter()
                .map(|(selected, other)| QuestionAnswer {
                    selected,
                    other: other.map(str::to_owned),
                })
                .collect(),
        })
    }

    fn answer_intent(answer: AskAnswer) -> Intent {
        Intent::Answer {
            ask_id: AskId("ask-1".to_owned()),
            answer,
        }
    }

    fn encoded(intent: &Intent, ask: Option<&AskKind>) -> Result<Vec<KeyStep>, InputError> {
        let program = program_for(intent, ask)?;
        let (answer, prompt) = match intent {
            Intent::Prompt { text } | Intent::SendNow { text } => (None, Some(text.as_str())),
            Intent::Answer { answer, .. } => (Some(answer), None),
            Intent::Interrupt | Intent::CyclePermissionMode => (None, None),
        };
        encode(
            &baked(),
            program,
            &Environment {
                ask,
                answer,
                prompt,
            },
        )
    }

    fn write(bytes: &[u8]) -> KeyStep {
        KeyStep::Write(bytes.to_vec())
    }

    fn delay(milliseconds: u64) -> KeyStep {
        KeyStep::Delay(Duration::from_millis(milliseconds))
    }

    #[test]
    fn fixed_intent_table_cannot_be_remapped_by_a_keymap() {
        let cases = [
            (
                Intent::Prompt {
                    text: "hi".to_owned(),
                },
                None,
                ProgramName::Prompt,
            ),
            (Intent::Interrupt, None, ProgramName::Interrupt),
            (Intent::CyclePermissionMode, None, ProgramName::ModeCycle),
        ];
        for (intent, ask, expected) in cases {
            assert_eq!(
                program_for(&intent, ask.as_ref()).expect("mapped"),
                expected
            );
        }

        let source = BAKED.replacen("prompt = \"prompt\"", "prompt = \"interrupt\"", 1);
        assert!(matches!(
            load_str(&source, "remapped.toml"),
            Err(KeymapError::ProgramTableViolation { ref program })
                if program.contains("Prompt") && program.contains("Interrupt")
        ));

        let recursive = BAKED.replacen(
            "{ step = \"paste\", text = \"prompt_text\", pieces = { chars = 700, between = \"between_pastes\" } }",
            "{ step = \"call\", program = \"prompt\" }",
            1,
        );
        assert!(matches!(
            load_str(&recursive, "recursive.toml"),
            Err(KeymapError::ProgramTableViolation { ref program })
                if program.contains("recursive")
        ));
    }

    #[test]
    fn answer_programs_are_selected_from_typed_ask_facts() {
        let permission_ask = permission(1);
        let permission_intent = answer_intent(AskAnswer::Permission(PermissionAnswer::AllowOnce));
        assert_eq!(
            program_for(&permission_intent, Some(&permission_ask)).expect("permission"),
            ProgramName::PermissionMenu
        );
        let plan_ask = plan();
        let plan_intent = answer_intent(AskAnswer::Plan(PlanAnswer::ApproveManual));
        assert_eq!(
            program_for(&plan_intent, Some(&plan_ask)).expect("plan"),
            ProgramName::PlanMenu
        );
        let question_ask = questions(&[(2, false)]);
        let question_intent = answer_intent(question_answer(vec![(vec![0], None)]));
        assert_eq!(
            program_for(&question_intent, Some(&question_ask)).expect("question"),
            ProgramName::QuestionForm
        );
        assert!(matches!(
            program_for(&plan_intent, Some(&permission_ask)),
            Err(InputError::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            program_for(&permission_intent, None),
            Err(InputError::UnknownAsk(AskId(ref id))) if id == "ask-1"
        ));
    }

    #[test]
    fn prompt_interrupt_and_mode_cycle_match_the_legacy_bytes() {
        assert_eq!(
            encoded(
                &Intent::Prompt {
                    text: "hello\r\nworld".to_owned(),
                },
                None,
            )
            .expect("prompt"),
            vec![
                write(b"\x1b[200~hello\nworld\x1b[201~"),
                delay(400),
                write(b"\r"),
            ]
        );
        assert_eq!(
            encoded(&Intent::Interrupt, None).expect("interrupt"),
            vec![write(b"\x1b")]
        );
        assert_eq!(
            encoded(&Intent::CyclePermissionMode, None).expect("mode cycle"),
            vec![write(b"\x1b[Z")]
        );
    }

    /// Claude 2.1.283 collapses one paste of about 1,100 characters into
    /// pasted content the model will not take instructions from, and keeps
    /// one of about 750 as typed. A long prompt therefore goes as pastes of
    /// at most 700 characters, cut inside words so Claude trims nothing,
    /// with a short pause between them.
    #[test]
    fn a_long_prompt_is_pasted_in_pieces_claude_keeps_as_typed() {
        let text = format!(
            "Reply with exactly SECOND.\n{}",
            "alpha bravo charlie delta echo foxtrot golf hotel india juliet. "
                .repeat(17)
                .trim_end()
        );
        assert!(text.chars().count() > 1_100);
        let steps = encoded(&Intent::Prompt { text: text.clone() }, None).expect("prompt");
        let (last, pastes) = steps.split_last().expect("steps");
        assert_eq!(last, &write(b"\r"));
        let mut sent = String::new();
        let mut pieces = 0;
        for (index, step) in pastes.iter().enumerate() {
            match (index % 2, step) {
                (0, KeyStep::Write(bytes)) => {
                    let body = bytes
                        .strip_prefix(b"\x1b[200~")
                        .and_then(|rest| rest.strip_suffix(b"\x1b[201~"))
                        .expect("a bracketed paste");
                    let piece = std::str::from_utf8(body).expect("utf-8");
                    assert!(piece.chars().count() <= 700, "{piece:?}");
                    assert!(!piece.starts_with(char::is_whitespace), "{piece:?}");
                    assert!(!piece.ends_with(char::is_whitespace), "{piece:?}");
                    sent.push_str(piece);
                    pieces += 1;
                }
                (1, step) => {
                    let pause = if index == pastes.len() - 1 { 400 } else { 100 };
                    assert_eq!(step, &delay(pause), "step {index}");
                }
                (_, step) => panic!("unexpected step {index}: {step:?}"),
            }
        }
        assert_eq!(pieces, 2);
        assert_eq!(sent, text);
    }

    #[test]
    fn paste_pieces_cut_between_letters_and_rejoin_exactly() {
        assert_eq!(paste_pieces("short", 500), ["short"]);
        assert_eq!(paste_pieces("ab cd", 3), ["a", "b c", "d"]);
        let spaced = "é".repeat(3) + &" ".repeat(6) + "wörds";
        let pieces = paste_pieces(&spaced, 4);
        assert_eq!(pieces.concat(), spaced);
        assert!(pieces.iter().all(|piece| piece.chars().count() <= 4));
    }

    #[test]
    fn permission_variants_match_the_legacy_bytes() {
        let ask = permission(1);
        let cases = [
            (
                AskAnswer::Permission(PermissionAnswer::AllowOnce),
                vec![write(b"1")],
            ),
            (
                AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
                vec![write(b"2")],
            ),
            (
                AskAnswer::Permission(PermissionAnswer::Deny { feedback: None }),
                vec![write(b"\x1b")],
            ),
            (
                AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some("try the other file".to_owned()),
                }),
                vec![
                    write(b"\x1b"),
                    delay(1_500),
                    write(b"\x1b[200~try the other file\x1b[201~"),
                    delay(400),
                    write(b"\r"),
                ],
            ),
        ];
        for (answer, expected) in cases {
            assert_eq!(
                encoded(&answer_intent(answer), Some(&ask)).expect("permission"),
                expected
            );
        }
        assert_eq!(
            encoded(
                &answer_intent(AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some("  ".to_owned()),
                })),
                Some(&ask),
            )
            .expect("plain deny"),
            vec![write(b"\x1b")]
        );
    }

    /// "Yes, and switch to auto mode" follows the scope entries: third on
    /// a menu with one suggestion, or two folded into one entry, as Claude
    /// 2.1.283 and 2.1.296 draw it. A menu whose entries are not known
    /// cannot reach it.
    #[test]
    fn the_switch_to_auto_mode_follows_the_scope_entries() {
        let auto = || {
            answer_intent(AskAnswer::Permission(
                PermissionAnswer::AllowSwitchingToAuto,
            ))
        };
        for suggestions in [1, 2] {
            assert_eq!(
                encoded(&auto(), Some(&permission(suggestions))).expect("auto"),
                vec![write(b"3")],
                "{suggestions} suggestions"
            );
        }
        assert!(matches!(
            encoded(&auto(), Some(&permission(3))),
            Err(InputError::UnverifiedShape { .. })
        ));
    }

    #[test]
    fn plan_variants_match_the_legacy_bytes_and_type_feedback() {
        let ask = plan();
        assert_eq!(
            encoded(
                &answer_intent(AskAnswer::Plan(PlanAnswer::ApproveAuto)),
                Some(&ask),
            )
            .expect("auto"),
            vec![delay(800), write(b"1")]
        );
        assert_eq!(
            encoded(
                &answer_intent(AskAnswer::Plan(PlanAnswer::ApproveManual)),
                Some(&ask),
            )
            .expect("manual"),
            vec![delay(800), write(b"2")]
        );
        assert_eq!(
            encoded(
                &answer_intent(AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "document VALUE too".to_owned(),
                })),
                Some(&ask),
            )
            .expect("changes"),
            vec![
                write(b"3"),
                delay(800),
                write(b"document VALUE too"),
                delay(400),
                write(b"\r"),
            ]
        );
    }

    #[test]
    fn single_select_and_other_match_the_legacy_bytes() {
        let ask = questions(&[(2, false)]);
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![(vec![0], None)])),
                Some(&ask),
            )
            .expect("single select"),
            vec![write(b"1")]
        );
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![(vec![], Some("a warm ochre"))])),
                Some(&ask),
            )
            .expect("Other"),
            vec![
                write(b"3"),
                delay(800),
                write(b"a warm ochre"),
                delay(400),
                write(b"\r"),
            ],
            "Other is typed raw into the inline editor"
        );
    }

    #[test]
    fn question_count_controls_the_review_steps() {
        let ask = questions(&[(2, false), (2, false)]);
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![(vec![0], None), (vec![1], None)])),
                Some(&ask),
            )
            .expect("two questions"),
            vec![
                write(b"1"),
                delay(800),
                write(b"2"),
                delay(800),
                write(b"\r"),
            ]
        );
    }

    #[test]
    fn multi_select_cursor_other_and_review_match_the_legacy_bytes() {
        let ask = questions(&[(3, true)]);
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![
                    (vec![0, 1], Some("a torque wrench"),)
                ])),
                Some(&ask),
            )
            .expect("multi select"),
            vec![
                write(b" "),
                delay(400),
                write(b"\x1b[B"),
                delay(300),
                write(b" "),
                delay(400),
                write(b"\x1b[B"),
                delay(300),
                write(b"\x1b[B"),
                delay(300),
                write(b"\r"),
                delay(800),
                write(b"a torque wrench"),
                delay(400),
                write(b"\r"),
                delay(600),
                write(b" "),
                delay(400),
                write(b"\t"),
                delay(800),
                write(b"\r"),
                delay(1_000),
                write(b"\r"),
            ],
            "Other is typed raw and MoveTo advances only the remaining rows"
        );
    }

    #[test]
    fn mixed_form_resets_each_question_cursor() {
        let ask = questions(&[(3, true), (2, false)]);
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![(vec![0, 1], None), (vec![1], None)])),
                Some(&ask),
            )
            .expect("mixed"),
            vec![
                write(b" "),
                delay(400),
                write(b"\x1b[B"),
                delay(300),
                write(b" "),
                delay(400),
                write(b"\t"),
                delay(800),
                write(b"2"),
                delay(800),
                write(b"\r"),
            ]
        );

        let ask = questions(&[(3, true), (3, true)]);
        let program = encoded(
            &answer_intent(question_answer(vec![(vec![2], None), (vec![1], None)])),
            Some(&ask),
        )
        .expect("two multi-select questions");
        let down_writes: Vec<_> = program
            .iter()
            .filter(|step| **step == write(b"\x1b[B"))
            .collect();
        assert_eq!(
            down_writes.len(),
            3,
            "two rows then one row after cursor reset"
        );
    }

    #[test]
    fn a_skipped_question_is_moved_past_with_tab_wherever_it_stands() {
        let ask = questions(&[(2, false), (3, true), (2, false)]);
        let skipped = |answers| {
            encoded(&answer_intent(question_answer(answers)), Some(&ask)).expect("a skip")
        };
        assert_eq!(
            skipped(vec![(vec![], None), (vec![0], None), (vec![1], None)]),
            vec![
                write(b"\t"),
                delay(800),
                write(b" "),
                delay(400),
                write(b"\t"),
                delay(800),
                write(b"2"),
                delay(800),
                write(b"\r"),
            ],
            "the first, a single-select question"
        );
        assert_eq!(
            skipped(vec![(vec![0], None), (vec![], None), (vec![1], None)]),
            vec![
                write(b"1"),
                delay(800),
                write(b"\t"),
                delay(800),
                write(b"2"),
                delay(800),
                write(b"\r"),
            ],
            "the middle, a multi-select question: nothing toggled"
        );
        assert_eq!(
            skipped(vec![(vec![0], None), (vec![2], None), (vec![], None)]),
            vec![
                write(b"1"),
                delay(800),
                write(b"\x1b[B"),
                delay(300),
                write(b"\x1b[B"),
                delay(300),
                write(b" "),
                delay(400),
                write(b"\t"),
                delay(800),
                write(b"\t"),
                delay(800),
                write(b"\r"),
            ],
            "the last: Tab reaches the review screen, which submits"
        );

        let ask = questions(&[(2, false), (3, true)]);
        assert_eq!(
            encoded(
                &answer_intent(question_answer(vec![(vec![1], None), (vec![], None)])),
                Some(&ask),
            )
            .expect("the last, a multi-select question"),
            vec![
                write(b"2"),
                delay(800),
                write(b"\t"),
                delay(800),
                write(b"\r"),
                delay(1_000),
                write(b"\r"),
            ]
        );
    }

    #[test]
    fn a_lone_question_cannot_be_skipped() {
        for multi_select in [false, true] {
            let ask = questions(&[(2, multi_select)]);
            let refused = encoded(
                &answer_intent(question_answer(vec![(vec![], None)])),
                Some(&ask),
            )
            .expect_err("a lone question is submitted by answering it");
            assert!(
                matches!(&refused, InputError::AnswerMismatchesAsk { detail } if detail.contains("lone question")),
                "{refused:?}"
            );
        }
    }

    fn assert_text_refused(source: TextSource, text: &str, typed: bool) {
        let keymap = baked();
        let permission_ask = permission(1);
        let permission_answer = AskAnswer::Permission(PermissionAnswer::Deny {
            feedback: Some(text.to_owned()),
        });
        let plan_ask = plan();
        let plan_answer = AskAnswer::Plan(PlanAnswer::RequestChanges {
            feedback: text.to_owned(),
        });
        let question_ask = questions(&[(1, false)]);
        let other_answer = question_answer(vec![(vec![], Some(text))]);
        let (ask, answer, prompt, question) = match source {
            TextSource::PromptText => (None, None, Some(text), None),
            TextSource::DenyFeedback => {
                (Some(&permission_ask), Some(&permission_answer), None, None)
            }
            TextSource::PlanFeedback => (Some(&plan_ask), Some(&plan_answer), None, None),
            TextSource::OtherText => (Some(&question_ask), Some(&other_answer), None, Some(0)),
        };
        let env = Environment {
            ask,
            answer,
            prompt,
        };
        let mut interpreter = Interpreter {
            keymap: &keymap,
            env: &env,
            output: Vec::new(),
            question,
            selected: None,
            cursors: vec![0],
        };
        let step = if typed {
            Step::Type { text: source }
        } else {
            Step::Paste {
                text: source,
                pieces: None,
            }
        };
        assert!(matches!(
            interpreter.run(&[step]),
            Err(InputError::UnsafeText { .. })
        ));
        assert!(interpreter.output.is_empty(), "validation precedes output");
    }

    #[test]
    fn every_text_source_rejects_controls_through_paste_and_type() {
        let controls = [
            "before\u{1b}after",
            "before\u{1b}[Bafter",
            "before\u{1b}[201~after",
            "before\tafter",
            "before\u{009b}after",
        ];
        for source in [
            TextSource::PromptText,
            TextSource::DenyFeedback,
            TextSource::PlanFeedback,
            TextSource::OtherText,
        ] {
            for control in controls {
                assert_text_refused(source, control, false);
                assert_text_refused(source, control, true);
            }
            assert_text_refused(source, "first\nsecond", true);
        }
    }

    #[test]
    fn repeat_counts_and_call_are_bounded_interpreter_steps() {
        let mut keymap = baked();
        keymap
            .programs
            .get_mut(&ProgramName::QuestionForm)
            .unwrap()
            .steps = vec![
            Step::Repeat {
                count: CountSource::Questions,
                steps: vec![Step::Key {
                    key: KeyName::Enter,
                }],
            },
            Step::ForEach {
                over: Iter::Questions,
                steps: vec![Step::Repeat {
                    count: CountSource::SelectedOptions,
                    steps: vec![Step::Key {
                        key: KeyName::Space,
                    }],
                }],
            },
            Step::Call {
                program: ProgramName::Interrupt,
            },
        ];
        let ask = questions(&[(2, false), (2, false)]);
        let answer = question_answer(vec![(vec![0], None), (vec![1], None)]);
        assert_eq!(
            encode(
                &keymap,
                ProgramName::QuestionForm,
                &Environment {
                    ask: Some(&ask),
                    answer: Some(&answer),
                    prompt: None,
                },
            )
            .expect("bounded program"),
            vec![
                write(b"\r"),
                write(b"\r"),
                write(b" "),
                write(b" "),
                write(b"\x1b"),
            ]
        );
    }
}

#[cfg(test)]
mod menus {
    use super::*;
    use crate::terminal::AskId;

    /// Send now is its own row: it types what a prompt types.
    #[test]
    fn send_now_types_what_a_prompt_types() {
        let keymap = baked();
        let text = Environment {
            ask: None,
            answer: None,
            prompt: Some("use the staging config"),
        };
        assert_eq!(
            encode(&keymap, ProgramName::SendNow, &text).unwrap(),
            encode(&keymap, ProgramName::Prompt, &text).unwrap()
        );
    }

    fn permission_menu(
        suggestions: usize,
        answer: PermissionAnswer,
    ) -> Result<Vec<KeyStep>, InputError> {
        let keymap = baked();
        let ask = AskKind::Permission {
            tool_name: "Bash".to_owned(),
            suggestions,
            is_plan: false,
        };
        let answer = AskAnswer::Permission(answer);
        encode(
            &keymap,
            ProgramName::PermissionMenu,
            &Environment {
                ask: Some(&ask),
                answer: Some(&answer),
                prompt: None,
            },
        )
    }

    /// Claude 2.1.283 shows two suggestions as one scoped entry, 2, between
    /// Yes and its auto-mode entry: allow-once, that entry and deny have
    /// keys, a second scope has none.
    #[test]
    fn two_suggestions_fold_into_one_scoped_entry() {
        assert_eq!(
            permission_menu(2, PermissionAnswer::AllowOnce).unwrap(),
            [KeyStep::Write(b"1".to_vec())]
        );
        assert_eq!(
            permission_menu(2, PermissionAnswer::AllowScoped { suggestion: 0 }).unwrap(),
            [KeyStep::Write(b"2".to_vec())]
        );
        assert_eq!(
            permission_menu(2, PermissionAnswer::Deny { feedback: None }).unwrap(),
            [KeyStep::Write(b"\x1b".to_vec())]
        );
        let error = permission_menu(2, PermissionAnswer::AllowScoped { suggestion: 1 })
            .expect_err("a folded menu has one scoped entry");
        assert_eq!(
            error.to_string(),
            "answer does not fit the ask: suggestion 1 selected on a permission menu that folds its 2 suggestions into one entry"
        );
    }

    /// Claude 2.1.283 in manual mode draws 1 Yes / 2 Yes, don't ask again /
    /// 3 Yes, and switch to auto mode / 4 No. A deny, with or without a
    /// note, never types a digit, so it cannot land on a Yes whatever
    /// entries the menu holds; Escape is Claude's No.
    #[test]
    fn a_deny_types_no_digit_on_any_menu() {
        for suggestions in [0, 1, 2, 3] {
            for feedback in [None, Some("No network in this test".to_owned())] {
                let keys =
                    permission_menu(suggestions, PermissionAnswer::Deny { feedback }).unwrap();
                let KeyStep::Write(first) = &keys[0] else {
                    panic!("a deny starts with a key: {keys:?}");
                };
                assert_eq!(first, b"\x1b", "{suggestions} suggestions: {keys:?}");
                assert!(
                    !keys.iter().any(|key| matches!(
                        key,
                        KeyStep::Write(bytes) if bytes.len() == 1 && bytes[0].is_ascii_digit()
                    )),
                    "{suggestions} suggestions: {keys:?}"
                );
            }
        }
    }

    /// Yes is the first entry of every permission menu, so allow-once is
    /// typed whatever suggestions the hook reports.
    #[test]
    fn allow_once_types_yes_on_any_menu() {
        for suggestions in [0, 1, 2, 3] {
            assert_eq!(
                permission_menu(suggestions, PermissionAnswer::AllowOnce).unwrap(),
                [KeyStep::Write(b"1".to_vec())],
                "{suggestions} suggestions"
            );
        }
    }

    #[test]
    fn permission_shape_refusal_names_hook_suggestion_count() {
        let keymap = baked();
        let ask = AskKind::Permission {
            tool_name: "Bash".to_owned(),
            suggestions: 3,
            is_plan: false,
        };
        let answer = AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 });
        let intent = Intent::Answer {
            ask_id: AskId("permission-1".to_owned()),
            answer: answer.clone(),
        };
        let program = program_for(&intent, Some(&ask)).unwrap();
        let error = encode(
            &keymap,
            program,
            &Environment {
                ask: Some(&ask),
                answer: Some(&answer),
                prompt: None,
            },
        )
        .expect_err("a scope on an unverified hook shape must be refused");
        assert_eq!(
            error.to_string(),
            "unverified keymap shape for PermissionMenu: a scoped allowance on a permission menu with 3 suggestions is not verified"
        );
    }
}
