//! The backends against real repositories in temporary folders: git's
//! worktrees, and (on Unix) wt's trees under a wt home of their own.

use std::path::{Path, PathBuf};
use std::process::Command;

use daemon_core::workspaces::{Key, Kind, PlanRefusal, Removal};
use uuid::Uuid;

use super::Workspaces;

const HOST: Uuid = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
const OTHER: Uuid = Uuid::from_u128(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000);

fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.fsmonitor=false",
        ])
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

/// A folder of its own for one test, canonical, holding a repository named
/// `shop` on `main` with one commit.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::plain_paths::canonical(dir.path()).unwrap();
        let repo = root.join("shop");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "shop\n").unwrap();
        run_git(&repo, &["add", "README"]);
        run_git(&repo, &["commit", "-q", "-m", "start"]);
        Self { _dir: dir, root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("shop")
    }

    fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    fn wt_home(&self) -> PathBuf {
        self.root.join("wt-home")
    }

    fn backends(&self, host: Uuid) -> Workspaces {
        Workspaces::new(host, self.worktrees(), Some(self.wt_home()))
    }
}

fn key(kind: Kind, path: &Path) -> Key {
    Key {
        kind,
        id: path.to_string_lossy().into_owned(),
    }
}

#[test]
fn a_folder_is_in_its_repositorys_working_tree_or_a_folder_of_its_own() {
    let place = Place::new();
    let backends = place.backends(HOST);
    let inside = place.repo().join("src");
    std::fs::create_dir_all(&inside).unwrap();
    let outside = place.root.join("notes");
    std::fs::create_dir_all(&outside).unwrap();

    let resolved = backends.resolve(&inside.to_string_lossy());
    assert_eq!(resolved.workspace, key(Kind::Folder, &place.repo()));
    assert!(!resolved.made_here);
    assert_eq!(
        backends.resolve(&outside.to_string_lossy()).workspace,
        key(Kind::Folder, &outside)
    );
    // Gone from the disk: the path as written.
    let gone = "/no/such/folder/anywhere";
    assert_eq!(
        backends.resolve(gone).workspace,
        Key {
            kind: Kind::Folder,
            id: gone.to_owned()
        }
    );
    assert!(backends.owned().is_empty());
}

#[test]
fn a_worktree_is_planned_made_and_known_as_this_hosts_by_its_marker() {
    let place = Place::new();
    let backends = place.backends(HOST);
    run_git(&place.repo(), &["switch", "-q", "-c", "feature"]);

    let planned = backends.plan(&place.repo(), "fix-scroll").unwrap();
    let path = place.worktrees().join("shop").join("fix-scroll");
    assert_eq!(planned.workspace, key(Kind::Git, &path));
    assert_eq!(planned.plan.branch, "fix-scroll");
    assert_eq!(planned.base_branch.as_deref(), Some("feature"));
    assert!(!path.exists(), "planning makes nothing");

    backends.make(&planned.workspace, &planned.plan).unwrap();
    assert_eq!(run_git(&path, &["branch", "--show-current"]), "fix-scroll");
    assert_eq!(
        run_git(&path, &["rev-parse", "HEAD"]),
        run_git(&place.repo(), &["rev-parse", "feature"]),
        "made from what the creating folder had checked out"
    );
    let git_dir = PathBuf::from(run_git(&path, &["rev-parse", "--absolute-git-dir"]));
    assert_eq!(
        std::fs::read_to_string(git_dir.join("amux-owner")).unwrap(),
        HOST.to_string()
    );
    assert!(
        run_git(&path, &["status", "--porcelain"]).is_empty(),
        "the marker is not in the working tree"
    );

    let deep = path.join("src");
    std::fs::create_dir_all(&deep).unwrap();
    let resolved = backends.resolve(&deep.to_string_lossy());
    assert_eq!(resolved.workspace, planned.workspace);
    assert!(resolved.made_here);
    assert_eq!(backends.owned(), vec![planned.workspace.clone()]);

    // Another profile of the installation sees a worktree of the
    // repository, not one it made.
    let other = place.backends(OTHER);
    let theirs = other.resolve(&deep.to_string_lossy());
    assert_eq!(theirs.workspace, key(Kind::Folder, &path));
    assert!(other.owned().is_empty());
    assert_eq!(other.delete(&planned.workspace, false), Removal::Kept);
    assert!(path.exists());
}

