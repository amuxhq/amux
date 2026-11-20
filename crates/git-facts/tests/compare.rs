//! Comparisons between commits a review names, read from real repositories
//! with snapshot commits kept in a store outside them.

use std::path::Path;
use std::process::Command;

use git_facts::compare::{
    self, CompareError, FileHunks, LineKind, LineMap, LineRange, Mapped, NumberedLine, Shown,
};
use git_facts::objects::ObjectStore;
use git_facts::{Base, Status, UsedBase, changes};

fn git(cwd: &Path, args: &[&str]) -> String {
    git_with(cwd, &[], args)
}

fn git_with(cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// `count` numbered lines, `line 1` first.
fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

/// `text` with line `number` (from 1) replaced by `with`.
fn replaced(text: &str, number: usize, with: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(at, line)| match at + 1 == number {
            true => format!("{with}\n"),
            false => format!("{line}\n"),
        })
        .collect()
}

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A repository on `main` whose first commit holds `files`.
fn repository(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    for (path, text) in files {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    commit_all(dir.path(), "start");
    dir
}

/// The working tree committed into the store at `store`, HEAD its parent,
/// writing nothing to the repository: what a review's snapshot is.
fn snapshot(repo: &Path, store: &Path) -> String {
    std::fs::create_dir_all(store).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("index");
    let objects = repo.join(".git/objects");
    let env = [
        ("GIT_INDEX_FILE", index.as_path()),
        ("GIT_OBJECT_DIRECTORY", store),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects.as_path()),
    ];
    git_with(repo, &env, &["read-tree", "HEAD"]);
    git_with(repo, &env, &["add", "-A"]);
    let tree = git_with(repo, &env, &["write-tree"]);
    git_with(
        repo,
        &env,
        &["commit-tree", &tree, "-p", "HEAD", "-m", "snapshot"],
    )
}

fn range(start: u32, end: u32) -> LineRange {
    LineRange::new(start, end)
}

/// The store a test's snapshots go in, beside the repository.
fn store() -> (tempfile::TempDir, ObjectStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ObjectStore::at(dir.path().join("review/objects"));
    (dir, store)
}

