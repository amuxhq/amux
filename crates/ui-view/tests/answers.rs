//! Every choice an ask card offers reaches the interpreter as an input it
//! accepts. Each kind's fixtures are driven through their interpreter and
//! committed into a session; wherever a card is open, every choice (and a
//! question card's picks) goes through `answer_input` to a copy of the
//! interpreter at that point, which must accept it and close the ask. Every
//! pick the settings view offers is likewise an input the interpreter takes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{
    Checkpoint, Committer, Effect, Event, Interpreter, decode_checkpoint, encode_checkpoint,
};
use ui_state::{InputOutcome, Msg, SessionState};
use ui_view::{
    Answer, AskBody, AskCard, CardState, Changeable, QuestionResponse, SettingChange, answer_input,
    ask_card, question_answer, reply_answer, setting_input, settings,
};
use wire::{Kind, SessionEvent, send_input_response, session_event};

use crate::support;

fn fixtures(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind)
}

/// One interpreter and the session its emission builds, in step.
#[derive(Clone)]
struct Driven<S> {
    interpreter: S,
    session: SessionState,
    committer: Committer,
    /// Hooks held for a decision so far, numbered as the agent numbers them.
    held: u64,
}

impl<S: Checkpoint + Clone> Driven<S> {
    fn start<I: Interpreter<State = S>>(kind: Kind, spec: &wire::AgentSpec) -> Self {
        let (interpreter, first) = I::initial(spec);
        let mut driven = Driven {
            interpreter,
            session: SessionState::new(support::agent(kind, wire::Phase::NeedsYou), support::CAP),
            committer: Committer::default(),
            held: 0,
        };
        driven.commit(&first);
        let caught_up = driven.committer.revision;
        driven.session.update(Msg::Event(SessionEvent {
            of: Some(session_event::Of::CaughtUp(wire::CaughtUp {
                revision: caught_up,
            })),
        }));
        driven
    }

    fn commit(&mut self, step: &wire::Step) {
        for event in self.committer.commit(step) {
            self.session.update(Msg::Event(event));
        }
    }

    /// A recorded hook Claude waits on for a decision, numbered as the agent
    /// holding its connection numbers it, so an answer has a hook to go on.
    fn held(&mut self, event: Event) -> Event {
        let Event::Fact(mut fact) = event else {
            return event;
        };
        if fact.channel == interpret::Channel::Hook
            && let Ok(serde_json::Value::Object(mut payload)) =
                serde_json::from_slice(&fact.payload)
            && matches!(
                payload
                    .get("hook_event_name")
                    .and_then(|name| name.as_str()),
                Some("PermissionRequest" | "Elicitation")
            )
            && !payload.contains_key(interpret::HOOK_ANSWER_FIELD)
        {
            self.held += 1;
            payload.insert(interpret::HOOK_ANSWER_FIELD.to_owned(), self.held.into());
            fact.payload = serde_json::to_vec(&payload).expect("a payload encodes");
        }
        Event::Fact(fact)
    }

    /// Feeds one event; the verdicts it replied.
    fn step<I: Interpreter<State = S>>(
        &mut self,
        event: Option<Event>,
    ) -> Vec<(Vec<u8>, wire::SendInputResponse)> {
        let Some(event) = event else {
            let bytes = encode_checkpoint(&self.interpreter);
            let decoded: S = decode_checkpoint(&bytes).expect("checkpoint decodes");
            let (resumed, step) = decoded.resume();
            self.interpreter = resumed;
            self.commit(&step);
            return Vec::new();
        };
        if let Event::Input(input) = &event {
            self.session.update(Msg::Send(input.clone()));
        }
        let event = self.held(event);
        let stepped = I::step(&mut self.interpreter, event);
        self.commit(&stepped.step);
        // The catalogue as a client fetches it once the snapshot names it.
        let named = stepped
            .step
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.catalogue.clone());
        for effect in &stepped.effects {
            if let Effect::WriteBlob { hash, bytes } = effect
                && Some(hash) == named.as_ref()
            {
                self.session.set_catalogue(
                    hash.clone(),
                    prost::Message::decode(bytes.as_slice()).expect("a catalogue"),
                );
            }
        }
        let replies: Vec<_> = stepped
            .effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Reply { input_id, verdict } => Some((input_id, verdict)),
                _ => None,
            })
            .collect();
        for (id, verdict) in &replies {
            self.session
                .update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        replies
    }
}

