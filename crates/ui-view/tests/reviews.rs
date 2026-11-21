//! The review page composed from a review's summary, the bodies fetched for
//! it and the person's choices: authored goldens for each state the page
//! can be in, read as text, and the anchors, chips and sent reviews built
//! beside it.

use std::fmt::Write as _;
use std::path::Path;

use ui_view::*;
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor as WireAnchor, ReviewComparedLines, ReviewDefinition,
    ReviewDefinitionKind, ReviewFileDiff, ReviewHunk, ReviewLine, ReviewLineKind, ReviewLineRange,
    ReviewPlacement, ReviewRound, ReviewRowEntry, ReviewRun as WireRun, ReviewSentComment,
    ReviewSide, ReviewSummary, ReviewTokenKind, ReviewerComment, SENT_REVIEW, SentReview,
    review_placement,
};

const SNAPSHOT: &str = "s2";
const BASE: &str = "b0";
const LAST_ROUND: &str = "s1";

fn line(kind: ReviewLineKind, old: u32, new: u32, text: &str) -> ReviewLine {
    ReviewLine {
        kind: kind as i32,
        old_line: old,
        new_line: new,
        text: text.into(),
        runs: Vec::new(),
    }
}

fn context(old: u32, new: u32, text: &str) -> ReviewLine {
    line(ReviewLineKind::Context, old, new, text)
}

fn added(new: u32, text: &str) -> ReviewLine {
    line(ReviewLineKind::Added, 0, new, text)
}

fn removed(old: u32, text: &str) -> ReviewLine {
    line(ReviewLineKind::Removed, old, 0, text)
}

fn def(
    kind: ReviewDefinitionKind,
    label: &str,
    start: u32,
    end: u32,
    parent: Option<u32>,
) -> ReviewDefinition {
    ReviewDefinition {
        kind: kind as i32,
        name: label.rsplit(' ').next().unwrap_or_default().into(),
        label: label.into(),
        start_line: start,
        end_line: end,
        parent,
        side: ReviewSide::Snapshot as i32,
    }
}

fn impl_store() -> ReviewDefinition {
    def(ReviewDefinitionKind::Impl, "impl Store", 5, 90, None)
}

fn fn_open() -> ReviewDefinition {
    def(ReviewDefinitionKind::Method, "fn open", 30, 48, Some(0))
}

fn fn_close() -> ReviewDefinition {
    def(ReviewDefinitionKind::Method, "fn close", 66, 75, Some(0))
}

fn changed(path: &str, status: wire::FileStatus, added: u32, removed: u32) -> wire::ChangedFile {
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
        ..Default::default()
    }
}

fn flagged(mut file: wire::ChangedFile, flag: wire::FileFlag) -> wire::ChangedFile {
    file.flags.push(flag as i32);
    file
}

fn range(start: u32, end: u32) -> ReviewLineRange {
    ReviewLineRange { start, end }
}

enum At {
    Lines(u32, u32),
    /// On removed lines of the comment's own commit.
    Compared(u32, u32),
    File,
    Outdated,
}

fn comment(
    id: &str,
    commit: &str,
    path: &str,
    lines: (u32, u32),
    text: &str,
    round: Option<u32>,
    at: At,
) -> ReviewerComment {
    ReviewerComment {
        id: id.as_bytes().to_vec(),
        anchor: Some(WireAnchor {
            commit: commit.into(),
            path: path.into(),
            lines: Some(range(lines.0, lines.1)),
        }),
        text: text.into(),
        snippet: Vec::new(),
        scope: Vec::new(),
        round,
        placement: Some(ReviewPlacement {
            at: Some(match at {
                At::Lines(start, end) => review_placement::At::Lines(range(start, end)),
                At::Compared(start, end) => review_placement::At::Compared(ReviewComparedLines {
                    commit: commit.into(),
                    lines: Some(range(start, end)),
                }),
                At::File => review_placement::At::File(wire::Empty {}),
                At::Outdated => review_placement::At::Outdated(wire::Empty {}),
            }),
            changed_near: false,
            path: path.into(),
        }),
        revision: 1,
        sent_revision: round.map(|_| 1),
    }
}

