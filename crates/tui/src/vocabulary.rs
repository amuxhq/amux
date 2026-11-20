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
    AnswerView, AskBody, AskCard, AskRow, AttachmentView, CallPhase, CardState, Choice,
    ChoiceOutcome, Decision, DecisionView, ExploreVerb, FileChangeView, FilePatch, FileRow,
    Granted, HunkStart, LineKind, Offline, OptionView, PatchHead, PatchHunk, PatchLine,
    PermissionGrant, PlanVerdict, QuestionView, QueuedRow, Reach, Refusal, Resolution, Row,
    RowKind, Scope, Segment,
};
use wire::{BlobRef, BoundaryKind, EnvelopeKind, SendState};

use crate::chat::ask::AskUi;
use crate::chat::composer::{QueueEntry, edge_row};
use crate::chat::rows::{RowFacts, RowState, row_lines};
use crate::chat::{composer_lines, feed, native_composer_lines, sign_in_composer_lines};
use crate::editor::Editor;
use crate::theme::Theme;

/// Columns every component is drawn at.
pub const WIDTH: u16 = 100;
/// Rows the full-screen review page is drawn at.
const PAGE_HEIGHT: u16 = 30;
/// What terminal Claude's interpreter says when Claude shows a tool server's
/// form in its own terminal.
const UNANSWERABLE: &str = "Claude is showing a form from a tool server this build can't read. Attach to Claude's terminal to answer it, or stop the agent.";

/// The chat surroundings' frames: columns and rows.
const CHAT_SIZE: (u16, u16) = (120, 28);

/// One named component drawn in `theme`.
pub struct Component {
    pub name: &'static str,
    /// What it shows, for the index of goldens.
    pub shows: &'static str,
    pub buffer: Buffer,
}

/// Set once the vocabulary draws: its pictures are references, so the
/// times on them read UTC rather than the drawing machine's clock.
pub(crate) static TIMES_IN_UTC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A reply in every markdown construct an agent writes, for the prose row
/// that renders them.
const EVERY_MARKDOWN: &str = r#"## What changed

The relay now **retries** after a dropped link, with *jittered* backoff, and the
retry budget lives in `RelayConfig::retries`.

1. The link notices the drop.
2. It waits, then dials again:
   - first after 200 ms,
   - then doubling up to 5 s.
3. After the budget it gives up and says so.

| Case | Before | After |
|---|---|---|
| Wi-Fi blip | dropped | resumes |
| Laptop asleep | dropped | resumes on wake |
| Relay restart | 30 s gap | under 2 s |

```rust
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 << attempt.min(5))
}
```

> The phone keeps its own timer; this only changes the host's side.

---

See [the relay design](https://example.com/docs/relay) and
https://example.com/a/very/long/link/that/runs/past/the/edge/of/a/narrow/terminal/window
for the measurements."#;

/// Every component, in catalogue order.
pub fn components(theme: Theme) -> Vec<Component> {
    TIMES_IN_UTC.store(true, std::sync::atomic::Ordering::Relaxed);
    // A fenced block is drawn plain until its grammar is built in the
    // background, so a reference waits for every grammar.
    crate::code::highlighter().preload();
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
    for (name, shows, overview, jobs) in edge_rows() {
        let mut lines: Vec<Line<'static>> =
            edge_row(&overview, jobs, false, written_at(), w, theme)
                .into_iter()
                .collect();
        lines.extend(composer_lines(
            &Editor::default(),
            &Composer::Send,
            "fixer",
            "studio",
            Reach::Online(wire::RouteKind::Direct),
            w,
            theme,
        ));
        add(name, shows, lines);
    }
    add(
        "composer_signed_out_codex",
        "Codex refused its sign-in: above a composer that still sends, a notice says Codex needs you to sign in, that sign-in failed and the provider's words why, to run codex login on the agent's host, and that someone who signed in elsewhere tries again by sending.",
        sign_in_composer_lines(
            wire::Kind::Codex,
            "studio",
            &ui_view::SignInView {
                state: wire::SignInState::Failed,
                account: String::new(),
                message: "unexpected status 401 Unauthorized: Missing bearer or basic authentication in header".into(),
            },
            "fixer",
            w,
            theme,
        ),
    );
    for (name, shows, kind, stop) in native_composers() {
        add(
            name,
            shows,
            native_composer_lines(
                kind,
                &stop,
                if kind == wire::Kind::Codex {
                    "GPT-5.5"
                } else {
                    "Sonnet 5"
                },
                w,
                theme,
            ),
        );
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
        shows: "Home with a row in every state, each with its name (word pairs where amux named the agent), its branch where it works in a repository, and its second line: a family placed under Needs you by its child's question, a command asked for with another ask behind it; under Live a step being run, what an idle agent last said, a signed-out and a usage-limited agent, a starting agent with nothing to say yet, and an agent that failed with its cause, and an agent on a host that is away; under Settled, unfolded, one that finished and one that was stopped, whose heading says all there is to say.",
        buffer: home_rows(theme),
    });
    for (name, shows, mut page) in review_pages() {
        let mut terminal =
            Terminal::new(TestBackend::new(WIDTH, PAGE_HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| page.draw(frame, frame.area(), None, theme))
            .expect("draw");
        out.push(Component {
            name,
            shows,
            buffer: terminal.backend().buffer().clone(),
        });
    }
    {
        let mut scene = chat_review_chip();
        let (width, height) = CHAT_SIZE;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| scene.draw(frame, frame.area(), theme))
            .expect("draw");
        out.push(Component {
            name: "composer_review_chip",
            shows: "A chat whose agent's review holds three drafts: the review chip on the composer's top edge, read from the agent's row alone, with its ✕ and the words saying the drafts were written on older code.",
            buffer: terminal.backend().buffer().clone(),
        });
    }
    out
}

/// The review page in each state a story shows, by component name.
fn review_pages() -> Vec<(&'static str, &'static str, crate::chat::review::Page)> {
    vec![
        (
            "review_first_look",
            "The review page on a first look: three files of all changes, the lock file folded, a hunk under the definitions it sits in with highlighted code and the words that changed, a draft under its line and the comment editor open on another.",
            review_first_look(),
        ),
        (
            "review_since_last",
            "The review page's next look, on Since your last review: the switch between the two views, the one file changed since, a sent comment off the lines shown with its saved code, an outdated one folded, and a draft on the new line.",
            review_since_last(),
        ),
        (
            "review_drafts_stale",
            "The review page held on its drafts' snapshot after the agent changed code: the banner offering to refresh, which moves the drafts.",
            review_drafts_stale(),
        ),
        (
            "review_empty",
            "The review page with nothing left to review.",
            review_empty(),
        ),
        (
            "review_not_offered",
            "The review page for an agent whose computer does not run reviews.",
            review_not_offered(),
        ),
    ]
}

// --- the review page --------------------------------------------------------

const REVIEW_BASE: &str = "b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9";
const REVIEW_ROUND: &str = "4f2a9c1e7b3d5f6a8c0e2b4d6f8a0c2e4b6d8f0a";
const REVIEW_SNAPSHOT: &str = "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d";

