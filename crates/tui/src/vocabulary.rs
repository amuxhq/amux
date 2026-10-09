//! The chat vocabulary as terminal components, drawn from authored view
//! values rather than from sessions: every row kind as the feed draws it,
//! each ask body in the composer's box, the queued prompts, the composer's
//! states and the review page, once each. The renderer never sees the
//! provider kind, and ui-view's own goldens prove the projection per kind,
//! so one drawing of each component is the whole terminal claim.
//!
//! The component goldens and the PNG renderer both draw this set.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{Composer, Waiting};
use ui_view::{
    AnswerView, AskBody, AskCard, AskRow, AttachmentView, Away, CallPhase, CardState, Choice,
    ChoiceOutcome, Decision, DecisionView, EditLine, ExploreVerb, FileChangeView, FileRow, Granted,
    LineKind, OptionView, PatchHead, PatchLine, PermissionGrant, PlanVerdict, QuestionView,
    QueuedRow, Reach, Refusal, RefusalReason, Resolution, Row, RowKind, Scope, Segment,
};
use wire::{BlobRef, BoundaryKind, EnvelopeKind, SendState};

use crate::chat::ask::AskUi;
use crate::chat::composer::QueueEntry;
use crate::chat::review::ReviewPage;
use crate::chat::rows::{RowFacts, RowState, row_lines};
use crate::chat::{composer_lines, feed};
use crate::editor::Editor;
use crate::theme::Theme;

/// Columns every component is drawn at.
pub const WIDTH: u16 = 100;
/// Rows the full-screen review page is drawn at.
const PAGE_HEIGHT: u16 = 30;
/// What terminal Claude's interpreter says when Claude shows a tool server's
/// form in its own terminal.
const UNANSWERABLE: &str = "Claude is showing a form from a tool server this build can't read. Attach to Claude's terminal to answer it, or stop the agent.";

/// The whole frames drawn from served hosts, with what each shows; the
/// frames test draws exactly these.
pub const FRAMES: &[(&str, &str)] = &[
    (
        "home",
        "Home at 110 columns as the laptop sees it: a terminal Claude, a headless Claude and a Codex on the desk, every one idle, the one whose turn ended last first, each row with its mark, name, folder and host and how long it has been idle, and under it the first line of what it last said; the first selected, its age giving way to the close mark; the keys under the list.",
    ),
    (
        "home_60col",
        "The same home at 60 columns: rows keep their mark, name, folder and host, and what each last said.",
    ),
    (
        "home_standings",
        "Home with every standing at 110 columns: the top line says the studio is away and counts who is working and who needs you; a headless Claude asking to run a command under Needs you; under Live, an unfolded family whose one-shot child finished, an agent on the offline studio saying so, an idle agent with what it last said and a working one; and the exited Codex folded under Exited.",
    ),
    ("home_standings_80col", "The same home at 80 columns."),
    (
        "hosts",
        "The hosts overlay over that home: every trusted host with how it is reached (the desk direct, this machine, the studio offline), with how to pair another and esc to close.",
    ),
    (
        "chat",
        "A headless Claude chat on the desk read from the laptop: the header with its folder and the way to the diff and home, the first turn, the composer with its model and mode, and its keys.",
    ),
    (
        "ask_escape",
        "A terminal Claude on the desk showing a tool server's sign-in dialog in its own terminal, read from the laptop: the escape in the composer's box, its reason sending the person to Claude's own terminal (a remote agent's terminal is not offered here) and ctrl+x to stop, with the conversation still above it: the prompt, the text that introduced the tool server's call, and the call folded.",
    ),
    (
        "running_call",
        "A terminal Claude on the desk mid-call, read from the laptop: the text that introduced the call above it and the call drawn running while it runs, before any result.",
    ),
    (
        "rewind_before_swap",
        "The same headless chat after the desk rewound under it: the Reset is pending, the second turn's rows stay on screen, and the header and the composer's edge say the host is away until the rebuilt transcript catches up.",
    ),
    (
        "rewind_after_swap",
        "After the CaughtUp: the rebuilt transcript swapped in, the rewound turn gone, and the composer live again.",
    ),
];

/// The chat surroundings' frames: columns and rows.
const CHAT_SIZE: (u16, u16) = (120, 28);

/// One named component drawn in `theme`.
pub struct Component {
    pub name: &'static str,
    /// What it shows, for the index of goldens.
    pub shows: &'static str,
    pub buffer: Buffer,
}

/// Every component, in catalogue order.
pub fn components(theme: Theme) -> Vec<Component> {
    let w = usize::from(WIDTH);
    let mut out = Vec::new();
    let mut add = |name, shows, lines: Vec<Line<'static>>| {
        out.push(Component {
            name,
            shows,
            buffer: paint_lines(lines),
        });
    };
    for (name, shows, rows) in row_sets() {
        let mut lines = Vec::new();
        for (row, state) in &rows {
            lines.extend(feed_lines(row, *state, w, theme));
        }
        add(name, shows, lines);
    }
    for (name, shows, card, attach, keys) in cards() {
        let mut ui = AskUi::default();
        ui.sync(&card);
        // Room for the whole ask, as on a tall terminal.
        ui.set_room(60, attach);
        for key in keys {
            ui.box_key(&card, key);
        }
        add(name, shows, ui.box_lines(&card, BOX_WIDTH, theme).lines);
    }
    add(
        "queue",
        "Prompts waiting above the composer: one queued, one waiting for its host, one that may not have arrived, and the queued one highlighted, its controls in place of how it waits.",
        queue(w, theme),
    );
    for (name, shows, composer, reach, draft) in composers() {
        let mut editor = Editor::default();
        editor.set(draft, Vec::new());
        let lines = composer_lines(&editor, &composer, "fixer", "studio", reach, w, theme);
        add(name, shows, lines);
    }
    for (name, shows, buffer) in chat_surroundings(theme)
        .into_iter()
        .chain(chat_controls(theme))
    {
        out.push(Component {
            name,
            shows,
            buffer,
        });
    }
    out.push(Component {
        name: "home_rows",
        shows: "Home with a row in every state, each with its name (word pairs where amux named the agent), its branch where it works in a repository, and its second line: a family placed under Needs you by its child's question, a command asked for with another ask behind it; under Live a step being run, what an idle agent last said, a signed-out and a usage-limited agent, a starting agent with nothing to say yet, and an agent on a host that is away; under Exited, unfolded, one that finished and one that failed with its cause.",
        buffer: home_rows(theme),
    });
    out.push(Component {
        name: "review_page",
        shows: "The full-screen review page over a working-tree diff: the file list beside one stream of files and hunks, added and removed lines tinted, a saved comment under its line and the comment editor open on another.",
        buffer: review_page(theme),
    });
    out
}

/// A row as the feed draws it outside a run: the agent's and the
/// person's words, turn ends, asks and steps in the feed's own drawing
/// (thinking draws nothing), everything else as a row; a focused one with
/// its bar.
fn feed_lines(row: &Row, state: RowState, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let feeds = matches!(
        row.kind,
        RowKind::Prompt { .. }
            | RowKind::Prose { .. }
            | RowKind::Thinking { .. }
            | RowKind::TurnEnd { .. }
            | RowKind::Stopped
            | RowKind::Ask(_)
    ) || feed::is_step(row);
    if !feeds {
        return row_lines(row, state, width, theme);
    }
    let Some(drawn) = feed::row_lines(
        row,
        &feed::Placement::Plain,
        state.expanded,
        &facts(row),
        width,
        theme,
    ) else {
        return Vec::new();
    };
    let mut lines = drawn.lines;
    if state.focused {
        for line in &mut lines {
            line.spans.insert(0, Span::styled("▌", theme.focus_bar()));
            if let Some(second) = line.spans.get_mut(1)
                && second.content.starts_with(' ')
            {
                second.content = second.content[1..].to_owned().into();
            }
        }
    }
    lines
}

/// The width an ask's box draws its words in, inside its frame and margins.
const BOX_WIDTH: usize = WIDTH as usize - 8;