/// The store's change against the base: an edit to `fn open` and a line
/// added to `fn close`.
fn store_all() -> ReviewFileDiff {
    let mut opened = removed(41, "        let file = open(&path)?;");
    opened.runs = vec![
        run(ReviewTokenKind::Plain, 8),
        run(ReviewTokenKind::Keyword, 3),
    ];
    let mut flagged = added(41, "        let file = open_with_flags(&path, flags)?;");
    flagged.runs = vec![
        run(ReviewTokenKind::Plain, 8),
        run(ReviewTokenKind::Keyword, 3),
    ];
    ReviewFileDiff {
        path: "src/store.rs".into(),
        hunks: vec![
            ReviewHunk {
                old_commit: BASE.into(),
                old_start: 40,
                new_start: 40,
                scope: vec![impl_store(), fn_open()],
                lines: vec![
                    context(40, 40, "    pub fn open(path: &Path) -> Store {"),
                    opened,
                    flagged,
                    added(42, "        log::debug!(\"opened\");"),
                    context(42, 43, "        Store { file }"),
                    context(43, 44, "    }"),
                    context(44, 45, ""),
                    context(45, 46, "    /// Closes the store."),
                ],
            },
            store_close(BASE, 68),
        ],
        outline: vec![impl_store(), fn_open(), fn_close()],
        context: Vec::new(),
    }
}

/// The hunk adding a line to `fn close`, against `old_commit`.
fn store_close(old_commit: &str, old_start: u32) -> ReviewHunk {
    ReviewHunk {
        old_commit: old_commit.into(),
        old_start,
        new_start: 70,
        scope: vec![impl_store(), fn_close()],
        lines: vec![
            context(old_start, 70, "    pub fn close(self) {"),
            added(71, "        self.file.sync_all();"),
            context(old_start + 1, 72, "        drop(self.file);"),
            context(old_start + 2, 73, "    }"),
            context(old_start + 3, 74, ""),
        ],
    }
}

fn run(kind: ReviewTokenKind, len: u32) -> WireRun {
    WireRun {
        kind: kind as i32,
        len,
    }
}

fn created() -> ReviewFileDiff {
    ReviewFileDiff {
        path: "src/new.rs".into(),
        hunks: vec![ReviewHunk {
            old_commit: BASE.into(),
            old_start: 0,
            new_start: 1,
            scope: Vec::new(),
            lines: vec![added(1, "//! New."), added(2, "pub fn new() {}")],
        }],
        outline: Vec::new(),
        context: Vec::new(),
    }
}

fn files() -> Vec<wire::ChangedFile> {
    vec![
        changed("src/store.rs", wire::FileStatus::Changed, 3, 1),
        changed("src/new.rs", wire::FileStatus::Created, 2, 0),
        flagged(
            changed("Cargo.lock", wire::FileStatus::Changed, 40, 12),
            wire::FileFlag::Lockfile,
        ),
        wire::ChangedFile {
            binary: true,
            ..changed("assets/logo.png", wire::FileStatus::Changed, 0, 0)
        },
        flagged(
            changed("vendor/zlib.c", wire::FileStatus::Changed, 9, 9),
            wire::FileFlag::Vendored,
        ),
        flagged(
            changed("src/schema.rs", wire::FileStatus::Changed, 900, 0),
            wire::FileFlag::Generated,
        ),
        flagged(
            changed("data/big.json", wire::FileStatus::Changed, 1, 1),
            wire::FileFlag::Large,
        ),
    ]
}

fn summary() -> ReviewSummary {
    ReviewSummary {
        id: b"review".to_vec(),
        base_branch: "main".into(),
        base_commit: BASE.into(),
        snapshot: SNAPSHOT.into(),
        files: files(),
        objects: "/agents/a/review/objects".into(),
        root: "/work/repo".into(),
        ..Default::default()
    }
}

/// The first look: drafts on an added line, a removed line and the
/// whole file.
fn first_look() -> ReviewSummary {
    ReviewSummary {
        comments: vec![
            comment(
                "d1",
                SNAPSHOT,
                "src/store.rs",
                (41, 41),
                "Why the flags?",
                None,
                At::Lines(41, 41),
            ),
            comment(
                "d2",
                BASE,
                "src/store.rs",
                (41, 41),
                "This read the path twice.",
                None,
                At::Compared(41, 41),
            ),
            comment(
                "d3",
                SNAPSHOT,
                "src/store.rs",
                (0, 0),
                "Split this file.",
                None,
                At::File,
            ),
        ],
        ..summary()
    }
}

fn snippet() -> Vec<ReviewLine> {
    vec![
        context(9, 9, "    fn ready(&self) -> bool {"),
        removed(10, "        self.open"),
        added(10, "        self.open && !self.closing"),
    ]
}

