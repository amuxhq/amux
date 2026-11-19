//! The facts of real repositories made in temporary folders.

use std::path::Path;
use std::process::Command;

use git_facts::{
    Base, Change, ChangeTotals, ChangedFile, Changes, Comparison, DeleteFacts, FileChange, Flags,
    GitError, GitFacts, LARGE_FILE_BYTES, Status, UsedBase, Worktree, WorktreeError, add_worktree,
    branch_goes, branch_own_commits, changes, compare, delete_facts, facts, own_commits,
    repository as repository_of,
};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
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

fn write(cwd: &Path, path: &str, lines: usize) {
    let text: String = (0..lines).map(|n| format!("line {n}\n")).collect();
    std::fs::write(cwd.join(path), text).unwrap();
}

/// A repository on `main` with one commit of a ten-line file.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "base.txt", 10);
    git(dir.path(), &["add", "base.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "start"]);
    dir
}

fn totals(files: u32, added: u32, removed: u32) -> Option<ChangeTotals> {
    Some(ChangeTotals {
        files,
        added,
        removed,
    })
}

#[tokio::test]
async fn a_folder_outside_a_repository_has_no_facts() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(facts(dir.path(), None).await.unwrap(), None);
}

#[tokio::test]
async fn a_clean_default_branch_has_nothing_uncommitted_or_on_the_branch() {
    let repo = repository();
    assert_eq!(
        facts(repo.path(), None).await.unwrap(),
        Some(GitFacts {
            branch: Some("main".into()),
            base_branch: Some("main".into()),
            uncommitted: totals(0, 0, 0),
            on_branch: totals(0, 0, 0),
            remote: None,
        })
    );
}

#[tokio::test]
async fn on_the_base_branch_its_uncommitted_work_is_what_is_on_the_branch() {
    let repo = repository();
    let dir = repo.path();
    write(dir, "base.txt", 6);
    write(dir, "untracked.txt", 2);

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(2, 2, 4));
    assert_eq!(found.on_branch, found.uncommitted);
}

#[tokio::test]
async fn a_branch_counts_its_commits_and_its_working_tree_from_where_it_left_its_base() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "feature"]);
    write(dir, "committed.txt", 4);
    git(dir, &["add", "committed.txt"]);
    git(dir, &["commit", "-q", "-m", "on the branch"]);
    // The base moves on after the branch left it; that does not count.
    git(dir, &["switch", "-q", "main"]);
    write(dir, "later-on-main.txt", 50);
    git(dir, &["add", "later-on-main.txt"]);
    git(dir, &["commit", "-q", "-m", "main moves on"]);
    git(dir, &["switch", "-q", "feature"]);
    // Uncommitted: a changed line, a staged new file and an untracked one.
    write(dir, "base.txt", 9);
    write(dir, "staged.txt", 2);
    git(dir, &["add", "staged.txt"]);
    write(dir, "untracked.txt", 3);
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("feature"));
    assert_eq!(found.base_branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(3, 5, 1));
    assert_eq!(
        found.on_branch,
        totals(4, 9, 1),
        "the commit's file and the three uncommitted ones"
    );
    assert_eq!(
        std::fs::read(dir.join(".git/index")).unwrap(),
        index,
        "the person's index is untouched"
    );
    assert_eq!(
        git(dir, &["status", "--porcelain", "--", "untracked.txt"]),
        "?? untracked.txt",
        "the untracked file is still untracked"
    );
}

#[tokio::test]
async fn a_recorded_base_wins_over_the_default() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "release"]);
    write(dir, "release.txt", 2);
    git(dir, &["add", "release.txt"]);
    git(dir, &["commit", "-q", "-m", "release"]);
    git(dir, &["switch", "-q", "-c", "fix"]);
    write(dir, "fix.txt", 1);

    let found = facts(dir, Some("release")).await.unwrap().unwrap();
    assert_eq!(found.base_branch.as_deref(), Some("release"));
    assert_eq!(found.on_branch, totals(1, 1, 0));
    let default = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(default.base_branch.as_deref(), Some("main"));
    assert_eq!(default.on_branch, totals(2, 3, 0));
}