/// Every answer the card offers, with the note the person may add.
fn offered(card: &AskCard) -> Vec<(String, Answer, &'static str)> {
    let mut answers: Vec<_> = card
        .choices
        .iter()
        .map(|choice| {
            let note = if choice.takes_note {
                "use the fixture"
            } else {
                ""
            };
            (format!("{:?}", choice.outcome), choice.answer.clone(), note)
        })
        .collect();
    if let AskBody::Question(questions) = &card.body {
        let note = |text: &str| {
            if card.question_note {
                text.to_owned()
            } else {
                String::new()
            }
        };
        let first: Vec<QuestionResponse> = questions
            .iter()
            .map(|question| match question.options.is_empty() {
                true => QuestionResponse {
                    other: Some("typed".into()),
                    note: note("a note"),
                    ..QuestionResponse::default()
                },
                false => QuestionResponse {
                    picked: vec![0],
                    note: note("a note"),
                    ..QuestionResponse::default()
                },
            })
            .collect();
        answers.push(("first options".into(), question_answer(card, &first), ""));
        if questions.iter().all(|question| question.allow_other) {
            let typed: Vec<QuestionResponse> = questions
                .iter()
                .map(|_| QuestionResponse {
                    other: Some("something else".into()),
                    ..QuestionResponse::default()
                })
                .collect();
            answers.push(("something else".into(), question_answer(card, &typed), ""));
        }
        // A multi-select takes ticked options and typed words together.
        if questions
            .iter()
            .all(|question| question.allow_other && question.multi_select)
            && questions.iter().all(|question| question.options.len() >= 2)
        {
            let both: Vec<QuestionResponse> = questions
                .iter()
                .map(|_| QuestionResponse {
                    picked: vec![0, 1],
                    other: Some("and something else".into()),
                    ..QuestionResponse::default()
                })
                .collect();
            answers.push((
                "picks and something else".into(),
                question_answer(card, &both),
                "",
            ));
        }
        if card.question_skip {
            let mut skipped = first.clone();
            skipped[0] = QuestionResponse::skip();
            answers.push(("one skipped".into(), question_answer(card, &skipped), ""));
        }
        if card.question_reply {
            let mut so_far = vec![QuestionResponse::skip(); questions.len()];
            so_far[0].picked = first[0].picked.clone();
            so_far[0].other = first[0].other.clone();
            answers.push((
                "reply instead".into(),
                reply_answer(card, "Let's talk it through first", &so_far),
                "",
            ));
        }
    }
    answers
}

fn body_name(body: &AskBody) -> &'static str {
    match body {
        AskBody::Command { .. } => "command",
        AskBody::Edit { .. } => "edit",
        AskBody::Tool { .. } => "tool",
        AskBody::Question(_) => "question",
        AskBody::Plan { .. } => "plan",
        AskBody::Form { .. } => "form",
        AskBody::Link { .. } => "link",
        AskBody::Access { .. } => "access",
        AskBody::Unanswerable { .. } => "unanswerable",
    }
}

