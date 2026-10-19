//! How the terminal client lays out text it was handed whole: a sent
//! message's words after its chips, an agent's line breaks, a multi-line
//! answer typed into an ask, a long file path, a long command in a short
//! terminal, and a rename's refusal at 80 columns. Each test reads the
//! drawn cells, so what it asserts is what a person sees.

use ui_view::{AskCard, Choice, Segment};

use super::*;

/// The buffer drawn for a whole chat, one string per row.
fn rows_of(
    view: &mut ChatView,
    state: &SessionState,
    at: i64,
    width: u16,
    height: u16,
) -> Vec<String> {
    let (buffer, _) = draw(view, state, at, width, height, theme());
    text(&buffer).lines().map(str::to_owned).collect()
}

/// A finished reply of exactly `words`.
fn reply(order: u64, words: &str) -> Item {
    use wire::claude_sdk_item::Kind as K;
    Item {
        key: format!("r{order}"),
        order,
        revision: order,
        text: words.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeSdkItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

/// The same, at the boundary that decides it: segment_lines wraps a text
/// segment that follows a chip at the room left on the chip's line for
/// every line, not only the first.
#[test]
fn segment_lines_wraps_later_lines_at_the_full_width() {
    let chip = Segment::Attachment(ui_view::AttachmentView::File(wire::BlobRef {
        name: "a-rather-long-attachment-name.txt".into(),
        size: 1024,
        ..Default::default()
    }));
    let words = Segment::Text(
        " one two three four five six seven eight nine ten eleven twelve thirteen fourteen".into(),
    );
    let lines = crate::chat::rows::segment_lines(
        &[chip, words],
        50,
        ratatui::style::Style::default(),
        theme(),
    );
    let widths: Vec<usize> = lines.iter().map(crate::text::line_width).collect();
    // The chip takes 42 of 50 columns; the words after it should fill the
    // next line to near 50, not 8.
    assert!(
        widths.iter().skip(1).any(|w| *w > 20),
        "every line after the chip's is as narrow as the chip's leftover room: {widths:?}"
    );
}

/// A chip that fills its row leaves the words after it no room there:
/// they start on the next line, and no line is wider than the message.
#[test]
fn words_after_a_chip_that_fills_its_row_start_on_the_next_line() {
    let chip = || {
        Segment::Attachment(ui_view::AttachmentView::File(wire::BlobRef {
            name: "a-rather-long-attachment-name.txt".into(),
            size: 1024,
            ..Default::default()
        }))
    };
    let style = ratatui::style::Style::default();
    let alone = crate::chat::rows::segment_lines(&[chip()], 200, style, theme());
    let full = crate::text::line_width(&alone[0]);
    let lines = crate::chat::rows::segment_lines(
        &[chip(), Segment::Text("x and more".into())],
        full,
        style,
        theme(),
    );
    let widths: Vec<usize> = lines.iter().map(crate::text::line_width).collect();
    assert!(
        widths.iter().all(|width| *width <= full),
        "a line overflows the {full} columns: {widths:?}"
    );
    assert!(
        lines.len() >= 2,
        "the words start on the next line: {widths:?}"
    );

    assert_eq!(
        crate::text::wrap_after("x and more", 0, 20),
        ["", "x and more"]
    );
    assert_eq!(
        crate::text::wrap_after("abcdefghijkl", 0, 5),
        ["", "abcde", "fghij", "kl"]
    );
}

/// An agent's reply written as lines, one item per line with single line
/// breaks, reads as those lines, as the agent's own interface shows it.
///
/// Today both clients read agent text as CommonMark, where a single line
/// break inside a paragraph is a space ("alpha beta gamma"). Whether to
/// keep that is a product decision; this test states the behaviour the
/// testers expected and Claude's and Codex's own interfaces have.
#[test]
fn an_agents_single_line_breaks_stay_line_breaks() {
    let state = chat(vec![reply(1, "alpha\nbeta\ngamma")]);
    let mut view = ChatView::new(b"agent".to_vec(), 1_000, false);
    let rows = rows_of(&mut view, &state, 1_000, 100, 30);
    let screen = rows.join("\n");
    for word in ["alpha", "beta", "gamma"] {
        assert!(
            rows.iter().any(|row| row.trim() == word),
            "{word} is not a line of its own:\n{screen}"
        );
    }
}

/// A multi-line paste into a question's "Something else" field shows its
/// line breaks: what is drawn before Enter is what is sent. Seen: "line
/// one\nline two\n\nline four" drew as "line oneline twoline four".
#[test]
fn a_multi_line_answer_shows_its_line_breaks_before_it_is_sent() {
    let (state, at) = multi_question();
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    for _ in 0..questions[0].options.len() {
        view.key(&state, key(KeyCode::Down), theme());
    }
    view.key(&state, key(KeyCode::Enter), theme());
    view.paste_text(&state, "line one\nline two\n\nline four");
    let screen = screen_of(&mut view, &state, at);
    assert!(
        !screen.contains("oneline") && !screen.contains("twoline"),
        "the answer's lines run together in the field:\n{screen}"
    );
}

/// A file-edit approval shows which file, however deep its folder: a path
/// too long for the box keeps its end (the file's name and the line) and
/// cuts the folder, as home cuts a folder from the left.
#[test]
fn a_long_edit_path_keeps_the_files_name() {
    let path =
        "/Users/jlw/.local/state/amux-qa/20261011-171536/t6/.testnet/net/desk/work/notes.txt";
    let card = AskCard {
        kind: Kind::Codex,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body: AskBody::Edit {
            path: path.into(),
            files: 1,
            added: 1,
            removed: 0,
            lines: vec![],
            line: Some(1),
            reason: String::new(),
            created: Some(true),
        },
        choices: Vec::<Choice>::new(),
        question_note: false,
        question_skip: false,
        question_reply: false,
        stops_turn: true,
        state: CardState::Open,
    };
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(&card);
    // The box's inside at 80 columns: the chat's margins and the box's
    // edges and padding take eight.
    let lines = ask.box_lines(&card, 72, theme());
    let screen: Vec<String> = lines
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    assert!(
        screen.contains("notes.txt"),
        "the file's name is cut off its path:\n{screen}"
    );
}

/// A Codex rule for a long command reads whole in the box, as Codex's own
/// view words it: the choice wraps instead of cutting the command's end,
/// which is what says what the rule covers.
#[test]
fn a_codex_rule_for_a_long_command_reads_whole() {
    let script = format!("echo {} > long.txt && touch END_MARK", "a".repeat(170));
    let answer = |decision: wire::Decision| {
        ui_view::Answer::Codex(wire::CodexAnswer {
            of: Some(wire::codex_answer::Of::Approve(decision as i32)),
            ..Default::default()
        })
    };
    let choice = |outcome, decision| Choice {
        outcome,
        primary: false,
        takes_note: false,
        answer: answer(decision),
    };
    let card = AskCard {
        kind: Kind::Codex,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body: ui_view::AskBody::Command {
            command: script.clone(),
            cwd: "/work".into(),
            reason: String::new(),
            description: String::new(),
        },
        choices: vec![
            choice(ui_view::ChoiceOutcome::AllowOnce, wire::Decision::Approve),
            choice(
                ui_view::ChoiceOutcome::AllowSimilar {
                    prefix: vec![script.clone()],
                },
                wire::Decision::ApproveSimilar,
            ),
            choice(
                ui_view::ChoiceOutcome::Deny { stops: false },
                wire::Decision::Deny,
            ),
        ],
        question_note: false,
        question_skip: false,
        question_reply: false,
        stops_turn: true,
        state: CardState::Open,
    };
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(&card);
    let lines = ask.box_lines(&card, 72, theme());
    let screen: Vec<String> = lines
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let at = screen
        .iter()
        .position(|row| row.contains("2. Yes, and allow commands starting with"))
        .expect("the rule's choice is in the box");
    let rule: String = screen[at..]
        .iter()
        .take_while(|row| !row.contains("3. No"))
        .map(|row| row.trim())
        .collect();
    assert!(
        rule.ends_with("> long.txt && touch END_MARK") && !rule.contains('…'),
        "the rule's command is cut short:\n{}",
        screen.join("\n")
    );
    assert!(
        screen.iter().any(|row| row.contains("3. No")),
        "{}",
        screen.join("\n")
    );
}

/// A permission ask for a long command keeps its choices and its key line
/// on screen in a short terminal: the command is cut harder, not the
/// choices. Seen at 80x18 (the last choice and the key line gone) and
/// 80x14 (every choice gone).
#[test]
fn a_long_commands_choices_stay_on_a_short_screen() {
    let command = format!("echo {}_END > /tmp/qa_t6_out.txt", "a".repeat(500));
    let input = serde_json::json!({
        "command": command,
        "description": "Write a long line to a file",
    });
    let grant = |rules: Vec<String>, directories: Vec<String>| wire::ScopeChoice {
        index: 0,
        grant: Some(wire::ClaudeGrant {
            rules,
            directories,
            destination_word: "localSettings".into(),
            ..Default::default()
        }),
    };
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "ask-1".into(),
            item_key: "call-1".into(),
            body: Some(wire::ask::Body::Permission(wire::PermissionAsk {
                tool_name: "Bash".into(),
                input_json: input.to_string().into_bytes(),
                scopes: vec![
                    grant(vec![format!("Bash({}:*)", &command[..40])], vec![]),
                    grant(vec![], vec!["/Users/jlw/work".into()]),
                ],
                reason: "outside the allowed directories: \
                         /Users/jlw/.local/state/amux-qa/20261011-171536/t6/.testnet/net/desk/work/qa_t6_out.txt"
                    .into(),
                ..Default::default()
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    let card = ask_card(&state).expect("the ask is open");
    // The box numbers the choices and puts the way out last.
    let first = "1. Yes".to_owned();
    let last = format!("{}. No", card.choices.len());
    for (width, height) in [(80, 24), (80, 18), (80, 14)] {
        let mut view = ChatView::new(b"agent".to_vec(), 0, false);
        let screen = rows_of(&mut view, &state, 0, width, height).join("\n");
        assert!(
            screen.contains(&first) && screen.contains(&last),
            "at {width}x{height} the choices ({first:?} … {last:?}) are off the screen:\n{screen}"
        );
        assert!(
            screen.contains("enter choose"),
            "at {width}x{height} the key line is off the screen:\n{screen}"
        );
    }
}

fn fleet_screen_at(view: &mut FleetView, fleet: &FleetState, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, fleet, &HashMap::new(), None, 0, theme());
        })
        .unwrap();
    text(terminal.backend().buffer())
}

/// A rename the name rule refuses says why in words that fit, and keeps
/// the way out (`esc cancel`) on the line, at 80 columns as at 140.
#[test]
fn a_refused_rename_keeps_its_reason_and_way_out_at_80_columns() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(
        &mut fleet,
        agent_row(b"p", "brisk-raven", Phase::Idle, None),
    );
    let mut view = FleetView::default();
    fleet_screen_at(&mut view, &fleet, 80, 24);
    view.key(&fleet, key(KeyCode::Char('r')));
    for c in " X".chars() {
        view.key(&fleet, key(KeyCode::Char(c)));
    }
    view.key(&fleet, key(KeyCode::Enter));
    let wide = fleet_screen_at(&mut view, &fleet, 140, 30);
    assert!(wide.contains("esc cancel"), "{wide}");
    let narrow = fleet_screen_at(&mut view, &fleet, 80, 24);
    assert!(
        narrow.contains("esc cancel"),
        "the way out is gone at 80 columns:\n{narrow}"
    );
    assert!(
        !narrow.contains("di…"),
        "the reason is cut mid-word at 80 columns:\n{narrow}"
    );
}

/// A question card from Codex with an agent's inline code (`q1/README.md`)
/// in its question and options' words.
fn code_question() -> AskCard {
    let option = |label: &str, description: &str| ui_view::OptionView {
        label: label.into(),
        description: description.into(),
        preview: String::new(),
        recommended: false,
    };
    AskCard {
        kind: Kind::Codex,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body: ui_view::AskBody::Question(vec![ui_view::QuestionView {
            header: "Exact target".into(),
            question: "`q1/README.md` is absent; `../plan1/README.md` has two lines \
                       (`UPDATED`, `done`). Which should be used?"
                .into(),
            multi_select: false,
            options: vec![
                option(
                    "Create it",
                    "Create the missing file with exactly one line: `UPDATED`.",
                ),
                option("Use `../plan1`", "Edit the other file instead."),
            ],
            allow_other: true,
            secret: false,
        }]),
        choices: vec![],
        question_note: false,
        question_skip: false,
        question_reply: true,
        stops_turn: true,
        state: CardState::Open,
    }
}

/// A question card draws an agent's inline code as code, as its prose is
/// drawn: the backticks around file names showed as literal characters in
/// the question and in every option.
#[test]
fn a_question_card_draws_inline_code_without_backticks() {
    let card = code_question();
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(&card);
    for width in [72, 40] {
        let lines = ask.box_lines(&card, width, theme());
        let screen: Vec<String> = lines
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let screen = screen.join("\n");
        assert!(
            !screen.contains('`'),
            "backticks drawn at {width}:\n{screen}"
        );
        for words in ["q1/README.md", "UPDATED", "../plan1"] {
            assert!(screen.contains(words), "{words} lost at {width}:\n{screen}");
        }
        let code = lines
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("q1/README.md"))
            .expect("the file name is drawn");
        assert_eq!(
            code.style,
            theme().code(),
            "the file name is not drawn as code"
        );
    }
}