/// Rust's runs, roughly as the host's highlighter colours them: keywords,
/// the name before a call, types and strings.
fn rust_runs(text: &str) -> Vec<wire::experimental::ReviewRun> {
    use wire::experimental::{ReviewRun, ReviewTokenKind as T};
    const KEYWORDS: &[&str] = &[
        "let", "fn", "pub", "self", "mut", "impl", "struct", "use", "return", "if", "Self",
    ];
    let mut out: Vec<ReviewRun> = Vec::new();
    let mut push = |kind: T, len: usize| match out.last_mut() {
        Some(last) if last.kind == kind as i32 => last.len += len as u32,
        _ => out.push(ReviewRun {
            kind: kind as i32,
            len: len as u32,
        }),
    };
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c == b'"' {
            let end = text[at + 1..]
                .find('"')
                .map_or(bytes.len(), |end| at + end + 2);
            push(T::String, end - at);
            at = end;
        } else if text[at..].starts_with("//") {
            push(T::Comment, bytes.len() - at);
            at = bytes.len();
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = text[at..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(bytes.len(), |end| at + end);
            let word = &text[at..end];
            let kind = if KEYWORDS.contains(&word) {
                T::Keyword
            } else if text[end..].starts_with('(') || text[end..].starts_with("!(") {
                T::Function
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                T::Type
            } else {
                T::Plain
            };
            push(kind, end - at);
            at = end;
        } else if c.is_ascii_digit() {
            push(T::Number, 1);
            at += 1;
        } else {
            push(T::Plain, 1);
            at += 1;
        }
    }
    out
}

fn review_line(
    kind: wire::experimental::ReviewLineKind,
    old: u32,
    new: u32,
    text: &str,
) -> wire::experimental::ReviewLine {
    wire::experimental::ReviewLine {
        kind: kind as i32,
        old_line: old,
        new_line: new,
        text: text.into(),
        runs: rust_runs(text),
    }
}

fn kept(old: u32, new: u32, text: &str) -> wire::experimental::ReviewLine {
    review_line(wire::experimental::ReviewLineKind::Context, old, new, text)
}

fn put(new: u32, text: &str) -> wire::experimental::ReviewLine {
    review_line(wire::experimental::ReviewLineKind::Added, 0, new, text)
}

fn cut(old: u32, text: &str) -> wire::experimental::ReviewLine {
    review_line(wire::experimental::ReviewLineKind::Removed, old, 0, text)
}

fn definition(
    kind: wire::experimental::ReviewDefinitionKind,
    label: &str,
    lines: (u32, u32),
    parent: Option<u32>,
) -> wire::experimental::ReviewDefinition {
    wire::experimental::ReviewDefinition {
        kind: kind as i32,
        name: label.rsplit(' ').next().unwrap_or_default().into(),
        label: label.into(),
        start_line: lines.0,
        end_line: lines.1,
        parent,
        side: wire::experimental::ReviewSide::Snapshot as i32,
    }
}

/// `impl Store`, `fn open` and `fn close` of `src/store.rs`.
fn store_definitions() -> [wire::experimental::ReviewDefinition; 3] {
    use wire::experimental::ReviewDefinitionKind as K;
    [
        definition(K::Impl, "impl Store", (12, 96), None),
        definition(K::Method, "fn open", (38, 48), Some(0)),
        definition(K::Method, "fn close", (68, 74), Some(0)),
    ]
}

/// The hunk of `fn close` against `old_commit`, as `ended` leaves its new
/// line.
fn close_hunk(old_commit: &str, old_start: u32, ended: &str) -> wire::experimental::ReviewHunk {
    let [store, _, close] = store_definitions();
    wire::experimental::ReviewHunk {
        old_commit: old_commit.into(),
        old_start,
        new_start: 70,
        scope: vec![store, close],
        lines: vec![
            kept(old_start, 70, "    pub fn close(self) -> io::Result<()> {"),
            put(71, ended),
            kept(old_start + 1, 72, "        drop(self.file);"),
            kept(old_start + 2, 73, "        Ok(())"),
            kept(old_start + 3, 74, "    }"),
        ],
    }
}

/// Every change since the base: an edit to `fn open` of the store, a new
/// file of flags, and the lock file.
fn review_files_all() -> Vec<wire::experimental::ReviewFileDiff> {
    use wire::experimental::{ReviewFileDiff, ReviewHunk};
    let [store, open, close] = store_definitions();
    vec![
        ReviewFileDiff {
            path: "src/store.rs".into(),
            hunks: vec![
                ReviewHunk {
                    old_commit: REVIEW_BASE.into(),
                    old_start: 39,
                    new_start: 39,
                    scope: vec![store.clone(), open.clone()],
                    lines: vec![
                        kept(
                            39,
                            39,
                            "    pub fn open(path: &Path, flags: Flags) -> io::Result<Store> {",
                        ),
                        kept(40, 40, "        let path = path.join(\"store\");"),
                        cut(41, "        let file = File::open(&path)?;"),
                        put(41, "        let file = open_with_flags(&path, flags)?;"),
                        put(42, "        log::debug!(\"opened {}\", path.display());"),
                        kept(42, 43, "        Ok(Store { file, flags })"),
                        kept(43, 44, "    }"),
                    ],
                },
                close_hunk(REVIEW_BASE, 69, "        self.file.sync_all()?;"),
            ],
            outline: vec![store, open, close],
            context: Vec::new(),
        },
        ReviewFileDiff {
            path: "src/flags.rs".into(),
            hunks: vec![ReviewHunk {
                old_commit: REVIEW_BASE.into(),
                old_start: 0,
                new_start: 1,
                scope: Vec::new(),
                lines: vec![
                    put(1, "/// How a store opens its file."),
                    put(2, "#[derive(Clone, Copy, Debug, Default)]"),
                    put(3, "pub struct Flags {"),
                    put(4, "    pub read_only: bool,"),
                    put(5, "    pub create: bool,"),
                    put(6, "}"),
                ],
            }],
            outline: vec![definition(
                wire::experimental::ReviewDefinitionKind::Struct,
                "struct Flags",
                (1, 6),
                None,
            )],
            context: Vec::new(),
        },
    ]
}

fn changed_file(
    path: &str,
    status: wire::FileStatus,
    added: u32,
    removed: u32,
    flags: &[wire::FileFlag],
) -> wire::ChangedFile {
    wire::ChangedFile {
        path: path.into(),
        status: status as i32,
        added,
        removed,
        language: if path.ends_with(".rs") {
            "rust".into()
        } else {
            String::new()
        },
        flags: flags.iter().map(|flag| *flag as i32).collect(),
        ..Default::default()
    }
}

/// A comment on `lines` of `src/store.rs` in `commit`, placed where `at`
/// says, sent in `round` or a draft.
fn store_comment(
    id: &str,
    commit: &str,
    lines: (u32, u32),
    words: &str,
    round: Option<u32>,
    at: wire::experimental::review_placement::At,
) -> wire::experimental::ReviewerComment {
    use wire::experimental::{ReviewAnchor, ReviewLineRange, ReviewPlacement, ReviewerComment};
    ReviewerComment {
        id: id.as_bytes().to_vec(),
        anchor: Some(ReviewAnchor {
            commit: commit.into(),
            path: "src/store.rs".into(),
            lines: Some(ReviewLineRange {
                start: lines.0,
                end: lines.1,
            }),
        }),
        text: words.into(),
        snippet: Vec::new(),
        scope: Vec::new(),
        round,
        placement: Some(ReviewPlacement {
            at: Some(at),
            changed_near: false,
            path: "src/store.rs".into(),
        }),
        revision: 1,
        sent_revision: round.map(|_| 1),
    }
}