/// The second look: a round sent on the last snapshot, the agent changed
/// `fn close` since.
fn second_look() -> ReviewSummary {
    let mut near = comment(
        "r3",
        LAST_ROUND,
        "src/store.rs",
        (10, 10),
        "Is ready still right?",
        Some(1),
        At::Lines(10, 10),
    );
    near.placement.as_mut().unwrap().changed_near = true;
    near.snippet = snippet();
    near.scope = vec![
        impl_store(),
        def(ReviewDefinitionKind::Method, "fn ready", 9, 12, Some(0)),
    ];
    let mut outdated = comment(
        "r2",
        LAST_ROUND,
        "src/store.rs",
        (41, 41),
        "Don't log the path.",
        Some(1),
        At::Outdated,
    );
    outdated.snippet = vec![added(41, "        log::debug!(\"{path:?}\");")];
    let mut readme = comment(
        "r4",
        LAST_ROUND,
        "README.md",
        (3, 3),
        "Mention the flags.",
        Some(1),
        At::Lines(3, 3),
    );
    readme.snippet = vec![context(3, 3, "Opens a store.")];
    let mut gone = comment(
        "r5",
        LAST_ROUND,
        "old/gone.rs",
        (1, 1),
        "Is this used?",
        Some(1),
        At::Outdated,
    );
    gone.snippet = vec![context(1, 1, "pub fn gone() {}")];
    let mut files = files();
    files.push(changed("README.md", wire::FileStatus::Changed, 2, 0));
    ReviewSummary {
        rounds: vec![ReviewRound {
            snapshot: LAST_ROUND.into(),
            base_commit: BASE.into(),
        }],
        since_last: vec![changed("src/store.rs", wire::FileStatus::Changed, 1, 0)],
        comments: vec![
            comment(
                "r1",
                LAST_ROUND,
                "src/store.rs",
                (70, 70),
                "Sync before drop.",
                Some(1),
                At::Lines(72, 72),
            ),
            outdated,
            near,
            readme,
            gone,
        ],
        files,
        ..summary()
    }
}

fn store_since_last() -> ReviewFileDiff {
    ReviewFileDiff {
        path: "src/store.rs".into(),
        hunks: vec![store_close(LAST_ROUND, 70)],
        outline: vec![impl_store(), fn_close()],
        context: Vec::new(),
    }
}

/// The page as text: its header, banners and rows, one to a line. A
/// line's changed words are in brackets.
fn render(page: &ReviewPage) -> String {
    let mut out = String::new();
    match &page.header {
        Some(header) => {
            let views: Vec<String> = header
                .views
                .iter()
                .map(|tab| format!("{:?} {}", tab.view, tab.files))
                .collect();
            writeln!(
                out,
                "header base {} at {} snapshot {} view {:?} against {} views [{}] rounds {} files {} +{} -{} drafts {} sent {} discard {} start-over {}",
                header.base_branch,
                header.base_commit,
                header.snapshot,
                header.view,
                header.compared_with,
                views.join(", "),
                header.rounds,
                header.files,
                header.added,
                header.removed,
                header.drafts,
                header.sent,
                header.discard_drafts,
                header.start_over,
            )
            .unwrap();
        }
        None => writeln!(out, "header none opening {}", page.opening).unwrap(),
    }
    writeln!(out, "banners {:?}", page.banners).unwrap();
    writeln!(out, "empty {:?} fetch {:?}", page.empty, page.to_fetch).unwrap();
    for row in &page.rows {
        writeln!(out, "  {}", row_text(&row.of)).unwrap();
    }
    out
}

fn row_text(row: &ReviewRowOf) -> String {
    match row {
        ReviewRowOf::File(head) => {
            let outline: Vec<String> = head
                .outline
                .iter()
                .map(|entry| {
                    format!(
                        "{}{} {}{}-{}{}",
                        "  ".repeat(entry.depth as usize),
                        entry.label,
                        if entry.compared { "compared " } else { "" },
                        entry.start_line,
                        entry.end_line,
                        entry.row.map(|row| format!(" @{row}")).unwrap_or_default()
                    )
                })
                .collect();
            format!(
                "file {} {:?} +{} -{}{}{} flags {:?}{}{} comments {}{}",
                head.path,
                head.status,
                head.added,
                head.removed,
                if head.binary { " binary" } else { "" },
                if head.folded { " folded" } else { "" },
                head.flags,
                if head.changed_since_last {
                    " changed-since"
                } else {
                    ""
                },
                if head.in_change {
                    ""
                } else {
                    " outside-the-change"
                },
                head.comments,
                if outline.is_empty() {
                    String::new()
                } else {
                    format!(" outline [{}]", outline.join(", "))
                }
            )
        }
        ReviewRowOf::Comment(comment) => {
            let mut text = format!(
                "    comment {} {:?} {:?}{}{} scope {:?} {:?}",
                String::from_utf8_lossy(&comment.id),
                comment.state,
                comment.at,
                if comment.changed_near {
                    " changed-near"
                } else {
                    ""
                },
                if comment.folded { " folded" } else { "" },
                comment.scope,
                comment.text,
            );
            for line in &comment.snippet {
                write!(text, "\n      snippet {}", code(line)).unwrap();
            }
            text
        }
        ReviewRowOf::Hunk(hunk) => format!(
            "hunk -{} +{} against {} in {}",
            hunk.old_start,
            hunk.new_start,
            hunk.old_commit,
            hunk.scope.join(" › ")
        ),
        ReviewRowOf::Line(line) => code(line),
        ReviewRowOf::Gap(gap) => {
            let ask = |ask: &Option<ReviewContextAsk>| match ask {
                Some(ask) => format!(
                    "{}-{}{}",
                    ask.start,
                    ask.end,
                    ask.definition
                        .as_ref()
                        .map(|label| format!(" to {label}"))
                        .unwrap_or_default()
                ),
                None => "-".into(),
            };
            format!(
                "gap hidden {} up {} down {}{}",
                gap.hidden.map_or("?".into(), |hidden| hidden.to_string()),
                ask(&gap.up),
                ask(&gap.down),
                if gap.loading { " loading" } else { "" }
            )
        }
        ReviewRowOf::Loading { path } => format!("loading {path}"),
        ReviewRowOf::NoLines { path, why } => format!("no lines {path} {why:?}"),
    }
}

