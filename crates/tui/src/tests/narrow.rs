//! At 80 columns, the narrowest terminal people commonly use, no screen
//! loses a word it needs without saying so: key lines keep their way out
//! and drop whole keys, sentences wrap, and rows keep what tells agents
//! apart.

use super::*;

const NARROW: u16 = 80;
const SHORT: u16 = 24;

fn agent_reply(order: u64, words: &str) -> Item {
    use wire::claude_item::Kind as K;
    Item {
        key: format!("r{order}"),
        order,
        revision: order,
        text: words.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

/// The review page's key line keeps "q back", the way off the page, and
/// marks what it had to leave out.
#[test]
fn the_review_pages_key_line_keeps_its_way_out_at_80_columns() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    let (buffer, _) = draw(&mut view, &state, 0, NARROW, SHORT, theme());
    let screen = text(&buffer);
    let keys = screen.lines().last().unwrap_or_default();
    assert!(
        keys.contains("q back") && keys.contains("j/k move"),
        "the review page's keys at 80 columns: {keys:?}"
    );
}

/// The keys reference wraps its words under their column: a rule cut at
/// the edge ("never stops the agent") read as something else.
#[test]
fn the_keys_reference_wraps_at_80_columns() {
    let lines = crate::app::help_lines('a', usize::from(NARROW), theme());
    let shown: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    for line in &shown {
        assert!(
            crate::text::str_width(line) <= usize::from(NARROW) && !line.ends_with('…'),
            "a keys line is cut at 80 columns: {line:?}"
        );
    }
    let all = shown.join(" ");
    let words = all.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains("never stops the agent"),
        "the rule about Esc is cut:\n{}",
        shown.join("\n")
    );
}

/// Terminal Claude's model and effort change by typing its own commands;
/// the one-line notice saying so fits 80 columns whole.
#[test]
fn terminal_claudes_model_hint_fits_80_columns() {
    use ui_view::Changeable;
    let words = crate::words::by_typing_words(
        &Changeable::ByTyping("/model".into()),
        &Changeable::ByTyping("/effort".into()),
    )
    .unwrap_or_default();
    assert!(
        words.contains("/effort <level>") && crate::text::str_width(&words) <= 76,
        "the hint does not fit 80 columns: {words:?}"
    );
}

/// A running step whose permission was answered "always allow" keeps its
/// verb and command at 80 columns; the long grant gives way instead.
#[test]
fn a_step_after_always_allow_keeps_its_command_at_80_columns() {
    use ui_view::{CallPhase, Decision, DecisionView, PermissionGrant, RowKind, Scope};
    let command = "python3 -c 'import time; time.sleep(28)'";
    let mut row = call_row(RowKind::Command {
        command: command.into(),
        state: CallPhase::Running,
        exit_code: None,
        output_head: vec![],
        more_lines: 0,
        output_trimmed: false,
        output_tail: vec![],
        duration_ms: None,
    });
    row.decision = Some(Decision {
        outcome: DecisionView::Allowed,
        granted: Some(PermissionGrant::Claude {
            subjects: vec![command.into()],
            directories: vec![],
            mode: String::new(),
            mode_name: String::new(),
            destination: Scope::Project,
        }),
        note: None,
        elsewhere: false,
    });
    let lines = crate::chat::feed::row_lines(
        &row,
        &crate::chat::feed::Placement::Plain,
        false,
        &Default::default(),
        usize::from(NARROW),
        theme(),
    )
    .map(|drawn| drawn.lines)
    .unwrap_or_default();
    let first: String = lines
        .first()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .unwrap_or_default();
    // The command keeps its place, cut from its start as every step's
    // subject is; it read "●  · always allowed …" with no verb or command.
    assert!(
        first.contains("Running ")
            && first.contains("time.sleep(28)' ·")
            && crate::text::str_width(&first) <= usize::from(NARROW),
        "the running step at 80 columns: {first:?}"
    );
}

/// A folder with no separators to cut at is cut in its middle, so the row
/// still says which agent it is and on which host.
#[test]
fn a_folder_with_no_separators_keeps_kind_and_host_on_its_row() {
    let mut fleet = home_fleet();
    inventory(&mut fleet, host(b"b", "buildbox", Reads::Online));
    let mut row = home_agent(b"d8", "dir8", Phase::Idle, None, now() - 1_000);
    if let wire::inventory_event::Of::Agent(agent) = &mut row {
        agent.host_id = b"b".to_vec();
        agent.kind = Kind::ClaudeSdk as i32;
        agent.cwd = "x".repeat(120);
    }
    inventory(&mut fleet, row);
    let mut home = new_home();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(NARROW, SHORT)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(frame, area, &fleet, &HashMap::new(), None, now(), theme());
        })
        .unwrap();
    let screen = text(terminal.backend().buffer());
    let line = screen
        .lines()
        .find(|line| line.contains("dir8"))
        .unwrap_or_else(|| panic!("no dir8 row:\n{screen}"));
    assert!(
        line.contains("Claude") && line.contains("buildbox") && line.contains('…'),
        "the long folder pushed the agent or its host off the row: {line:?}"
    );
}

/// With the overview open on a narrow terminal the pane lies over the
/// feed's right side; the chat's words wrap to what is left rather than
/// hide under it.
#[test]
fn the_overview_wraps_the_chat_rather_than_hide_its_words() {
    let said = "I'll create all three files in a single patch; the read-only sandbox \
                should prompt for your approval before anything is written.";
    let state = chat(vec![agent_reply(1, said)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.pane_open = true;
    let (buffer, _) = draw(&mut view, &state, 0, NARROW, SHORT, theme());
    let screen = text(&buffer);
    let words = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    for word in said.split_whitespace() {
        assert!(
            words.contains(word),
            "{word:?} is hidden under the overview:\n{screen}"
        );
    }
    assert!(
        words.contains("patch; the read-only sandbox"),
        "the chat's words are not all shown in order:\n{screen}"
    );
}