fn placed_on(start: u32, end: u32) -> wire::experimental::review_placement::At {
    wire::experimental::review_placement::At::Lines(wire::experimental::ReviewLineRange {
        start,
        end,
    })
}

/// The summary of a first look: three files changed since `main`, and a
/// draft on the store's new call.
fn first_look_summary() -> wire::experimental::ReviewSummary {
    use wire::{FileFlag, FileStatus};
    wire::experimental::ReviewSummary {
        id: b"review".to_vec(),
        base_branch: "main".into(),
        base_commit: REVIEW_BASE.into(),
        snapshot: REVIEW_SNAPSHOT.into(),
        files: vec![
            changed_file(
                "Cargo.lock",
                FileStatus::Changed,
                12,
                4,
                &[FileFlag::Lockfile],
            ),
            changed_file("src/flags.rs", FileStatus::Created, 6, 0, &[]),
            changed_file("src/store.rs", FileStatus::Changed, 3, 1, &[]),
        ],
        comments: vec![store_comment(
            "draft-open",
            REVIEW_SNAPSHOT,
            (41, 41),
            "Keep File::open for read-only stores: open_with_flags creates the file.",
            None,
            placed_on(41, 41),
        )],
        objects: "/srv/amux/.amux/review/objects".into(),
        root: "/srv/amux".into(),
        began_at: REVIEW_BASE.into(),
        ..Default::default()
    }
}

/// A page over `summary` with `all` and `since_last` fetched, named for
/// the chat it opens from.
fn review_scene(
    summary: wire::experimental::ReviewSummary,
    all: Vec<wire::experimental::ReviewFileDiff>,
    since_last: Vec<wire::experimental::ReviewFileDiff>,
) -> crate::chat::review::Page {
    use std::sync::Arc;
    let by_path = |files: Vec<wire::experimental::ReviewFileDiff>| {
        files
            .into_iter()
            .map(|file| (file.path.clone(), Arc::new(file)))
            .collect()
    };
    let mut page = crate::chat::review::Page::new();
    page.set_owner("fixer", "/srv/amux");
    page.update(crate::chat::review::Fetched {
        summary: Some(Arc::new(summary)),
        all: by_path(all),
        since_last: by_path(since_last),
        context: Vec::new(),
    });
    page
}

/// The review page on a first look, with a draft under the store's new
/// call and a second being written on `fn close`'s new line.
pub fn review_first_look() -> crate::chat::review::Page {
    let mut page = review_scene(first_look_summary(), review_files_all(), Vec::new());
    page.show_file("src/store.rs");
    let key = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
    for code in [
        KeyCode::Char('J'),
        KeyCode::Char('J'),
        KeyCode::Char('j'),
        KeyCode::Char('c'),
    ] {
        page.key(key(code));
    }
    for c in "Does close need to sync if the store is read-only?".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    page
}

/// The review's next look, after one round: since the last review, the
/// store's `fn close` changed again.
pub fn review_since_last() -> crate::chat::review::Page {
    use wire::experimental::review_placement::At;
    use wire::experimental::{ReviewRound, ReviewSummary};
    let mut sent = store_comment(
        "sent-open",
        REVIEW_ROUND,
        (41, 41),
        "Keep File::open for read-only stores: open_with_flags creates the file.",
        Some(1),
        placed_on(41, 41),
    );
    sent.snippet = vec![
        kept(
            39,
            39,
            "    pub fn open(path: &Path, flags: Flags) -> io::Result<Store> {",
        ),
        kept(40, 40, "        let path = path.join(\"store\");"),
        cut(41, "        let file = File::open(&path)?;"),
        put(41, "        let file = open_with_flags(&path, flags)?;"),
    ];
    let [store, open, close] = store_definitions();
    sent.scope = vec![store.clone(), open];
    let mut outdated = store_comment(
        "sent-close",
        REVIEW_ROUND,
        (71, 71),
        "sync_all can fail: don't drop its error.",
        Some(1),
        At::Outdated(wire::Empty {}),
    );
    outdated.snippet = vec![
        kept(69, 70, "    pub fn close(self) -> io::Result<()> {"),
        put(71, "        self.file.sync_all();"),
    ];
    outdated.scope = vec![store, close];
    let draft = store_comment(
        "draft-close",
        REVIEW_SNAPSHOT,
        (71, 71),
        "Good. Say in a comment why the error matters here.",
        None,
        placed_on(71, 71),
    );
    let summary = ReviewSummary {
        rounds: vec![ReviewRound {
            snapshot: REVIEW_ROUND.into(),
            base_commit: REVIEW_BASE.into(),
            sent_at_ms: written_at() - 3_600_000,
        }],
        since_last: vec![changed_file(
            "src/store.rs",
            wire::FileStatus::Changed,
            1,
            1,
            &[],
        )],
        comments: vec![sent, outdated, draft],
        ..first_look_summary()
    };
    let since = wire::experimental::ReviewFileDiff {
        path: "src/store.rs".into(),
        hunks: vec![{
            let mut hunk = close_hunk(REVIEW_ROUND, 70, "        self.file.sync_all()?;");
            hunk.lines
                .insert(1, cut(71, "        self.file.sync_all();"));
            hunk
        }],
        outline: store_definitions().to_vec(),
        context: Vec::new(),
    };
    let mut page = review_scene(summary, review_files_all(), vec![since]);
    page.show_file("src/store.rs");
    page
}

/// The round the review sent before the next look closed, opened from the
/// sent review in the chat: its whole diff, from the base to the round's
/// snapshot, with its two comments on the lines they were written on.
pub fn review_round() -> crate::chat::review::Page {
    let summary = review_since_last()
        .fetched()
        .summary
        .clone()
        .expect("the next look's summary");
    let sent: Vec<ui_view::SentCommentRef> = summary
        .comments
        .iter()
        .filter_map(|comment| {
            Some(ui_view::SentCommentRef {
                id: comment.id.clone(),
                revision: comment.sent_revision?,
            })
        })
        .collect();
    let (_, mut round) =
        ui_runtime::review::round_of(&summary, &summary.id, &sent).expect("the round");
    round.files = first_look_summary().files;
    // The round's snapshot, before the agent made close keep sync_all's
    // error.
    let mut files: Vec<_> = review_files_all()
        .into_iter()
        .map(|mut file| {
            if file.path == "src/store.rs" {
                file.hunks[1] = close_hunk(REVIEW_BASE, 69, "        self.file.sync_all();");
            }
            file
        })
        .collect();
    // A past round opens its lockfile too, which the review folds.
    files.push(wire::experimental::ReviewFileDiff {
        path: "Cargo.lock".into(),
        hunks: vec![wire::experimental::ReviewHunk {
            old_commit: REVIEW_BASE.into(),
            old_start: 210,
            new_start: 210,
            scope: Vec::new(),
            lines: vec![
                kept(210, 210, "[[package]]"),
                kept(211, 211, "name = \"store\""),
                cut(212, "version = \"0.3.0\""),
                put(212, "version = \"0.4.0\""),
            ],
        }],
        outline: Vec::new(),
        context: Vec::new(),
    });
    let state = ui_view::ReviewPageState {
        toggled_files: vec!["Cargo.lock".into()],
        ..ui_view::ReviewPageState::default()
    };
    let mut page = crate::chat::review::Page::round(1);
    page.set_owner("fixer", "/srv/amux");
    page.round_read(
        Ok(Some(ui_runtime::review::Round {
            summary: round,
            files,
            notices: Vec::new(),
            state,
        })),
        "",
    );
    page
}