/// Settled: an agent whose only work is in the background reads as idle,
/// and its row counts its background jobs. After an interrupt left a Codex
/// command running, the chat said "1 background job" and the home row
/// said nothing at all.
#[test]
fn an_idle_rows_background_jobs_are_counted_on_it() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    let wire::inventory_event::Of::Agent(mut agent) = agent_row(b"p", "cx-cmd", Phase::Idle, None)
    else {
        unreachable!()
    };
    agent.background_jobs = 1;
    inventory(&mut fleet, wire::inventory_event::Of::Agent(agent.clone()));
    let mut view = FleetView::default();
    let screen = fleet_screen_at(&mut view, &fleet, 80, 24);
    assert!(
        screen.contains("1 background job"),
        "the row does not count its background job:\n{screen}"
    );
    agent.background_jobs = 2;
    inventory(&mut fleet, wire::inventory_event::Of::Agent(agent));
    let screen = fleet_screen_at(&mut view, &fleet, 80, 24);
    assert!(screen.contains("2 background jobs"), "{screen}");
}

/// A multi-select question of three options, each with a description, and
/// Something else, as an agent asks it.
fn toppings() -> SessionState {
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "q-1".into(),
            item_key: "call-q".into(),
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Toppings".into(),
                    question: "Which toppings do you want on the pizza?".into(),
                    options: [
                        ("Cheese", "Mozzarella, melted"),
                        ("Basil", "Fresh leaves, added after baking"),
                        ("Olives", "Black, sliced"),
                    ]
                    .map(|(label, description)| wire::QuestionOption {
                        label: label.into(),
                        description: description.into(),
                        ..Default::default()
                    })
                    .into(),
                    multi_select: true,
                    allow_other: true,
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 2));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    state
}

