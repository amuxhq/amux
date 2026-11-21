//! The bodies of a review's files, as the agent's host answers GetFiles in
//! memory: hunks highlighted from each side's whole file, each hunk's place
//! in the code from its changed lines, an outline of what the change
//! touches on either side, more context read from the snapshot, "since your
//! last review" limited to the agent's own change, answers bounded by file
//! count and size, and the highlighting kept by object id. And the snippet
//! a put comment is stored with.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use node::ProfileRuntime;
use node::harness::{Calling, Served, served_as};
use support::synthetic::*;
use support::*;
use wire::Lifecycle;
use wire::experimental::{
    ReviewAnchor, ReviewDefinition, ReviewEdit, ReviewEditRequest, ReviewFileDiff,
    ReviewFileRequest, ReviewGetFilesRequest, ReviewHunk, ReviewLine, ReviewLineKind,
    ReviewLineRange, ReviewOpenRequest, ReviewPutComment, ReviewSide, ReviewSummary,
    ReviewTokenKind, ReviewView, review_edit, reviews1_client,
};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn launch() -> node::Launch {
    let mut launch = quiet_launch();
    launch.experiments = settings::experiments::Enabled::of(&[&settings::experiments::REVIEWS]);
    launch
}

/// An exited agent working in `cwd`, made from `main`.
fn agent_in(install: &Install, cwd: &Path) -> SyntheticAgent {
    let agent = SyntheticAgent::new(install, "coder", SEGMENTS);
    let mut row = agent.row(install);
    row.cwd = cwd.to_string_lossy().into_owned();
    row.lifecycle = Lifecycle::Exited as i32;
    row.exit_cause = wire::ExitCause::Finished as i32;
    install.store().put_agent(&row).unwrap();
    let spec = wire::AgentSpec {
        agent_id: agent.id.as_bytes().to_vec(),
        kind: KIND as i32,
        cwd: row.cwd.clone(),
        name: agent.name.clone(),
        incarnation: 1,
        base_branch: Some("main".to_owned()),
        ..wire::AgentSpec::default()
    };
    std::fs::write(
        agent.dir.join("spec.1"),
        prost::Message::encode_to_vec(&spec),
    )
    .unwrap();
    agent
}

/// The work folder with `files` committed on `main`, then on `feature`.
fn repository(work: &Path, files: &[(&str, &str)]) {
    git(work, &["init", "-q", "-b", "main"]);
    write(work, files);
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(work, &["switch", "-q", "-c", "feature"]);
}

fn write(work: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

struct Host {
    install: Install,
    daemon: Option<node::Daemon>,
    runtime: Arc<ProfileRuntime>,
}

impl Host {
    async fn start(install: Install) -> Self {
        let daemon = install.start("boot-1", launch()).await;
        let runtime = runtime(&daemon, &install);
        Self {
            install,
            daemon: Some(daemon),
            runtime,
        }
    }

    async fn stop(mut self) {
        drop(self.runtime);
        self.daemon.take().unwrap().shutdown().await.unwrap();
    }

    fn client(
        &self,
    ) -> wire::experimental::reviews1_client::Reviews1Client<node::harness::InMemory> {
        reviews1_client(served_as(
            &self.runtime,
            Served::Profile,
            Calling::PersonHere(analytics::Client::Terminal),
        ))
        .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
    }

    async fn open(&self, agent: &SyntheticAgent) -> ReviewSummary {
        let request = ReviewOpenRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            base_branch: None,
            refresh: false,
            read_only: false,
        };
        self.client().open(request).await.unwrap().into_inner()
    }

    /// The files `files` of `view` of the comparison `summary` hands out.
    async fn files(
        &self,
        agent: &SyntheticAgent,
        summary: &ReviewSummary,
        view: ReviewView,
        files: Vec<ReviewFileRequest>,
    ) -> Vec<ReviewFileDiff> {
        let request = ReviewGetFilesRequest {
            files,
            ..request(agent, summary, view)
        };
        self.client()
            .get_files(request)
            .await
            .unwrap()
            .into_inner()
            .files
    }

    async fn all(&self, agent: &SyntheticAgent, summary: &ReviewSummary) -> Vec<ReviewFileDiff> {
        let asked = summary.files.iter().map(|file| asked(&file.path)).collect();
        self.files(agent, summary, ReviewView::All, asked).await
    }

    /// Closes a round on `summary`'s snapshot, as the host does when it
    /// accepts the message sending it.
    fn send(&self, agent: &SyntheticAgent, summary: &ReviewSummary) {
        let mut store = self.install.store();
        store
            .settle_experiments(&[settings::experiments::REVIEWS.version])
            .unwrap();
        let key = agent.key(&self.install);
        let review = store.review(agent.id.as_bytes()).unwrap().unwrap();
        let n = review.rounds.last().map_or(0, |round| round.n) + 1;
        store
            .write_review(&key, |writer| -> Result<(), store::StoreError> {
                writer.add_round(
                    &review.id,
                    &store::reviews::Round {
                        n,
                        snapshot: summary.snapshot.clone(),
                        base_commit: summary.base_commit.clone(),
                        input_id: format!("round {n}").into_bytes(),
                        sent_ms: 1,
                    },
                )
            })
            .unwrap();
    }
}