/// The first look's draft held on its snapshot after the agent changed
/// code.
pub fn review_drafts_stale() -> crate::chat::review::Page {
    let summary = wire::experimental::ReviewSummary {
        stale: true,
        ..first_look_summary()
    };
    let mut page = review_scene(summary, review_files_all(), Vec::new());
    page.show_file("src/store.rs");
    page
}

/// A review with nothing changed and no comments.
pub fn review_empty() -> crate::chat::review::Page {
    let summary = wire::experimental::ReviewSummary {
        files: Vec::new(),
        comments: Vec::new(),
        ..first_look_summary()
    };
    review_scene(summary, Vec::new(), Vec::new())
}

/// The page for an agent whose computer does not run reviews.
pub fn review_not_offered() -> crate::chat::review::Page {
    let mut page = crate::chat::review::Page::new();
    page.set_owner("fixer", "/srv/amux");
    page.notice(ui_view::ReviewNotice::NotOffered, true);
    page
}

/// A chat whose agent's review holds three drafts, written before the
/// agent last changed code: its row says so, and the composer's chip
/// follows the row.
pub fn chat_review_chip() -> ChatScene {
    let mut scene = chat_formatted();
    let mut agent = scene.state.agent().clone();
    wire::experimental::REVIEW_ROW.put(
        &mut agent.experimental,
        &wire::experimental::ReviewRowEntry {
            drafts: 3,
            counter: 7,
            moved: true,
            review_id: b"review".to_vec(),
        },
    );
    scene.state.update(ui_state::Msg::Entry(agent));
    for c in "Here's my review of the store change.".chars() {
        scene.view.key(
            &scene.state,
            KeyEvent::from(KeyCode::Char(c)),
            Theme::default(),
        );
    }
    scene
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
    // A call that started an agent draws as the layout places it: on its
    // own, or as its group; an open group's calls sit together.
    let placement = if row.opens.is_some() || row.group.is_some() {
        feed::Placement::Call {
            joined: row
                .group
                .as_ref()
                .is_some_and(|group| group.open && row.id != "call-4"),
        }
    } else {
        feed::Placement::Plain
    };
    let Some(drawn) = feed::row_lines(row, &placement, state.expanded, &facts(row), width, theme)
    else {
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
    let mut scene = authored_home();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HOME_HEIGHT)).expect("test terminal");
    terminal
        .draw(|frame| scene.draw(frame, frame.area(), theme))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Home as the person sees it, over the fleet it is drawn from.
pub struct HomeScene {
    pub home: crate::home::Home,
    pub fleet: ui_state::FleetState,
    pub lines: std::collections::HashMap<ui_state::AgentKey, ui_view::SessionLine>,
    pub now: i64,
}

/// Home on the authored desk, chatting in amux, a new agent starting with
/// the shipped settings.
pub fn desk_home() -> crate::home::Home {
    crate::home::Home::new(crate::home::Place {
        local_host: b"desk".to_vec(),
        version: String::new(),
        working_dir: "~/src/amux".into(),
        chat_in: settings::ChatInSetting::Amux,
    })
}

impl HomeScene {
    pub fn draw(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        theme: Theme,
    ) {
        self.home
            .draw(frame, area, &self.fleet, &self.lines, None, self.now, theme);
    }

    /// The open new-agent composer told what the desk offers its agent.
    pub fn offer(&mut self, provider: wire::Provider, catalogue: &wire::Catalogue) {
        if let Some(setup) = self.home.open_setup() {
            setup.offer(b"desk", provider, &[1; 32], catalogue);
        }
    }

    /// A key pressed on home, as the person would press it.
    pub fn key(&mut self, key: impl Into<crossterm::event::KeyEvent>) {
        let _ = self.home.key(&self.fleet, key.into());
    }
}

/// Home over the authored fleet: two hosts, one of them away, and a dozen
/// agents in every standing, with families, branches and what each
/// session says for its second line. The exited fold is open and the
/// highlight on the first agent.
pub fn authored_home() -> HomeScene {
    use std::collections::HashMap;

    use ui_state::{Activity, ActivityKind, Connection, FleetMsg, FleetState};
    use ui_view::{ActivityLine, SessionLine};
    use wire::inventory_event::Of;
    use wire::trusted::Reach;
    use wire::{Kind, Lifecycle, Phase, Said, Wants};

    const NOW: i64 = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;
    let mut fleet = FleetState::new();
    let mut event = |of| {
        fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(of),
        })))
    };
    for (id, name, reach) in [
        (
            b"desk".as_slice(),
            "desk",
            Reach::Online(wire::RouteKind::Direct as i32),
        ),
        (
            b"studio".as_slice(),
            "studio",
            Reach::Offline(wire::Offline::default()),
        ),
    ] {
        event(Of::Host(wire::HostEntry::trusted_host(id, name, reach)));
    }
    let mut lines = HashMap::new();
    let asks = |wants: Wants, more: u32| Said::Ask {
        key: "ask".into(),
        wants,
        why: String::new(),
        more,
    };
    let line = |ask: Option<Said>, step: Option<&str>, said: Option<&str>| SessionLine {
        said: ask,
        step: step.map(|step| ActivityLine {
            activity: Activity {
                kind: ActivityKind::Running { key: "call".into() },
                since_ms: NOW,
                elapsed_ms: 0,
                turn_elapsed_ms: 0,
            },
            step: Some(step.to_owned()),
            subagents: None,
        }),
        last_said: said.map(str::to_owned),
        opens: Vec::new(),
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
                Some(asks(
                    Wants::Question {
                        text: "Should the old specs be deleted or kept?".into(),
                    },
                    0,
                )),
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
                said: Some(asks(
                    Wants::Run {
                        command: "cargo test -p auth".into(),
                    },
                    1,
                )),
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
                said: Some(Said::SignedOut),
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
                said: Some(Said::UsageLimit { resets_at_ms: None }),
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
            parent: parent.map(|parent| wire::AgentRef {
                host_id: host.as_bytes().to_vec(),
                agent_id: parent.as_bytes().to_vec(),
            }),
            git: Some(wire::Git {
                branch: branch.map(str::to_owned),
                ..Default::default()
            }),
            incarnation: 1,
            // A one-shot child that finished, and one the person put away,
            // are settled; one that crashed reads as an ordinary agent.
            filing: if matches!(name, "doc-sweep" | "clean-up") {
                wire::Filing::Closed
            } else {
                wire::Filing::Open
            } as i32,
            ..Default::default()
        };
        if let Some(said) = said {
            lines.insert(ui_state::agent_key(&agent), said);
        }
        event(Of::Agent(agent));
    }
    event(Of::CaughtUp(wire::CaughtUp { revision: 0 }));
    fleet.update(FleetMsg::Connection(Connection::Live));

    let mut home = desk_home();
    // Exited unfolded, as the person would open it; the highlight back on
    // the first agent.
    let key = crossterm::event::KeyEvent::from;
    home.key(&fleet, key(crossterm::event::KeyCode::End));
    home.key(&fleet, key(crossterm::event::KeyCode::Enter));
    home.select(ui_state::agent_key(fleet.find(b"planner").expect("listed")));
    HomeScene {
        home,
        fleet,
        lines,
        now: NOW,
    }
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
        opens: None,
        group: None,
        turn: None,
    }
}