fn code(line: &ReviewCodeLine) -> String {
    let sign = match line.kind {
        LineKind::Context => ' ',
        LineKind::Added => '+',
        LineKind::Removed => '-',
    };
    let number = |n: Option<u32>| n.map_or("    ".into(), |n| format!("{n:>4}"));
    let mut text = String::new();
    for run in &line.runs {
        if run.changed {
            write!(text, "[{}]", run.text).unwrap();
        } else {
            text.push_str(&run.text);
        }
    }
    format!(
        "{sign} {} {} |{}{}{}",
        number(line.old_line),
        number(line.new_line),
        text,
        if line.commented { "   ◆" } else { "" },
        if line.extra { "   (more)" } else { "" }
    )
}

fn page(summary: &ReviewSummary, files: &[ReviewFileDiff], state: &ReviewPageState) -> ReviewPage {
    review_page(Some(summary), files, &[], state)
}

fn row_of(page: &ReviewPage, matches: impl Fn(&ReviewRowOf) -> bool) -> usize {
    page.rows
        .iter()
        .position(|row| matches(&row.of))
        .expect("the row is on the page")
}

fn line_at(
    page: &ReviewPage,
    path: &str,
    kind: LineKind,
    old: Option<u32>,
    new: Option<u32>,
) -> usize {
    row_of(page, |row| {
        matches!(row, ReviewRowOf::Line(line)
            if line.path == path && line.kind == kind && line.old_line == old && line.new_line == new)
    })
}