/// A request for nothing yet of `view` of the comparison `summary` hands
/// out: from its base commit for all changes, from the last round's
/// snapshot for what changed since.
fn request(
    agent: &SyntheticAgent,
    summary: &ReviewSummary,
    view: ReviewView,
) -> ReviewGetFilesRequest {
    let (from, base, from_base) = match view {
        ReviewView::SinceLast => {
            let last = summary.rounds.last().cloned().unwrap_or_default();
            (last.snapshot, summary.base_commit.clone(), last.base_commit)
        }
        _ => (summary.base_commit.clone(), String::new(), String::new()),
    };
    ReviewGetFilesRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        snapshot: summary.snapshot.clone(),
        view: view as i32,
        from,
        base,
        from_base,
        ..ReviewGetFilesRequest::default()
    }
}

fn asked(path: &str) -> ReviewFileRequest {
    ReviewFileRequest {
        path: path.to_owned(),
        ..ReviewFileRequest::default()
    }
}

fn file<'a>(files: &'a [ReviewFileDiff], path: &str) -> &'a ReviewFileDiff {
    files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} answered"))
}

fn labels(scope: &[ReviewDefinition]) -> Vec<&str> {
    scope
        .iter()
        .map(|definition| definition.label.as_str())
        .collect()
}

/// The hunk holding a line whose text is `text`.
fn hunk_with<'a>(diff: &'a ReviewFileDiff, text: &str) -> &'a ReviewHunk {
    diff.hunks
        .iter()
        .find(|hunk| hunk.lines.iter().any(|line| line.text == text))
        .unwrap_or_else(|| panic!("a hunk with {text:?} in {:#?}", diff.hunks))
}

fn line<'a>(diff: &'a ReviewFileDiff, text: &str) -> &'a ReviewLine {
    hunk_with(diff, text)
        .lines
        .iter()
        .find(|line| line.text == text)
        .unwrap()
}

fn kinds(line: &ReviewLine) -> Vec<ReviewTokenKind> {
    line.runs.iter().map(|run| run.kind()).collect()
}

/// Thirty numbered lines, with `edits` replacing some by number.
fn numbered(edits: &[(usize, &str)]) -> String {
    (1..=30)
        .map(|n| match edits.iter().find(|(at, _)| *at == n) {
            Some((_, line)) => format!("{line}\n"),
            None => format!("line {n}\n"),
        })
        .collect()
}

const STORE_BEFORE: &str = "\
#[derive(Debug)]
pub struct Point {
    x: u32,
}








impl Store {
    fn open() {
        let a = 1;
        let b = 2;
        let c = 3;
    }








    fn close() {
        let d = 4;
        let e = 5;
        let f = 6;
    }








    fn gone() {
        let g = 7;
    }
}
";

/// [`STORE_BEFORE`] with a derive widened, a line of `open` edited, a line
/// of `close` removed and `gone` deleted whole.
fn store_after() -> String {
    STORE_BEFORE
        .replace("#[derive(Debug)]", "#[derive(Debug, Clone)]")
        .replace("let b = 2;", "let b = 20;")
        .replace("        let e = 5;\n", "")
        .replace("    fn gone() {\n        let g = 7;\n    }\n", "")
}