#[tokio::test]
async fn a_clone_takes_its_default_branch_from_origin() {
    let upstream = repository();
    git(upstream.path(), &["branch", "-q", "-m", "main", "trunk"]);
    let place = tempfile::tempdir().unwrap();
    git(
        place.path(),
        &["clone", "-q", &upstream.path().to_string_lossy(), "clone"],
    );
    let clone = place.path().join("clone");
    git(&clone, &["switch", "-q", "-c", "work"]);
    write(&clone, "work.txt", 7);

    let found = facts(&clone, None).await.unwrap().unwrap();
    assert_eq!(found.base_branch.as_deref(), Some("trunk"));
    assert_eq!(found.on_branch, totals(1, 7, 0));
    let upstream_path = upstream.path().to_string_lossy();
    assert_eq!(
        found.remote.as_deref(),
        Some(upstream_path.trim_end_matches('/'))
    );
    assert_eq!(
        facts(upstream.path(), None).await.unwrap().unwrap().remote,
        None
    );
}

#[tokio::test]
async fn a_detached_head_has_no_branch_but_is_measured_from_its_base() {
    let repo = repository();
    let dir = repo.path();
    write(dir, "base.txt", 12);
    git(dir, &["commit", "-q", "-am", "two more lines"]);
    git(dir, &["switch", "-q", "--detach", "HEAD"]);
    git(dir, &["branch", "-q", "-f", "main", "HEAD~1"]);

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch, None);
    assert_eq!(found.on_branch, totals(1, 2, 0));
}

#[tokio::test]
async fn a_repository_without_commits_counts_everything_as_uncommitted() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "first.txt", 5);

    let found = facts(dir.path(), None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(1, 5, 0));
    assert_eq!(found.on_branch, None);
}

#[tokio::test]
async fn untracked_paths_past_the_command_line_limit_still_count() {
    // More path text than one command line holds: 32 KiB on Windows, about
    // 1 MiB on macOS and usually 2 MiB on Linux. Windows names stay short
    // enough that the temporary folder and a name together fit in its
    // 260-character path limit, which git there keeps by default.
    let (files, path_text, padding) = if cfg!(windows) {
        (800, 64 * 1024, 100)
    } else {
        (13_000, 5 * 1024 * 1024 / 2, 200)
    };
    let repo = repository();
    let dir = repo.path();
    let padding = "x".repeat(padding);
    let mut written = 0;
    for n in 0..files {
        let name = format!("{n:05}-{padding}.txt");
        written += name.len();
        std::fs::write(dir.join(&name), "one line\n").unwrap();
    }
    assert!(written > path_text, "{written} bytes of untracked paths");

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(files, files, 0));
}

fn file(path: &str, added: u32, removed: u32, change: Change, binary: bool) -> FileChange {
    FileChange {
        path: path.to_owned(),
        added,
        removed,
        change,
        binary,
    }
}