/// Every state of the page, in one golden.
#[test]
fn review_page_goldens() {
    let mut out = String::new();
    let mut case = |title: &str, page: ReviewPage| {
        let mut keys = std::collections::HashSet::new();
        for row in &page.rows {
            assert!(
                keys.insert(row.key.clone()),
                "{title}: key {} twice",
                row.key
            );
        }
        writeln!(out, "## {title}\n{}", render(&page)).unwrap();
    };
    let none = ReviewPageState::default();

    case(
        "first look: drafts on an added line, a removed line and the whole file; flagged files folded; changed words",
        page(&first_look(), &[store_all(), created()], &none),
    );

    let mut asked = ReviewPageState::default();
    let top = ReviewContextAsk {
        path: "src/store.rs".into(),
        commit: SNAPSHOT.into(),
        start: 30,
        end: 39,
        definition: Some("fn open".into()),
    };
    let past_the_end = ReviewContextAsk {
        path: "src/store.rs".into(),
        commit: SNAPSHOT.into(),
        start: 75,
        end: 76,
        definition: Some("fn close".into()),
    };
    let waiting = ReviewContextAsk {
        path: "src/store.rs".into(),
        commit: SNAPSHOT.into(),
        start: 47,
        end: 48,
        definition: Some("fn open".into()),
    };
    asked.asked = vec![top.clone(), past_the_end.clone(), waiting];
    let answers = vec![
        (
            top,
            (30..=39)
                .map(|n| context(n, n, &format!("    // line {n}")))
                .collect(),
        ),
        (past_the_end, vec![context(75, 75, "}")]),
    ];
    let mut with_comment = first_look();
    with_comment.comments = vec![comment(
        "d4",
        SNAPSHOT,
        "src/store.rs",
        (33, 34),
        "Say why.",
        None,
        At::Lines(33, 34),
    )];
    case(
        "more context: the top filled to fn open's start, the end found, one ask waiting, a comment on lines read for context",
        review_page(Some(&with_comment), &[store_all()], &answers, &asked),
    );

    // A file whose change only removes a method: the hunk's scope is the
    // compared commit's, ten lines on from the snapshot's above it, and the
    // hunk reaches the file's end. A comment written on the file before it
    // was renamed sits where the host placed it.
    let mut removing = summary();
    removing.files = vec![wire::ChangedFile {
        old_path: Some("src/old_gone.rs".into()),
        ..changed("src/gone.rs", wire::FileStatus::Renamed, 0, 3)
    }];
    let mut renamed = comment(
        "d6",
        SNAPSHOT,
        "src/old_gone.rs",
        (41, 41),
        "Keep the closing brace's comment.",
        None,
        At::Lines(41, 41),
    );
    renamed.placement.as_mut().unwrap().path = "src/gone.rs".into();
    removing.comments = vec![renamed];
    let mut gone_method = def(ReviewDefinitionKind::Method, "fn gone", 47, 53, None);
    gone_method.side = ReviewSide::Compared as i32;
    let only_removed = ReviewFileDiff {
        path: "src/gone.rs".into(),
        hunks: vec![ReviewHunk {
            old_commit: BASE.into(),
            old_start: 50,
            new_start: 40,
            scope: vec![gone_method.clone()],
            lines: vec![
                context(50, 40, "    }"),
                removed(51, "    fn gone() {"),
                removed(52, "        todo!()"),
                removed(53, "    }"),
                context(54, 41, "}"),
            ],
        }],
        outline: vec![gone_method],
        context: Vec::new(),
    };
    case(
        "a hunk that only removes: more context up to its compared-side definition, no gap past the file's end, a comment seated by its placed path",
        page(&removing, &[only_removed], &none),
    );

    case(
        "since your last review: placed, outdated, changed near, a file unchanged since, one outside the change",
        page(&second_look(), &[store_since_last()], &none),
    );

    let mut opened = ReviewPageState::default();
    opened.toggle_comment(b"r2");
    case(
        "since your last review with the outdated comment opened",
        page(&second_look(), &[store_since_last()], &opened),
    );

    let all = ReviewPageState {
        view: Some(ReviewView::All),
        ..ReviewPageState::default()
    };
    case(
        "all changes, one switch away on the second look",
        page(&second_look(), &[store_all(), created()], &all),
    );

    let round_gone = ReviewPageState {
        notices: vec![ReviewNotice::RoundGone],
        ..ReviewPageState::default()
    };
    case(
        "a round whose commits are gone: since your last review from the comments' snippets",
        page(&second_look(), &[], &round_gone),
    );

    let mut stale = first_look();
    stale.stale = true;
    stale.snapshot = LAST_ROUND.into();
    for comment in &mut stale.comments {
        if let Some(anchor) = &mut comment.anchor
            && anchor.commit == SNAPSHOT
        {
            anchor.commit = LAST_ROUND.into();
        }
    }
    case(
        "drafts on an old snapshot: changed since you started, refresh",
        page(&stale, &[], &none),
    );

    let mut refreshed = first_look();
    refreshed.moved_drafts = 2;
    let mut moved = comment(
        "d5",
        SNAPSHOT,
        "src/store.rs",
        (41, 41),
        "Was on an edited line.",
        None,
        At::File,
    );
    moved.snippet = vec![added(41, "        let file = open(&path)?;")];
    refreshed.comments.push(moved);
    case(
        "refreshed: two drafts moved, the edited one now on its file",
        page(&refreshed, &[], &none),
    );

    let moved_since = ReviewPageState {
        notices: vec![ReviewNotice::Moved],
        ..ReviewPageState::default()
    };
    case(
        "no drafts, the agent changed code since",
        page(&summary(), &[], &moved_since),
    );

    let mut merging = summary();
    merging.merge_in_progress = true;
    case("a merge under way", page(&merging, &[], &none));

    let empty = ReviewSummary {
        files: Vec::new(),
        ..summary()
    };
    case("nothing left to review", page(&empty, &[], &none));

    let mut quiet = second_look();
    quiet.since_last.clear();
    quiet.comments.clear();
    case(
        "nothing changed since your last review",
        page(&quiet, &[], &none),
    );

    case("opening", review_page(None, &[], &[], &none));
    let not_offered = ReviewPageState {
        notices: vec![ReviewNotice::NotOffered],
        ..ReviewPageState::default()
    };
    case(
        "not offered on this host",
        review_page(None, &[], &[], &not_offered),
    );
    let not_a_repository = ReviewPageState {
        notices: vec![ReviewNotice::NotARepository],
        ..ReviewPageState::default()
    };
    case(
        "not a repository",
        review_page(None, &[], &[], &not_a_repository),
    );
    let unreachable = ReviewPageState {
        notices: vec![
            ReviewNotice::SnapshotFailed {
                message: "a file vanished while it was read".into(),
            },
            ReviewNotice::Unreachable,
        ],
        ..ReviewPageState::default()
    };
    case(
        "a snapshot that failed and a host gone: what was loaded still reads",
        page(&first_look(), &[store_all()], &unreachable),
    );

    let mut toggled = ReviewPageState::default();
    for path in ["Cargo.lock", "data/big.json", "src/schema.rs", "src/new.rs"] {
        toggled.toggle_file(path);
    }
    case(
        "folds flipped: a lock file, a large and a generated file opened, an ordinary file folded",
        page(&summary(), &[], &toggled),
    );

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/reviews.golden");
    if std::env::var("UI_VIEW_UPDATE_GOLDENS").as_deref() == Ok("1") {
        std::fs::write(&path, &out).unwrap();
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(golden) if golden == out => {}
        _ => panic!(
            "{} differs; rerun with UI_VIEW_UPDATE_GOLDENS=1 and review\n{out}",
            path.display()
        ),
    }
}