/// On a short terminal the question card keeps the focused option on
/// screen, wherever the focus moves: at 80x16 and 40x16 it showed the
/// question, "↓ more" and "4. Something else", and options 1 to 3 and the
/// focus marker never, while Up and Down moved a focus nobody could see.
#[test]
fn a_question_card_on_a_short_screen_keeps_the_focused_option_in_sight() {
    let state = toppings();
    for (width, height) in [(80, 16), (40, 16), (80, 14)] {
        let mut view = ChatView::new(b"agent".to_vec(), 0, false);
        let rows = rows_of(&mut view, &state, 0, width, height);
        for (step, label) in [
            "1. [ ] Cheese",
            "2. [ ] Basil",
            "3. [ ] Olives",
            "4. Something else",
        ]
        .iter()
        .enumerate()
        {
            if step > 0 {
                view.key(&state, key(KeyCode::Down), theme());
            }
            let rows = rows_of(&mut view, &state, 0, width, height);
            let screen = rows.join("\n");
            let focused = rows.iter().find(|row| row.contains('›'));
            assert!(
                focused.is_some_and(|row| row.contains(label)),
                "at {width}x{height} the focus on {label:?} is not in sight:\n{screen}"
            );
            assert!(
                screen.contains('╰'),
                "at {width}x{height} the card runs off the screen:\n{screen}"
            );
        }
        drop(rows);
    }
}