#[tokio::test]
async fn a_comparison_lists_each_file_and_builds_a_patch_only_when_asked() {
    let repo = repository();
    let dir = repo.path();
    write(dir, "gone.txt", 2);
    git(dir, &["add", "gone.txt"]);
    git(dir, &["commit", "-q", "-m", "a file to delete"]);
    let fork = git(dir, &["rev-parse", "HEAD"]);
    git(dir, &["switch", "-q", "-c", "feature"]);
    write(dir, "committed.txt", 4);
    git(dir, &["add", "committed.txt"]);
    git(dir, &["commit", "-q", "-m", "on the branch"]);
    let head = git(dir, &["rev-parse", "HEAD"]);
    write(dir, "base.txt", 8);
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    std::fs::write(dir.join("logo.png"), [0u8, 1, 2, 0, 255]).unwrap();
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let uncommitted = compare(dir, &Comparison::Uncommitted, false).await.unwrap();
    assert_eq!(uncommitted.head.as_deref(), Some(head.as_str()));
    assert_eq!(uncommitted.merge_base, None);
    assert_eq!(uncommitted.patch, None, "no patch unless asked");
    assert_eq!(
        uncommitted.files,
        vec![
            file("base.txt", 0, 2, Change::Changed, false),
            file("gone.txt", 0, 2, Change::Deleted, false),
            file("logo.png", 0, 0, Change::Created, true),
        ]
    );

    let on_branch = Comparison::OnBranch {
        base: "main".into(),
    };
    let branch = compare(dir, &on_branch, true).await.unwrap();
    assert_eq!(branch.merge_base.as_deref(), Some(fork.as_str()));
    assert_eq!(
        branch
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["base.txt", "committed.txt", "gone.txt", "logo.png"],
        "the branch's commit and its uncommitted work"
    );
    let patch = String::from_utf8(branch.patch.unwrap()).unwrap();
    assert!(
        patch.contains("+line 3") && patch.contains("-line 9"),
        "{patch}"
    );
    assert!(
        patch
            .lines()
            .any(|line| line.starts_with("index ") && line.len() > 80),
        "index lines carry full object ids"
    );
    assert_eq!(
        std::fs::read(dir.join(".git/index")).unwrap(),
        index,
        "the person's index is untouched"
    );

    let unknown = Comparison::OnBranch {
        base: "nowhere".into(),
    };
    assert!(matches!(
        compare(dir, &unknown, false).await,
        Err(GitError::NoForkPoint(base)) if base == "nowhere"
    ));
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        compare(outside.path(), &Comparison::Uncommitted, false).await,
        Err(GitError::NotARepository)
    ));
}

#[tokio::test]
async fn a_worktree_branches_from_what_the_folder_has_checked_out() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "feature"]);
    write(dir, "feature.txt", 3);
    git(dir, &["add", "feature.txt"]);
    git(dir, &["commit", "-q", "-m", "on feature"]);
    std::fs::create_dir(dir.join("sub")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("repo").join("quiet-otter");

    // Asked from a folder inside the repository.
    let made = add_worktree(&dir.join("sub"), &destination, "quiet-otter")
        .await
        .unwrap();
    assert_eq!(
        made,
        Worktree {
            path: destination.clone(),
            branch: "quiet-otter".into(),
            base_branch: "feature".into(),
        }
    );
    assert!(destination.join("feature.txt").is_file());
    assert_eq!(git(dir, &["branch", "--show-current"]), "feature");
    assert_eq!(
        repository_of(&destination).await.unwrap(),
        repository_of(dir).await.unwrap(),
        "a worktree belongs to the repository it was made from"
    );
    assert_eq!(
        repository_of(dir)
            .await
            .unwrap()
            .unwrap()
            .canonicalize()
            .unwrap(),
        dir.canonicalize().unwrap()
    );

    // Its facts, measured from the recorded base.
    write(&destination, "new.txt", 2);
    let found = facts(&destination, Some(&made.base_branch))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.branch.as_deref(), Some("quiet-otter"));
    assert_eq!(found.base_branch.as_deref(), Some("feature"));
    assert_eq!(found.on_branch, totals(1, 2, 0));
}