#[tokio::test(flavor = "multi_thread")]
async fn hunks_say_where_they_sit_and_the_outline_keeps_what_was_deleted() {
    let install = Install::new();
    repository(&install.work, &[("src/store.rs", STORE_BEFORE)]);
    write(&install.work, &[("src/store.rs", &store_after())]);
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    let files = host.all(&agent, &summary).await;
    let diff = file(&files, "src/store.rs");
    println!("{diff:#?}");
    assert_eq!(diff.hunks.len(), 4, "the four changes stay apart");
    for hunk in &diff.hunks {
        assert_eq!(hunk.old_commit, summary.base_commit);
    }

    // A change to an attribute sits in what it describes.
    assert_eq!(
        labels(&hunk_with(diff, "#[derive(Debug, Clone)]").scope),
        ["struct Point"]
    );
    // An edited line: from the added line, on the snapshot.
    let open = hunk_with(diff, "        let b = 20;");
    assert_eq!(labels(&open.scope), ["impl Store", "fn open"]);
    assert_eq!(open.scope[1].parent, Some(0));
    assert!(
        open.scope
            .iter()
            .all(|definition| definition.side() == ReviewSide::Snapshot)
    );
    // A hunk that only removes: from the removed line, in the base.
    let close = hunk_with(diff, "        let e = 5;");
    assert!(
        close
            .lines
            .iter()
            .all(|line| line.kind() != ReviewLineKind::Added)
    );
    assert_eq!(labels(&close.scope), ["impl Store", "fn close"]);
    // Its lines are the base's, and the scope says so.
    assert!(
        close
            .scope
            .iter()
            .all(|definition| definition.side() == ReviewSide::Compared)
    );
    // Removed whole: its chain names it, from the base.
    let gone = hunk_with(diff, "    fn gone() {");
    assert_eq!(labels(&gone.scope), ["impl Store", "fn gone"]);
    assert_eq!(gone.scope[1].side(), ReviewSide::Compared);

    // The outline holds what the change touches on both sides, a function
    // that is gone among them, each under its parent.
    let outline: Vec<(&str, Option<&str>)> = diff
        .outline
        .iter()
        .map(|definition| {
            let parent = definition
                .parent
                .map(|parent| diff.outline[parent as usize].label.as_str());
            (definition.label.as_str(), parent)
        })
        .collect();
    for wanted in [
        ("struct Point", None),
        ("impl Store", None),
        ("fn open", Some("impl Store")),
        ("fn close", Some("impl Store")),
        ("fn gone", Some("impl Store")),
    ] {
        assert!(outline.contains(&wanted), "{wanted:?} in {outline:?}");
    }
    assert_eq!(
        outline
            .iter()
            .filter(|(label, _)| *label == "impl Store")
            .count(),
        1,
        "one impl Store, though both sides touch it"
    );
    // A definition only the base holds is numbered in the base, and says
    // so; one the snapshot holds, in the snapshot.
    let side = |label: &str| {
        diff.outline
            .iter()
            .find(|definition| definition.label == label)
            .map(ReviewDefinition::side)
    };
    assert_eq!(side("fn gone"), Some(ReviewSide::Compared));
    assert_eq!(side("fn open"), Some(ReviewSide::Snapshot));

    // Each side's lines are coloured from that side's file.
    let added = line(diff, "        let b = 20;");
    assert!(kinds(added).contains(&ReviewTokenKind::Keyword));
    assert!(kinds(added).contains(&ReviewTokenKind::Number));
    let removed = line(diff, "        let e = 5;");
    assert_eq!(removed.kind(), ReviewLineKind::Removed);
    assert!(kinds(removed).contains(&ReviewTokenKind::Keyword));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hunk_starting_inside_a_block_comment_is_coloured_as_comment() {
    let comment: String = (1..=20).map(|n| format!(" * note {n}\n")).collect();
    let before = format!("fn a() {{}}\n/*\n{comment} */\nfn b() {{}}\n");
    let after = before.replace(" * note 15\n", " * note fifteen\n");
    let install = Install::new();
    repository(&install.work, &[("lib.rs", &before)]);
    write(&install.work, &[("lib.rs", &after)]);
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    let files = host.all(&agent, &summary).await;
    let diff = file(&files, "lib.rs");
    assert_eq!(diff.hunks.len(), 1);
    let hunk = &diff.hunks[0];
    assert_eq!(
        hunk.lines[0].text, " * note 12",
        "the hunk starts mid-comment"
    );
    for line in &hunk.lines {
        assert_eq!(kinds(line), [ReviewTokenKind::Comment], "{line:?}");
    }
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn more_context_reads_the_snapshot_not_the_tree_edited_since() {
    let install = Install::new();
    repository(&install.work, &[("notes.rs", &numbered(&[]))]);
    write(
        &install.work,
        &[("notes.rs", &numbered(&[(3, "line three")]))],
    );
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    // The agent goes on editing after the snapshot was taken.
    write(
        &host.install.work,
        &[(
            "notes.rs",
            &numbered(&[(3, "line three"), (20, "edited since")]),
        )],
    );
    let ask = ReviewFileRequest {
        path: "notes.rs".to_owned(),
        context: vec![ReviewLineRange { start: 18, end: 21 }],
        context_side: ReviewSide::Snapshot as i32,
    };
    let files = host
        .files(&agent, &summary, ReviewView::All, vec![ask])
        .await;
    let context: Vec<(u32, u32, &str)> = files[0]
        .context
        .iter()
        .map(|line| (line.old_line, line.new_line, line.text.as_str()))
        .collect();
    assert_eq!(
        context,
        [
            (0, 18, "line 18"),
            (0, 19, "line 19"),
            (0, 20, "line 20"),
            (0, 21, "line 21"),
        ]
    );
    assert!(
        files[0]
            .context
            .iter()
            .all(|line| line.kind() == ReviewLineKind::Context)
    );
    assert_eq!(files[0].hunks.len(), 1, "the hunks come along");

    // The base's side, numbered as old lines.
    let ask = ReviewFileRequest {
        path: "notes.rs".to_owned(),
        context: vec![ReviewLineRange { start: 3, end: 3 }],
        context_side: ReviewSide::Compared as i32,
    };
    let files = host
        .files(&agent, &summary, ReviewView::All, vec![ask])
        .await;
    assert_eq!(files[0].context.len(), 1);
    assert_eq!(
        (
            files[0].context[0].old_line,
            files[0].context[0].text.as_str()
        ),
        (3, "line 3")
    );

    // Context names a side of the checked comparison, never a commit: one
    // naming none is refused before anything is read.
    let ask = ReviewFileRequest {
        path: "notes.rs".to_owned(),
        context: vec![ReviewLineRange { start: 3, end: 3 }],
        context_side: ReviewSide::Unspecified as i32,
    };
    let refused = host
        .client()
        .get_files(ReviewGetFilesRequest {
            files: vec![ask],
            ..request(&agent, &summary, ReviewView::All)
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{refused:?}");
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn since_the_last_round_after_a_merge_leaves_out_what_the_base_brought() {
    let install = Install::new();
    repository(&install.work, &[("notes.txt", &numbered(&[]))]);
    let work = install.work.clone();
    write(&work, &[("notes.txt", &numbered(&[(25, "feature")]))]);
    git(&work, &["commit", "-q", "-am", "feature"]);
    let agent = agent_in(&install, &work);
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.send(&agent, &first);

    git(&work, &["switch", "-q", "main"]);
    write(&work, &[("notes.txt", &numbered(&[(3, "from the base")]))]);
    git(&work, &["commit", "-q", "-am", "on main"]);
    git(&work, &["switch", "-q", "feature"]);
    git(&work, &["merge", "-q", "--no-edit", "main"]);
    write(
        &work,
        &[(
            "notes.txt",
            &numbered(&[(3, "from the base"), (25, "feature again")]),
        )],
    );

    let after = host.open(&agent).await;
    assert_ne!(after.snapshot, first.snapshot);
    let files = host
        .files(
            &agent,
            &after,
            ReviewView::SinceLast,
            vec![asked("notes.txt")],
        )
        .await;
    let diff = file(&files, "notes.txt");
    let changed: Vec<&str> = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind() != ReviewLineKind::Context)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(changed, ["feature", "feature again"], "{diff:#?}");
    for hunk in &diff.hunks {
        assert_eq!(hunk.old_commit, first.snapshot, "compared with the round");
    }
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_read_of_the_same_files_is_served_from_the_cache() {
    let install = Install::new();
    let sources: Vec<(String, String)> = (0..5)
        .map(|n| (format!("src/m{n}.rs"), format!("fn f{n}() {{}}\n")))
        .collect();
    let files: Vec<(&str, &str)> = sources
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    repository(&install.work, &files);
    for (path, text) in &sources {
        write(&install.work, &[(path, &format!("{text}fn more() {{}}\n"))]);
    }
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;

    let before = node::harness::files_highlighted();
    let first = host.all(&agent, &summary).await;
    let highlighted = node::harness::files_highlighted();
    assert_eq!(highlighted - before, 10, "both sides of five files");
    let again = host.all(&agent, &summary).await;
    assert_eq!(
        node::harness::files_highlighted(),
        highlighted,
        "nothing highlighted again"
    );
    assert_eq!(first, again);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_holds_what_fits_and_leaves_the_rest_to_ask_for_again() {
    let install = Install::new();
    // Twelve files of about 900 KiB each, all added: more than an answer's
    // 8 MiB together, each under the size a snapshot leaves out.
    let line = format!("{}\n", "x".repeat(99));
    let big = line.repeat(9_000);
    let mut sources: Vec<(String, String)> = (0..12)
        .map(|n| (format!("big/{n:02}.txt"), big.clone()))
        .collect();
    // And seventy small ones: more than an answer's 64 files.
    sources.extend((0..70).map(|n| (format!("small/{n:02}.txt"), format!("{n}\n"))));
    repository(&install.work, &[]);
    for (path, text) in &sources {
        write(&install.work, &[(path, text)]);
    }
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;

    let big_paths: Vec<String> = (0..12).map(|n| format!("big/{n:02}.txt")).collect();
    let asked_big: Vec<ReviewFileRequest> = big_paths.iter().map(|path| asked(path)).collect();
    let answered = host
        .files(&agent, &summary, ReviewView::All, asked_big)
        .await;
    let size: usize = answered.iter().map(prost::Message::encoded_len).sum();
    println!("{} of 12 files, {size} bytes", answered.len());
    assert!(!answered.is_empty() && answered.len() < 12);
    assert!(size <= 8 << 20, "{size} bytes");
    let got: Vec<&str> = answered.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(got, big_paths[..answered.len()], "the first ones, in order");
    assert!(
        answered
            .iter()
            .all(|file| file.hunks[0].lines.len() == 9_000)
    );
    // The rest come when asked for again.
    let rest: Vec<ReviewFileRequest> = big_paths[answered.len()..]
        .iter()
        .map(|path| asked(path))
        .collect();
    let later = host.files(&agent, &summary, ReviewView::All, rest).await;
    assert!(!later.is_empty());
    assert_eq!(later[0].path, big_paths[answered.len()]);

    let small: Vec<ReviewFileRequest> = (0..70)
        .map(|n| asked(&format!("small/{n:02}.txt")))
        .collect();
    let answered = host.files(&agent, &summary, ReviewView::All, small).await;
    assert_eq!(answered.len(), 64);
    assert_eq!(answered[63].path, "small/63.txt");
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_and_large_files_come_back_with_no_lines() {
    let install = Install::new();
    repository(&install.work, &[("kept.rs", "fn a() {}\n")]);
    let work = install.work.clone();
    let large = "y".repeat(99) + "\n";
    // Committed on the branch, so the snapshot holds it.
    write(&work, &[("committed.txt", &large.repeat(21_000))]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "a large file"]);
    // Only in the working tree, so the snapshot leaves it out.
    write(&work, &[("loose.txt", &large.repeat(21_000))]);
    std::fs::write(work.join("image.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    write(&work, &[("kept.rs", "fn a() {}\nfn b() {}\n")]);
    let agent = agent_in(&install, &work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    let files = host
        .files(
            &agent,
            &summary,
            ReviewView::All,
            ["committed.txt", "loose.txt", "image.bin", "kept.rs"]
                .into_iter()
                .map(asked)
                .collect(),
        )
        .await;
    for path in ["committed.txt", "loose.txt", "image.bin"] {
        let diff = file(&files, path);
        assert!(diff.hunks.is_empty(), "{path}: {} hunks", diff.hunks.len());
        assert!(diff.outline.is_empty());
    }
    assert_eq!(file(&files, "kept.rs").hunks.len(), 1);
    host.stop().await;
}

/// A file the snapshot left out for its size is listed as changed when the
/// base held it, which the base's tree says without the file being read:
/// here its object is gone from the repository altogether, and it is still
/// listed as changed rather than made.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_file_is_listed_from_the_bases_tree_alone() {
    let install = Install::new();
    let large = "y".repeat(99) + "\n";
    repository(&install.work, &[("asset.dat", &large.repeat(21_000))]);
    let work = install.work.clone();
    let id = git(&work, &["rev-parse", "HEAD:asset.dat"]);
    std::fs::remove_file(work.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
    write(&work, &[("asset.dat", &large.repeat(22_000))]);
    write(&work, &[("loose.dat", &large.repeat(21_000))]);
    let agent = agent_in(&install, &work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    let listed: Vec<(&str, wire::FileStatus, bool)> = summary
        .files
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.status(),
                file.flags.contains(&(wire::FileFlag::Large as i32)),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("asset.dat", wire::FileStatus::Changed, true),
            ("loose.dat", wire::FileStatus::Created, true),
        ]
    );
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_put_comment_is_stored_with_three_lines_above_and_its_scope() {
    let install = Install::new();
    repository(&install.work, &[("src/store.rs", STORE_BEFORE)]);
    write(&install.work, &[("src/store.rs", &store_after())]);
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    // `let b = 20;` is line 16 of the snapshot.
    let request = ReviewEditRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        edits: vec![ReviewEdit {
            of: Some(review_edit::Of::Put(ReviewPutComment {
                id: b"c1".to_vec(),
                anchor: Some(ReviewAnchor {
                    commit: summary.snapshot.clone(),
                    path: "src/store.rs".to_owned(),
                    lines: Some(ReviewLineRange { start: 16, end: 16 }),
                }),
                text: "why twenty".to_owned(),
            })),
        }],
    };
    let after = host.client().edit(request).await.unwrap().into_inner();
    let comment = &after.comments[0];
    let snippet: Vec<(ReviewLineKind, u32, u32, &str)> = comment
        .snippet
        .iter()
        .map(|line| {
            (
                line.kind(),
                line.old_line,
                line.new_line,
                line.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        snippet,
        [
            (ReviewLineKind::Context, 13, 13, "impl Store {"),
            (ReviewLineKind::Context, 14, 14, "    fn open() {"),
            (ReviewLineKind::Context, 15, 15, "        let a = 1;"),
            (ReviewLineKind::Removed, 16, 0, "        let b = 2;"),
            (ReviewLineKind::Added, 0, 16, "        let b = 20;"),
        ]
    );
    assert!(
        comment.snippet.iter().all(|line| !line.runs.is_empty()),
        "highlighted"
    );
    assert_eq!(labels(&comment.scope), ["impl Store", "fn open"]);
    host.stop().await;
}

/// What the host's highlighting costs: a first and a cached GetFiles of a
/// fifty-file change, and the daemon's resident size before and after one
/// that touches every grammar. A measurement, printed: run it alone, in a
/// release build, with `--run-ignored only`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run alone in a release build"]
async fn what_highlighting_costs_the_host() {
    let install = Install::new();
    // Fifty Rust files of 400 lines, each edited in three places.
    let body: String = (0..100)
        .map(|n| format!("impl S{n} {{\n    fn f(&self) -> u32 {{\n        {n} + 1\n    }}\n}}\n"))
        .collect::<String>();
    let mut files: Vec<(String, String)> = (0..50)
        .map(|n| (format!("src/f{n:02}.rs"), body.clone()))
        .collect();
    let samples = [
        ("every/a.swift", "struct A {\n    let x = 1\n}\n"),
        ("every/a.ts", "export const x: number = 1;\n"),
        ("every/a.tsx", "export const A = () => <div />;\n"),
        ("every/a.js", "const x = 1;\n"),
        ("every/a.py", "def f():\n    return 1\n"),
        ("every/a.go", "package a\n\nfunc F() int { return 1 }\n"),
        ("every/a.sh", "echo \"hi\"\n"),
        ("every/a.json", "{\"a\": 1}\n"),
        ("every/a.yaml", "a: 1\n"),
        ("every/a.toml", "a = 1\n"),
        ("every/a.c", "int f(void) { return 1; }\n"),
        ("every/a.cpp", "int f() { return 1; }\n"),
        ("every/A.java", "class A { int f() { return 1; } }\n"),
        ("every/a.html", "<p>hi</p>\n"),
        ("every/a.css", "a { color: red; }\n"),
        ("every/a.md", "# Title\n"),
        ("every/a.diff", "+a\n"),
    ];
    files.extend(
        samples
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string())),
    );
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    repository(&install.work, &refs);
    for (path, text) in &files {
        let edited = text
            .replacen("10 + 1", "10 + 2", 1)
            .replacen("50 + 1", "50 + 2", 1)
            .replacen("90 + 1", "90 + 2", 1)
            + "// more\n";
        write(&install.work, &[(path, &edited)]);
    }
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    let rust: Vec<ReviewFileRequest> = (0..50).map(|n| asked(&format!("src/f{n:02}.rs"))).collect();

    let resident_before = resident_kib();
    let started = Instant::now();
    let first = host
        .files(&agent, &summary, ReviewView::All, rust.clone())
        .await;
    let first_took = started.elapsed();
    let started = Instant::now();
    let cached = host.files(&agent, &summary, ReviewView::All, rust).await;
    let cached_took = started.elapsed();
    assert_eq!(first.len(), 50);
    assert_eq!(first, cached);
    let resident_rust = resident_kib();

    let every: Vec<ReviewFileRequest> = samples.iter().map(|(path, _)| asked(path)).collect();
    let started = Instant::now();
    let touched = host.files(&agent, &summary, ReviewView::All, every).await;
    let every_took = started.elapsed();
    assert_eq!(touched.len(), samples.len());
    let resident_after = resident_kib();
    println!(
        "fifty files: first {first_took:?}, cached {cached_took:?}; every grammar: {every_took:?}; \
         resident {resident_before} KiB before, {resident_rust} KiB after the fifty, \
         {resident_after} KiB after every grammar"
    );
    host.stop().await;
}

/// This process's resident size, as `ps` reports it.
fn resident_kib() -> u64 {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("a size in KiB")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_no_longer_held_is_not_found_and_one_unreadable_is_data_loss() {
    let install = Install::new();
    repository(&install.work, &[("notes.txt", &numbered(&[]))]);
    write(&install.work, &[("notes.txt", &numbered(&[(3, "three")]))]);
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    host.send(&agent, &summary);
    let summary = host.open(&agent).await;
    let request = |snapshot: &str, view: ReviewView| ReviewGetFilesRequest {
        snapshot: snapshot.to_owned(),
        files: vec![asked("notes.txt")],
        ..request(&agent, &summary, view)
    };

    // A snapshot the review does not hold.
    let unknown = host
        .client()
        .get_files(request(&"0".repeat(40), ReviewView::All))
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), tonic::Code::NotFound, "{unknown:?}");

    // One it names whose objects are gone, as after the store was lost.
    std::fs::remove_dir_all(&summary.objects).unwrap();
    for view in [ReviewView::All, ReviewView::SinceLast] {
        let gone = host
            .client()
            .get_files(request(&summary.snapshot, view))
            .await
            .unwrap_err();
        assert_eq!(gone.code(), tonic::Code::DataLoss, "{view:?}: {gone:?}");
    }
    host.stop().await;
}

/// One file's more context is bounded however the client splits its ask:
/// ranges are merged, and together read at most 2,000 lines.
#[tokio::test(flavor = "multi_thread")]
async fn more_context_reads_at_most_two_thousand_lines_a_file() {
    let install = Install::new();
    let long: String = (1..=5_000).map(|n| format!("line {n}\n")).collect();
    repository(&install.work, &[("long.txt", &long)]);
    write(
        &install.work,
        &[("long.txt", &long.replace("line 3\n", "line three\n"))],
    );
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let summary = host.open(&agent).await;
    // Overlapping and touching ranges read once each; far more than the
    // bound is asked for in many small pieces.
    let mut context = vec![
        ReviewLineRange { start: 10, end: 20 },
        ReviewLineRange { start: 15, end: 30 },
        ReviewLineRange { start: 31, end: 40 },
    ];
    context.extend((0..100).map(|n| ReviewLineRange {
        start: 100 + n * 40,
        end: 100 + n * 40 + 39,
    }));
    let ask = ReviewFileRequest {
        path: "long.txt".to_owned(),
        context,
        context_side: ReviewSide::Snapshot as i32,
    };
    let files = host
        .files(&agent, &summary, ReviewView::All, vec![ask])
        .await;
    let read: Vec<u32> = files[0].context.iter().map(|line| line.new_line).collect();
    assert_eq!(
        read.len(),
        2_000,
        "the bound, not the 4,031 lines asked for"
    );
    assert_eq!(&read[..3], [10, 11, 12]);
    let mut unique = read.clone();
    unique.dedup();
    assert_eq!(unique, read, "no line read twice");
    host.stop().await;
}

/// GetFiles reads exactly the comparison it names, and only one the review
/// handed out; with `list` it also lists the comparison's files, as the
/// summary lists all changes. A past round's whole diff is its base to its
/// snapshot, readable after later snapshots, and the one before the latest
/// stays readable for a page that still shows it.
#[tokio::test(flavor = "multi_thread")]
async fn get_files_reads_the_comparison_it_names_and_lists_its_files() {
    let install = Install::new();
    repository(
        &install.work,
        &[("notes.txt", &numbered(&[])), ("other.txt", "other\n")],
    );
    write(&install.work, &[("notes.txt", &numbered(&[(3, "three")]))]);
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.send(&agent, &first);

    write(
        &host.install.work,
        &[
            ("notes.txt", &numbered(&[(3, "three"), (20, "twenty")])),
            ("new.txt", "new\n"),
        ],
    );
    let second = host.open(&agent).await;
    assert_ne!(second.snapshot, first.snapshot);

    // The listing is the summary's own list of all changes.
    let listing = host
        .client()
        .get_files(ReviewGetFilesRequest {
            list: true,
            ..request(&agent, &second, ReviewView::All)
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        listing.files.is_empty(),
        "no files asked for, only the list"
    );
    assert_eq!(listing.changed, second.files);

    // Since the last round: new.txt and notes.txt, counted as shown.
    let since = host
        .client()
        .get_files(ReviewGetFilesRequest {
            list: true,
            files: vec![asked("notes.txt")],
            ..request(&agent, &second, ReviewView::SinceLast)
        })
        .await
        .unwrap()
        .into_inner();
    let listed: Vec<(&str, u32, u32)> = since
        .changed
        .iter()
        .map(|file| (file.path.as_str(), file.added, file.removed))
        .collect();
    assert_eq!(listed, [("new.txt", 1, 0), ("notes.txt", 1, 1)]);
    let shown: Vec<&str> = since.files[0]
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind() != ReviewLineKind::Context)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(shown, ["line 20", "twenty"]);
    // The summary's since-last list is the listing.
    assert_eq!(second.since_last, since.changed);

    // The first round's whole diff: its base to its snapshot.
    let round = &second.rounds[0];
    let whole = host
        .client()
        .get_files(ReviewGetFilesRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            from: round.base_commit.clone(),
            snapshot: round.snapshot.clone(),
            view: ReviewView::All as i32,
            list: true,
            files: vec![asked("notes.txt")],
            ..ReviewGetFilesRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let paths: Vec<&str> = whole
        .changed
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["notes.txt"], "the round's own change, not today's");
    let changed: Vec<&str> = whole.files[0]
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind() != ReviewLineKind::Context)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(changed, ["line 3", "three"]);

    // A comparison the review never handed out is refused.
    for (from, view) in [
        (second.snapshot.clone(), ReviewView::All),
        ("1".repeat(40), ReviewView::All),
        (second.base_commit.clone(), ReviewView::SinceLast),
        (String::new(), ReviewView::Unspecified),
    ] {
        let refused = host
            .client()
            .get_files(ReviewGetFilesRequest {
                from: from.clone(),
                view: view as i32,
                files: vec![asked("notes.txt")],
                ..request(&agent, &second, ReviewView::All)
            })
            .await
            .unwrap_err();
        assert_eq!(
            refused.code(),
            tonic::Code::InvalidArgument,
            "{from} {view:?}"
        );
    }
    host.stop().await;
}

/// The agent undoing its own change since the last round is part of what
/// changed since it: a line put back and a file made then deleted are
/// gone from all changes, yet the summary's file states count them from
/// since-last's own comparison, and since-last shows them.
#[tokio::test(flavor = "multi_thread")]
async fn an_undo_since_the_last_round_is_counted_and_shown_since_it() {
    let install = Install::new();
    repository(&install.work, &[("notes.txt", &numbered(&[]))]);
    write(
        &install.work,
        &[
            ("notes.txt", &numbered(&[(3, "three")])),
            ("made.txt", "made\n"),
        ],
    );
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.send(&agent, &first);

    write(&host.install.work, &[("notes.txt", &numbered(&[]))]);
    std::fs::remove_file(host.install.work.join("made.txt")).unwrap();
    let undone = host.open(&agent).await;
    assert_ne!(undone.snapshot, first.snapshot);
    assert!(
        undone.files.is_empty(),
        "all changes lists nothing: {:?}",
        undone.files
    );
    let states: Vec<(&str, wire::FileStatus, u32, u32)> = undone
        .since_last
        .iter()
        .map(|file| (file.path.as_str(), file.status(), file.added, file.removed))
        .collect();
    assert_eq!(
        states,
        [
            ("made.txt", wire::FileStatus::Deleted, 0, 1),
            ("notes.txt", wire::FileStatus::Changed, 1, 1)
        ]
    );

    let since = host
        .client()
        .get_files(ReviewGetFilesRequest {
            list: true,
            files: vec![asked("notes.txt"), asked("made.txt")],
            ..request(&agent, &undone, ReviewView::SinceLast)
        })
        .await
        .unwrap()
        .into_inner();
    let listed: Vec<(&str, u32, u32)> = since
        .changed
        .iter()
        .map(|file| (file.path.as_str(), file.added, file.removed))
        .collect();
    assert_eq!(listed, [("made.txt", 0, 1), ("notes.txt", 1, 1)]);
    let shown: Vec<(&str, Vec<&str>)> = since
        .files
        .iter()
        .map(|file| {
            let changed = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind() != ReviewLineKind::Context)
                .map(|line| line.text.as_str())
                .collect();
            (file.path.as_str(), changed)
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("notes.txt", vec!["three", "line 3"]),
            ("made.txt", vec!["made"])
        ]
    );

    // What every client draws from this summary: the page opens on since
    // your last review, lists and counts both undos from that comparison's
    // own list, asks for both bodies, and with them shows their lines.
    let state = ui_view::ReviewPageState::default();
    let opened = ui_view::review_page(Some(&undone), &[], &[], &state);
    let header = opened.header.as_ref().unwrap();
    assert_eq!(header.view, ui_view::ReviewView::SinceLast);
    assert_eq!((header.files, header.added, header.removed), (2, 1, 2));
    assert_eq!(opened.empty, None);
    assert_eq!(opened.to_fetch, ["made.txt", "notes.txt"]);
    let shown = ui_view::review_page(Some(&undone), &since.files, &[], &state);
    assert!(shown.to_fetch.is_empty(), "{:?}", shown.to_fetch);
    let drawn: Vec<(&str, &str)> = shown
        .rows
        .iter()
        .filter_map(|row| match &row.of {
            ui_view::ReviewRowOf::File(head) => Some((head.path.as_str(), "")),
            ui_view::ReviewRowOf::Line(line) if line.kind != ui_view::LineKind::Context => {
                Some((line.path.as_str(), line.text.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        drawn,
        [
            ("made.txt", ""),
            ("made.txt", "made"),
            ("notes.txt", ""),
            ("notes.txt", "three"),
            ("notes.txt", "line 3"),
        ]
    );
    host.stop().await;
}

/// A comparison the review handed out answers the same however the base is
/// picked after: one person re-picks the base of a snapshot whose tree has
/// not moved, and another's page, still on the comparisons it was given,
/// reads all changes and what changed since the last round exactly as
/// before, while the new base's comparisons answer too.
#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_handed_out_answers_the_same_after_the_base_is_picked_again() {
    let install = Install::new();
    repository(&install.work, &[("notes.txt", &numbered(&[]))]);
    write(&install.work, &[("notes.txt", &numbered(&[(10, "ten")]))]);
    git(&install.work, &["commit", "-q", "-am", "feature"]);
    git(&install.work, &["branch", "dev"]);
    write(
        &install.work,
        &[("notes.txt", &numbered(&[(3, "three"), (10, "ten")]))],
    );
    let agent = agent_in(&install, &install.work);
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.send(&agent, &first);
    write(
        &host.install.work,
        &[(
            "notes.txt",
            &numbered(&[(3, "three"), (10, "ten"), (20, "twenty")]),
        )],
    );
    let second = host.open(&agent).await;
    assert_ne!(second.snapshot, first.snapshot);

    let read = |summary: &ReviewSummary, view: ReviewView| {
        let request = ReviewGetFilesRequest {
            list: true,
            files: vec![asked("notes.txt")],
            ..request(&agent, summary, view)
        };
        let client = host.client();
        async move {
            let mut client = client;
            client
                .get_files(request)
                .await
                .map(|answer| answer.into_inner())
        }
    };
    let all = read(&second, ReviewView::All).await.unwrap();
    let since = read(&second, ReviewView::SinceLast).await.unwrap();

    // Another person picks dev, where the feature's own commit already is:
    // the same snapshot, compared from then on against another base.
    let picked = host
        .client()
        .open(ReviewOpenRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            base_branch: Some("dev".to_owned()),
            refresh: false,
            read_only: false,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(picked.snapshot, second.snapshot);
    assert_ne!(picked.base_commit, second.base_commit);
    assert_eq!(picked.base_branch, "dev");

    assert_eq!(read(&second, ReviewView::All).await.unwrap(), all);
    assert_eq!(read(&second, ReviewView::SinceLast).await.unwrap(), since);
    let all_now = read(&picked, ReviewView::All).await.unwrap();
    assert_ne!(
        all_now, all,
        "the new base's all changes leave out dev's commit"
    );
    read(&picked, ReviewView::SinceLast).await.unwrap();
    host.stop().await;
}