#[tokio::test]
async fn a_renamed_and_edited_file_is_followed_from_its_old_path_in_one_diff() {
    let text = numbered(20);
    let repo = repository(&[("old.rs", &text)]);
    let dir = repo.path();
    let (_held, store) = store();
    let from = git(dir, &["rev-parse", "HEAD"]);
    git(dir, &["mv", "old.rs", "new.rs"]);
    std::fs::write(dir.join("new.rs"), replaced(&text, 10, "line ten")).unwrap();
    let to = snapshot(dir, store.dir());

    let map = LineMap::between(dir, &store, &from, &to, "old.rs")
        .await
        .unwrap();
    assert_eq!(map.path(), Some("new.rs"));
    assert_eq!(map.map(range(1, 9)), Mapped::Untouched(range(1, 9)));
    assert_eq!(map.map(range(9, 10)), Mapped::Touched);
    assert_eq!(map.map(range(11, 20)), Mapped::Untouched(range(11, 20)));
    assert_eq!(map.changed(), vec![range(10, 10)]);

    let files = compare::files(dir, &store, &from, &to).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "new.rs");
    assert_eq!(files[0].old_path.as_deref(), Some("old.rs"));
    assert_eq!(files[0].status, Status::Renamed);
    assert_eq!((files[0].added, files[0].removed), (1, 1));

    let hunks = compare::hunks(dir, &store, &from, &to, &["new.rs"], 3)
        .await
        .unwrap();
    assert_eq!(hunks.len(), 1);
    let file = &hunks[0];
    assert_eq!(file.old_path.as_deref(), Some("old.rs"));
    assert!(file.old_blob.is_some() && file.new_blob.is_some());
    assert_eq!(file.hunks.len(), 1);
    let lines: Vec<_> = file.hunks[0]
        .lines
        .iter()
        .map(|line| (line.kind, line.old_line, line.new_line, line.text.as_str()))
        .collect();
    assert_eq!(
        lines,
        [
            (LineKind::Context, 7, 7, "line 7"),
            (LineKind::Context, 8, 8, "line 8"),
            (LineKind::Context, 9, 9, "line 9"),
            (LineKind::Removed, 10, 0, "line 10"),
            (LineKind::Added, 0, 10, "line ten"),
            (LineKind::Context, 11, 11, "line 11"),
            (LineKind::Context, 12, 12, "line 12"),
            (LineKind::Context, 13, 13, "line 13"),
        ]
    );
    // The blob ids read the whole file on each side.
    let blobs = compare::blobs(
        dir,
        &store,
        &[
            file.old_blob.as_deref().unwrap(),
            file.new_blob.as_deref().unwrap(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(blobs[0].as_deref(), Some(text.as_bytes()));
    assert_eq!(
        blobs[1].as_deref(),
        Some(replaced(&text, 10, "line ten").as_bytes())
    );
}

#[tokio::test]
async fn lines_above_an_edit_keep_their_numbers_and_lines_below_shift() {
    let text = numbered(30);
    let repo = repository(&[("a.txt", &text), ("gone.txt", "bye\n"), ("same.txt", "x\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let from = git(dir, &["rev-parse", "HEAD"]);
    // Two lines put in after line 5, line 12 edited, lines 20 and 21
    // deleted; a file deleted; one left alone.
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    lines[11] = "line twelve".into();
    lines.drain(19..21);
    lines.splice(5..5, ["new a".to_owned(), "new b".to_owned()]);
    let edited: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(dir.join("a.txt"), edited).unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    let to = snapshot(dir, store.dir());

    let maps = LineMap::for_paths(
        dir,
        &store,
        &from,
        &to,
        &["a.txt", "gone.txt", "same.txt", "never.txt"],
    )
    .await
    .unwrap();
    let a = &maps["a.txt"];
    assert_eq!(a.map(range(1, 5)), Mapped::Untouched(range(1, 5)));
    assert_eq!(a.map(range(5, 6)), Mapped::Touched, "lines put in between");
    assert_eq!(a.map(range(6, 11)), Mapped::Untouched(range(8, 13)));
    assert_eq!(a.map(range(12, 12)), Mapped::Touched, "edited");
    assert_eq!(a.map(range(13, 19)), Mapped::Untouched(range(15, 21)));
    assert_eq!(a.map(range(19, 20)), Mapped::Touched, "deleted");
    assert_eq!(a.map(range(22, 30)), Mapped::Untouched(range(22, 30)));
    assert_eq!(
        a.map(LineRange::file()),
        Mapped::Untouched(LineRange::file())
    );
    assert!(
        a.touches(range(15, 17).widened(3)),
        "within three of line 12"
    );
    assert!(
        !a.touches(range(16, 16).widened(3)),
        "four from 12 and from 20"
    );
    assert_eq!(maps["gone.txt"].map(range(1, 1)), Mapped::Gone);
    assert_eq!(maps["gone.txt"].map(LineRange::file()), Mapped::Gone);
    assert_eq!(maps["gone.txt"].path(), None);
    assert_eq!(
        maps["same.txt"].map(range(1, 1)),
        Mapped::Untouched(range(1, 1))
    );
    assert_eq!(maps["never.txt"].map(range(1, 1)), Mapped::Gone);
}

#[tokio::test]
async fn reads_come_from_the_commit_named_not_the_working_tree() {
    let text = numbered(10);
    let repo = repository(&[("a.txt", &text)]);
    let dir = repo.path();
    let (_held, store) = store();
    std::fs::write(dir.join("a.txt"), replaced(&text, 4, "four")).unwrap();
    std::fs::write(dir.join("untracked.txt"), "only in the snapshot\n").unwrap();
    let taken = snapshot(dir, store.dir());
    // The tree moves on after the snapshot.
    std::fs::write(dir.join("a.txt"), "rewritten\n").unwrap();
    std::fs::remove_file(dir.join("untracked.txt")).unwrap();

    let read = compare::lines(dir, &store, &taken, "a.txt", &[range(3, 5), range(9, 40)])
        .await
        .unwrap()
        .unwrap();
    let expected: Vec<NumberedLine> = [(3, "line 3"), (4, "four"), (5, "line 5")]
        .into_iter()
        .chain([(9, "line 9"), (10, "line 10")])
        .map(|(number, text)| NumberedLine {
            number,
            text: text.to_owned(),
        })
        .collect();
    assert_eq!(read, expected);
    assert_eq!(
        compare::read(dir, &store, &taken, "untracked.txt")
            .await
            .unwrap()
            .as_deref(),
        Some(b"only in the snapshot\n".as_slice())
    );
    assert_eq!(
        compare::read(dir, &store, &taken, "nowhere.txt")
            .await
            .unwrap(),
        None
    );
    // Nothing of the snapshot is in the repository itself.
    let elsewhere = ObjectStore::at(dir.join("no-such-store"));
    assert!(matches!(
        compare::read(dir, &elsewhere, &taken, "a.txt").await,
        Err(CompareError::Missing(commit)) if commit == taken
    ));
}

#[tokio::test]
async fn flags_between_commits_are_read_from_the_commits() {
    let repo = repository(&[("plain.txt", "x\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let from = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("made.rs"), "// @generated by a tool\nfn f() {}\n").unwrap();
    std::fs::write(dir.join("Cargo.lock"), "# lock\n").unwrap();
    std::fs::write(dir.join("big.txt"), vec![b'a'; 2 * 1024 * 1024]).unwrap();
    let to = snapshot(dir, store.dir());
    // Gone from the working tree since: the flags come from the snapshot.
    for path in ["made.rs", "Cargo.lock", "big.txt"] {
        std::fs::remove_file(dir.join(path)).unwrap();
    }

    let files = compare::files(dir, &store, &from, &to).await.unwrap();
    let flags = |path: &str| {
        files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} listed"))
            .flags
    };
    assert!(flags("made.rs").generated);
    assert!(flags("Cargo.lock").lockfile);
    assert!(flags("big.txt").large);
    assert!(!flags("made.rs").large && !flags("big.txt").generated);
    assert_eq!(files.len(), 3);
}

/// Between commits, `.gitattributes` is the later commit's, not what the
/// working tree holds now: here the two say the opposite of each file.
#[tokio::test]
async fn attributes_between_commits_are_the_later_commits() {
    let repo = repository(&[("plain.txt", "x\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let from = git(dir, &["rev-parse", "HEAD"]);
    let write = |path: &str, text: &str| {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        ".gitattributes",
        "made.txt linguist-generated\nlib/vendor/** -linguist-vendored\nkept/** linguist-vendored\n",
    );
    write("made.txt", "no header\n");
    write("lib/vendor/x.rs", "fn x() {}\n");
    write("kept/y.rs", "fn y() {}\n");
    let to = snapshot(dir, store.dir());
    write(
        ".gitattributes",
        "made.txt -linguist-generated\nlib/vendor/** linguist-vendored\nkept/** -linguist-vendored\n",
    );

    let files = compare::files(dir, &store, &from, &to).await.unwrap();
    let flags = |path: &str| {
        files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} listed"))
            .flags
    };
    assert!(flags("made.txt").generated, "said generated, no header");
    assert!(
        !flags("lib/vendor/x.rs").vendored,
        "said not vendored, in a vendor folder"
    );
    assert!(flags("kept/y.rs").vendored, "said vendored");
    // The working tree's own list reads its own attributes.
    let listed = changes(dir, &Base::Head).await.unwrap();
    let listed = |path: &str| {
        listed
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .flags
    };
    assert!(!listed("made.txt").generated);
    assert!(listed("lib/vendor/x.rs").vendored);
    assert!(!listed("kept/y.rs").vendored);
}

/// The lines each file's hunks change, by path.
fn changed_lines(files: &[FileHunks]) -> Vec<(String, Vec<String>)> {
    files
        .iter()
        .map(|file| {
            let lines = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind != LineKind::Context)
                .map(|line| line.text.clone())
                .collect();
            (file.path.clone(), lines)
        })
        .collect()
}

/// After the agent merges a base that moved, the last round's snapshot
/// differs from now by everything the base brought as well; what is shown
/// since the last review is only what is still the agent's change.
#[tokio::test]
async fn since_the_last_review_after_a_merge_from_a_moved_base_holds_only_the_agents_hunks() {
    let text = numbered(40);
    let repo = repository(&[("a.txt", &text), ("b.txt", "base\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    git(dir, &["switch", "-q", "-c", "work"]);
    let round_base = git(dir, &["rev-parse", "main"]);
    std::fs::write(dir.join("a.txt"), replaced(&text, 5, "agent five")).unwrap();
    let round = snapshot(dir, store.dir());
    commit_all(dir, "the agent's first change");
    // The base moves: an edit far from the agent's, and another file.
    git(dir, &["switch", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), replaced(&text, 30, "base thirty")).unwrap();
    std::fs::write(dir.join("b.txt"), "base moved\n").unwrap();
    commit_all(dir, "the base moves on");
    git(dir, &["switch", "-q", "work"]);
    git(dir, &["merge", "-q", "--no-edit", "main"]);
    // And the agent changes the line after its first.
    let merged = std::fs::read_to_string(dir.join("a.txt")).unwrap();
    std::fs::write(dir.join("a.txt"), replaced(&merged, 6, "agent six")).unwrap();
    let now = snapshot(dir, store.dir());

    let base = compare::base_for_review(dir, None, None, None)
        .await
        .unwrap();
    let base = base.commit.unwrap();
    assert_ne!(base, round_base, "the merge moved the base");
    let last = compare::hunks(dir, &store, &round, &now, &["a.txt", "b.txt"], 3)
        .await
        .unwrap();
    assert_eq!(last.len(), 2, "both files moved since the round");
    assert_eq!(
        last[0].hunks.len(),
        2,
        "the agent's line and the base's in a.txt"
    );

    let limited = compare::since_last(
        dir,
        &store,
        Shown {
            base: &round_base,
            snapshot: &round,
        },
        Shown {
            base: &base,
            snapshot: &now,
        },
        None,
        3,
    )
    .await
    .unwrap();
    assert_eq!(
        changed_lines(&limited),
        [(
            "a.txt".to_owned(),
            vec!["line 6".to_owned(), "agent six".to_owned()]
        )]
    );
    assert_eq!(limited[0].counts(), (1, 1));
}

/// The agent undoing its own change since the last round is part of what
/// changed since then, though all changes no longer lists the file: a line
/// it changed then put back, a file it made then deleted. What arrived from
/// the base in between still goes.
#[tokio::test]
async fn since_the_last_review_shows_the_agent_undoing_its_own_change() {
    let text = numbered(40);
    let repo = repository(&[("a.txt", &text), ("b.txt", "base\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    git(dir, &["switch", "-q", "-c", "work"]);
    let round_base = git(dir, &["rev-parse", "main"]);
    std::fs::write(dir.join("a.txt"), replaced(&text, 5, "agent five")).unwrap();
    std::fs::write(dir.join("made.txt"), "made\nby the agent\n").unwrap();
    let round = snapshot(dir, store.dir());
    // The base moves, and the agent merges it.
    git(dir, &["stash", "-q", "-u"]);
    git(dir, &["switch", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), replaced(&text, 30, "base thirty")).unwrap();
    std::fs::write(dir.join("b.txt"), "base moved\n").unwrap();
    commit_all(dir, "the base moves on");
    git(dir, &["switch", "-q", "work"]);
    git(dir, &["merge", "-q", "--no-edit", "main"]);
    // Then puts line 5 back and deletes the file it made: nothing of its
    // own is left.
    let now = snapshot(dir, store.dir());
    let base = compare::base_for_review(dir, None, None, None)
        .await
        .unwrap()
        .commit
        .unwrap();
    assert!(
        compare::files(dir, &store, &base, &now)
            .await
            .unwrap()
            .is_empty(),
        "all changes lists nothing"
    );

    let last = Shown {
        base: &round_base,
        snapshot: &round,
    };
    let now = Shown {
        base: &base,
        snapshot: &now,
    };
    let every = compare::since_last(dir, &store, last, now, None, 3)
        .await
        .unwrap();
    assert_eq!(
        changed_lines(&every),
        [
            (
                "a.txt".to_owned(),
                vec!["agent five".to_owned(), "line 5".to_owned()]
            ),
            (
                "made.txt".to_owned(),
                vec!["made".to_owned(), "by the agent".to_owned()]
            ),
        ]
    );
    let made = every.iter().find(|file| file.path == "made.txt").unwrap();
    assert_eq!(made.status, Status::Deleted);
    assert_eq!(made.counts(), (0, 2));
    // Asked by path, the same.
    let asked = compare::since_last(dir, &store, last, now, Some(&["made.txt", "b.txt"]), 3)
        .await
        .unwrap();
    assert_eq!(changed_lines(&asked), changed_lines(&every[1..]));
}

/// Thousands of long paths, more bytes of them than a command line holds
/// on any platform, are compared all the same: the whole tree's since-last
/// names no path to git, and where only some files are wanted they are
/// asked for a bounded number of bytes at a time.
#[tokio::test]
async fn a_change_of_thousands_of_long_paths_is_compared_whole() {
    const FILES: usize = 8_000;
    let repo = repository(&[("keep.txt", "one\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let base = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("keep.txt"), "two\n").unwrap();
    let round = snapshot(dir, store.dir());

    // Each path some 250 bytes: 2 MB of them, past what macOS (1 MB) or
    // Linux (2 MB, less the environment) takes in one call's arguments.
    let folder = format!("{}{}", "nested-folder-".repeat(8), "end");
    std::fs::create_dir_all(dir.join(&folder)).unwrap();
    let paths: Vec<String> = (0..FILES)
        .map(|n| format!("{folder}/{}-{n:05}.txt", "a-long-file-name".repeat(8)))
        .collect();
    assert!(paths.iter().map(String::len).sum::<usize>() > 2_000_000);
    for path in &paths {
        std::fs::write(dir.join(path), "made\n").unwrap();
    }
    let now = snapshot(dir, store.dir());

    let last = Shown {
        base: &base,
        snapshot: &round,
    };
    let shown = Shown {
        base: &base,
        snapshot: &now,
    };
    // Since the round, every new file; all changes holds keep.txt besides,
    // so the files since are picked from it by path.
    let since = compare::since_last(dir, &store, last, shown, None, 3)
        .await
        .unwrap();
    assert_eq!(since.len(), FILES);
    assert!(since.iter().all(|file| file.status == Status::Created
        && file.counts() == (1, 0)
        && file.hunks.len() == 1));
    let mut listed: Vec<&str> = since.iter().map(|file| file.path.as_str()).collect();
    listed.sort_unstable();
    let mut expected: Vec<&str> = paths.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(listed, expected);

    // Asked by path, every one of them, in batches.
    let asked: Vec<&str> = paths.iter().map(String::as_str).collect();
    let hunks = compare::hunks(dir, &store, &base, &now, &asked, 3)
        .await
        .unwrap();
    assert_eq!(hunks.len(), FILES);
    assert!(!hunks.iter().any(|file| file.path == "keep.txt"));
}

/// The review's base follows the rule the changes list uses, except where
/// that rule falls back to HEAD: there the review keeps the commit it began
/// on, so the agent's commits on the base branch stay in all changes.
#[tokio::test]
async fn on_the_base_branch_with_no_upstream_the_review_runs_from_where_it_began() {
    let repo = repository(&[("a.txt", "one\n")]);
    let dir = repo.path();
    let began = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    let committed = commit_all(dir, "the agent commits on main");

    let review = compare::base_for_review(dir, None, None, Some(&began))
        .await
        .unwrap();
    assert_eq!(
        review,
        UsedBase {
            branch: Some("main".into()),
            upstream: None,
            commit: Some(began.clone()),
        }
    );
    let listed = changes(
        dir,
        &Base::Default {
            recorded_branch: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        listed.base.commit.as_deref(),
        Some(committed.as_str()),
        "the changes list has no review to ask, so runs from HEAD"
    );

    // With an upstream, both run from where the branch left it.
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        dir,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(dir, &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(dir.join("a.txt"), "three\n").unwrap();
    let unpushed = commit_all(dir, "not pushed");
    let review = compare::base_for_review(dir, None, None, Some(&began))
        .await
        .unwrap();
    assert_eq!(review.upstream.as_deref(), Some("origin/main"));
    assert_eq!(review.commit.as_deref(), Some(committed.as_str()));
    assert_ne!(review.commit.as_deref(), Some(unpushed.as_str()));

    // On a branch of its own, from where it left the base; a branch
    // picked wins over the one recorded.
    git(dir, &["switch", "-q", "-c", "work"]);
    std::fs::write(dir.join("a.txt"), "four\n").unwrap();
    commit_all(dir, "on the branch");
    let review = compare::base_for_review(dir, Some("main"), None, Some(&began))
        .await
        .unwrap();
    assert_eq!(review.branch.as_deref(), Some("main"));
    assert_eq!(review.commit.as_deref(), Some(unpushed.as_str()));
    let picked = compare::base_for_review(dir, Some("main"), Some("work"), Some(&began))
        .await
        .unwrap();
    assert_eq!(picked.branch.as_deref(), Some("work"));
    assert_eq!(picked.commit, Some(began), "work itself, with no upstream");
}

#[tokio::test]
async fn a_review_begun_before_the_first_commit_runs_from_the_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    commit_all(dir, "first");

    let review = compare::base_for_review(dir, None, None, None)
        .await
        .unwrap();
    assert_eq!(review.branch.as_deref(), Some("main"));
    assert_eq!(review.commit, None);
    let (_held, store) = store();
    let empty = compare::empty_tree(dir).await.unwrap();
    let files = compare::files(dir, &store, &empty, "HEAD").await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, Status::Created);
}

fn missing<T>(result: Result<T, CompareError>, commit: &str) {
    match result {
        Err(CompareError::Missing(named)) => assert_eq!(named, commit),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("{commit} read"),
    }
}

/// A round can outlive what it names: a store removed, or the repository's
/// objects beneath a snapshot collected after a rebase. Every read says so
/// with a typed error.
#[tokio::test]
async fn a_commit_that_is_gone_is_a_typed_error() {
    let repo = repository(&[("a.txt", "one\n")]);
    let dir = repo.path();
    let (held, store) = store();
    let head = git(dir, &["rev-parse", "HEAD"]);
    let unknown = "0123456789abcdef0123456789abcdef01234567";
    missing(compare::files(dir, &store, unknown, &head).await, unknown);
    missing(
        compare::hunks(dir, &store, &head, unknown, &["a.txt"], 3).await,
        unknown,
    );
    missing(
        LineMap::between(dir, &store, unknown, &head, "a.txt").await,
        unknown,
    );
    missing(compare::read(dir, &store, unknown, "a.txt").await, unknown);
    missing(
        compare::lines(dir, &store, unknown, "a.txt", &[range(1, 1)]).await,
        unknown,
    );

    // A blob only the repository held, beneath a snapshot, collected.
    std::fs::write(dir.join("b.txt"), "beneath\n").unwrap();
    let id = git(dir, &["hash-object", "-w", "b.txt"]);
    let taken = snapshot(dir, store.dir());
    assert_eq!(
        compare::read(dir, &store, &taken, "b.txt")
            .await
            .unwrap()
            .as_deref(),
        Some(b"beneath\n".as_slice())
    );
    std::fs::remove_file(dir.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
    missing(compare::read(dir, &store, &taken, "b.txt").await, &taken);
    missing(compare::files(dir, &store, &head, &taken).await, &taken);
    missing(
        compare::hunks(dir, &store, &head, &taken, &["b.txt"], 3).await,
        &taken,
    );

    // The store itself gone.
    drop(held);
    missing(compare::files(dir, &store, &head, &taken).await, &taken);
}

/// A submodule holds no file of this repository: reading it is no file,
/// not a commit gone, though its commit is in another repository.
#[tokio::test]
async fn a_submodule_reads_as_no_file() {
    let repo = repository(&[("a.txt", "one\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let elsewhere = "0123456789abcdef0123456789abcdef01234567";
    git(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{elsewhere},lib"),
        ],
    );
    git(dir, &["commit", "-q", "-m", "a submodule"]);
    let head = git(dir, &["rev-parse", "HEAD"]);

    assert_eq!(
        compare::read(dir, &store, &head, "lib").await.unwrap(),
        None
    );
    assert_eq!(
        compare::lines(dir, &store, &head, "lib", &[range(1, 1)])
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        compare::read(dir, &store, &head, "a.txt")
            .await
            .unwrap()
            .as_deref(),
        Some(b"one\n".as_slice())
    );
}

#[tokio::test]
async fn binary_and_created_files_have_their_sides_named() {
    let repo = repository(&[("a.txt", "one\n")]);
    let dir = repo.path();
    let (_held, store) = store();
    let from = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("image.bin"), [0u8, 1, 2, 0, 255]).unwrap();
    std::fs::write(dir.join("fresh.txt"), "a\nb\n").unwrap();
    std::fs::remove_file(dir.join("a.txt")).unwrap();
    let to = snapshot(dir, store.dir());

    let hunks = compare::hunks(
        dir,
        &store,
        &from,
        &to,
        &["image.bin", "fresh.txt", "a.txt"],
        3,
    )
    .await
    .unwrap();
    let file = |path: &str| hunks.iter().find(|file| file.path == path).unwrap();
    assert!(file("image.bin").binary && file("image.bin").hunks.is_empty());
    assert_eq!(file("fresh.txt").status, Status::Created);
    assert_eq!(file("fresh.txt").old_blob, None);
    assert_eq!(file("fresh.txt").hunks[0].lines.len(), 2);
    assert_eq!(file("a.txt").status, Status::Deleted);
    assert_eq!(file("a.txt").new_blob, None);
    assert_eq!(file("a.txt").hunks[0].lines[0].old_line, 1);
}

/// Every comparison can run on a multi-threaded runtime's handler, as the
/// host's review calls do.
#[test]
fn every_comparison_can_be_sent_between_threads() {
    fn sent<T: Send>(_: T) {}
    let (dir, store) = (Path::new("."), ObjectStore::at("store"));
    sent(compare::base_for_review(dir, None, None, None));
    sent(compare::files(dir, &store, "a", "b"));
    sent(compare::hunks(dir, &store, "a", "b", &["p"], 3));
    sent(compare::read(dir, &store, "a", "p"));
    sent(compare::lines(dir, &store, "a", "p", &[range(1, 1)]));
    sent(compare::blobs(dir, &store, &["a"]));
    sent(LineMap::between(dir, &store, "a", "b", "p"));
    sent(LineMap::for_paths(dir, &store, "a", "b", &["p"]));
    let shown = Shown {
        base: "a",
        snapshot: "b",
    };
    sent(compare::since_last(dir, &store, shown, shown, None, 3));
    sent(changes(dir, &Base::Head));
}