#[tokio::test]
async fn a_worktree_is_refused_rather_than_reusing_a_branch_or_a_folder() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["branch", "taken"]);
    let home = tempfile::tempdir().unwrap();

    let outside = tempfile::tempdir().unwrap();
    let refused = add_worktree(outside.path(), &home.path().join("a"), "a").await;
    assert!(
        matches!(refused, Err(WorktreeError::NotARepository(_))),
        "{refused:?}"
    );
    let refused = add_worktree(dir, &home.path().join("taken"), "taken").await;
    assert!(
        matches!(&refused, Err(WorktreeError::BranchExists(name)) if name == "taken"),
        "{refused:?}"
    );
    assert_eq!(
        refused.unwrap_err().to_string(),
        "the repository already has a branch named taken"
    );
    let refused = add_worktree(dir, &home.path().join("bad"), "two words").await;
    assert!(
        matches!(refused, Err(WorktreeError::BadBranchName(_))),
        "{refused:?}"
    );
    std::fs::create_dir(home.path().join("there")).unwrap();
    let refused = add_worktree(dir, &home.path().join("there"), "there").await;
    assert!(
        matches!(refused, Err(WorktreeError::DestinationExists(_))),
        "{refused:?}"
    );
    git(dir, &["switch", "-q", "--detach"]);
    let refused = add_worktree(dir, &home.path().join("loose"), "loose").await;
    assert!(
        matches!(refused, Err(WorktreeError::NoBranch(_))),
        "{refused:?}"
    );
    assert_eq!(
        git(
            dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"]
        ),
        "main\ntaken",
        "no refusal made a branch"
    );
    assert_eq!(
        git(dir, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

/// A branch's own commits are those no other branch, tag or remote holds,
/// its own upstream aside: none for a branch just made, or one merged
/// elsewhere, and none to count on a detached head. Zero is exactly when a
/// delete takes the branch with its workspace.
#[tokio::test]
async fn a_branch_counts_the_commits_no_other_ref_holds() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "fix"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(0), "just made");
    for n in 0..3 {
        write(dir, &format!("fix{n}.txt"), 1);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "fix"]);
    }
    assert_eq!(own_commits(dir).await.unwrap(), Some(3));
    // Uncommitted work is not a commit.
    write(dir, "draft.txt", 2);
    assert_eq!(own_commits(dir).await.unwrap(), Some(3));
    // A stash is not a ref that keeps them.
    git(dir, &["stash", "-q", "-u"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(3));
    // Its upstream does not keep them either: deleting the branch would
    // still lose them here.
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        dir,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(dir, &["push", "-q", "-u", "origin", "fix"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(3));
    // Merged into another branch, they are held elsewhere.
    git(dir, &["switch", "-q", "main"]);
    git(dir, &["merge", "-q", "--ff-only", "fix"]);
    git(dir, &["switch", "-q", "fix"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(0), "merged");
    // A tag holds a commit too.
    write(dir, "more.txt", 1);
    git(dir, &["add", "more.txt"]);
    git(dir, &["commit", "-q", "-m", "more"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(1));
    git(dir, &["tag", "kept"]);
    assert_eq!(own_commits(dir).await.unwrap(), Some(0), "tagged");
    // The blocking face counts a branch by name, checked out or not.
    git(dir, &["switch", "-q", "-c", "later"]);
    write(dir, "later.txt", 1);
    git(dir, &["add", "later.txt"]);
    git(dir, &["commit", "-q", "-m", "later"]);
    git(dir, &["switch", "-q", "fix"]);
    assert_eq!(branch_own_commits(dir, "later").unwrap(), Some(1));
    assert_eq!(branch_own_commits(dir, "fix").unwrap(), Some(0));
    assert_eq!(branch_own_commits(dir, "nowhere").unwrap(), None);
    assert!(branch_goes(Some(0)) && !branch_goes(Some(1)) && !branch_goes(None));
    git(dir, &["switch", "-q", "--detach"]);
    assert_eq!(own_commits(dir).await.unwrap(), None, "detached");
}

/// Git trusts a file whose size and times match its index entry, except an
/// entry written in the same second as the index itself, whose content it
/// reads again ("racily clean"). That check compares with the index file's
/// own modification time, so the temporary copy must keep it: a fresh time
/// would make git trust an edit it never saw.
#[tokio::test]
async fn an_edit_in_the_same_second_as_the_index_write_is_seen() {
    let repo = repository();
    let dir = repo.path();
    // A change time moves on every write and cannot be set back; only the
    // modification time decides here, as on a file system with coarse
    // change times.
    git(dir, &["config", "core.trustctime", "false"]);
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    let touch = |path: &Path| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    };
    std::fs::write(dir.join("same.txt"), "aaaa\n").unwrap();
    touch(&dir.join("same.txt"));
    git(dir, &["add", "same.txt"]);
    git(dir, &["commit", "-q", "-m", "same"]);
    // The index written in the same second as the file, then an edit of
    // the same size within that second.
    touch(&dir.join(".git/index"));
    std::fs::write(dir.join("same.txt"), "bbbb\n").unwrap();
    touch(&dir.join("same.txt"));

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.uncommitted, totals(1, 1, 1), "the edit is seen");
    // Adding the untracked files rewrites the copy; git then marks the
    // racy entry so the rewrite does not hide it.
    write(dir, "untracked.txt", 1);
    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.uncommitted, totals(2, 2, 1));
    assert_eq!(
        git(dir, &["status", "--porcelain", "--", "same.txt"]),
        "M same.txt"
    );
}