fn queue(width: usize, theme: Theme) -> Vec<Line<'static>> {
    let entries = [
        QueueEntry::Queued(QueuedRow {
            input_id: vec![1],
            text: text("Then run the relay tests."),
            from_agent: None,
            steered: false,
            can_withdraw: true,
            can_send_now: true,
        }),
        QueueEntry::Sending {
            input_id: vec![2],
            text: text("Check the deployment once."),
            waiting: Some("laptop".into()),
        },
        QueueEntry::Unconfirmed {
            input_id: vec![3],
            text: text("Stop the server."),
        },
    ];
    let mut lines: Vec<Line<'static>> = entries
        .iter()
        .map(|entry| feed::queued_line(entry, false, width, theme).line)
        .collect();
    lines.push(Line::default());
    lines.push(feed::queued_line(&entries[0], true, width, theme).line);
    lines
}

fn paint_lines(lines: Vec<Line<'static>>) -> Buffer {
    let height = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("test terminal");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Rows the home component is drawn at: tall enough for a blank line
/// between rows.
const HOME_HEIGHT: u16 = 48;

/// Home over an authored fleet: one agent in each state, with what its
/// session says for its second line.
fn home_rows(theme: Theme) -> Buffer {
    use std::collections::HashMap;

    use ui_state::{Activity, ActivityKind, Connection, FleetMsg, FleetState};
    use ui_view::{ActivityLine, AskSubject, AskSummary, SessionLine, StuckReason};
    use wire::inventory_event::Of;
    use wire::{Kind, Lifecycle, Phase, Presence};

    const NOW: i64 = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;
    let mut fleet = FleetState::new();
    let mut event = |of| {
        fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(of),
        })))
    };
    for (id, name, presence) in [
        (b"desk".as_slice(), "desk", Presence::Online),
        (b"studio".as_slice(), "studio", Presence::Offline),
    ] {
        event(Of::Host(wire::HostEntry {
            host_id: id.to_vec(),
            name: name.into(),
            trust: wire::Trust::Trusted as i32,
            presence: presence as i32,
            via: wire::HostVia::Direct as i32,
            ..Default::default()
        }));
    }
    let mut lines = HashMap::new();
    let line = |ask: Option<AskSubject>, step: Option<&str>, said: Option<&str>| SessionLine {
        ask: ask.map(|subject| AskSummary { subject, count: 1 }),
        step: step.map(|step| ActivityLine {
            activity: Activity {
                kind: ActivityKind::Running { key: "call".into() },
                since_ms: NOW,
                elapsed_ms: 0,
            },
            step: Some(step.to_owned()),
        }),
        last_said: said.map(str::to_owned),
        stuck: None,
    };
    #[allow(clippy::type_complexity)]
    let agents: [(
        &str,
        &str,
        Kind,
        Phase,
        Option<&str>,
        i64,
        Option<&str>,
        Option<SessionLine>,
    ); 12] = [
        (
            "planner",
            "desk",
            Kind::ClaudeSdk,
            Phase::Idle,
            Some("specs"),
            50,
            None,
            None,
        ),
        (
            "nimble-wren",
            "desk",
            Kind::ClaudeSdk,
            Phase::NeedsYou,
            Some("specs"),
            12,
            Some("planner"),
            Some(line(
                Some(AskSubject::Question {
                    question: "Should the old specs be deleted or kept?".into(),
                    count: 1,
                }),
                None,
                None,
            )),
        ),
        (
            "brisk-otter",
            "desk",
            Kind::ClaudeSdk,
            Phase::NeedsYou,
            Some("fix-auth"),
            3,
            None,
            Some(SessionLine {
                ask: Some(AskSummary {
                    subject: AskSubject::Command {
                        command: "cargo test -p auth".into(),
                    },
                    count: 2,
                }),
                ..SessionLine::default()
            }),
        ),
        (
            "quiet-heron",
            "desk",
            Kind::Codex,
            Phase::Working,
            Some("relay-retry"),
            1,
            None,
            Some(line(None, Some("cargo test -p relay"), None)),
        ),
        (
            "fixer",
            "desk",
            Kind::ClaudePty,
            Phase::Idle,
            Some("main"),
            8,
            None,
            Some(line(
                None,
                None,
                Some("The relay now retries after a dropped link."),
            )),
        ),
        (
            "amber-finch",
            "desk",
            Kind::Codex,
            Phase::Idle,
            Some("main"),
            20,
            None,
            Some(SessionLine {
                stuck: Some(StuckReason::SignedOut {
                    state: wire::SignInState::SignedOut,
                    account: String::new(),
                }),
                ..SessionLine::default()
            }),
        ),
        (
            "tidy-lynx",
            "desk",
            Kind::ClaudeSdk,
            Phase::Idle,
            None,
            30,
            None,
            Some(SessionLine {
                stuck: Some(StuckReason::UsageLimit { resets_at_ms: None }),
                ..SessionLine::default()
            }),
        ),
        (
            "pale-moth",
            "desk",
            Kind::ClaudeSdk,
            Phase::Starting,
            Some("main"),
            0,
            None,
            None,
        ),
        (
            "archivist",
            "studio",
            Kind::ClaudePty,
            Phase::Idle,
            Some("logs"),
            90,
            None,
            None,
        ),
        (
            "doc-sweep",
            "desk",
            Kind::ClaudeSdk,
            Phase::Idle,
            Some("docs"),
            60,
            None,
            None,
        ),
        (
            "crasher",
            "desk",
            Kind::Codex,
            Phase::Idle,
            None,
            70,
            None,
            None,
        ),
        (
            "clean-up",
            "desk",
            Kind::Codex,
            Phase::Idle,
            Some("main"),
            600,
            None,
            None,
        ),
    ];
    for (name, host, kind, phase, branch, minutes, parent, said) in agents {
        let exit_cause = match name {
            "doc-sweep" => Some((wire::ExitCause::Finished, None)),
            "crasher" => Some((wire::ExitCause::ProviderExited, Some("exit code 1"))),
            "clean-up" => Some((wire::ExitCause::Stopped, None)),
            _ => None,
        };
        let agent = wire::Agent {
            agent_id: name.as_bytes().to_vec(),
            host_id: host.as_bytes().to_vec(),
            kind: kind as i32,
            name: name.into(),
            cwd: "/home/sam/src/amux".into(),
            lifecycle: if exit_cause.is_some() {
                Lifecycle::Exited
            } else {
                Lifecycle::Live
            } as i32,
            exit_cause: exit_cause.map_or(0, |(cause, _)| cause as i32),
            exit_detail: exit_cause.and_then(|(_, detail)| detail.map(str::to_owned)),
            phase: phase as i32,
            phase_since_ms: NOW - minutes * MINUTE,
            parent: parent.map(|parent| wire::AgentParent {
                host_id: host.as_bytes().to_vec(),
                agent_id: parent.as_bytes().to_vec(),
            }),
            git: Some(wire::Git {
                branch: branch.map(str::to_owned),
                ..Default::default()
            }),
            incarnation: 1,
            ..Default::default()
        };
        if let Some(said) = said {
            lines.insert(ui_state::agent_key(&agent), said);
        }
        event(Of::Agent(agent));
    }
    event(Of::CaughtUp(wire::CaughtUp { revision: 0 }));
    fleet.update(FleetMsg::Connection(Connection::Live));

    let defaults = crate::setup::Defaults::default();
    let place = crate::home::Place {
        local_host: b"desk",
        version: "",
        working_dir: "~/src/amux",
        attach: false,
        chat_in: crate::setup::ChatIn::Amux,
        defaults: &defaults,
    };
    let mut home = crate::home::Home::default();
    // Exited unfolded, as the person would open it; the highlight back on
    // the first agent.
    let key = crossterm::event::KeyEvent::from;
    home.key(&fleet, key(crossterm::event::KeyCode::End), false);
    home.key(&fleet, key(crossterm::event::KeyCode::Enter), false);
    home.select(ui_state::agent_key(fleet.find(b"planner").expect("listed")));
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HOME_HEIGHT)).expect("test terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(frame, area, &fleet, &lines, None, NOW, theme, &place);
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn review_page(theme: Theme) -> Buffer {
    let patch = "\
diff --git a/src/sum.rs b/src/sum.rs
index 1111111..2222222 100644
--- a/src/sum.rs
+++ b/src/sum.rs
@@ -1,5 +1,6 @@
 fn main() {
     let a = 1;
-    let b = 2;
-    println!(\"{}\", a + b);
+    let b = 3;
+    let c = 4;
+    println!(\"{}\", a + b + c);
 }
diff --git a/NOTES.md b/NOTES.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/NOTES.md
@@ -0,0 +1 @@
+Remember the sum changed.
";
    let diff = wire::Diff {
        patch: Some(BlobRef {
            hash: vec![7; 32],
            name: "working-tree.diff".into(),
            mime: "text/x-diff".into(),
            size: patch.len() as u64,
        }),
        base: Some(wire::DiffBase {
            base: Some(wire::diff_base::Base::WorkingTree(wire::Empty {})),
        }),
        head: "0463fb2c".into(),
        merge_base: None,
        files: vec![
            wire::DiffFile {
                path: "src/sum.rs".into(),
                added: 3,
                removed: 2,
                change: wire::DiffFileChange::Changed as i32,
                binary: false,
            },
            wire::DiffFile {
                path: "NOTES.md".into(),
                added: 1,
                removed: 0,
                change: wire::DiffFileChange::Created as i32,
                binary: false,
            },
        ],
    };
    let mut page = ReviewPage::new(diff, patch.into());
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('c')));
    for c in "why 3? the spec says b stays 2".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    page.key(key(KeyCode::Enter));
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('c')));
    for c in "and c".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, PAGE_HEIGHT)).expect("test terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            page.draw(frame, area, None, theme);
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

// --- rows ------------------------------------------------------------------

fn row(kind: RowKind) -> Row {
    Row {
        id: "k".into(),
        order: 1,
        at_ms: written_at(),
        kind,
        run: None,
        collapsed: false,
        decision: None,
        attention: false,
        parent: None,
    }
}

/// When every row was written: 14:07, which the crate's tests read in UTC.
fn written_at() -> i64 {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(2026, 1, 15, 14, 7, 0)
        .unwrap()
        .timestamp_millis()
}

fn text(words: &str) -> Vec<Segment> {
    vec![Segment::Text(words.into())]
}

const CLOSED: RowState = RowState {
    focused: false,
    expanded: false,
    rail: false,
    joined: false,
};
const OPEN: RowState = RowState {
    focused: false,
    expanded: true,
    rail: false,
    joined: false,
};
const FOCUSED: RowState = RowState {
    focused: true,
    expanded: false,
    rail: false,
    joined: false,
};

/// What the layout would look up for an authored row: the first edit's
/// landed patch.
fn facts(row: &Row) -> RowFacts {
    let mut facts = RowFacts::default();
    if let RowKind::FileChange { files, state } = &row.kind
        && *state == CallPhase::Succeeded
        && files
            .first()
            .is_some_and(|file| file.path == "crates/tui/src/fleet.rs")
    {
        let line = |number, kind, text: &str| PatchLine {
            number: Some(number),
            kind,
            text: text.into(),
        };
        facts.patch = Some(PatchHead {
            lines: vec![
                line(
                    41,
                    LineKind::Context,
                    "    fn redraw(&mut self, frame: &mut Frame) {",
                ),
                line(42, LineKind::Removed, "        self.paint_all(frame);"),
                line(42, LineKind::Added, "        if self.dirty {"),
                line(43, LineKind::Added, "            self.paint_all(frame);"),
                line(44, LineKind::Added, "        }"),
            ],
            more: 10,
        });
    }
    facts
}

fn decided(
    mut row: Row,
    outcome: DecisionView,
    granted: Option<PermissionGrant>,
    note: Option<&str>,
) -> Row {
    row.decision = Some(Decision {
        outcome,
        granted,
        note: note.map(Into::into),
        elsewhere: false,
    });
    row
}

type RowSet = (&'static str, &'static str, Vec<(Row, RowState)>);

#[allow(clippy::too_many_lines)]
fn row_sets() -> Vec<RowSet> {
    let image = BlobRef {
        hash: vec![1; 32],
        name: "screen.png".into(),
        mime: "image/png".into(),
        size: 48_213,
    };
    let command = |state, exit_code, output: &[&str], more| {
        row(RowKind::Command {
            command: "cargo test -p store".into(),
            state,
            exit_code,
            output_head: output.iter().map(|s| (*s).into()).collect(),
            more_lines: more,
            output_trimmed: false,
            output_tail: output.iter().map(|s| (*s).into()).collect(),
            duration_ms: Some(12_400),
        })
    };
    let question = |multi| QuestionView {
        header: if multi { "Logging" } else { "Rollout" }.into(),
        question: if multi {
            "What should be logged?"
        } else {
            "How should the migration roll out?"
        }
        .into(),
        multi_select: multi,
        options: vec![
            OptionView {
                label: "Behind a flag".into(),
                description: "Off by default for one release".into(),
                preview: String::new(),
                recommended: true,
            },
            OptionView {
                label: "All at once".into(),
                description: String::new(),
                preview: String::new(),
                recommended: false,
            },
        ],
        allow_other: true,
        secret: false,
    };
    let answer = |picked: &[&str], other: Option<&str>| AnswerView {
        picked: picked.iter().map(|s| (*s).into()).collect(),
        other: other.map(Into::into),
        hidden: false,
        note: None,
        skipped: picked.is_empty() && other.is_none(),
    };
    let mut attention = row(RowKind::Prose {
        text: text("Waiting on you above."),
        streaming: false,
        working_note: false,
    });
    attention.attention = true;
    vec![
        (
            "row_prompt",
            "A prompt with an image chip and a review chip at their places, and a steered prompt.",
            vec![
                (
                    row(RowKind::Prompt {
                        text: vec![
                            Segment::Text("Why does the fleet flicker? See ".into()),
                            Segment::Attachment(AttachmentView::Image(image.clone())),
                            Segment::Text(" and my notes ".into()),
                            Segment::Attachment(AttachmentView::Review {
                                patch: None,
                                comments: 2,
                            }),
                        ],
                        steered: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prompt {
                        text: text("Also check the relay path."),
                        steered: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_prose",
            "A finished reply with markdown, a reply still streaming, a working note set apart from the answer, a focused row and a row flagged for attention.",
            vec![
                (
                    row(RowKind::Prose {
                        text: text(
                            "The flicker comes from **two redraws** per tick:\n\n- the fleet repaints on every inventory event\n- the header repaints on the clock\n\nRun `just test-tui` after the fix.",
                        ),
                        streaming: false,
                        working_note: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("Checking the redraw path now"),
                        streaming: true,
                        working_note: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("I'll read the fleet module first."),
                        streaming: false,
                        working_note: true,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("A focused reply."),
                        streaming: false,
                        working_note: false,
                    }),
                    FOCUSED,
                ),
                (attention, CLOSED),
            ],
        ),
        (
            "row_thinking",
            "Thinking is never drawn, closed or opened, with its duration or without.",
            vec![
                (
                    row(RowKind::Thinking {
                        text: "The redraw happens twice because the tick and the event both mark the screen dirty.".into(),
                        open: false,
                        duration_ms: Some(4_200),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Thinking {
                        text: "The redraw happens twice because the tick and the event both mark the screen dirty.".into(),
                        open: false,
                        duration_ms: Some(4_200),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_tool_call",
            "A tool-server call running, one allowed for the session with its decision on the row, one denied with a note, and one failed, opened to its result.",
            vec![
                (
                    row(RowKind::ToolCall {
                        server: "github".into(),
                        tool: "create_issue".into(),
                        fact: "jlw/amux".into(),
                        state: CallPhase::Running,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::ToolCall {
                            server: String::new(),
                            tool: "WebFetch".into(),
                            fact: "docs.rs/ratatui".into(),
                            state: CallPhase::Succeeded,
                            result: String::new(),
                        }),
                        DecisionView::Allowed,
                        Some(PermissionGrant::Session),
                        None,
                    ),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::ToolCall {
                            server: "linear".into(),
                            tool: "delete_issue".into(),
                            fact: "FOX-12".into(),
                            state: CallPhase::Denied,
                            result: String::new(),
                        }),
                        DecisionView::Denied,
                        None,
                        Some("never delete issues"),
                    ),
                    CLOSED,
                ),
                (
                    row(RowKind::ToolCall {
                        server: "github".into(),
                        tool: "get_pr".into(),
                        fact: "#412".into(),
                        state: CallPhase::Failed,
                        result: "404 Not Found".into(),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_file_change",
            "An edit across two files with counts, a created file auto-approved, and a deletion with a move waiting for permission.",
            vec![
                (
                    row(RowKind::FileChange {
                        files: vec![
                            FileRow {
                                path: "crates/tui/src/fleet.rs".into(),
                                change: FileChangeView::Edited,
                                added: 12,
                                removed: 3,
                                line: None,
                            },
                            FileRow {
                                path: "crates/tui/src/app.rs".into(),
                                change: FileChangeView::Edited,
                                added: 1,
                                removed: 1,
                                line: None,
                            },
                        ],
                        state: CallPhase::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::FileChange {
                            files: vec![FileRow {
                                path: "NOTES.md".into(),
                                change: FileChangeView::Created { lines: 14 },
                                added: 14,
                                removed: 0,
                                line: None,
                            }],
                            state: CallPhase::Succeeded,
                        }),
                        DecisionView::AutoApproved,
                        None,
                        None,
                    ),
                    CLOSED,
                ),
                (
                    row(RowKind::FileChange {
                        files: vec![
                            FileRow {
                                path: "old.txt".into(),
                                change: FileChangeView::Deleted,
                                added: 0,
                                removed: 9,
                                line: None,
                            },
                            FileRow {
                                path: "a.rs".into(),
                                change: FileChangeView::Moved { to: "b.rs".into() },
                                added: 0,
                                removed: 0,
                                line: None,
                            },
                        ],
                        state: CallPhase::Asking,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_command",
            "A command waiting for the person's permission, one running, one denied with a note, one succeeded with its output head, three allowed with what each allowance granted from then on (Claude's rule saved in the project, Codex's command prefix and its network hosts), one failed with its exit code, opened.",
            vec![
                (
                    Row {
                        attention: true,
                        ..command(CallPhase::Asking, None, &[], 0)
                    },
                    CLOSED,
                ),
                (command(CallPhase::Running, None, &[], 0), CLOSED),
                (
                    decided(
                        command(CallPhase::Denied, None, &[], 0),
                        DecisionView::Denied,
                        None,
                        Some("Use cargo clean instead"),
                    ),
                    CLOSED,
                ),
                (
                    command(
                        CallPhase::Succeeded,
                        Some(0),
                        &["running 42 tests", "test result: ok. 42 passed"],
                        0,
                    ),
                    CLOSED,
                ),
                (
                    decided(
                        command(CallPhase::Succeeded, Some(0), &[], 0),
                        DecisionView::Allowed,
                        Some(PermissionGrant::Claude {
                            subjects: vec!["cargo test".into()],
                            directories: vec![],
                            mode: String::new(),
                            mode_name: String::new(),
                            saved_to: Scope::Project,
                        }),
                        None,
                    ),
                    CLOSED,
                ),
                (
                    decided(
                        command(CallPhase::Succeeded, Some(0), &[], 0),
                        DecisionView::Allowed,
                        Some(PermissionGrant::CommandPrefix {
                            words: vec!["cargo".into(), "test".into()],
                        }),
                        None,
                    ),
                    CLOSED,
                ),
                (
                    decided(
                        command(CallPhase::Succeeded, Some(0), &[], 0),
                        DecisionView::Allowed,
                        Some(PermissionGrant::NetworkHosts {
                            hosts: vec!["crates.io".into()],
                        }),
                        None,
                    ),
                    CLOSED,
                ),
                (
                    command(
                        CallPhase::Failed,
                        Some(101),
                        &[
                            "running 42 tests",
                            "test reopen ... FAILED",
                            "failures: reopen",
                        ],
                        12,
                    ),
                    OPEN,
                ),
            ],
        ),
        (
            "row_explore",
            "Exploration steps: a read, a search, a listing, a fetch and a web search.",
            vec![
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Read,
                        subject: "crates/tui/src/fleet.rs".into(),
                        state: CallPhase::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Search,
                        subject: "redraw".into(),
                        state: CallPhase::Running,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::List,
                        subject: "crates/".into(),
                        state: CallPhase::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Fetch,
                        subject: "https://docs.rs/ratatui".into(),
                        state: CallPhase::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::WebSearch,
                        subject: "ratatui flicker".into(),
                        state: CallPhase::Succeeded,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_subagent",
            "A subagent at work with its last tool, then one finished with its answer, opened.",
            vec![
                (
                    row(RowKind::Subagent {
                        description: "Survey the redraw paths".into(),
                        running: true,
                        tool_count: 7,
                        last_tool: "Read crates/tui/src/app.rs".into(),
                        answer: String::new(),
                        duration_ms: None,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Subagent {
                        description: "Survey the redraw paths".into(),
                        running: false,
                        tool_count: 12,
                        last_tool: String::new(),
                        answer: "Two paths redraw: the tick and inventory events.".into(),
                        duration_ms: Some(95_000),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_background",
            "A background command still running and one that ended.",
            vec![
                (
                    row(RowKind::Background {
                        command: "npm run dev".into(),
                        running: true,
                        duration_ms: None,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Background {
                        command: "cargo watch".into(),
                        running: false,
                        duration_ms: Some(240_000),
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_image",
            "An image the agent read and one it generated.",
            vec![
                (
                    row(RowKind::Image {
                        image: Some(image.clone()),
                        path: "docs/screen.png".into(),
                        generated: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Image {
                        image: None,
                        path: "out/logo.png".into(),
                        generated: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_slash_output",
            "A slash command's output, the same closed or opened.",
            vec![
                (
                    row(RowKind::SlashOutput {
                        command: "/cost".into(),
                        args: String::new(),
                        output: "Total cost: $0.42\nTotal duration: 3m 12s".into(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::SlashOutput {
                        command: "/cost".into(),
                        args: String::new(),
                        output: "Total cost: $0.42\nTotal duration: 3m 12s".into(),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_ask",
            "Asks that became rows: a question answered with a note, several questions answered, one of two skipped with a note on it, a reply instead quoting the person's words with the answer given before it, a plan approved and one sent back with its note, a form sent with the fields it carried, a link declined, access granted for the turn, an open question, and two dismissed.",
            vec![
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false)],
                        answers: vec![AnswerView {
                            note: Some("flip it on for the desk first".into()),
                            ..answer(&["Behind a flag"], None)
                        }],
                        skipped: 0,
                        reply: None,
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false), question(true)],
                        answers: vec![
                            answer(&["All at once"], None),
                            AnswerView {
                                note: Some("keep the old strings for one release".into()),
                                ..answer(&[], Some("Only failures, with the host id"))
                            },
                        ],
                        skipped: 0,
                        reply: None,
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false), question(true)],
                        answers: vec![
                            answer(&["Behind a flag"], None),
                            AnswerView {
                                note: Some("ask me again once the flag ships".into()),
                                ..answer(&[], None)
                            },
                        ],
                        skipped: 1,
                        reply: None,
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false), question(true)],
                        answers: vec![answer(&["All at once"], None), answer(&[], None)],
                        skipped: 1,
                        reply: Some("Neither yet: show me what the flag would cost first.".into()),
                        resolution: Resolution::Replied,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Plan {
                        title: None,
                        body: "Collapse the pairing failures into one error.".into(),
                        verdict: PlanVerdict::Approved,
                        writing: false,
                        note: None,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Plan {
                        title: None,
                        body: "Rename every wire code.".into(),
                        verdict: PlanVerdict::SentBack,
                        writing: false,
                        note: Some("Don't touch the wire codes yet".into()),
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Form {
                        server: "github".into(),
                        message: "Create the issue in which repository?".into(),
                        fields: vec!["repository".into(), "labels".into(), "assign".into()],
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Link {
                        server: "linear".into(),
                        message: "Sign in to Linear".into(),
                        url: "https://linear.app/oauth".into(),
                        resolution: Resolution::Declined,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Grant {
                        reason: "Write the build output".into(),
                        read: vec![],
                        write: vec!["~/src/amux/target".into()],
                        network: false,
                        hosts: vec![],
                        granted: Some(Granted {
                            read: vec![],
                            write: vec!["~/src/amux/target".into()],
                            network: false,
                            for_session: false,
                        }),
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false)],
                        answers: vec![],
                        skipped: 0,
                        reply: None,
                        resolution: Resolution::Open,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Unanswerable {
                        reason: UNANSWERABLE.into(),
                        resolution: Resolution::Dismissed,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Unanswerable {
                        reason: UNANSWERABLE.into(),
                        resolution: Resolution::Cancelled,
                    })),
                    OPEN,
                ),
            ],
        ),
        (
            "row_turn_end",
            "The quiet line under a finished turn, and a failed turn.",
            vec![
                (
                    row(RowKind::TurnEnd {
                        duration_ms: Some(102_000),
                        cost_usd: Some(0.42),
                        failed: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::TurnEnd {
                        duration_ms: Some(3_000),
                        cost_usd: None,
                        failed: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        ("row_stopped", "You stopped it.", vec![(row(RowKind::Stopped), CLOSED)]),
        (
            "row_compaction",
            "Compaction with its counts, and an automatic one without.",
            vec![
                (
                    row(RowKind::Compaction {
                        tokens_before: Some(148_000),
                        tokens_after: Some(22_000),
                        automatic: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Compaction {
                        tokens_before: None,
                        tokens_after: None,
                        automatic: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_error",
            "An error retried and given up on, and one still being retried.",
            vec![
                (
                    row(RowKind::Error {
                        error_kind: "overloaded".into(),
                        message: "The API is overloaded; try again shortly.".into(),
                        attempts: 10,
                        gave_up: true,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Error {
                        error_kind: "rate_limit".into(),
                        message: "Rate limited.".into(),
                        attempts: 2,
                        gave_up: false,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_model_switch",
            "A model reroute with its reason.",
            vec![(
                row(RowKind::ModelSwitch {
                    from: "gpt-5".into(),
                    to: "gpt-5-mini".into(),
                    reason: "usage limit".into(),
                }),
                CLOSED,
            )],
        ),
        (
            "row_boundary",
            "Session boundaries: started, cleared, compacted, resumed, forked, restarted, exited with its cause and the daemon lost.",
            [
                (BoundaryKind::Started, ""),
                (BoundaryKind::Cleared, ""),
                (BoundaryKind::Compacted, ""),
                (BoundaryKind::Resumed, ""),
                (BoundaryKind::Forked, ""),
                (BoundaryKind::Restarted, ""),
                (BoundaryKind::Exited, "exit 1"),
                (BoundaryKind::DaemonLost, ""),
            ]
            .into_iter()
            .map(|(kind, cause)| {
                (
                    row(RowKind::Boundary {
                        kind,
                        cause: cause.into(),
                    }),
                    CLOSED,
                )
            })
            .collect(),
        ),
        (
            "row_agent_message",
            "Messages between agents: one received, a child's finished report, one sent and delivered, and one the recipient refused.",
            vec![
                (
                    row(RowKind::AgentMessage {
                        from: "planner".into(),
                        kind: EnvelopeKind::Message,
                        text: "Take the relay half.".into(),
                        to: String::new(),
                        sent: SendState::Unspecified,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: "worker-2".into(),
                        kind: EnvelopeKind::Finished,
                        text: "3 specs updated".into(),
                        to: String::new(),
                        sent: SendState::Unspecified,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: String::new(),
                        kind: EnvelopeKind::Message,
                        text: "Start on the store.".into(),
                        to: "worker-1".into(),
                        sent: SendState::Sent,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: String::new(),
                        kind: EnvelopeKind::Message,
                        text: "Stop now.".into(),
                        to: "worker-3".into(),
                        sent: SendState::Rejected,
                        rejection: "exited".into(),
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_auto_review",
            "An automatic reviewer's verdict with its risk, opened to its rationale.",
            vec![(
                row(RowKind::AutoReview {
                    status: wire::ReviewStatus::Approved,
                    status_word: "approved".into(),
                    risk: "low".into(),
                    rationale: "Reads only; no writes outside the workspace.".into(),
                    subject: "k2".into(),
                }),
                OPEN,
            )],
        ),
        (
            "row_unrecognized",
            "A record the interpreter kept without understanding it.",
            vec![(
                row(RowKind::Unrecognized {
                    what: "system/hook_progress".into(),
                    summary: "a hook reported progress".into(),
                }),
                CLOSED,
            )],
        ),
    ]
}

// --- the activity line -----------------------------------------------------

fn choice(outcome: ChoiceOutcome) -> Choice {
    Choice {
        outcome,
        primary: false,
        takes_note: false,
        answer: ui_view::Answer::Claude(wire::ClaudeAnswer::default()),
    }
}

/// One numbered line of an authored edit ask's patch.
fn patch(number: Option<u32>, kind: LineKind, text: &str) -> EditLine {
    EditLine::Line(PatchLine {
        number,
        kind,
        text: text.into(),
    })
}

fn card(body: AskBody, mut choices: Vec<Choice>) -> AskCard {
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    AskCard {
        kind: wire::Kind::ClaudeSdk,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body,
        choices,
        question_note: true,
        question_skip: true,
        question_reply: true,
        stops_turn: true,
        state: CardState::Open,
    }
}

/// A card component: its name, what it shows, the card, whether the
/// agent's terminal can be attached, and the keys pressed on it first.
type CardSet = (&'static str, &'static str, AskCard, bool, Vec<KeyEvent>);

/// The keys of `typed`, one per character; a word in angle brackets is a
/// named key: `<tab>`, `<down>`, `<enter>`, `<esc>`.
fn keys(typed: &str) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    let mut rest = typed;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            out.push(KeyEvent::from(match &rest[1..end] {
                "tab" => KeyCode::Tab,
                "down" => KeyCode::Down,
                "enter" => KeyCode::Enter,
                "esc" => KeyCode::Esc,
                other => panic!("no key named {other}"),
            }));
            rest = &rest[end + 1..];
            continue;
        }
        out.push(KeyEvent::from(KeyCode::Char(c)));
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[allow(clippy::too_many_lines)]
fn cards() -> Vec<CardSet> {
    let command = || AskBody::Command {
        command: "deploy --check".into(),
        cwd: "/workspace".into(),
        reason: "Verify the deploy before tagging".into(),
        description: "Runs the deploy dry run".into(),
    };
    let command_choices = || {
        vec![
            choice(ChoiceOutcome::AllowOnce),
            choice(ChoiceOutcome::AllowAlways {
                subjects: vec!["deploy --check".into()],
                directories: vec![],
                mode: String::new(),
                mode_name: String::new(),
                scope: Scope::Project,
            }),
            choice(ChoiceOutcome::AllowForSession),
            Choice {
                takes_note: true,
                ..choice(ChoiceOutcome::Deny { stops: false })
            },
            choice(ChoiceOutcome::DenyAndStop),
        ]
    };
    let option = |label: &str, description: &str, preview: &str, recommended| OptionView {
        label: label.into(),
        description: description.into(),
        preview: preview.into(),
        recommended,
    };
    let rollout = QuestionView {
        header: "Rollout".into(),
        question: "How should the migration roll out?".into(),
        multi_select: false,
        options: vec![
            option("Behind a flag", "Off by default for one release", "", true),
            option("All at once", "", "", false),
        ],
        allow_other: true,
        secret: false,
    };
    let platforms = QuestionView {
        header: "Platforms".into(),
        question: "Which platforms ship first?".into(),
        multi_select: true,
        options: vec![
            option("macOS", "", "", false),
            option("Linux", "x86_64 and arm64", "", false),
            option("Windows", "", "", false),
        ],
        allow_other: true,
        secret: false,
    };
    let layout = QuestionView {
        header: "Layout".into(),
        question: "Which layout for the host list?".into(),
        multi_select: false,
        options: vec![
            option(
                "Cards",
                "One card per host",
                "┌──────────────────────┐\n│ Studio        ● live │\n│ ~/src/amux  3 agents │\n└──────────────────────┘",
                false,
            ),
            option(
                "Grouped list",
                "",
                "Studio   ● live   3 agents\nLaptop   ○ away   1 agent",
                false,
            ),
        ],
        allow_other: false,
        secret: false,
    };
    let token = QuestionView {
        header: "Token".into(),
        question: "Paste the deploy token".into(),
        multi_select: false,
        options: vec![],
        allow_other: true,
        secret: true,
    };
    let mut sending = card(command(), command_choices());
    sending.state = CardState::Sending;
    let mut rejected = card(command(), command_choices());
    rejected.state = CardState::Rejected(Refusal::of(RefusalReason::ClosedAsk));
    let mut not_confirmed = card(command(), command_choices());
    not_confirmed.state = CardState::NotConfirmed;
    let mut second = card(command(), command_choices());
    second.position = 1;
    second.count = 3;
    vec![
        (
            "ask_command",
            "A command permission in the composer's box: the command and why, every scope stated as what happens, and No with its note; one of three waiting.",
            second,
            false,
            vec![],
        ),
        (
            "ask_edit",
            "A file edit permission with its counts and diff.",
            card(
                AskBody::Edit {
                    path: "crates/tui/src/fleet.rs".into(),
                    files: 1,
                    added: 2,
                    removed: 1,
                    lines: vec![
                        EditLine::Hunk {
                            old_start: 10,
                            new_start: 10,
                        },
                        patch(Some(10), LineKind::Context, "let a = 1;"),
                        patch(Some(11), LineKind::Removed, "let b = 2;"),
                        patch(Some(11), LineKind::Added, "let b = 3;"),
                        patch(Some(12), LineKind::Added, "let c = 4;"),
                    ],
                    line: Some(11),
                    reason: String::new(),
                    created: Some(false),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::AllowForSession),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_tool",
            "A tool-server permission with its arguments.",
            card(
                AskBody::Tool {
                    server: "github".into(),
                    tool: "create_issue".into(),
                    arguments: "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Flicker\"\n}".into(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_codex_command",
            "A Codex command approval: allow similar commands by prefix, a network rule for its hosts, and decline.",
            card(
                AskBody::Command {
                    command: "curl localhost:8080/health".into(),
                    cwd: "/workspace".into(),
                    reason: String::new(),
                    description: String::new(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::AllowSimilar {
                        prefix: vec!["curl".into()],
                    }),
                    choice(ChoiceOutcome::AllowNetwork {
                        hosts: vec!["localhost".into()],
                    }),
                    choice(ChoiceOutcome::Deny { stops: false }),
                    choice(ChoiceOutcome::DenyAndStop),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_question_single",
            "One pick-one question with the recommended option named and Something else.",
            card(AskBody::Question(vec![rollout.clone()]), vec![]),
            false,
            vec![],
        ),
        (
            "ask_question_multi",
            "One multi-select question: square boxes before any pick.",
            card(AskBody::Question(vec![platforms.clone()]), vec![]),
            false,
            vec![],
        ),
        (
            "ask_questions_steps",
            "Several questions: the header chips are the steps, the first open.",
            card(
                AskBody::Question(vec![rollout.clone(), platforms.clone(), token.clone()]),
                vec![],
            ),
            false,
            vec![],
        ),
        (
            "ask_question_previews",
            "A question with previews: the highlighted option's preview beside the options.",
            card(AskBody::Question(vec![layout]), vec![]),
            false,
            vec![],
        ),
        (
            "ask_question_secret",
            "A question whose answer is secret: typed characters show as bullets.",
            card(AskBody::Question(vec![token]), vec![]),
            false,
            vec![],
        ),
        (
            "ask_plan",
            "A plan to approve, approve with edits accepted automatically, or send back with a note.",
            card(
                AskBody::Plan {
                    title: None,
                    body: "\n1. Collapse the pairing failures into one error.\n2. Update the three specs.\n3. Keep the wire codes.".into(),
                },
                vec![
                    choice(ChoiceOutcome::ApprovePlan {
                        auto_accept_edits: false,
                    }),
                    choice(ChoiceOutcome::ApprovePlan {
                        auto_accept_edits: true,
                    }),
                    Choice {
                        takes_note: true,
                        ..choice(ChoiceOutcome::SendBack)
                    },
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_question_note",
            "A question's note being typed after Tab, under its options; Skip and Reply instead below.",
            card(AskBody::Question(vec![rollout.clone()]), vec![]),
            false,
            keys("<down><tab>flip it on for the desk first"),
        ),
        (
            "ask_question_skip",
            "Skip highlighted: Enter leaves the question unanswered.",
            card(AskBody::Question(vec![rollout.clone()]), vec![]),
            false,
            keys("<down><down><down>"),
        ),
        (
            "ask_question_reply",
            "Reply instead open and typed in: Enter sends the words in place of answers.",
            card(AskBody::Question(vec![rollout.clone()]), vec![]),
            false,
            keys("<esc><enter>Neither yet, show me the cost first"),
        ),
        (
            "ask_questions_review_skipped",
            "The review after answering one question and skipping the next with a note on it: the skip reads skipped, and Send answers goes.",
            card(
                AskBody::Question(vec![rollout.clone(), platforms.clone()]),
                vec![],
            ),
            false,
            keys("<enter><tab>after the flag ships<tab><down><down><down><down><enter>"),
        ),
        (
            "ask_question_terminal_claude",
            "Terminal Claude's question: no note, no Skip, only Reply instead below the options.",
            AskCard {
                kind: wire::Kind::ClaudePty,
                question_note: false,
                question_skip: false,
                ..card(AskBody::Question(vec![rollout.clone()]), vec![])
            },
            false,
            vec![],
        ),
        (
            "ask_plan_codex",
            "Codex's plan to implement or keep planning with a note.",
            AskCard {
                kind: wire::Kind::Codex,
                stops_turn: false,
                ..card(
                    AskBody::Plan {
                        title: None,
                        body: "\n1. Collapse the pairing failures into one error.".into(),
                    },
                    vec![
                        choice(ChoiceOutcome::ApprovePlan {
                            auto_accept_edits: false,
                        }),
                        Choice {
                            takes_note: true,
                            ..choice(ChoiceOutcome::SendBack)
                        },
                    ],
                )
            },
            false,
            vec![],
        ),
        (
            "ask_form",
            "A form from a tool server, a step per field in the server's order, to submit or decline.",
            card(
                AskBody::Form {
                    server: "github".into(),
                    message: "Create the issue in which repository?".into(),
                    fields: ui_view::form_fields(br#"{"type":"object","properties":{"repository":{"type":"string","title":"Repository"},"labels":{"type":"string","enum":["bug","ios","docs"],"title":"Labels"},"assign":{"type":"boolean","title":"Assign to me"}},"required":["repository"]}"#),
                },
                vec![
                    choice(ChoiceOutcome::Submit),
                    choice(ChoiceOutcome::Decline),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_link",
            "A tool server asking to open a link.",
            card(
                AskBody::Link {
                    server: "linear".into(),
                    message: "Sign in to Linear".into(),
                    url: "https://linear.app/oauth/authorize".into(),
                },
                vec![
                    choice(ChoiceOutcome::OpenLink),
                    choice(ChoiceOutcome::Decline),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_access",
            "An access grant for files and network, for the turn or the session, or deny.",
            card(
                AskBody::Access {
                    reason: "Write the build output and fetch crates".into(),
                    read: vec!["~/.cargo".into()],
                    write: vec!["~/src/amux/target".into()],
                    network: true,
                    hosts: vec!["crates.io".into()],
                },
                vec![
                    choice(ChoiceOutcome::GrantForTurn),
                    choice(ChoiceOutcome::GrantForSession),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_unanswerable",
            "The escape from an ask this client cannot answer: the reason, and the agent's own terminal where it has one; ctrl+x stops the turn.",
            card(
                AskBody::Unanswerable {
                    reason: UNANSWERABLE.into(),
                },
                vec![],
            ),
            true,
            vec![],
        ),
        (
            "ask_sending",
            "An answer on its way: the box says so and takes no second answer.",
            sending,
            false,
            vec![],
        ),
        (
            "ask_rejected",
            "An answer the agent refused, with its reason.",
            rejected,
            false,
            vec![],
        ),
        (
            "ask_not_confirmed",
            "An answer that was sent but never confirmed, with r to resend and d to discard.",
            not_confirmed,
            false,
            vec![],
        ),
    ]
}

// --- the composer ----------------------------------------------------------

fn composers() -> Vec<(&'static str, &'static str, Composer, Reach, &'static str)> {
    vec![
        (
            "composer_exited",
            "The exited composer: one Enter resumes the agent with the draft as its first prompt.",
            Composer::Resume,
            Reach::Online(wire::HostVia::Direct),
            "Carry on from the failing test.",
        ),
        (
            "composer_exited_empty",
            "The exited composer before anything is typed.",
            Composer::Resume,
            Reach::Online(wire::HostVia::Direct),
            "",
        ),
        (
            "composer_detached",
            "The composer while the agent's host is away: the draft is kept and sending waits.",
            Composer::Disabled(Waiting::Detached),
            Reach::Offline,
            "",
        ),
        (
            "composer_detached_signed_out",
            "The composer while the agent's host is away and this machine is signed out of its account: the cause is this machine's, the draft is kept and sending waits until it signs in.",
            Composer::Disabled(Waiting::Detached),
            Reach::Away(Away::SignedOut),
            "",
        ),
        (
            "composer_detached_revoked",
            "The composer while the agent's host has said it no longer trusts this machine: the draft is kept and sending waits until the two are paired again.",
            Composer::Disabled(Waiting::Detached),
            Reach::Away(Away::Revoked),
            "",
        ),
    ]
}

/// Whole chats around their conversation: the header's change totals in
/// both comparisons with the overview open on the changed files by folder
/// and the background jobs, and the overview's usage windows for each
/// provider.
fn chat_surroundings(theme: Theme) -> Vec<(&'static str, &'static str, Buffer)> {
    use prost::Message as _;
    use ui_state::{Connection, Msg, SessionState};
    use wire::{Kind, session_event};

    use crate::chat::{ChatView, Comparison, FetchKey};

    // A Thursday afternoon in this machine's zone, as the clock reads it.
    let now = written_at();
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;

    let totals = |files, added, removed| wire::ChangeTotals {
        files,
        added,
        removed,
    };
    let git = wire::Git {
        branch: Some("overview-files".into()),
        base_branch: Some("main".into()),
        uncommitted: Some(totals(4, 42, 7)),
        on_branch: Some(totals(6, 120, 30)),
    };
    let file = |path: &str, added, removed, change: wire::DiffFileChange| wire::DiffFile {
        path: path.into(),
        added,
        removed,
        change: change as i32,
        binary: false,
    };
    use wire::DiffFileChange::{Changed, Created, Deleted};
    let uncommitted = wire::Diff {
        files: vec![
            file("crates/ui-view/src/overview.rs", 30, 4, Changed),
            file("README.md", 2, 0, Changed),
            file("crates/tui/src/chat/pane.rs", 9, 3, Changed),
            file("crates/tui/src/chat/changes.rs", 1, 0, Created),
        ],
        ..Default::default()
    };
    let on_branch = wire::Diff {
        files: vec![
            file("crates/ui-view/src/overview.rs", 64, 10, Changed),
            file("README.md", 2, 0, Changed),
            file("crates/tui/src/chat/pane.rs", 25, 8, Changed),
            file("crates/tui/src/chat/changes.rs", 20, 0, Created),
            file("crates/tui/src/strip.rs", 0, 12, Deleted),
            file("docs/OVERVIEW.md", 9, 0, Created),
        ],
        ..Default::default()
    };
    let jobs = wire::BackgroundJobs {
        known: true,
        jobs: vec![
            wire::BackgroundJob {
                step: "t1".into(),
                command: "npm run dev -- --port 5173".into(),
                started_at_ms: now - 14 * MINUTE,
            },
            wire::BackgroundJob {
                step: "t2".into(),
                command: "cargo watch -x 'test -p ui-view'".into(),
                started_at_ms: now - 40_000,
            },
        ],
    };
    let meter = |used_percent, resets_at_ms: i64, state: wire::UsageState| {
        Some(wire::UsageMeter {
            used_percent,
            resets_at_ms: Some(resets_at_ms),
            state: state as i32,
        })
    };
    use wire::UsageState::{NearLimit, Ok};
    let claude_usage = wire::ClaudeUsage {
        state: NearLimit as i32,
        windows: vec![
            wire::ClaudeUsageWindow {
                limit: wire::ClaudeLimit::FiveHour as i32,
                model: None,
                provider_name: "five_hour".into(),
                meter: meter(91.0, now + 2 * HOUR, NearLimit),
            },
            wire::ClaudeUsageWindow {
                limit: wire::ClaudeLimit::Weekly as i32,
                model: None,
                provider_name: "seven_day".into(),
                meter: meter(40.0, now + 72 * HOUR, Ok),
            },
            wire::ClaudeUsageWindow {
                limit: wire::ClaudeLimit::Weekly as i32,
                model: Some("Fable".into()),
                provider_name: "seven_day_overage_included".into(),
                meter: meter(84.0, now + 72 * HOUR, NearLimit),
            },
        ],
    };
    let codex_usage = wire::CodexUsage {
        state: NearLimit as i32,
        windows: vec![
            wire::CodexUsageWindow {
                limit: wire::CodexLimit::FiveHour as i32,
                window_minutes: 300,
                meter: meter(95.0, now + 3 * HOUR, NearLimit),
            },
            wire::CodexUsageWindow {
                limit: wire::CodexLimit::Weekly as i32,
                window_minutes: 10_080,
                meter: meter(30.0, now + 100 * HOUR, Ok),
            },
        ],
        credits: Some("12.50".into()),
    };

    let item = |order: u64, prompt: bool, words: &str, kind: Kind| {
        let body = match kind {
            Kind::Codex => {
                use wire::codex_item::Kind as K;
                wire::CodexItem {
                    kind: Some(if prompt {
                        K::Prompt(wire::Prompt {})
                    } else {
                        K::Message(wire::Text { complete: true })
                    }),
                }
                .encode_to_vec()
            }
            _ => {
                use wire::claude_sdk_item::Kind as K;
                wire::ClaudeSdkItem {
                    kind: Some(if prompt {
                        K::Prompt(wire::Prompt {})
                    } else {
                        K::Message(wire::Text { complete: true })
                    }),
                }
                .encode_to_vec()
            }
        };
        wire::Item {
            key: format!("k{order}"),
            order,
            revision: order,
            text: words.into(),
            kind: kind as i32,
            body,
            at_ms: now - 20 * MINUTE + order as i64 * 1_000,
            ..Default::default()
        }
    };
    let session = |kind: Kind, body: Vec<u8>| {
        let agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            host_id: b"host".to_vec(),
            kind: kind as i32,
            name: "fixer".into(),
            cwd: "/srv/amux".into(),
            lifecycle: wire::Lifecycle::Live as i32,
            phase: wire::Phase::Idle as i32,
            incarnation: 1,
            git: Some(git.clone()),
            ..Default::default()
        };
        let mut state = SessionState::new(agent, crate::chat::layout::CAP as usize);
        let event = |of| Msg::Event(wire::SessionEvent { of: Some(of) });
        state.update(Msg::Connection(Connection::Live));
        state.update(event(session_event::Of::Snapshot(wire::Snapshot {
            kind: kind as i32,
            body,
            phase: wire::Phase::Idle as i32,
            ..Default::default()
        })));
        for item in [
            item(
                1,
                true,
                "Show the changed files by folder in the overview",
                kind,
            ),
            item(
                2,
                false,
                "Done: the overview lists them under their folders, and the dev server and the test watcher keep running.",
                kind,
            ),
        ] {
            state.update(event(session_event::Of::Item(item)));
        }
        state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
            revision: 2,
        })));
        state
    };
    let draw = |state: &SessionState, comparison: Comparison, diff: Option<&wire::Diff>| {
        let mut view = ChatView::new(b"agent".to_vec(), now, false);
        view.pane_open = true;
        view.comparison = comparison;
        if let Some(diff) = diff {
            view.changes_fetched(
                FetchKey {
                    comparison,
                    totals: None,
                },
                diff.clone(),
            );
        }
        let (width, height) = CHAT_SIZE;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                view.draw(frame, area, state, None, None, now, theme);
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    };

    let claude_working = wire::ClaudeSdkSnapshot {
        model: Some("opus".into()),
        model_name: Some("Opus".into()),
        permission: Some("default".into()),
        background_jobs: Some(jobs.clone()),
        ..Default::default()
    }
    .encode_to_vec();
    let changes = session(Kind::ClaudeSdk, claude_working);
    let claude = session(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("opus".into()),
            model_name: Some("Opus".into()),
            permission: Some("default".into()),
            usage: Some(claude_usage),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let codex = session(
        Kind::Codex,
        wire::CodexSnapshot {
            model: Some("gpt-5.5".into()),
            model_name: Some("GPT-5.5".into()),
            approval_policy: Some("on-request".into()),
            sandbox: Some("workspace-write".into()),
            usage: Some(codex_usage),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    vec![
        (
            "chat_changes_uncommitted",
            "A chat counting uncommitted changes: the header's [Diff +42 −7] from the agent's row, and the overview open beside it with the two background jobs, each command with how long it has run, and the four changed files under their folders, root files first, each with its lines added and removed.",
            draw(&changes, Comparison::Uncommitted, Some(&uncommitted)),
        ),
        (
            "chat_changes_on_branch",
            "The same chat counting everything on its branch: the header's [Diff vs main +120 −30], and the overview listing the branch's six changed files by folder, a created and a deleted file among them.",
            draw(&changes, Comparison::OnBranch, Some(&on_branch)),
        ),
        (
            "chat_usage_claude",
            "A Claude chat near a usage limit, the overview listing every window with its name, how full it is, its state and when it resets: the 5-hour limit near, the weekly limit fine, and Fable's own weekly limit near.",
            draw(&claude, Comparison::Uncommitted, None),
        ),
        (
            "chat_usage_codex",
            "A Codex chat near a usage limit, the overview listing its 5-hour limit near and its weekly limit fine, each with its fullness and reset time, and the credits left.",
            draw(&codex, Comparison::Uncommitted, None),
        ),
    ]
}

/// The permission and mode controls on a chat's composer, each agent with
/// the catalogue its interpreter writes: the edge naming what is not the
/// agent's normal one, the key under it that changes it, and the
/// permission picker Ctrl+S then p opens.
fn chat_controls(theme: Theme) -> Vec<(&'static str, &'static str, Buffer)> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use prost::Message as _;
    use ui_state::{Connection, Msg, SessionState};
    use wire::{Kind, OfferedMode, OfferedPermission, session_event};

    use crate::chat::ChatView;

    let now = written_at();
    let permission = |value: &str, name: &str, settable: bool| OfferedPermission {
        value: value.into(),
        display_name: name.into(),
        normal: value == "default",
        settable,
        ..Default::default()
    };
    let claude = |settable: bool| {
        vec![
            permission("default", "Ask", settable),
            permission("acceptEdits", "Accept edits", settable),
            permission("plan", "Plan", settable),
            OfferedPermission {
                models: vec!["opus".into(), "sonnet".into()],
                ..permission("auto", "Auto", settable)
            },
        ]
    };
    let codex = vec![
        permission("read-only", "Read only", true),
        permission("default", "Default", true),
        permission("auto", "Auto", true),
        OfferedPermission {
            never_asks: true,
            ..permission("full-access", "Full access", true)
        },
    ];
    let codex_modes = vec![
        OfferedMode {
            value: "default".into(),
            display_name: "Default".into(),
            normal: true,
            settable: true,
        },
        OfferedMode {
            value: "plan".into(),
            display_name: "Plan".into(),
            normal: false,
            settable: true,
        },
    ];
    let model = |value: &str, name: &str, efforts: &[&str]| wire::OfferedModel {
        value: value.into(),
        display_name: name.into(),
        efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
        ..Default::default()
    };
    let item = |order: u64, prompt: bool, words: &str, kind: Kind| {
        let body = match kind {
            Kind::Codex => {
                use wire::codex_item::Kind as K;
                wire::CodexItem {
                    kind: Some(if prompt {
                        K::Prompt(wire::Prompt {})
                    } else {
                        K::Message(wire::Text { complete: true })
                    }),
                }
                .encode_to_vec()
            }
            Kind::ClaudePty => {
                use wire::claude_pty_item::Kind as K;
                wire::ClaudePtyItem {
                    kind: Some(if prompt {
                        K::Prompt(wire::Prompt {})
                    } else {
                        K::Message(wire::Text { complete: true })
                    }),
                }
                .encode_to_vec()
            }
            _ => {
                use wire::claude_sdk_item::Kind as K;
                wire::ClaudeSdkItem {
                    kind: Some(if prompt {
                        K::Prompt(wire::Prompt {})
                    } else {
                        K::Message(wire::Text { complete: true })
                    }),
                }
                .encode_to_vec()
            }
        };
        wire::Item {
            key: format!("k{order}"),
            order,
            revision: order,
            text: words.into(),
            kind: kind as i32,
            body,
            at_ms: now - 60_000 + order as i64 * 1_000,
            ..Default::default()
        }
    };
    let session = |kind: Kind, body: Vec<u8>, catalogue: wire::Catalogue| {
        let agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            host_id: b"host".to_vec(),
            kind: kind as i32,
            name: "fixer".into(),
            cwd: "/srv/amux".into(),
            lifecycle: wire::Lifecycle::Live as i32,
            phase: wire::Phase::Idle as i32,
            incarnation: 1,
            ..Default::default()
        };
        let mut state = SessionState::new(agent, crate::chat::layout::CAP as usize);
        let event = |of| Msg::Event(wire::SessionEvent { of: Some(of) });
        state.update(Msg::Connection(Connection::Live));
        state.update(event(session_event::Of::Snapshot(wire::Snapshot {
            kind: kind as i32,
            body,
            phase: wire::Phase::Idle as i32,
            catalogue: Some(b"offered".to_vec()),
            ..Default::default()
        })));
        state.update(Msg::Catalogue {
            hash: b"offered".to_vec(),
            catalogue,
        });
        for item in [
            item(
                1,
                true,
                "Plan the overview's file list before changing it",
                kind,
            ),
            item(
                2,
                false,
                "Ready: I'll group the changed files by folder, root files first.",
                kind,
            ),
        ] {
            state.update(event(session_event::Of::Item(item)));
        }
        state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
            revision: 2,
        })));
        state
    };
    let draw = |state: &SessionState, keys: &[KeyEvent]| {
        let mut view = ChatView::new(b"agent".to_vec(), now, false);
        let (width, height) = CHAT_SIZE;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        let mut paint = |view: &mut ChatView| {
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    view.draw(frame, area, state, None, None, now, theme);
                })
                .expect("draw");
        };
        paint(&mut view);
        for key in keys {
            view.key(state, *key, theme);
            paint(&mut view);
        }
        terminal.backend().buffer().clone()
    };
    let permission_picker = [
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    ];

    let claude_sdk = session(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("claude-opus-5-5".into()),
            model_name: Some("Opus 5.5".into()),
            effort: Some("high".into()),
            permission: Some("acceptEdits".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        wire::Catalogue {
            models: vec![
                wire::OfferedModel {
                    resolved_model: "claude-opus-5-5".into(),
                    ..model("opus", "Opus 5.5", &["low", "medium", "high"])
                },
                model("haiku", "Haiku 4.5", &[]),
            ],
            permissions: claude(true),
            ..Default::default()
        },
    );
    let codex_settings = |settings: wire::CodexSnapshot| {
        session(
            Kind::Codex,
            wire::CodexSnapshot {
                model: Some("gpt-5.5".into()),
                model_name: Some("GPT-5.5".into()),
                effort: Some("medium".into()),
                ..settings
            }
            .encode_to_vec(),
            wire::Catalogue {
                models: vec![model("gpt-5.5", "GPT-5.5", &["low", "medium", "high"])],
                permissions: codex.clone(),
                modes: codex_modes.clone(),
                ..Default::default()
            },
        )
    };
    let codex_plan = codex_settings(wire::CodexSnapshot {
        permission: Some("full-access".into()),
        mode: Some("plan".into()),
        approval_policy: Some("never".into()),
        sandbox: Some("danger-full-access".into()),
        ..Default::default()
    });
    let codex_custom = codex_settings(wire::CodexSnapshot {
        mode: Some("default".into()),
        approval_policy: Some("untrusted".into()),
        sandbox: Some("workspace-write".into()),
        ..Default::default()
    });
    let claude_pty = session(
        Kind::ClaudePty,
        wire::ClaudePtySnapshot {
            model: Some("claude-opus-5-5".into()),
            model_name: Some("Opus 5.5".into()),
            permission: Some("plan".into()),
            ..Default::default()
        }
        .encode_to_vec(),
        wire::Catalogue {
            permissions: claude(false),
            ..Default::default()
        },
    );
    vec![
        (
            "chat_controls_claude",
            "A headless Claude chat in accept edits: the composer's edge names the model, its effort and the permission, the keys under it offer shift+tab for the next permission, and Ctrl+S then p has opened the permission picker over the edge, accept edits marked, ask, plan and auto beside it.",
            draw(&claude_sdk, &permission_picker),
        ),
        (
            "chat_controls_codex",
            "A Codex chat with full access in plan mode: the edge names both, shift+tab moves the mode, and Ctrl+S then p has opened the permission picker with read only, default, auto and full access, full access marked as acting without asking.",
            draw(&codex_plan, &permission_picker),
        ),
        (
            "chat_controls_codex_custom",
            "A Codex chat whose approval policy and sandbox match no named permission: the edge reads custom, and shift+tab still moves the mode.",
            draw(&codex_custom, &[]),
        ),
        (
            "chat_controls_claude_terminal",
            "A terminal Claude chat in plan: the edge names the permission, and shift+tab presses Claude's own cycle key, the only way its permission changes.",
            draw(&claude_pty, &[]),
        ),
    ]
}