/// Drives every fixture of `dir` and answers every open card every way it
/// offers; returns the card bodies it answered.
fn answer_all<I>(kind: Kind, dir: &str) -> BTreeSet<&'static str>
where
    I: Interpreter,
    I::State: Clone,
{
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json").then_some(path)
        })
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut answered = BTreeSet::new();
    for path in &names {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let script = interpret::fixture_script::<I>(path).unwrap();
        let mut driven = Driven::start::<I>(kind, &script.spec);
        let mut tried = BTreeSet::new();
        for (label, event) in script.events {
            driven.step::<I>(event);
            let Some(card) = ask_card(&driven.session) else {
                continue;
            };
            if card.state != CardState::Open || !tried.insert(card.key.clone()) {
                continue;
            }
            for (choice, answer, note) in offered(&card) {
                let at = format!("{dir}/{name} after {label:.80}: {} {choice}", card.key);
                let Some(mut input) = answer_input(&card, &answer, note) else {
                    failures.push(format!("{at}: no input"));
                    continue;
                };
                input.input_id = format!("answer {choice}").into_bytes();
                let mut fork = driven.clone();
                let replies = fork.step::<I>(Some(Event::Input(input.clone())));
                let verdict = replies
                    .iter()
                    .find(|(id, _)| *id == input.input_id)
                    .and_then(|(_, verdict)| verdict.of.clone());
                match verdict {
                    Some(send_input_response::Of::Accepted(_)) => {}
                    other => {
                        failures.push(format!("{at}: not accepted: {other:?}"));
                        continue;
                    }
                }
                if fork
                    .session
                    .open_asks()
                    .iter()
                    .any(|ask| ask.key() == card.key)
                {
                    failures.push(format!("{at}: the ask is still open"));
                    continue;
                }
                answered.insert(body_name(&card.body));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    answered
}

#[test]
fn claude_pty_cards_answer_the_interpreter() {
    let answered = answer_all::<ClaudePty>(Kind::ClaudePty, "claude_pty");
    assert_eq!(
        answered,
        BTreeSet::from(["command", "edit", "form", "link", "plan", "question"]),
        "card bodies answered"
    );
}

#[test]
fn claude_sdk_cards_answer_the_interpreter() {
    let answered = answer_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk");
    assert_eq!(
        answered,
        BTreeSet::from([
            "command", "edit", "form", "link", "plan", "question", "tool"
        ]),
        "card bodies answered"
    );
}

#[test]
fn codex_cards_answer_the_interpreter() {
    let answered = answer_all::<Codex>(Kind::Codex, "codex");
    assert_eq!(
        answered,
        BTreeSet::from([
            "access", "command", "edit", "form", "link", "plan", "question", "tool"
        ]),
        "card bodies answered"
    );
}

/// Every pick the settings view offers once a fixture has played, sent as
/// `setting_input` makes it, to a copy of the interpreter: each must be
/// accepted. A setting the view refuses has no input at all.
fn pick_all<I>(kind: Kind, dir: &str) -> BTreeSet<&'static str>
where
    I: Interpreter,
    I::State: Clone,
{
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json").then_some(path)
        })
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut picked = BTreeSet::new();
    for path in &names {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let script = interpret::fixture_script::<I>(path).unwrap();
        let mut driven = Driven::start::<I>(kind, &script.spec);
        for (_, event) in script.events {
            driven.step::<I>(event);
        }
        let view = settings(&driven.session);
        let mut picks: Vec<(&'static str, SettingChange, bool)> = Vec::new();
        for model in view.models.iter().filter(|model| !model.current) {
            let change = SettingChange::Model(model.value.clone());
            picks.push(("model", change, view.changeable.model == Changeable::Pick));
        }
        for effort in view.efforts.iter().filter(|effort| !effort.current) {
            let change = SettingChange::Effort(effort.value.clone());
            picks.push(("effort", change, view.changeable.effort == Changeable::Pick));
        }
        // A permission or mode the view marks not settable is not offered
        // as a pick; those it offers must be accepted.
        for permission in view
            .permissions
            .iter()
            .filter(|permission| !permission.current && permission.settable)
        {
            let change = SettingChange::Permission(permission.value.clone());
            picks.push((
                "permission",
                change,
                view.changeable.permission == Changeable::Pick,
            ));
        }
        for mode in view
            .modes
            .iter()
            .filter(|mode| !mode.current && mode.settable)
        {
            let change = SettingChange::Mode(mode.value.clone());
            picks.push(("mode", change, view.changeable.mode == Changeable::Pick));
        }
        if view.changeable.permission == Changeable::Cycle {
            picks.push(("cycle", SettingChange::CyclePermission, true));
        }
        for (setting, change, offered) in picks {
            let at = format!("{dir}/{name}: {change:?}");
            let input = setting_input(kind, &change);
            let Some(mut input) = input.filter(|_| offered) else {
                if offered {
                    failures.push(format!("{at}: offered but no input"));
                } else if setting_input(kind, &change).is_some() {
                    failures.push(format!("{at}: refused but has an input"));
                }
                continue;
            };
            input.input_id = format!("pick {change:?}").into_bytes();
            let mut fork = driven.clone();
            let replies = fork.step::<I>(Some(Event::Input(input.clone())));
            let verdict = replies
                .iter()
                .find(|(id, _)| *id == input.input_id)
                .and_then(|(_, verdict)| verdict.of.clone());
            match verdict {
                Some(send_input_response::Of::Accepted(_)) => {
                    picked.insert(setting);
                }
                other => failures.push(format!("{at}: not accepted: {other:?}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    picked
}

#[test]
fn settings_picks_reach_the_interpreter() {
    assert_eq!(
        pick_all::<ClaudePty>(Kind::ClaudePty, "claude_pty"),
        BTreeSet::from(["cycle"]),
        "terminal Claude's picks"
    );
    assert_eq!(
        pick_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk"),
        BTreeSet::from(["effort", "model", "permission"]),
        "headless Claude's picks"
    );
    assert_eq!(
        pick_all::<Codex>(Kind::Codex, "codex"),
        BTreeSet::from(["effort", "mode", "model", "permission"]),
        "Codex's picks"
    );
}

/// Options ticked on a multi-select travel with the words typed as
/// "Something else": neither drops the other on the way to the agent.
#[test]
fn a_multi_select_answer_keeps_its_picks_beside_something_else() {
    let path = fixtures("claude_pty").join("recorded_question_multi_other.json");
    let script = interpret::fixture_script::<ClaudePty>(&path).unwrap();
    let mut driven = Driven::start::<ClaudePty>(Kind::ClaudePty, &script.spec);
    let mut card = None;
    for (_, event) in script.events {
        driven.step::<ClaudePty>(event);
        if let Some(open) = ask_card(&driven.session)
            && let AskBody::Question(questions) = &open.body
            && questions.iter().any(|question| question.multi_select)
        {
            card = Some(open);
            break;
        }
    }
    let card = card.expect("the fixture opens a multi-select question");
    let AskBody::Question(questions) = &card.body else {
        unreachable!()
    };
    let given: Vec<QuestionResponse> = questions
        .iter()
        .map(|question| match question.multi_select {
            true => QuestionResponse {
                picked: vec![0, 1],
                other: Some("and the docs".into()),
                ..QuestionResponse::default()
            },
            false => QuestionResponse {
                picked: vec![0],
                ..QuestionResponse::default()
            },
        })
        .collect();
    let answer = question_answer(&card, &given);
    let Answer::Claude(wire::ClaudeAnswer {
        of: Some(wire::claude_answer::Of::Question(sent)),
        ..
    }) = &answer
    else {
        panic!("{answer:?}");
    };
    let multi = questions
        .iter()
        .position(|question| question.multi_select)
        .unwrap();
    assert_eq!(sent.answers[multi].selected, [0, 1]);
    assert_eq!(sent.answers[multi].other.as_deref(), Some("and the docs"));

    let mut input = answer_input(&card, &answer, "").expect("an input");
    input.input_id = b"both".to_vec();
    let replies = driven.step::<ClaudePty>(Some(Event::Input(input)));
    assert!(
        replies.iter().any(|(id, verdict)| id == b"both"
            && matches!(verdict.of, Some(send_input_response::Of::Accepted(_)))),
        "{replies:?}"
    );
}