fn changed<'a>(found: &'a Changes, path: &str) -> &'a ChangedFile {
    found
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} in {:?}", found.files))
}

fn head_of(dir: &Path) -> Option<String> {
    Some(git(dir, &["rev-parse", "HEAD"]))
}

/// A rename is one file with its old path, whether git was told of it or
/// the file was just moved, and a created, a deleted and a binary file are
/// each what they are.
#[tokio::test]
async fn changes_report_a_rename_once_and_each_file_as_what_it_is() {
    let repo = repository();
    let dir = repo.path();
    write(dir, "moved.txt", 20);
    // Unlike anything else here, so git pairs it with nothing.
    std::fs::write(dir.join("gone.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "files"]);
    git(dir, &["mv", "moved.txt", "told.txt"]);
    git(dir, &["mv", "told.txt", "renamed.txt"]);
    write(dir, "renamed.txt", 21);
    std::fs::rename(dir.join("base.txt"), dir.join("unsaid.txt")).unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    write(dir, "new.txt", 2);
    std::fs::write(dir.join("logo.png"), [0u8, 1, 2, 0, 255]).unwrap();
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let found = changes(dir, &Base::Head).await.unwrap();
    assert_eq!(
        found.base,
        UsedBase {
            branch: None,
            upstream: None,
            commit: head_of(dir),
        }
    );
    assert_eq!(found.head, head_of(dir));
    let file = |path: &str, old: Option<&str>, status, added, removed, binary| ChangedFile {
        path: path.to_owned(),
        old_path: old.map(str::to_owned),
        status,
        added,
        removed,
        binary,
        flags: Flags::default(),
    };
    assert_eq!(
        found.files,
        vec![
            file("gone.txt", None, Status::Deleted, 0, 3, false),
            file("logo.png", None, Status::Created, 0, 0, true),
            file("new.txt", None, Status::Created, 2, 0, false),
            file(
                "renamed.txt",
                Some("moved.txt"),
                Status::Renamed,
                1,
                0,
                false
            ),
            file("unsaid.txt", Some("base.txt"), Status::Renamed, 0, 0, false),
        ]
    );
    assert_eq!(
        std::fs::read(dir.join(".git/index")).unwrap(),
        index,
        "the person's index is untouched"
    );
    // The old comparison still lists a rename as the two files it was.
    let compared = compare(dir, &Comparison::Uncommitted, false).await.unwrap();
    assert!(
        compared
            .files
            .iter()
            .any(|file| file.path == "moved.txt" && file.change == Change::Deleted)
    );
}

#[tokio::test]
async fn changes_flag_lockfiles_vendored_generated_and_large_files() {
    let repo = repository();
    let dir = repo.path();
    for folder in ["web/node_modules/left", "vendor", "lib", "gen", "src"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    std::fs::write(
        dir.join(".gitattributes"),
        "lib/** linguist-vendored\ngen/** linguist-generated\nvendor/ours.go -linguist-vendored\nsrc/notes.txt linguist-generated=false\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/old.rs"),
        "// @generated by a tool\nfn old() {}\n",
    )
    .unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "attributes"]);
    std::fs::remove_file(dir.join("src/old.rs")).unwrap();
    write(dir, "Cargo.lock", 3);
    write(dir, "web/yarn.lock", 3);
    write(dir, "web/node_modules/left/index.js", 1);
    write(dir, "vendor/theirs.go", 1);
    write(dir, "vendor/ours.go", 1);
    write(dir, "lib/bundled.js", 1);
    write(dir, "gen/schema.rs", 1);
    std::fs::write(
        dir.join("src/api.pb.go"),
        "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage api\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/notes.txt"), "DO NOT EDIT this by hand\n").unwrap();
    std::fs::write(
        dir.join("src/late.rs"),
        "fn a() {}\n".repeat(20) + "// @generated, said too late\n",
    )
    .unwrap();
    let big = "x".repeat(99) + "\n";
    std::fs::write(
        dir.join("big.txt"),
        big.repeat(LARGE_FILE_BYTES as usize / 100 + 1),
    )
    .unwrap();
    write(dir, "src/plain.rs", 2);

    let found = changes(dir, &Base::Head).await.unwrap();
    let flags = |path: &str| changed(&found, path).flags;
    let lockfile = Flags {
        lockfile: true,
        ..Flags::default()
    };
    let vendored = Flags {
        vendored: true,
        ..Flags::default()
    };
    let generated = Flags {
        generated: true,
        ..Flags::default()
    };
    assert_eq!(flags("Cargo.lock"), lockfile);
    assert_eq!(flags("web/yarn.lock"), lockfile, "wherever it sits");
    assert_eq!(flags("web/node_modules/left/index.js"), vendored);
    assert_eq!(flags("vendor/theirs.go"), vendored);
    assert_eq!(flags("lib/bundled.js"), vendored, "by attribute");
    assert_eq!(
        flags("vendor/ours.go"),
        Flags::default(),
        "unset by attribute"
    );
    assert_eq!(flags("gen/schema.rs"), generated, "by attribute");
    assert_eq!(flags("src/api.pb.go"), generated, "by its header");
    assert_eq!(
        flags("src/old.rs"),
        generated,
        "a deleted file by its header"
    );
    assert_eq!(changed(&found, "src/old.rs").status, Status::Deleted);
    assert_eq!(
        flags("src/notes.txt"),
        Flags::default(),
        "unset by attribute"
    );
    assert_eq!(flags("src/late.rs"), Flags::default(), "past the header");
    assert_eq!(
        flags("big.txt"),
        Flags {
            large: true,
            ..Flags::default()
        }
    );
    assert_eq!(flags("src/plain.rs"), Flags::default());

    // A large file stays large when it is deleted, from its base side.
    git(dir, &["add", "big.txt"]);
    git(dir, &["commit", "-q", "-m", "big"]);
    std::fs::remove_file(dir.join("big.txt")).unwrap();
    let found = changes(dir, &Base::Head).await.unwrap();
    assert!(changed(&found, "big.txt").flags.large);
}