/// The agent undoing its own change since the last round: a line put back
/// and a file it made and then deleted. All changes lists neither, yet since
/// your last review lists, counts, fetches and shows both from that
/// comparison's own list.
#[test]
fn an_undo_since_the_last_round_is_listed_fetched_and_shown() {
    let summary = ReviewSummary {
        files: Vec::new(),
        rounds: vec![ReviewRound {
            snapshot: LAST_ROUND.into(),
            base_commit: BASE.into(),
        }],
        since_last: vec![
            changed("made.txt", wire::FileStatus::Deleted, 0, 1),
            changed("notes.txt", wire::FileStatus::Changed, 1, 1),
        ],
        comments: Vec::new(),
        ..summary()
    };
    let none = ReviewPageState::default();
    let opened = page(&summary, &[], &none);
    let header = opened.header.as_ref().unwrap();
    assert_eq!(header.view, ReviewView::SinceLast);
    assert_eq!(
        (header.files, header.added, header.removed),
        (2, 1, 2),
        "counted from since-last's own list"
    );
    assert_eq!(
        header
            .views
            .iter()
            .map(|tab| (tab.view, tab.files))
            .collect::<Vec<_>>(),
        [(ReviewView::All, 0), (ReviewView::SinceLast, 2)]
    );
    assert_eq!(opened.empty, None, "something changed since");
    assert_eq!(opened.to_fetch, ["made.txt", "notes.txt"]);
    let heads: Vec<(&str, FileStatus, u32, u32, bool)> = opened
        .rows
        .iter()
        .filter_map(|row| match &row.of {
            ReviewRowOf::File(head) => Some((
                head.path.as_str(),
                head.status,
                head.added,
                head.removed,
                head.changed_since_last,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        heads,
        [
            ("made.txt", FileStatus::Deleted, 0, 1, true),
            ("notes.txt", FileStatus::Modified, 1, 1, true),
        ]
    );

    let bodies = [
        ReviewFileDiff {
            path: "made.txt".into(),
            hunks: vec![ReviewHunk {
                old_commit: LAST_ROUND.into(),
                old_start: 1,
                new_start: 0,
                scope: Vec::new(),
                lines: vec![removed(1, "made")],
            }],
            ..ReviewFileDiff::default()
        },
        ReviewFileDiff {
            path: "notes.txt".into(),
            hunks: vec![ReviewHunk {
                old_commit: LAST_ROUND.into(),
                old_start: 3,
                new_start: 3,
                scope: Vec::new(),
                lines: vec![removed(3, "three"), added(3, "line 3")],
            }],
            ..ReviewFileDiff::default()
        },
    ];
    let shown = page(&summary, &bodies, &none);
    assert!(shown.to_fetch.is_empty(), "{:?}", shown.to_fetch);
    let lines: Vec<(&str, &str)> = shown
        .rows
        .iter()
        .filter_map(|row| match &row.of {
            ReviewRowOf::Line(line) => Some((line.path.as_str(), line.text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("made.txt", "made"),
            ("notes.txt", "three"),
            ("notes.txt", "line 3")
        ]
    );
    // All changes lists nothing, and says so.
    let all = page(
        &summary,
        &[],
        &ReviewPageState {
            view: Some(ReviewView::All),
            ..ReviewPageState::default()
        },
    );
    assert_eq!(all.empty, Some(ReviewEmpty::Nothing));
}

#[test]
fn a_selection_anchors_to_the_commit_its_lines_belong_to() {
    let page = page(
        &first_look(),
        &[store_all(), created()],
        &ReviewPageState::default(),
    );
    let path = "src/store.rs";
    let on = |commit: &str, start, end| ReviewAnchor {
        commit: commit.into(),
        path: path.into(),
        start,
        end,
    };
    let context_line = line_at(&page, path, LineKind::Context, Some(40), Some(40));
    let added_line = line_at(&page, path, LineKind::Added, None, Some(41));
    let removed_line = line_at(&page, path, LineKind::Removed, Some(41), None);
    let below = line_at(&page, path, LineKind::Context, Some(42), Some(43));
    assert_eq!(
        anchor(&page, context_line, context_line),
        Some(on(SNAPSHOT, 40, 40))
    );
    assert_eq!(
        anchor(&page, added_line, added_line),
        Some(on(SNAPSHOT, 41, 41))
    );
    assert_eq!(
        anchor(&page, removed_line, removed_line),
        Some(on(BASE, 41, 41)),
        "a removed line is the compared commit's"
    );
    assert_eq!(
        anchor(&page, below, context_line),
        Some(on(SNAPSHOT, 40, 43)),
        "a range across a removed line takes the snapshot's lines in it, in either order"
    );
    let head = row_of(
        &page,
        |row| matches!(row, ReviewRowOf::File(head) if head.path == path),
    );
    assert_eq!(
        anchor(&page, head, head),
        Some(on(SNAPSHOT, 0, 0)),
        "a head alone is the whole file"
    );
    let other = line_at(&page, "src/new.rs", LineKind::Added, None, Some(1));
    assert_eq!(
        anchor(&page, context_line, other),
        None,
        "a selection across files"
    );
    assert_eq!(
        anchor(&page, head, other),
        None,
        "from one file's head into another"
    );
    let hunk = row_of(&page, |row| matches!(row, ReviewRowOf::Hunk(_)));
    assert_eq!(anchor(&page, hunk, hunk), None, "no line selected");
    assert_eq!(
        anchor(&page, context_line, context_line).unwrap().to_wire(),
        WireAnchor {
            commit: SNAPSHOT.into(),
            path: path.into(),
            lines: Some(range(40, 40)),
        }
    );

    // Lines read for more context are the snapshot's.
    let ask = ReviewContextAsk {
        path: path.into(),
        commit: SNAPSHOT.into(),
        start: 30,
        end: 39,
        definition: None,
    };
    let answers = vec![(ask, vec![context(36, 36, "    /// Opens it.")])];
    let page = review_page(
        Some(&first_look()),
        &[store_all()],
        &answers,
        &ReviewPageState::default(),
    );
    let extra = line_at(&page, path, LineKind::Context, Some(36), Some(36));
    assert_eq!(anchor(&page, extra, extra), Some(on(SNAPSHOT, 36, 36)));

    // In since your last review a removed line is the last round's.
    let mut since = second_look();
    since.comments.clear();
    let mut diff = store_since_last();
    diff.hunks[0]
        .lines
        .insert(1, removed(71, "        self.file.flush();"));
    let page = review_page(Some(&since), &[diff], &[], &ReviewPageState::default());
    let gone = line_at(&page, path, LineKind::Removed, Some(71), None);
    assert_eq!(anchor(&page, gone, gone), Some(on(LAST_ROUND, 71, 71)));
}

#[test]
fn highlighting_runs_arrive_with_the_lines_and_split_at_changed_words() {
    let page = page(&first_look(), &[store_all()], &ReviewPageState::default());
    let at = line_at(&page, "src/store.rs", LineKind::Added, None, Some(41));
    let ReviewRowOf::Line(line) = &page.rows[at].of else {
        unreachable!()
    };
    let runs: Vec<(&str, TokenKind, bool)> = line
        .runs
        .iter()
        .map(|run| (run.text.as_str(), run.kind, run.changed))
        .collect();
    assert_eq!(
        runs,
        [
            ("        ", TokenKind::Plain, false),
            ("let", TokenKind::Keyword, false),
            (" file = ", TokenKind::Plain, false),
            ("open_with_flags", TokenKind::Plain, true),
            ("(&path", TokenKind::Plain, false),
            (", flags", TokenKind::Plain, true),
            (")?;", TokenKind::Plain, false),
        ]
    );
    assert_eq!(
        line.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>(),
        line.text,
        "the runs cover the text"
    );
}

#[test]
fn runs_that_overrun_or_cut_a_character_are_cut_at_the_next_one() {
    let mut summary = first_look();
    summary.comments.clear();
    let mut diff = store_all();
    diff.hunks.truncate(1);
    let mut line = added(41, "é = 1");
    // "é" is two bytes; a run of one ends inside it.
    line.runs = vec![
        run(ReviewTokenKind::Keyword, 1),
        run(ReviewTokenKind::Number, 400),
    ];
    diff.hunks[0].lines = vec![line];
    let page = page(&summary, &[diff], &ReviewPageState::default());
    let at = line_at(&page, "src/store.rs", LineKind::Added, None, Some(41));
    let ReviewRowOf::Line(line) = &page.rows[at].of else {
        unreachable!()
    };
    let runs: Vec<(&str, TokenKind)> = line
        .runs
        .iter()
        .map(|run| (run.text.as_str(), run.kind))
        .collect();
    assert_eq!(
        runs,
        [("é", TokenKind::Keyword), (" = 1", TokenKind::Number)]
    );
}

#[test]
fn changed_words_mark_an_edit_and_leave_a_line_with_no_partner() {
    let lines = [
        (LineKind::Removed, "let file = open(&path)?;"),
        (
            LineKind::Added,
            "let file = open_with_flags(&path, flags)?;",
        ),
        (LineKind::Added, "log::debug!(\"opened\");"),
    ];
    let spans = changed_words(&lines);
    let marked = |at: usize| -> Vec<&str> {
        spans[at]
            .iter()
            .map(|span| &lines[at].1[span.clone()])
            .collect()
    };
    assert_eq!(marked(0), ["open"]);
    assert_eq!(marked(1), ["open_with_flags", ", flags"]);
    assert!(
        spans[2].is_empty(),
        "a line with no partner carries no marks"
    );
}

#[test]
fn the_chip_comes_from_the_agents_row() {
    let mut agent = wire::Agent::default();
    assert_eq!(review_chip(&agent), None, "no review");
    REVIEW_ROW.put(
        &mut agent.experimental,
        &ReviewRowEntry {
            drafts: 0,
            counter: 4,
            moved: false,
            review_id: b"review".to_vec(),
        },
    );
    assert_eq!(review_chip(&agent), None, "a review with nothing to send");
    REVIEW_ROW.put(
        &mut agent.experimental,
        &ReviewRowEntry {
            drafts: 3,
            counter: 5,
            moved: true,
            review_id: b"review".to_vec(),
        },
    );
    assert_eq!(
        review_chip(&agent),
        Some(ReviewChip {
            comments: 3,
            moved: true,
        })
    );
}

#[test]
fn a_text_attachment_with_the_entry_draws_as_a_sent_review() {
    let text = wire::attachment::Of::Text(wire::InlineText {
        name: "review".into(),
        text: "src/store.rs:41\nWhy the flags?".into(),
    });
    let mut attachment = wire::Attachment {
        of: Some(text),
        ..Default::default()
    };
    assert!(matches!(
        AttachmentView::of(&attachment),
        AttachmentView::Text { .. }
    ));
    SENT_REVIEW.put(
        &mut attachment.experimental,
        &SentReview {
            review_id: b"review".to_vec(),
            round: 2,
            comments: vec![
                ReviewSentComment {
                    id: b"d1".to_vec(),
                    revision: 1,
                },
                ReviewSentComment {
                    id: b"d3".to_vec(),
                    revision: 2,
                },
            ],
        },
    );
    assert_eq!(
        segments(PLACEHOLDER.to_string().as_str(), &[attachment]),
        [Segment::Attachment(AttachmentView::SentReview {
            comments: 2,
            review: SentReviewRef {
                review_id: b"review".to_vec(),
                round: 2,
                comments: vec![
                    SentCommentRef {
                        id: b"d1".to_vec(),
                        revision: 1,
                    },
                    SentCommentRef {
                        id: b"d3".to_vec(),
                        revision: 2,
                    },
                ],
            },
            text: "src/store.rs:41\nWhy the flags?".into(),
        })]
    );
}

#[test]
fn the_page_and_its_state_cross_as_json() {
    let mut state = ReviewPageState {
        view: Some(ReviewView::SinceLast),
        notices: vec![ReviewNotice::Moved],
        ..ReviewPageState::default()
    };
    state.toggle_file("Cargo.lock");
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(
        serde_json::from_str::<ReviewPageState>(&json).unwrap(),
        state
    );
    let page = page(&second_look(), &[store_since_last()], &state);
    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["header"]["view"], "SinceLast");
    assert!(value["rows"][0]["of"]["File"]["path"].is_string());
    #[cfg(feature = "schema")]
    {
        let schema = schemars::schema_for!(ReviewPage);
        assert!(
            serde_json::to_string(&schema)
                .unwrap()
                .contains("ReviewGap")
        );
    }
}

#[test]
fn toggling_flips_and_flips_back() {
    let mut state = ReviewPageState::default();
    state.toggle_file("a");
    state.toggle_comment(b"c");
    assert_eq!(state.toggled_files, ["a"]);
    state.toggle_file("a");
    state.toggle_comment(b"c");
    assert_eq!(state, ReviewPageState::default());
}
