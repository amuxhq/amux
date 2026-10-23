//! Sessions built from the interpreter's recorded fixtures, committed the
//! way the owning daemon commits them, and frames drawn from them: what
//! the TUI's tests look at.

use std::path::Path;

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Replayed, replay};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ui_state::{InputOutcome, Msg, SessionState};
use wire::Kind;

use crate::chat::ChatView;
pub use crate::sessions::{Committer, agent, caught_up};
use crate::theme::Theme;

/// The session after each frame of one fixture, with its label and the
/// newest item time: caught up after the first frame, as a live chat is.
pub fn frames(kind: Kind, name: &str) -> Vec<(String, SessionState, i64)> {
    let dir = match kind {
        Kind::ClaudePty => "claude_pty",
        Kind::ClaudeSdk => "claude_sdk",
        _ => "codex",
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(dir)
        .join(format!("{name}.json"));
    let replayed: Vec<Replayed> = match kind {
        Kind::ClaudePty => replay::<ClaudePty>(&path),
        Kind::ClaudeSdk => replay::<ClaudeSdk>(&path),
        _ => replay::<Codex>(&path),
    }
    .unwrap_or_else(|error| panic!("{dir}/{name}: {error}"));
    let mut state = SessionState::new(agent(kind), crate::chat::layout::CAP as usize);
    state.update(Msg::Connection(ui_state::Connection::Live));
    let mut committer = Committer::default();
    let mut out = Vec::new();
    let mut at = 0;
    for (index, frame) in replayed.iter().enumerate() {
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        for event in committer.commit(&frame.step) {
            state.update(Msg::Event(event));
        }
        if index == 0 {
            state.update(caught_up(committer.revision));
        }
        // The catalogue as a client fetches it once the snapshot names it.
        if let Some((hash, catalogue)) = &frame.catalogue {
            state.update(Msg::Catalogue {
                hash: hash.clone(),
                catalogue: catalogue.clone(),
            });
        }
        at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(at)
            .max(at);
        out.push((frame.label.clone(), state.clone(), at));
    }
    out
}

/// The first frame of a fixture where `pred` holds.
pub fn frame_where(
    kind: Kind,
    name: &str,
    pred: impl Fn(&SessionState) -> bool,
) -> (SessionState, i64) {
    frames(kind, name)
        .into_iter()
        .find(|(_, state, _)| pred(state))
        .map(|(_, state, at)| (state, at))
        .unwrap_or_else(|| panic!("{name}: no frame matches"))
}

/// A chat drawn at `width`x`height`.
pub fn draw(
    view: &mut ChatView,
    state: &SessionState,
    now_ms: i64,
    width: u16,
    height: u16,
    theme: Theme,
) -> (Buffer, Option<u32>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    let mut page = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            page = view.draw(frame, area, state, None, None, now_ms, theme);
        })
        .expect("draw");
    (terminal.backend().buffer().clone(), page)
}

/// The buffer's text, one line per row, trailing blanks trimmed.
pub fn text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The three frames every theme is held to: a turn at work, a permission
/// ask, and a Codex approval.
#[derive(Clone, Copy, Debug)]
pub enum Named {
    ClaudeWorking,
    ClaudePermissionAsk,
    CodexApproval,
}

impl Named {
    pub fn name(self) -> &'static str {
        match self {
            Named::ClaudeWorking => "claude working",
            Named::ClaudePermissionAsk => "claude permission ask",
            Named::CodexApproval => "codex approval",
        }
    }

    pub fn state(self) -> (SessionState, i64) {
        match self {
            Named::ClaudeWorking => frame_where(Kind::ClaudeSdk, "recorded_multi_turn", |state| {
                state.phase() == ui_state::PhaseView::Working && state.transcript().len() > 2
            }),
            Named::ClaudePermissionAsk => {
                frame_where(Kind::ClaudeSdk, "recorded_permission_callback", |state| {
                    ui_view::ask_card(state).is_some()
                })
            }
            Named::CodexApproval => frame_where(Kind::Codex, "recorded_approval_scopes", |state| {
                ui_view::ask_card(state).is_some()
            }),
        }
    }

    pub fn render(self, theme: Theme) -> Buffer {
        let (state, at) = self.state();
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        draw(&mut view, &state, at, 120, 40, theme).0
    }
}

/// Compares a drawn frame with the golden `name` under tests/golden (its
/// text, then its style classes, as the component goldens are), or
/// rewrites it under UPDATE_GOLDENS outside CI.
pub fn assert_frame_golden(name: &str, buffer: &Buffer, theme: Theme) {
    let area = buffer.area;
    let mut words = String::new();
    let mut styles = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            line.push_str(cell.symbol());
            styles.push(theme.classify(cell.style()));
        }
        words.push_str(line.trim_end());
        words.push('\n');
        styles.push('\n');
    }
    let rendered = format!("{words}--- styles\n{styles}");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "UPDATE_GOLDENS is refused in CI: rewrite goldens locally and review them"
        );
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with UPDATE_GOLDENS=1 and review"));
    assert!(
        rendered == expected,
        "{name} differs from its golden; if intended, rerun with UPDATE_GOLDENS=1 and review.\n\
         rendered:\n{rendered}"
    );
}