/// The descriptions of the authored group's four calls.
const GROUP_TASKS: [&str; 4] = [
    "Survey the redraw paths",
    "Check the journal's retention",
    "Read the store's migrations",
    "Look for stale fixtures",
];

/// An Agent call starting authored subagent `n`, which the fleet holds as
/// [`authored_agent`] says; its answer once it returned one.
fn subagent_call(n: usize, description: &str, answer: Option<&str>) -> Row {
    let mut call = row(RowKind::Subagent {
        description: description.into(),
        continued: false,
        state: if answer.is_some() {
            CallPhase::Succeeded
        } else {
            CallPhase::Running
        },
        answer: answer.unwrap_or_default().into(),
        duration_ms: answer.map(|_| 95_000),
    });
    call.opens = Some(format!("sub-{n}").into_bytes());
    call
}

/// The authored group: its first call, its four agents, and whether the
/// reader opened it.
fn group_of(open: bool) -> ui_view::SubagentGroup {
    ui_view::SubagentGroup {
        first: "call-1".into(),
        agents: (1..=4).map(|n| format!("sub-{n}").into_bytes()).collect(),
        open,
    }
}

/// How the fleet holds authored subagent `n`: at work, waiting on you,
/// done, stopped, failed.
fn authored_agent(id: &[u8]) -> Option<ui_view::OpenedAgent> {
    let n: usize = std::str::from_utf8(id)
        .ok()?
        .strip_prefix("sub-")?
        .parse()
        .ok()?;
    let phase = match n {
        1 => ui_state::PhaseView::Working,
        2 => ui_state::PhaseView::NeedsYou,
        3 => ui_state::PhaseView::Exited {
            cause: ui_view::ExitCause::Finished,
        },
        4 => ui_state::PhaseView::Exited {
            cause: ui_view::ExitCause::Ended,
        },
        _ => ui_state::PhaseView::Exited {
            cause: ui_view::ExitCause::Failed("the provider ended it".into()),
        },
    };
    Some(ui_view::OpenedAgent {
        agent: ui_state::AgentKey {
            host: b"host".to_vec(),
            agent: id.to_vec(),
        },
        name: format!("sub-{n:08}"),
        kind: wire::Kind::ClaudeSdk,
        phase,
    })
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
};
const OPEN: RowState = RowState {
    focused: false,
    expanded: true,
};
const FOCUSED: RowState = RowState {
    focused: true,
    expanded: false,
};