#[test]
fn a_worktree_plan_is_refused_before_anything_is_made() {
    let place = Place::new();
    let backends = place.backends(HOST);
    let outside = place.root.join("loose");
    std::fs::create_dir_all(&outside).unwrap();
    assert!(matches!(
        backends.plan(&outside, "tidy"),
        Err(PlanRefusal::Refused(error)) if error.message.contains("not in a git repository")
    ));
    assert!(matches!(
        backends.plan(&place.repo(), "main"),
        Err(PlanRefusal::Taken(why)) if why.contains("a branch named main")
    ));
    let taken = place.worktrees().join("shop").join("there");
    std::fs::create_dir_all(&taken).unwrap();
    assert!(matches!(
        backends.plan(&place.repo(), "there"),
        Err(PlanRefusal::Taken(why)) if why.contains("already exists")
    ));
    run_git(&place.repo(), &["switch", "-q", "--detach"]);
    assert!(matches!(
        backends.plan(&place.repo(), "loose-end"),
        Err(PlanRefusal::Refused(error)) if error.message.contains("no branch checked out")
    ));
}

#[test]
fn a_clean_worktree_goes_with_its_empty_branch_and_a_dirty_one_stays_nobodys() {
    let place = Place::new();
    let backends = place.backends(HOST);
    let make = |name: &str| {
        let planned = backends.plan(&place.repo(), name).unwrap();
        backends.make(&planned.workspace, &planned.plan).unwrap();
        planned.workspace
    };
    let branches = || run_git(&place.repo(), &["branch", "--format=%(refname:short)"]);

    // What a delete's warning says of the branch and what the removal does
    // with it read one count: the branch goes exactly when it is zero.
    let own_commits = |name: &str| git_facts::branch_own_commits(&place.repo(), name).unwrap();

    // Clean, nothing committed: the worktree and its branch go.
    let empty = make("empty");
    std::fs::create_dir_all(Path::new(&empty.id).join("target")).unwrap();
    std::fs::write(Path::new(&empty.id).join(".gitignore"), "target\n").unwrap();
    run_git(Path::new(&empty.id), &["add", ".gitignore"]);
    run_git(Path::new(&empty.id), &["stash", "-q"]);
    assert_eq!(own_commits("empty"), Some(0));
    assert!(git_facts::branch_goes(own_commits("empty")));
    assert_eq!(backends.delete(&empty, false), Removal::Removed);
    assert!(!Path::new(&empty.id).exists());
    assert!(!branches().contains("empty"), "{}", branches());
    assert_eq!(own_commits("empty"), None, "no branch left to count");

    // Clean with commits of its own: the branch keeps them.
    let worked = make("worked");
    std::fs::write(Path::new(&worked.id).join("work.txt"), "work\n").unwrap();
    run_git(Path::new(&worked.id), &["add", "work.txt"]);
    run_git(Path::new(&worked.id), &["commit", "-q", "-m", "work"]);
    assert_eq!(own_commits("worked"), Some(1));
    assert!(!git_facts::branch_goes(own_commits("worked")));
    assert_eq!(backends.delete(&worked, false), Removal::Removed);
    assert!(!Path::new(&worked.id).exists());
    assert!(branches().contains("worked"));
    assert_eq!(
        own_commits("worked"),
        Some(1),
        "its commits are still its own"
    );

    // Uncommitted work: kept, and no longer this host's.
    let dirty = make("dirty");
    std::fs::write(Path::new(&dirty.id).join("draft.txt"), "draft\n").unwrap();
    assert_eq!(backends.delete(&dirty, false), Removal::Kept);
    assert!(Path::new(&dirty.id).join("draft.txt").exists());
    assert!(!backends.resolve(&dirty.id).made_here);
    assert!(backends.owned().is_empty());
    // A workspace gone from the disk has nothing left to remove.
    assert_eq!(backends.delete(&empty, false), Removal::Removed);
}