/// One question of about 400 characters and three options.
fn long_question() -> (SessionState, String) {
    let question = "We have been weighing two ways to store the session history for a while now. \
        The first keeps every row in one table and is simple to reason about, but it grows \
        without bound and its queries slow down as it does. The second splits rows by day and \
        prunes old days, which is faster but harder to migrate and to test, and there are open \
        questions about whether the choice will be easy to reverse later. Given all of that, \
        which direction should we take for this decision?"
        .to_owned();
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "q-1".into(),
            item_key: "call-q".into(),
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Storage".into(),
                    question: question.clone(),
                    options: ["One table", "Split by day", "Decide later"]
                        .map(|label| wire::QuestionOption {
                            label: label.into(),
                            ..Default::default()
                        })
                        .into(),
                    allow_other: true,
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 2));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    (state, question)
}

/// A long question cut on a small terminal keeps the sentence that asks,
/// says how much is hidden, and shows the rest on `f`: at 80x24 the card
/// ended "which direction should we…", the question itself in the lost
/// tail, with no way to read it.
#[test]
fn a_long_question_on_a_small_screen_keeps_its_ask_and_shows_the_rest_on_f() {
    let (state, question) = long_question();
    let words = |rows: &[String]| -> String {
        rows.iter()
            .map(|row| row.trim_matches(|c: char| c == ' ' || c == '│'))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let whole = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = |rows: &[String]| words(rows).split_whitespace().collect::<Vec<_>>().join(" ");
    for (width, height) in [(80, 20), (40, 30), (80, 16)] {
        let mut view = ChatView::new(b"agent".to_vec(), 0, false);
        rows_of(&mut view, &state, 0, width, height);
        let rows = rows_of(&mut view, &state, 0, width, height);
        let screen = rows.join("\n");
        assert!(
            !flat(&rows).contains(&whole),
            "at {width}x{height} the question is not cut; pick a smaller screen:\n{screen}"
        );
        assert!(
            words(&rows).contains("this decision?") && screen.contains("[Show All]"),
            "at {width}x{height} the cut loses the asking sentence or hides it silently:\n{screen}"
        );
        assert!(screen.contains("1. One table"), "{screen}");
        view.key(&state, key(KeyCode::Char('f')), theme());
        let rows = rows_of(&mut view, &state, 0, width, height);
        assert!(
            flat(&rows).contains(&whole),
            "at {width}x{height} f does not show the whole question:\n{}",
            rows.join("\n")
        );
    }
}

/// Codex asking to apply a patch to three new files, approvable for the
/// session or refused.
fn three_file_patch() -> (SessionState, i64) {
    let (mut state, at) = fixtures::Named::CodexApproval.state();
    let file = |name: &str| wire::FileChange {
        path: format!("/work/{name}"),
        kind: wire::FileChangeKind::Add as i32,
        patch: format!("@@ -0,0 +1,3 @@\n+{name} one\n+{name} two\n+{name} three\n"),
        ..Default::default()
    };
    let agent = state.agent_state();
    state.update(event(session_event::Of::Snapshot(wire::Snapshot {
        kind: Kind::Codex as i32,
        body: wire::CodexSnapshot {
            asks: vec![wire::CodexAsk {
                key: "7".into(),
                item_key: "patch-1".into(),
                body: Some(wire::codex_ask::Body::FileChange(
                    wire::FileChangeApproval {
                        changes: vec![file("one.txt"), file("two.txt"), file("three.txt")],
                        ..Default::default()
                    },
                )),
                decisions: vec![
                    wire::Decision::Approve as i32,
                    wire::Decision::ApproveSession as i32,
                    wire::Decision::Deny as i32,
                ],
            }],
            model: agent.model.clone(),
            ..Default::default()
        }
        .encode_to_vec(),
        phase: Phase::NeedsYou as i32,
        revision: agent.revision + 1,
        catalogue: agent.catalogue.clone(),
        ..wire::Snapshot::default()
    })));
    (state, at)
}

/// A patch approval on a short terminal keeps every choice and its frame on
/// screen, the diff giving way: at 80x16 a three-file patch ran the card
/// off the bottom, with "3. No" and the key line gone and Down moving the
/// highlight there unseen.
#[test]
fn a_patch_card_on_a_short_screen_keeps_its_choices() {
    let (state, at) = three_file_patch();
    assert!(ask_card(&state).is_some(), "the patch ask is open");
    for (width, height) in [(80, 16), (80, 14), (40, 16)] {
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        rows_of(&mut view, &state, at, width, height);
        for down in 0..3 {
            if down > 0 {
                view.key(&state, key(KeyCode::Down), theme());
            }
            let rows = rows_of(&mut view, &state, at, width, height);
            let screen = rows.join("\n");
            assert!(
                screen.contains("1. Yes") && screen.contains("3. No") && screen.contains('╰'),
                "at {width}x{height} the card runs off the screen:\n{screen}"
            );
            assert!(
                rows.iter().any(|row| row.contains('›')),
                "at {width}x{height} the highlight is off the screen:\n{screen}"
            );
        }
    }
}