/// What the layout would look up for an authored row: the first edit's
/// landed patch.
fn facts(row: &Row) -> RowFacts {
    let mut facts = RowFacts {
        opened: row.opens.as_deref().and_then(authored_agent),
        group: row
            .group
            .as_ref()
            .filter(|group| group.first == row.id)
            .map(|group| crate::chat::rows::GroupLine {
                state: ui_view::GroupState {
                    agents: 4,
                    running: 1,
                    needs_you: 1,
                    finished: 1,
                    ended: 1,
                    ..ui_view::GroupState::default()
                },
                open: group.open,
            }),
        ..RowFacts::default()
    };
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
            hunks: vec![PatchHunk {
                start: Some(HunkStart { old: 41, new: 41 }),
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
            }],
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
            description: String::new(),
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
                            Segment::Attachment(AttachmentView::SentReview {
                                comments: 2,
                                review: ui_view::SentReviewRef {
                                    review_id: vec![1; 16],
                                    round: 1,
                                    comments: vec![
                                        ui_view::SentCommentRef {
                                            id: vec![1],
                                            revision: 1,
                                        },
                                        ui_view::SentCommentRef {
                                            id: vec![2],
                                            revision: 1,
                                        },
                                    ],
                                },
                                text: "Two comments on src/sum.rs.".into(),
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
            "row_prose_markdown",
            "A reply in every markdown construct an agent writes: headings, nested and numbered lists, a table, fenced code, a quote, links, a rule and emphasis.",
            vec![(
                row(RowKind::Prose {
                    text: text(EVERY_MARKDOWN),
                    streaming: false,
                    working_note: false,
                }),
                CLOSED,
            )],
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
                        fact: "ada/amux".into(),
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
                            destination: Scope::Project,
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
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Search,
                        subject: "redraw".into(),
                        state: CallPhase::Running,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::List,
                        subject: "crates/".into(),
                        state: CallPhase::Succeeded,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Fetch,
                        subject: "https://docs.rs/ratatui".into(),
                        state: CallPhase::Succeeded,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::WebSearch,
                        subject: "ratatui flicker".into(),
                        state: CallPhase::Succeeded,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_subagent",
            "Calls that started subagents, each as its subagent stands in the fleet: one at work, one waiting on you for an ask in its own chat, one done with its answer opened, one stopped, one that failed; then a call from before subagents had chats of their own, reading its own state.",
            vec![
                (subagent_call(1, "Survey the redraw paths", None), CLOSED),
                (subagent_call(2, "Check the journal's retention", None), CLOSED),
                (
                    subagent_call(
                        3,
                        "Read the store's migrations",
                        Some("Two paths redraw: the tick and inventory events."),
                    ),
                    OPEN,
                ),
                (subagent_call(4, "Look for stale fixtures", None), CLOSED),
                (subagent_call(5, "Profile the first frame", None), CLOSED),
                (
                    row(RowKind::Subagent {
                        description: "Survey the redraw paths".into(),
                        continued: false,
                        state: CallPhase::Succeeded,
                        answer: String::new(),
                        duration_ms: Some(95_000),
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_subagent_group",
            "Four subagents started in one turn, folded into one row with where they stand, one waiting on you; then the same group opened, each call on its own line under the row that folds it again.",
            {
                let mut folded = subagent_call(1, "Survey the redraw paths", None);
                folded.id = "call-1".into();
                folded.group = Some(group_of(false));
                let mut open: Vec<(Row, RowState)> = (1..=4)
                    .map(|n| {
                        let mut call = subagent_call(n, GROUP_TASKS[n - 1], None);
                        call.id = format!("call-{n}");
                        call.group = Some(group_of(true));
                        (call, CLOSED)
                    })
                    .collect();
                open.insert(0, (folded, CLOSED));
                open
            },
        ),
        (
            "row_background",
            "A background command still running and one that ended.",
            vec![
                (
                    row(RowKind::Background {
                        command: "npm run dev".into(),
                        description: String::new(),
                        running: true,
                        duration_ms: None,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Background {
                        command: "cargo watch".into(),
                        description: String::new(),
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
                        verdict: PlanVerdict::ApprovedSwitching,
                        mode: "auto".into(),
                        mode_name: "Auto".into(),
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
                        mode: String::new(),
                        mode_name: String::new(),
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
                        failed: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::TurnEnd {
                        duration_ms: Some(3_000),
                        failed: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        ("row_stopped", "You stopped it.", vec![(row(RowKind::Stopped), CLOSED)]),
        (
            "row_compaction",
            "Compaction with its counts, an automatic one without, and one amux ran while the agent sat idle.",
            vec![
                (
                    row(RowKind::Compaction {
                        tokens_before: Some(148_000),
                        tokens_after: Some(22_000),
                        automatic: false,
                        idle: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Compaction {
                        tokens_before: None,
                        tokens_after: None,
                        automatic: true,
                        idle: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Compaction {
                        tokens_before: Some(248_000),
                        tokens_after: Some(31_000),
                        automatic: false,
                        idle: true,
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
                    to: "gpt-5-mini".into(),
                    reason: "usage limit".into(),
                }),
                CLOSED,
            )],
        ),
        (
            "row_boundary",
            "Session boundaries: cleared and the daemon lost.",
            [(BoundaryKind::Cleared, ""), (BoundaryKind::DaemonLost, "")]
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
            "Messages between agents: one received with a file attached, a child's finished report, one sent and delivered, and one the recipient refused.",
            vec![
                (
                    row(RowKind::AgentMessage {
                        from: "planner".into(),
                        kind: EnvelopeKind::Message,
                        text: vec![
                            Segment::Text("Take the relay half. ".into()),
                            Segment::Attachment(AttachmentView::File(wire::BlobRef {
                                name: "relay-notes.md".into(),
                                mime: "text/markdown".into(),
                                size: 2_048,
                                ..Default::default()
                            })),
                        ],
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
                        text: vec![Segment::Text("3 specs updated".into())],
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
                        text: vec![Segment::Text("Start on the store.".into())],
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
                        text: vec![Segment::Text("Stop now.".into())],
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
fn patch(number: Option<u32>, kind: LineKind, text: &str) -> PatchLine {
    PatchLine {
        number,
        kind,
        text: text.into(),
    }
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
        default_to_no: false,
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
                ..choice(ChoiceOutcome::Deny)
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
    rejected.state = CardState::Rejected(Refusal::of(wire::Refusal::ClosedAsk));
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
                    patch: vec![FilePatch {
                        file: None,
                        hunks: vec![PatchHunk {
                            start: Some(HunkStart { old: 10, new: 10 }),
                            lines: vec![
                                patch(Some(10), LineKind::Context, "let a = 1;"),
                                patch(Some(11), LineKind::Removed, "let b = 2;"),
                                patch(Some(11), LineKind::Added, "let b = 3;"),
                                patch(Some(12), LineKind::Added, "let c = 4;"),
                            ],
                        }],
                    }],
                    line: Some(11),
                    reason: String::new(),
                    created: Some(false),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::AllowForSession),
                    choice(ChoiceOutcome::Deny),
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
                    arguments: "{\n  \"repo\": \"ada/amux\",\n  \"title\": \"Flicker\"\n}".into(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::Deny),
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
                    choice(ChoiceOutcome::Deny),
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
            "A plan to approve, approve switching to the permission the agent offers, or send back with a note.",
            card(
                AskBody::Plan {
                    title: None,
                    body: "\n1. Collapse the pairing failures into one error.\n2. Update the three specs.\n3. Keep the wire codes.".into(),
                },
                vec![
                    choice(ChoiceOutcome::ApprovePlan {
                        mode: String::new(),
                        mode_name: String::new(),
                    }),
                    choice(ChoiceOutcome::ApprovePlan {
                        mode: "auto".into(),
                        mode_name: "Auto".into(),
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
                default_to_no: false,
                ..card(
                    AskBody::Plan {
                        title: None,
                        body: "\n1. Collapse the pairing failures into one error.".into(),
                    },
                    vec![
                        choice(ChoiceOutcome::ApprovePlan {
                            mode: String::new(),
                            mode_name: String::new(),
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
                    local_page: false,
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
            "ask_link_local",
            "A tool server's link to a page on the agent's own machine.",
            card(
                AskBody::Link {
                    server: "docs".into(),
                    message: "Sign in to finish connecting docs".into(),
                    url: "http://localhost:8765/oauth/start".into(),
                    local_page: true,
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
                    choice(ChoiceOutcome::Deny),
                ],
            ),
            false,
            vec![],
        ),
        (
            "ask_unanswerable",
            "The escape from an ask this client cannot answer: the reason, and the agent's own terminal where it has one; ctrl+x interrupts the turn.",
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

/// The row resting on the composer's box, over a plain composer: what is
/// still in flight in the chat, and how many background jobs run.
fn edge_rows() -> Vec<(&'static str, &'static str, ui_view::Overview, usize)> {
    use ui_view::{Overview, TaskLine, TaskMark, TasksView};

    let task = |subject: &str, mark| TaskLine {
        subject: subject.into(),
        mark,
    };
    vec![(
        "composer_tasks",
        "A task list part done on the row above the composer: the task in progress by name, how many of the tasks are done, the background jobs still running and ctrl+o.",
        Overview {
            tasks: Some(TasksView {
                done: 1,
                total: 3,
                current: "Reviewing the draft".into(),
                entries: vec![
                    task("Draft", TaskMark::Done),
                    task("Review", TaskMark::Current),
                    task("Publish", TaskMark::Todo),
                ],
            }),
            ..Overview::default()
        },
        2,
    )]
}

/// A provider subagent's composer in each way its stop can stand.
fn native_composers() -> Vec<(&'static str, &'static str, wire::Kind, ui_state::JobStop)> {
    use ui_state::JobStop;
    vec![
        (
            "composer_native",
            "A headless Claude subagent's composer: no field, only that Claude runs it and it takes no messages, and Ctrl+X to stop it.",
            wire::Kind::ClaudeSdk,
            JobStop::Offered {
                key: "task-1".into(),
            },
        ),
        (
            "composer_native_stopping",
            "A subagent's composer once its stop went: stopping, and no Stop to press again.",
            wire::Kind::Codex,
            JobStop::Stopping,
        ),
        (
            "composer_native_refused",
            "A subagent's composer after its provider refused the stop: the provider's words in red, and Stop offered again.",
            wire::Kind::ClaudeSdk,
            JobStop::Refused {
                key: "task-1".into(),
                error: "no running task with that id".into(),
            },
        ),
        (
            "composer_native_terminal",
            "A terminal Claude subagent's composer, or one that ended: no field and no Stop, since terminal Claude stops no single subagent.",
            wire::Kind::ClaudePty,
            JobStop::Unavailable,
        ),
    ]
}

fn composers() -> Vec<(&'static str, &'static str, Composer, Reach, &'static str)> {
    vec![
        (
            "composer_exited",
            "The exited composer: one Enter resumes the agent with the draft as its first prompt.",
            Composer::Resume,
            Reach::Online(wire::RouteKind::Direct),
            "Carry on from the failing test.",
        ),
        (
            "composer_exited_empty",
            "The exited composer before anything is typed.",
            Composer::Resume,
            Reach::Online(wire::RouteKind::Direct),
            "",
        ),
        (
            "composer_detached",
            "The composer while the agent's host is away: Enter sends nothing and the draft stays until the host is back.",
            Composer::Waiting(Waiting::Detached),
            Reach::Offline(Offline::Plain),
            "",
        ),
        (
            "composer_detached_signed_out",
            "The composer while the agent's host is away and this machine is signed out of its account: the cause is this machine's, and nothing sends until it signs in.",
            Composer::Waiting(Waiting::Detached),
            Reach::Offline(Offline::SignedOut),
            "",
        ),
        (
            "composer_detached_revoked",
            "The composer while the agent's host has said it no longer trusts this machine: nothing sends until the two are paired again.",
            Composer::Waiting(Waiting::Detached),
            Reach::Offline(Offline::Revoked),
            "",
        ),
    ]
}

/// Whole chats around their conversation: the header's change totals in
/// both comparisons with the overview open on the changed files by folder
/// and the background jobs, and the overview's usage windows for each
/// provider.
fn chat_surroundings(theme: Theme) -> Vec<(&'static str, &'static str, Buffer)> {
    surroundings()
        .into_iter()
        .map(|(name, shows, mut scene)| {
            let (width, height) = CHAT_SIZE;
            let mut terminal =
                Terminal::new(TestBackend::new(width, height)).expect("test terminal");
            terminal
                .draw(|frame| scene.draw(frame, frame.area(), theme))
                .expect("draw");
            (name, shows, terminal.backend().buffer().clone())
        })
        .collect()
}

/// A chat as the person sees it, over the session it is drawn from.
pub struct ChatScene {
    pub view: crate::chat::ChatView,
    pub state: ui_state::SessionState,
    pub now: i64,
}

impl ChatScene {
    pub fn draw(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        theme: Theme,
    ) {
        self.view
            .draw(frame, area, &self.state, None, None, self.now, theme);
    }
}

/// Chats with the overview open beside them: changes uncommitted and on
/// the branch, and Claude and Codex near a usage limit.
pub fn surroundings() -> Vec<(&'static str, &'static str, ChatScene)> {
    around_a_chat().0
}

/// A Claude chat with the overview open on everything at once: its
/// background jobs, the uncommitted files by folder and the usage windows.
pub fn chat_overview() -> ChatScene {
    around_a_chat().1
}

/// The [`chat_overview`] with the Stop window asking before its first
/// background job is stopped.
pub fn chat_overview_stop() -> ChatScene {
    let mut scene = chat_overview();
    scene.view.open_pane();
    scene.view.key(
        &scene.state,
        crossterm::event::KeyCode::Char('x').into(),
        Theme::default(),
    );
    scene
}

/// The [`surroundings`], and the [`chat_overview`] built from the same
/// parts.
fn around_a_chat() -> (Vec<(&'static str, &'static str, ChatScene)>, ChatScene) {
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
        checkout: None,
        branch: Some("overview-files".into()),
        base_branch: Some("main".into()),
        uncommitted: Some(totals(4, 42, 7)),
        on_branch: Some(totals(6, 120, 30)),
    };
    use wire::FileStatus::{Changed, Created, Deleted};
    use wire::{ChangedFile, FileStatus, GetChangesResponse};
    let file = |path: &str, added, removed, status: FileStatus| ChangedFile {
        path: path.into(),
        added,
        removed,
        status: status as i32,
        language: "rust".into(),
        ..Default::default()
    };
    let uncommitted = GetChangesResponse {
        files: vec![
            file("crates/ui-view/src/overview.rs", 30, 4, Changed),
            file("README.md", 2, 0, Changed),
            file("crates/tui/src/chat/pane.rs", 9, 3, Changed),
            file("crates/tui/src/chat/changes.rs", 1, 0, Created),
        ],
        ..Default::default()
    };
    let on_branch = GetChangesResponse {
        base_branch: "main".into(),
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
                item_key: "t1".into(),
                command: "npm run dev -- --port 5173".into(),
                started_at_ms: now - 14 * MINUTE,
                key: "b1".into(),
                ..Default::default()
            },
            wire::BackgroundJob {
                item_key: "t2".into(),
                command: "cargo watch -x 'test -p ui-view'".into(),
                started_at_ms: now - 40_000,
                key: "b2".into(),
                stopping: true,
                ..Default::default()
            },
        ],
    };
    let meter = |used_percent, resets_at_ms: i64, state: wire::UsageState| {
        Some(wire::UsageMeter {
            used_percent,
            resets_at_ms: Some(resets_at_ms),
            state: state as i32,
            used_unknown: false,
        })
    };
    use wire::UsageState::{NearLimit, Ok};
    let claude_usage = wire::ClaudeUsage {
        state: NearLimit as i32,
        windows: vec![
            wire::ClaudeUsageWindow {
                limit: wire::Limit::FiveHour as i32,
                model: None,
                provider_name: "five_hour".into(),
                meter: meter(91.0, now + 2 * HOUR, NearLimit),
            },
            wire::ClaudeUsageWindow {
                limit: wire::Limit::Weekly as i32,
                model: None,
                provider_name: "seven_day".into(),
                meter: meter(40.0, now + 72 * HOUR, Ok),
            },
            wire::ClaudeUsageWindow {
                limit: wire::Limit::Weekly as i32,
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
                limit: wire::Limit::FiveHour as i32,
                window_minutes: 300,
                meter: meter(95.0, now + 3 * HOUR, NearLimit),
            },
            wire::CodexUsageWindow {
                limit: wire::Limit::Weekly as i32,
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
                use wire::claude_item::Kind as K;
                wire::ClaudeItem {
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
    let scene =
        |state: &SessionState, comparison: Comparison, diff: Option<&GetChangesResponse>| {
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
            ChatScene {
                view,
                state: state.clone(),
                now,
            }
        };

    let claude_working = wire::ClaudeSnapshot {
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
        wire::ClaudeSnapshot {
            model: Some("opus".into()),
            model_name: Some("Opus".into()),
            permission: Some("default".into()),
            usage: Some(claude_usage.clone()),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let everything = session(
        Kind::ClaudeSdk,
        wire::ClaudeSnapshot {
            model: Some("opus".into()),
            model_name: Some("Opus".into()),
            permission: Some("default".into()),
            background_jobs: Some(jobs.clone()),
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
            usage: Some(codex_usage),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let overview = scene(&everything, Comparison::Uncommitted, Some(&uncommitted));
    let goldens = vec![
        (
            "chat_changes_uncommitted",
            "A chat counting uncommitted changes: the header's [Diff +42 −7] from the agent's row, and the overview open beside it with the two background jobs, each command with how long it has run, and the four changed files under their folders, root files first, each with its lines added and removed.",
            scene(&changes, Comparison::Uncommitted, Some(&uncommitted)),
        ),
        (
            "chat_changes_on_branch",
            "The same chat counting everything on its branch: the header's [Diff vs main +120 −30], and the overview listing the branch's six changed files by folder, a created and a deleted file among them.",
            scene(&changes, Comparison::OnBranch, Some(&on_branch)),
        ),
        (
            "chat_usage_claude",
            "A Claude chat near a usage limit, the overview listing every window with its name, how full it is, its state and when it resets: the 5-hour limit near, the weekly limit fine, and Fable's own weekly limit near.",
            scene(&claude, Comparison::Uncommitted, None),
        ),
        (
            "chat_usage_codex",
            "A Codex chat near a usage limit, the overview listing its 5-hour limit near and its weekly limit fine, each with its fullness and reset time, and the credits left.",
            scene(&codex, Comparison::Uncommitted, None),
        ),
    ];
    (goldens, overview)
}

/// The permission and mode controls on a chat's composer, each agent with
/// the catalogue its interpreter writes: the edge naming what is not the
/// agent's normal one, the key under it that changes it, and the
/// permission picker Ctrl+S then p opens.
fn chat_controls(theme: Theme) -> Vec<(&'static str, &'static str, Buffer)> {
    use crate::chat::ChatView;

    let now = written_at();
    let draw = |state: &ui_state::SessionState, keys: &[KeyEvent]| {
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
    control_chats()
        .into_iter()
        .map(|(name, shows, state, keys)| (name, shows, draw(&state, &keys)))
        .collect()
}

/// A headless Claude chat with Ctrl+S then m pressed: the model picker
/// open over the composer's edge.
pub fn chat_settings() -> ChatScene {
    use crossterm::event::KeyModifiers;

    let (_, _, state, _) = control_chats()
        .into_iter()
        .next()
        .expect("the headless Claude chat comes first");
    let now = written_at();
    let mut view = crate::chat::ChatView::new(b"agent".to_vec(), now, false);
    let theme = Theme::default();
    for key in [
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        KeyEvent::from(KeyCode::Char('m')),
    ] {
        view.key(&state, key, theme);
    }
    ChatScene { view, state, now }
}

/// The commands a headless Claude chat offers, as its catalogue lists them.
fn offered_commands() -> Vec<wire::OfferedCommand> {
    [
        (
            "clear",
            "Clear the conversation history and free up context",
            "",
            "",
        ),
        (
            "compact",
            "Clear the history but keep a summary in context",
            "[instructions]",
            "",
        ),
        ("context", "Show what is using the context window", "", ""),
        ("cost", "Show what this session has cost so far", "", ""),
        (
            "init",
            "Write a CLAUDE.md that documents the codebase",
            "",
            "",
        ),
        (
            "pr-comments",
            "Get the comments from a GitHub pull request",
            "<pr>",
            "",
        ),
        ("review", "Review a pull request", "[pr]", ""),
        (
            "security-review",
            "Review the pending changes on this branch for security problems",
            "",
            "",
        ),
        (
            "code-review:review",
            "Review the current diff for correctness bugs",
            "[level] [target]",
            "code-review",
        ),
        (
            "release-notes",
            "Write release notes since the last tag",
            "",
            "repo",
        ),
    ]
    .into_iter()
    .map(
        |(name, description, argument_hint, source)| wire::OfferedCommand {
            name: name.into(),
            description: description.into(),
            argument_hint: argument_hint.into(),
            source: source.into(),
        },
    )
    .collect()
}

/// A headless Claude chat with `typed` in its composer: the commands it
/// starts offered over the composer.
pub fn chat_commands(typed: &str) -> ChatScene {
    let (_, _, state, _) = control_chats()
        .into_iter()
        .next()
        .expect("the headless Claude chat comes first");
    let now = written_at();
    let mut view = crate::chat::ChatView::new(b"agent".to_vec(), now, false);
    for c in typed.chars() {
        view.key(&state, KeyEvent::from(KeyCode::Char(c)), Theme::default());
    }
    ChatScene { view, state, now }
}

/// A reply using every markdown form, on a Codex chat with full access, so
/// one screen shows how formatted prose and the permission edge are drawn.
pub fn chat_formatted() -> ChatScene {
    use prost::Message as _;
    use ui_state::Msg;
    use wire::codex_item::Kind as K;

    const REPLY: &str = "\
# Grouping the changed files

The overview now lists files under their folders. See the \
[design notes](https://amux.sh/docs) or https://amux.sh for more.

## What changed

- **Folders** come after root files
- `crates/tui/src/chat/pane.rs` draws the headings
- *Renames* keep their old path

> A quote from the earlier discussion.

### Next

```rust
fn group(files: &[File]) -> Vec<Folder> {
    todo!(\"sort by folder\")
}
```
";
    let (_, _, mut state, _) = control_chats()
        .into_iter()
        .find(|(name, ..)| *name == "chat_controls_codex")
        .expect("the full-access Codex chat is drawn");
    let now = written_at();
    let reply = wire::Item {
        key: "k3".into(),
        order: 3,
        revision: 3,
        text: REPLY.into(),
        kind: wire::Kind::Codex as i32,
        body: wire::CodexItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        at_ms: now,
        ..Default::default()
    };
    state.update(Msg::Event(wire::SessionEvent {
        of: Some(wire::session_event::Of::Item(reply)),
    }));
    let view = crate::chat::ChatView::new(b"agent".to_vec(), now, false);
    ChatScene { view, state, now }
}

/// A Codex chat with full access and its permission picker open.
pub fn chat_permissions() -> ChatScene {
    let (_, _, state, keys) = control_chats()
        .into_iter()
        .find(|(name, ..)| *name == "chat_controls_codex")
        .expect("the full-access Codex chat is drawn");
    let now = written_at();
    let mut view = crate::chat::ChatView::new(b"agent".to_vec(), now, false);
    for key in keys {
        view.key(&state, key, Theme::default());
    }
    ChatScene { view, state, now }
}

/// The chats [`chat_controls`] draws, each with the keys pressed on it.
#[allow(clippy::type_complexity)]
fn control_chats() -> Vec<(
    &'static str,
    &'static str,
    ui_state::SessionState,
    Vec<KeyEvent>,
)> {
    use crossterm::event::KeyModifiers;
    use prost::Message as _;
    use ui_state::{Connection, Msg, SessionState};
    use wire::{Kind, OfferedMode, OfferedPermission, session_event};

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
        permission("default", "Ask", true),
        permission("auto", "Auto", true),
        OfferedPermission {
            never_asks: true,
            ..permission("full-access", "Full access", true)
        },
    ];
    let codex_modes = vec![
        OfferedMode {
            value: "default".into(),
            display_name: "Normal".into(),
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
            _ => {
                use wire::claude_item::Kind as K;
                wire::ClaudeItem {
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
    let permission_picker = [
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    ];

    let claude_sdk = session(
        Kind::ClaudeSdk,
        wire::ClaudeSnapshot {
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
            commands: offered_commands(),
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
        ..Default::default()
    });
    let codex_custom = codex_settings(wire::CodexSnapshot {
        mode: Some("default".into()),
        approval_policy: Some("untrusted".into()),
        ..Default::default()
    });
    let claude_pty = session(
        Kind::ClaudePty,
        wire::ClaudeSnapshot {
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
            claude_sdk,
            permission_picker.to_vec(),
        ),
        (
            "chat_controls_codex",
            "A Codex chat with full access in plan mode: the edge names both, shift+tab moves the mode, and Ctrl+S then p has opened the permission picker with read only, default, auto and full access, full access marked as acting without asking.",
            codex_plan,
            permission_picker.to_vec(),
        ),
        (
            "chat_controls_codex_custom",
            "A Codex chat whose approval policy and sandbox match no named permission: the edge reads custom, and shift+tab still moves the mode.",
            codex_custom,
            vec![],
        ),
        (
            "chat_controls_claude_terminal",
            "A terminal Claude chat in plan: the edge names the permission, and shift+tab presses Claude's own cycle key, the only way its permission changes.",
            claude_pty,
            vec![],
        ),
    ]
}
