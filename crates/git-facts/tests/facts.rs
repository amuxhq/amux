//! The facts of real repositories made in temporary folders.

use std::path::Path;
use std::process::Command;

use git_facts::{
    Change, ChangeTotals, Checkout, Comparison, FileChange, GitError, GitFacts, Worktree,
    WorktreeError, add_worktree, checkout, compare, facts, origin, own_commits,
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
async fn a_checkout_is_its_working_trees_root_and_a_worktree_names_its_clone() {
    let repo = repository();
    let dir = repo.path().canonicalize().unwrap();
    git(
        &dir,
        &["remote", "add", "origin", "git@github.com:amuxhq/amux.git"],
    );
    std::fs::create_dir(dir.join("sub")).unwrap();
    let remote = Some("github.com/amuxhq/amux".to_owned());

    // A folder inside the clone is in the clone's working tree.
    assert_eq!(
        checkout(&dir.join("sub")),
        Some(Checkout {
            root: dir.clone(),
            main: None,
            remote: remote.clone(),
        })
    );

    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("quiet-otter");
    add_worktree(&dir, &destination, "quiet-otter")
        .await
        .unwrap();
    let destination = destination.canonicalize().unwrap();
    assert_eq!(
        checkout(&destination),
        Some(Checkout {
            root: destination.clone(),
            main: Some(dir.clone()),
            remote,
        }),
        "a linked worktree names the clone it was made from"
    );

    let outside = tempfile::tempdir().unwrap();
    assert_eq!(checkout(outside.path()), None);
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
    git(dir, &["switch", "-q", "--detach"]);
    assert_eq!(own_commits(dir).await.unwrap(), None, "detached");
}

#[test]
fn a_folders_origin_names_its_repository_as_facts_do() {
    let dir = repository();
    assert_eq!(origin(dir.path()), None);
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:amuxhq/amux.git"],
    );
    assert_eq!(
        origin(dir.path()).as_deref(),
        Some("github.com/amuxhq/amux")
    );
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(origin(outside.path()), None);
}