#[test]
fn untracked_files_count_as_changes_whatever_the_repository_shows() {
    let place = Place::new();
    let backends = place.backends(HOST);
    // Large repositories often hide untracked files from `git status`; the
    // forced removal would still delete them.
    run_git(
        &place.repo(),
        &["config", "status.showUntrackedFiles", "no"],
    );
    let planned = backends.plan(&place.repo(), "draft").unwrap();
    backends.make(&planned.workspace, &planned.plan).unwrap();
    let draft = Path::new(&planned.workspace.id).join("draft.txt");
    std::fs::write(&draft, "draft\n").unwrap();
    assert_eq!(backends.delete(&planned.workspace, false), Removal::Kept);
    assert!(
        draft.exists(),
        "an untracked file was deleted without consent"
    );
}

#[cfg(unix)]
#[test]
fn a_worktree_that_cannot_be_marked_is_undone_with_its_branch() {
    use std::os::unix::fs::PermissionsExt as _;

    let place = Place::new();
    let backends = place.backends(HOST);
    // A post-checkout hook, which `git worktree add` runs, puts a folder
    // where the marker goes, so writing it fails after the worktree and its
    // branch were made.
    let hooks = place.root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    std::fs::write(
        &hook,
        "#!/bin/sh\nmkdir \"$(git rev-parse --absolute-git-dir)/amux-owner\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    run_git(
        &place.repo(),
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );

    let planned = backends.plan(&place.repo(), "unmarked").unwrap();
    let made = backends.make(&planned.workspace, &planned.plan);
    assert!(
        made.is_err(),
        "a worktree with no marker was made: {made:?}"
    );
    assert!(
        !Path::new(&planned.workspace.id).exists(),
        "the worktree was left behind"
    );
    let branches = run_git(&place.repo(), &["branch", "--format=%(refname:short)"]);
    assert!(
        !branches.contains("unmarked"),
        "the branch was left behind: {branches}"
    );
    let worktrees = run_git(&place.repo(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("unmarked"), "{worktrees}");
}

/// The wt backend, against a wt home and trees folder of the test's own.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
mod wt {
    use super::*;

    /// The repository's `.wt.toml`, committed so its trees have it: an
    /// environment, and hooks that record each run in the tree.
    const CONFIG: &str = r#"
[env]
SHOP_MODE = "tree-{{name}}"

[lifecycle]
create = "echo created >> .wt/ran"
start = "echo started >> .wt/ran"
stop = "echo stopped >> .wt/ran"
"#;

    /// `place`'s repository configured for wt, with a bare `origin` whose
    /// default branch is `main`.
    fn configured(place: &Place) {
        let repo = place.repo();
        std::fs::write(repo.join(".wt.toml"), CONFIG).unwrap();
        run_git(&repo, &["add", ".wt.toml"]);
        run_git(&repo, &["commit", "-q", "-m", "wt"]);
        let origin = place.root.join("origin.git");
        run_git(
            &place.root,
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        run_git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        run_git(&repo, &["push", "-q", "-u", "origin", "main"]);
        run_git(&repo, &["remote", "set-head", "origin", "main"]);
    }

    fn ran(tree: &Key) -> String {
        std::fs::read_to_string(Path::new(&tree.id).join(".wt/ran")).unwrap_or_default()
    }

    /// Waits for what `ran` says to include `what`: the create hook runs on
    /// after the tree is made.
    fn wait_for(tree: &Key, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !ran(tree).contains(what) {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} never ran: {}",
                ran(tree)
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn a_repository_without_wt_gets_a_worktree() {
        let place = Place::new();
        let planned = place.backends(HOST).plan(&place.repo(), "plain").unwrap();
        assert_eq!(planned.workspace.kind, Kind::Git);
    }

    #[test]
    fn a_tree_is_made_owned_and_run_in_its_environment() {
        let place = Place::new();
        configured(&place);
        let backends = place.backends(HOST);

        // On the default branch: the new branch starts from origin's,
        // fetched.
        let planned = backends.plan(&place.repo(), "fix-scroll").unwrap();
        let path = place
            .wt_home()
            .join("trees")
            .join("shop")
            .join("fix-scroll");
        assert_eq!(planned.workspace, key(Kind::Wt, &path));
        assert_eq!(planned.plan.branch, "fix-scroll");
        assert_eq!(planned.plan.from, None);
        assert_eq!(planned.base_branch.as_deref(), Some("main"));
        assert!(!path.exists(), "planning makes nothing");

        backends.make(&planned.workspace, &planned.plan).unwrap();
        let tree = planned.workspace.clone();
        assert_eq!(
            std::fs::read_to_string(path.join(".wt/owner")).unwrap(),
            format!("amux:{HOST}")
        );
        wait_for(&tree, "created");
        std::fs::create_dir_all(path.join("src")).unwrap();
        let resolved = backends.resolve(&path.join("src").to_string_lossy());
        assert_eq!(resolved.workspace, tree);
        assert!(resolved.made_here);
        assert_eq!(backends.owned(), vec![tree.clone()]);
        // To another host it is hand-made.
        let other = place.backends(OTHER);
        assert!(!other.resolve(&tree.id).made_here);
        assert!(other.owned().is_empty());

        let env = backends.environment(&tree).unwrap().unwrap();
        assert_eq!(env.vars["SHOP_MODE"], "tree-fix-scroll");
        assert_eq!(env.vars["WT_TREE"], "shop/fix-scroll");
        assert_eq!(env.path_prefix, vec![path.join(".wt/bin")]);
        let mut provider = std::collections::HashMap::from([
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            ("KEPT".to_owned(), "yes".to_owned()),
        ]);
        env.apply(&mut provider);
        assert_eq!(
            provider["PATH"],
            format!("{}:/usr/bin", path.join(".wt/bin").display())
        );
        assert_eq!(provider["KEPT"], "yes");

        // A name the tree has is taken.
        assert!(matches!(
            backends.plan(&place.repo(), "fix-scroll"),
            Err(PlanRefusal::Taken(_))
        ));

        // A folder on another branch starts its new branch from its own
        // commit.
        run_git(&place.repo(), &["switch", "-q", "-c", "feature"]);
        let child = backends.plan(&place.repo(), "child").unwrap();
        assert_eq!(
            child.plan.from.as_deref(),
            Some(run_git(&place.repo(), &["rev-parse", "HEAD"]).as_str())
        );
        assert_eq!(child.base_branch.as_deref(), Some("feature"));
    }

    #[test]
    fn a_held_tree_stops_only_once_amux_lets_go_and_starts_again_when_asked() {
        let place = Place::new();
        configured(&place);
        let backends = std::sync::Arc::new(place.backends(HOST));
        let planned = backends.plan(&place.repo(), "busy").unwrap();
        backends.make(&planned.workspace, &planned.plan).unwrap();
        let tree = planned.workspace.clone();
        wait_for(&tree, "created");

        assert!(backends.try_claim(&tree).unwrap());
        backends.start(&tree).unwrap();
        assert!(ran(&tree).contains("started"));

        let stopper = {
            let (backends, tree) = (backends.clone(), tree.clone());
            std::thread::spawn(move || backends.stop_when_unheld(&tree))
        };
        // A held tree blocks the stopper's flock; wait briefly and confirm
        // neither the stopper finishes nor the stop hook runs while claimed.
        let held_until = std::time::Instant::now() + std::time::Duration::from_millis(150);
        while std::time::Instant::now() < held_until {
            assert!(!stopper.is_finished(), "a claim keeps the stopper waiting");
            assert!(!ran(&tree).contains("stopped"), "a claim keeps it running");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        backends.release(&tree);
        stopper.join().unwrap().unwrap();
        assert!(ran(&tree).contains("stopped"));

        // Removal never waits: held, it is kept and disowned.
        assert!(backends.try_claim(&tree).unwrap());
        assert_eq!(backends.delete(&tree, false), Removal::Kept);
        assert!(!backends.resolve(&tree.id).made_here);
        backends.release(&tree);
    }

    #[test]
    fn a_tree_another_host_made_is_neither_removed_nor_disowned() {
        // Two profiles that planned the same tree: the one whose make lost
        // deletes what it would have made, and finds the winner's there.
        let place = Place::new();
        configured(&place);
        let winner = place.backends(HOST);
        let planned = winner.plan(&place.repo(), "shared").unwrap();
        winner.make(&planned.workspace, &planned.plan).unwrap();
        let tree = planned.workspace;
        wait_for(&tree, "created");

        let loser = place.backends(OTHER);
        assert_eq!(loser.delete(&tree, false), Removal::Kept);
        assert_eq!(loser.delete(&tree, true), Removal::Kept);
        assert!(Path::new(&tree.id).is_dir(), "the winner's tree stays");
        assert!(winner.resolve(&tree.id).made_here, "and stays the winner's");
        assert_eq!(winner.owned(), vec![tree.clone()]);
        // Its own host still removes it.
        assert_eq!(winner.delete(&tree, false), Removal::Removed);
    }

    #[test]
    fn a_clean_tree_is_removed_and_a_dirty_one_kept_nobodys() {
        let place = Place::new();
        configured(&place);
        let backends = place.backends(HOST);
        let make = |name: &str| {
            let planned = backends.plan(&place.repo(), name).unwrap();
            backends.make(&planned.workspace, &planned.plan).unwrap();
            wait_for(&planned.workspace, "created");
            planned.workspace
        };

        let branches = || run_git(&place.repo(), &["branch", "--format=%(refname:short)"]);
        let own_commits = |name: &str| git_facts::branch_own_commits(&place.repo(), name).unwrap();

        let clean = make("clean");
        assert_eq!(own_commits("clean"), Some(0));
        assert_eq!(backends.delete(&clean, false), Removal::Removed);
        assert!(!Path::new(&clean.id).exists());
        assert!(
            !branches().contains("clean"),
            "its empty branch goes: {}",
            branches()
        );

        // Commits of its own keep the branch, by the same count.
        let worked = make("worked");
        std::fs::write(Path::new(&worked.id).join("work.txt"), "work\n").unwrap();
        run_git(Path::new(&worked.id), &["add", "work.txt"]);
        run_git(Path::new(&worked.id), &["commit", "-q", "-m", "work"]);
        assert_eq!(own_commits("worked"), Some(1));
        assert_eq!(backends.delete(&worked, false), Removal::Removed);
        assert!(!Path::new(&worked.id).exists());
        assert!(branches().contains("worked"), "{}", branches());

        let dirty = make("dirty");
        std::fs::write(Path::new(&dirty.id).join("draft.txt"), "draft\n").unwrap();
        assert_eq!(backends.delete(&dirty, false), Removal::Kept);
        assert!(Path::new(&dirty.id).join("draft.txt").exists());
        assert!(!Path::new(&dirty.id).join(".wt/owner").exists());
        assert!(backends.owned().is_empty());
    }

    #[test]
    fn a_tree_whose_configuration_does_not_load_gives_no_environment() {
        let place = Place::new();
        configured(&place);
        let backends = place.backends(HOST);
        let planned = backends.plan(&place.repo(), "broken").unwrap();
        backends.make(&planned.workspace, &planned.plan).unwrap();
        wait_for(&planned.workspace, "created");
        std::fs::write(
            Path::new(&planned.workspace.id).join(".wt.toml"),
            "[env\nnot toml",
        )
        .unwrap();
        assert!(backends.environment(&planned.workspace).is_err());
    }
}