/// A branch of its own runs from where it left its base branch, whether the
/// rule finds the base or the request names it; the base moving on after
/// does not count.
#[tokio::test]
async fn changes_on_a_branch_of_its_own_run_from_where_it_left_its_base() {
    let repo = repository();
    let dir = repo.path();
    let fork = head_of(dir);
    git(dir, &["switch", "-q", "-c", "feature"]);
    write(dir, "committed.txt", 4);
    git(dir, &["add", "committed.txt"]);
    git(dir, &["commit", "-q", "-m", "on the branch"]);
    git(dir, &["switch", "-q", "main"]);
    write(dir, "later-on-main.txt", 5);
    git(dir, &["add", "later-on-main.txt"]);
    git(dir, &["commit", "-q", "-m", "main moves on"]);
    git(dir, &["switch", "-q", "feature"]);
    write(dir, "untracked.txt", 1);
    let paths = |found: &Changes| {
        found
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>()
    };
    let branch = UsedBase {
        branch: Some("main".into()),
        upstream: None,
        commit: fork.clone(),
    };

    let found = changes(
        dir,
        &Base::Default {
            recorded_branch: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(found.base, branch);
    assert_eq!(paths(&found), ["committed.txt", "untracked.txt"]);
    let named = changes(dir, &Base::Branch("main".into())).await.unwrap();
    assert_eq!(named, found);
    let head = changes(dir, &Base::Head).await.unwrap();
    assert_eq!(paths(&head), ["untracked.txt"]);

    // A recorded base wins over the default.
    let recorded = changes(
        dir,
        &Base::Default {
            recorded_branch: Some("feature".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(recorded.base.branch.as_deref(), Some("feature"));
    assert_eq!(recorded.base.commit, head_of(dir));
    assert_eq!(paths(&recorded), ["untracked.txt"]);

    assert!(matches!(
        changes(dir, &Base::Branch("nowhere".into())).await,
        Err(GitError::NoForkPoint(base)) if base == "nowhere"
    ));
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        changes(outside.path(), &Base::Head).await,
        Err(GitError::NotARepository)
    ));
}

/// On the base branch the merge base is HEAD, so the default rule runs from
/// where the branch left its upstream: commits not pushed yet count.
/// Without an upstream it runs from HEAD.
#[tokio::test]
async fn changes_on_the_base_branch_run_from_its_upstream_else_head() {
    let repo = repository();
    let dir = repo.path();
    let default = Base::Default {
        recorded_branch: None,
    };
    write(dir, "draft.txt", 1);

    let alone = changes(dir, &default).await.unwrap();
    assert_eq!(
        alone.base,
        UsedBase {
            branch: Some("main".into()),
            upstream: None,
            commit: head_of(dir),
        },
        "no upstream: from HEAD"
    );
    assert_eq!(alone.files.len(), 1);

    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(
        dir,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(dir, &["push", "-q", "-u", "origin", "main"]);
    let pushed = head_of(dir);
    write(dir, "unpushed.txt", 2);
    git(dir, &["add", "unpushed.txt"]);
    git(dir, &["commit", "-q", "-m", "not pushed"]);

    let found = changes(dir, &default).await.unwrap();
    assert_eq!(
        found.base,
        UsedBase {
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            commit: pushed,
        }
    );
    let paths: Vec<_> = found.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["draft.txt", "unpushed.txt"]);

    // The row's own counts follow the same rule.
    let row = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(row.uncommitted, totals(1, 1, 0));
    assert_eq!(row.on_branch, totals(2, 3, 0));
    // Once pushed, only the uncommitted work is left.
    git(dir, &["push", "-q"]);
    let row = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(row.on_branch, row.uncommitted);
}

/// Before the first commit the default rule has nothing to run from but the
/// empty tree; a named branch cannot be found.
#[tokio::test]
async fn changes_before_the_first_commit_are_everything() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "first.txt", 5);
    let found = changes(
        dir.path(),
        &Base::Default {
            recorded_branch: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(found.head, None);
    assert_eq!(found.base.commit, None);
    assert_eq!(found.files.len(), 1);
    assert_eq!(found.files[0].status, Status::Created);
    assert!(matches!(
        changes(dir.path(), &Base::Branch("main".into())).await,
        Err(GitError::NoForkPoint(_))
    ));
}

/// What a delete's warning reads: the branch, its own commits by the rule
/// the removal applies, and the uncommitted work.
#[tokio::test]
async fn delete_facts_say_what_a_delete_takes() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "fix"]);
    write(dir, "draft.txt", 2);
    assert_eq!(
        delete_facts(dir).await.unwrap(),
        Some(DeleteFacts {
            branch: Some("fix".into()),
            own_commits: Some(0),
            branch_goes: true,
            uncommitted: totals(1, 2, 0).unwrap(),
        })
    );
    git(dir, &["add", "draft.txt"]);
    git(dir, &["commit", "-q", "-m", "fix"]);
    assert_eq!(
        delete_facts(dir).await.unwrap(),
        Some(DeleteFacts {
            branch: Some("fix".into()),
            own_commits: Some(1),
            branch_goes: false,
            uncommitted: ChangeTotals::default(),
        })
    );
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(delete_facts(outside.path()).await.unwrap(), None);
}
