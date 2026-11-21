//! Snapshots of real repositories made in temporary folders, read back
//! through the store, with the repository's `.git` compared file by file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use git_facts::GitError;
use git_facts::objects::ObjectStore;
use git_facts::snapshot::{self, Limits, Snapshot};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
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

/// Whether git in `cwd` says yes, with `alternate` as an alternate.
fn git_says(cwd: &Path, alternate: Option<&Path>, args: &[&str]) -> bool {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    if let Some(alternate) = alternate {
        command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate);
    }
    command.output().unwrap().status.success()
}

fn write(cwd: &Path, path: &str, text: &str) {
    let path = cwd.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// A repository on `main` with one commit of a file.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "base.txt", "one\ntwo\nthree\n");
    git(dir.path(), &["add", "base.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "start"]);
    dir
}

/// The limits a host takes snapshots with.
const LIMITS: Limits = Limits {
    large_file_bytes: git_facts::LARGE_FILE_BYTES,
    max_paths: snapshot::MAX_PATHS,
    tries: snapshot::TRIES,
    retry_after: snapshot::RETRY_AFTER,
};

/// A store in a folder of its own, as an agent's would be.
struct Store {
    _dir: tempfile::TempDir,
    store: ObjectStore,
}

fn store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let store = ObjectStore::at(dir.path().join("review/objects"));
    Store { _dir: dir, store }
}

async fn take(repo: &Path, store: &ObjectStore) -> Snapshot {
    snapshot::take(repo, store, &Limits::default(), SystemTime::now())
        .await
        .unwrap()
}

/// What the snapshot holds at `path`, read in the repository through the
/// store; None when it holds nothing there.
async fn read(repo: &Path, store: &ObjectStore, commit: &str, path: &str) -> Option<Vec<u8>> {
    let spec = format!("{commit}:{path}");
    let output = store
        .reading(repo, &["cat-file", "blob", &spec])
        .output()
        .await
        .unwrap();
    output.status.success().then_some(output.stdout)
}

/// Every path in the snapshot, with its mode and object id.
async fn tree(
    repo: &Path,
    store: &ObjectStore,
    commit: &str,
) -> BTreeMap<String, (String, String)> {
    let output = store
        .reading(repo, &["ls-tree", "-r", "-z", "--full-tree", commit])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let record = String::from_utf8_lossy(record);
            let (head, path) = record.split_once('\t').unwrap();
            let mut parts = head.split(' ');
            let mode = parts.next().unwrap().to_owned();
            let id = parts.nth(1).unwrap().to_owned();
            (path.to_owned(), (mode, id))
        })
        .collect()
}

fn paths(tree: &BTreeMap<String, (String, String)>) -> Vec<&str> {
    tree.keys().map(String::as_str).collect()
}

/// Every file and folder under `.git`, with its kind, size, modification
/// time and bytes: a snapshot that writes, renames, freshens or creates
/// anything there changes it.
fn manifest(repo: &Path) -> BTreeMap<PathBuf, (bool, u64, SystemTime, Vec<u8>)> {
    let mut found = BTreeMap::new();
    let mut folders = vec![repo.join(".git")];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            let bytes = if meta.is_file() {
                std::fs::read(entry.path()).unwrap()
            } else {
                Vec::new()
            };
            if meta.is_dir() {
                folders.push(entry.path());
            }
            found.insert(
                entry.path().strip_prefix(repo).unwrap().to_owned(),
                (meta.is_dir(), meta.len(), meta.modified().unwrap(), bytes),
            );
        }
    }
    found
}

/// The repository's `.git` is the same before and after.
#[track_caller]
fn unchanged(
    before: &BTreeMap<PathBuf, (bool, u64, SystemTime, Vec<u8>)>,
    repo: &Path,
    step: &str,
) {
    let after = manifest(repo);
    let added: Vec<_> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    let gone: Vec<_> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let changed: Vec<_> = before
        .iter()
        .filter(|(path, entry)| after.get(*path).is_some_and(|now| now != *entry))
        .map(|(path, _)| path)
        .collect();
    assert!(
        added.is_empty() && gone.is_empty() && changed.is_empty(),
        "{step} touched .git: added {added:?}, removed {gone:?}, changed {changed:?}"
    );
}

/// A repository whose history is packed with a loose object beside it, and
/// a working tree that changes, deletes, stages and adds files, among them
/// ones whose content the repository already holds (an empty file, a copy
/// of a committed one). Git asked to write an object it already stores
/// refreshes that object's file time; none of that may reach the
/// repository, whether taking, asking whether the tree moved, or dropping.
#[tokio::test]
async fn nothing_in_the_repository_changes_taking_asking_or_dropping() {
    let repo = repository();
    let root = repo.path();
    write(root, "empty.txt", "");
    write(root, "src/lib.rs", "fn main() {}\n");
    write(root, "gone.txt", "gone\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "more"]);
    git(root, &["gc", "-q"]);
    write(root, "loose.txt", "loose\n");
    git(root, &["add", "loose.txt"]);
    git(root, &["commit", "-q", "-m", "loose"]);
    write(root, "base.txt", "one\nTWO\nthree\n");
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    write(root, "staged.txt", "staged\n");
    git(root, &["add", "staged.txt"]);
    write(root, "new-empty.txt", "");
    write(root, "copy.rs", "fn main() {}\n");
    write(root, "src/new.rs", "pub fn new() {}\n");
    let objects_before = git(root, &["count-objects", "-v"]);
    let refs_before = git(root, &["for-each-ref"]);
    // Let a second pass so a time git sets now cannot equal one it set
    // while the repository was made.
    std::thread::sleep(Duration::from_millis(1100));

    let store = store();
    let before = manifest(root);
    let first = take(root, &store.store).await;
    unchanged(&before, root, "taking a snapshot");
    assert!(
        !snapshot::moved(root, &store.store, &LIMITS, &first.commit, &first.omitted)
            .await
            .unwrap()
    );
    unchanged(&before, root, "asking whether the tree moved");
    write(root, "src/new.rs", "pub fn newer() {}\n");
    let before = manifest(root);
    assert!(
        snapshot::moved(root, &store.store, &LIMITS, &first.commit, &first.omitted)
            .await
            .unwrap()
    );
    let second = take(root, &store.store).await;
    let files = tree(root, &store.store, &first.commit).await;
    snapshot::drop(root, &store.store, [&second.commit])
        .await
        .unwrap();
    unchanged(&before, root, "a second snapshot and a drop");
    assert_eq!(git(root, &["count-objects", "-v"]), objects_before);
    assert_eq!(git(root, &["for-each-ref"]), refs_before);

    assert_eq!(
        paths(&files),
        [
            "base.txt",
            "copy.rs",
            "empty.txt",
            "loose.txt",
            "new-empty.txt",
            "src/lib.rs",
            "src/new.rs",
            "staged.txt"
        ]
    );
    assert_eq!(
        read(root, &store.store, &second.commit, "base.txt")
            .await
            .unwrap(),
        b"one\nTWO\nthree\n"
    );
    assert_eq!(
        read(root, &store.store, &second.commit, "src/new.rs")
            .await
            .unwrap(),
        b"pub fn newer() {}\n"
    );
    let head = git(root, &["rev-parse", "HEAD"]);
    let parent = store
        .store
        .reading(root, &["rev-parse", &format!("{}^", second.commit)])
        .output()
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&parent.stdout).trim(), head);
    assert_eq!(first.omitted, Vec::<String>::new());
    assert!(!first.merging);
}

/// `count` numbered lines, `line 1` first, with the lines `changed` (from
/// 1) rewritten.
fn numbered(count: usize, changed: &[usize]) -> String {
    (1..=count)
        .map(|n| match changed.contains(&n) {
            true => format!("changed {n}\n"),
            false => format!("line {n}\n"),
        })
        .collect()
}

/// What every call of the crate answers for the repository at `root`,
/// committed at `from`, as text to compare: the changes and facts the
/// fleet shows, a snapshot and whether the tree moved since, every
/// comparison between `from` and the snapshot, and a drop. The store is a
/// fresh one each time, so the answers can be compared.
async fn every_answer(root: &Path, from: &str) -> Vec<String> {
    use git_facts::compare::{self, LineMap, LineRange, Shown};

    let store = store();
    let store = &store.store;
    let mut answers = vec![
        format!(
            "{:?}",
            git_facts::changes(root, &git_facts::Base::Head).await
        ),
        format!(
            "{:?}",
            git_facts::changes(
                root,
                &git_facts::Base::Default {
                    recorded_branch: None
                }
            )
            .await
        ),
        format!("{:?}", git_facts::facts(root, None).await),
        format!("{:?}", git_facts::delete_facts(root).await),
        format!("{:?}", git_facts::branch_own_commits(root, "main")),
        format!("{:?}", git_facts::origin(root)),
    ];
    let taken_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let taken = snapshot::take(root, store, &LIMITS, taken_at)
        .await
        .unwrap();
    let to = taken.commit.as_str();
    answers.push(format!("{taken:?}"));
    answers.push(format!(
        "{:?}",
        snapshot::moved(root, store, &LIMITS, to, &taken.omitted).await
    ));
    answers.push(format!("{:?}", compare::files(root, store, from, to).await));
    let paths = ["a.txt", "lib", "new.txt", "x.bin"];
    for context in [0, 3] {
        answers.push(format!(
            "{:?}",
            compare::hunks(root, store, from, to, &paths, context).await
        ));
    }
    let maps = LineMap::for_paths(root, store, from, to, &paths)
        .await
        .unwrap();
    for path in paths {
        let map = &maps[path];
        let ranges = [(1, 4), (5, 5), (6, 12), (21, 22), (26, 27), (30, 40)];
        let mapped: Vec<_> = ranges
            .iter()
            .map(|&(start, end)| map.map(LineRange::new(start, end)))
            .collect();
        answers.push(format!(
            "{path}: {:?} {mapped:?} {:?}",
            map.path(),
            map.changed()
        ));
    }
    for path in paths {
        answers.push(format!("{:?}", compare::read(root, store, to, path).await));
    }
    let shown = Shown {
        base: from,
        snapshot: to,
    };
    let last = Shown {
        base: from,
        snapshot: from,
    };
    answers.push(format!(
        "{:?}",
        compare::since_last(root, store, last, shown, None, 3).await
    ));
    answers.push(format!("{:?}", snapshot::drop(root, store, [to]).await));
    answers
}

/// The person's configuration changes neither what the crate answers nor
/// anything in `.git`, whatever it asks for: a filesystem monitor and a
/// split index, both of which write into `.git`, the index split for real
/// so that reading it would refresh its shared index; LFS configured as
/// required but not installed, with a file LFS tracks whose cached times
/// are stale, so any read that runs the filter fails; and every diff
/// setting that changes what git writes (submodules as a log, hunks joined
/// across more context, other prefixes and paths, colour, an external diff,
/// another algorithm, an order file, no refresh of cached times).
#[tokio::test]
async fn the_persons_git_configuration_changes_no_answer_and_nothing_in_git() {
    let library = repository();
    let repo = repository();
    let root = repo.path();
    write(root, "a.txt", &numbered(40, &[]));
    write(
        root,
        ".gitattributes",
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    );
    // A pointer, committed as is: no LFS runs here.
    write(
        root,
        "x.bin",
        "version https://git-lfs.github.com/spec/v1\noid sha256:\
         4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n",
    );
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "submodule",
            "add",
            "-q",
            library.path().to_str().unwrap(),
            "lib",
        ],
    );
    git(root, &["commit", "-q", "-m", "more"]);
    let from = git(root, &["rev-parse", "HEAD"]);
    // Changes 8 lines apart, which more inter-hunk context joins at three
    // lines of context, and 3 apart, which it joins at none.
    write(root, "a.txt", &numbered(40, &[5, 13, 25, 28]));
    write(root, "new.txt", "new\n");
    let inner = root.join("lib");
    write(&inner, "base.txt", "moved on\n");
    git(&inner, &["commit", "-q", "-am", "inner"]);
    // Its cached times stale, its content as committed.
    std::fs::File::options()
        .write(true)
        .open(root.join("x.bin"))
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();
    let order = tempfile::tempdir().unwrap();
    let order = order.path().join("order");
    std::fs::write(&order, "x.bin\nnew.txt\nlib\n").unwrap();
    std::thread::sleep(Duration::from_millis(1100));

    let before = manifest(root);
    let defaults = every_answer(root, &from).await;
    unchanged(&before, root, "every call, with git's defaults");
    assert!(
        defaults.iter().all(|answer| !answer.contains("Err(")),
        "{defaults:#?}"
    );

    let order = order.to_str().unwrap();
    for (key, value) in [
        ("core.fsmonitor", "true"),
        ("core.splitIndex", "true"),
        ("gc.auto", "1"),
        ("filter.lfs.process", "amux-no-such-lfs filter-process"),
        ("filter.lfs.clean", "amux-no-such-lfs clean -- %f"),
        ("filter.lfs.smudge", "amux-no-such-lfs smudge -- %f"),
        ("filter.lfs.required", "true"),
        ("diff.submodule", "log"),
        ("diff.interHunkContext", "3"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.relative", "true"),
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("diff.external", "amux-no-such-diff"),
        ("diff.algorithm", "patience"),
        ("diff.indentHeuristic", "false"),
        ("diff.orderFile", order),
        ("diff.autoRefreshIndex", "false"),
    ] {
        git(root, &["config", key, value]);
    }
    // The index split for real, as git writes it with that setting: what
    // the person's index links to is a shared index in `.git`, whose time
    // git refreshes whenever it reads an index linked to it.
    git(root, &["update-index", "--split-index"]);
    let shared: Vec<_> = std::fs::read_dir(root.join(".git"))
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
        })
        .collect();
    assert_eq!(shared.len(), 1, "the index is split");
    std::thread::sleep(Duration::from_millis(1100));
    let before = manifest(root);
    let configured = every_answer(root, &from).await;
    unchanged(&before, root, "every call, with the person's configuration");
    for (default, configured) in defaults.iter().zip(&configured) {
        assert_eq!(configured, default);
    }
}

/// An untracked file, a secret not yet ignored, is in the snapshot and its
/// store and nowhere in the repository; an ignored one is in neither.
#[tokio::test]
async fn an_untracked_secret_is_in_the_store_and_never_the_repository() {
    let repo = repository();
    let root = repo.path();
    write(root, ".gitignore", "*.log\n");
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-q", "-m", "ignore"]);
    write(root, ".env", "API_KEY=not-for-the-repository\n");
    write(root, "build.log", "ignored\n");
    write(root, "notes/todo.txt", "untracked in a folder\n");

    let store = store();
    let snapshot = take(root, &store.store).await;
    let files = tree(root, &store.store, &snapshot.commit).await;
    assert_eq!(
        paths(&files),
        [".env", ".gitignore", "base.txt", "notes/todo.txt"]
    );
    let secret = &files[".env"].1;
    assert!(!git_says(root, None, &["cat-file", "-e", secret]));
    assert!(git_says(
        root,
        Some(store.store.dir()),
        &["cat-file", "-e", secret]
    ));
    assert!(!git_says(root, None, &["cat-file", "-e", &snapshot.commit]));
    assert_eq!(
        read(root, &store.store, &snapshot.commit, ".env")
            .await
            .unwrap(),
        b"API_KEY=not-for-the-repository\n"
    );
}

/// The snapshot is of the whole working tree, whichever folder it is asked
/// from, named from the top.
#[tokio::test]
async fn a_snapshot_from_a_subfolder_is_of_the_whole_tree() {
    let repo = repository();
    let root = repo.path();
    write(root, "deep/inner/here.txt", "here\n");
    write(root, "elsewhere.txt", "elsewhere\n");
    let store = store();
    let snapshot = take(&root.join("deep/inner"), &store.store).await;
    assert_eq!(
        paths(&tree(root, &store.store, &snapshot.commit).await),
        ["base.txt", "deep/inner/here.txt", "elsewhere.txt"]
    );
}

/// Before the first commit there is no HEAD to be the parent: the snapshot
/// is a root commit of whatever the folder holds.
#[tokio::test]
async fn before_the_first_commit_a_snapshot_has_no_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "first.txt", "first\n");
    let store = store();
    let snapshot = take(root, &store.store).await;
    let parents = store
        .store
        .reading(
            root,
            &["rev-list", "--parents", "-n", "1", &snapshot.commit],
        )
        .output()
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout).trim(),
        snapshot.commit
    );
    assert_eq!(
        paths(&tree(root, &store.store, &snapshot.commit).await),
        ["first.txt"]
    );
    let author = store
        .store
        .reading(
            root,
            &[
                "log",
                "-1",
                "--format=%an <%ae>|%cn <%ce>",
                &snapshot.commit,
            ],
        )
        .output()
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&author.stdout).trim(),
        "amux <amux@amux.invalid>|amux <amux@amux.invalid>"
    );
}

/// A file rewritten, to the same size, in the same second the index was
/// written is in the snapshot as rewritten: the index's copy keeps the
/// index's time, by which git knows to read such an entry again.
#[tokio::test]
async fn an_edit_in_the_same_second_as_the_index_write_is_taken() {
    let repo = repository();
    let root = repo.path();
    let second = |path: &Path| {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let mut same_second = false;
    for _ in 0..10 {
        write(root, "racy.txt", "2222\n");
        git(root, &["add", "racy.txt"]);
        write(root, "racy.txt", "3333\n");
        if second(&root.join(".git/index")) == second(&root.join("racy.txt")) {
            same_second = true;
            break;
        }
    }
    assert!(same_second, "never wrote the file in the index's second");
    // A copy made in a later second with a fresh time would trust the
    // entry's cached size and time and miss the rewrite.
    std::thread::sleep(Duration::from_millis(1100));
    // Had git smudged the entry (zeroed its cached size, as it does to an
    // entry it finds racily clean when it writes the index), any copy would
    // read the file again, whatever its time, and this test would prove
    // nothing. The copy's time is checked itself in the crate's own tests.
    let entry = git(root, &["ls-files", "--debug", "--", "racy.txt"]);
    assert!(entry.contains("size: 5\t"), "{entry}");
    let index_time = std::fs::metadata(root.join(".git/index"))
        .unwrap()
        .modified()
        .unwrap();
    let store = store();
    let snapshot = take(root, &store.store).await;
    assert_eq!(
        std::fs::metadata(root.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap(),
        index_time,
        "the person's index is untouched"
    );
    assert_eq!(
        read(root, &store.store, &snapshot.commit, "racy.txt")
            .await
            .unwrap(),
        b"3333\n"
    );
}

/// A submodule whose checkout moved to a new commit, with uncommitted work
/// inside it too, is in the snapshot as the new commit; its inner changes
/// are not seen, and do not make the tree read as moved.
#[tokio::test]
async fn a_dirty_submodule_reads_as_a_change_of_commit() {
    let library = repository();
    let repo = repository();
    let root = repo.path();
    git(
        root,
        &[
            "submodule",
            "add",
            "-q",
            library.path().to_str().unwrap(),
            "lib",
        ],
    );
    git(root, &["commit", "-q", "-m", "submodule"]);
    let inner = root.join("lib");
    write(&inner, "base.txt", "moved on\n");
    git(&inner, &["commit", "-q", "-am", "inner"]);
    let moved_to = git(&inner, &["rev-parse", "HEAD"]);
    write(&inner, "base.txt", "uncommitted inside\n");
    write(&inner, "inner-untracked.txt", "inside\n");
    std::thread::sleep(Duration::from_millis(1100));

    let store = store();
    let before = manifest(root);
    let snapshot = take(root, &store.store).await;
    assert!(
        !snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted
        )
        .await
        .unwrap()
    );
    unchanged(&before, root, "a snapshot with a dirty submodule");
    let files = tree(root, &store.store, &snapshot.commit).await;
    assert_eq!(files["lib"], ("160000".to_owned(), moved_to));
    assert_eq!(paths(&files), [".gitmodules", "base.txt", "lib"]);
}

/// A file over the size cap is left out and named, whether it is new or a
/// tracked file that grew; the snapshot keeps the tracked one as the index
/// has it. Editing it does not make the tree read as moved; editing another
/// file does.
#[tokio::test]
async fn a_file_over_the_cap_is_left_out_and_named() {
    let repo = repository();
    let root = repo.path();
    write(root, "grows.txt", "small\n");
    git(root, &["add", "grows.txt"]);
    git(root, &["commit", "-q", "-m", "small"]);
    let large = "x".repeat(2 * 1024 * 1024);
    write(root, "big.bin", &large);
    write(root, "grows.txt", &large);
    write(root, "small.txt", "small\n");

    let store = store();
    let snapshot = take(root, &store.store).await;
    let mut omitted = snapshot.omitted.clone();
    omitted.sort();
    assert_eq!(omitted, ["big.bin", "grows.txt"]);
    assert_eq!(
        paths(&tree(root, &store.store, &snapshot.commit).await),
        ["base.txt", "grows.txt", "small.txt"]
    );
    assert_eq!(
        read(root, &store.store, &snapshot.commit, "grows.txt")
            .await
            .unwrap(),
        b"small\n"
    );
    let moved = || {
        snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted,
        )
    };
    assert!(!moved().await.unwrap());
    write(root, "big.bin", &"y".repeat(2 * 1024 * 1024));
    write(root, "grows.txt", &"z".repeat(3 * 1024 * 1024));
    assert!(!moved().await.unwrap());
    write(root, "small.txt", "edited\n");
    assert!(moved().await.unwrap());
}

/// A file left out of a snapshot moves the tree once a snapshot would take
/// it: shrunk under the cap, deleted, or put back as the index has it,
/// however large. While it would still be left out it does not.
#[tokio::test]
async fn a_left_out_file_moves_the_tree_once_it_would_be_taken() {
    let repo = repository();
    let root = repo.path();
    let limits = Limits {
        large_file_bytes: 100,
        ..LIMITS
    };
    let large = |fill: &str| fill.repeat(200);
    write(root, "committed.txt", &large("c"));
    git(root, &["add", "committed.txt"]);
    git(root, &["commit", "-q", "-m", "large"]);
    write(root, "big.bin", &large("b"));
    write(root, "committed.txt", &large("d"));

    let store = store();
    let snapshot = snapshot::take(root, &store.store, &limits, SystemTime::now())
        .await
        .unwrap();
    let mut omitted = snapshot.omitted.clone();
    omitted.sort();
    assert_eq!(omitted, ["big.bin", "committed.txt"]);
    let moved = || snapshot::moved(root, &store.store, &limits, &snapshot.commit, &omitted);
    assert!(!moved().await.unwrap());
    write(root, "big.bin", &large("e"));
    assert!(!moved().await.unwrap(), "still over the cap");

    write(root, "big.bin", "small now\n");
    assert!(moved().await.unwrap(), "shrunk under the cap");
    write(root, "big.bin", &large("b"));
    assert!(!moved().await.unwrap());

    std::fs::remove_file(root.join("big.bin")).unwrap();
    assert!(moved().await.unwrap(), "deleted");
    write(root, "big.bin", &large("b"));
    assert!(!moved().await.unwrap());

    write(root, "committed.txt", &large("c"));
    assert!(moved().await.unwrap(), "as the index has it");
    write(root, "committed.txt", &large("d"));
    assert!(!moved().await.unwrap());
}

/// More changed and untracked paths than the cap refuse the snapshot,
/// before anything is stored.
#[tokio::test]
async fn more_paths_than_the_cap_refuse_the_snapshot() {
    let repo = repository();
    let root = repo.path();
    write(root, "base.txt", "changed\n");
    write(root, "a.txt", "a\n");
    write(root, "b.txt", "b\n");
    let store = store();
    let limits = Limits {
        max_paths: 2,
        ..Limits::default()
    };
    let error = snapshot::take(root, &store.store, &limits, SystemTime::now())
        .await
        .unwrap_err();
    assert!(
        matches!(error, GitError::TooManyPaths { count: 3, cap: 2 }),
        "{error:?}"
    );
    let stored = std::fs::read_dir(store.store.dir()).unwrap().count();
    assert_eq!(stored, 0);
    assert_eq!(snapshot::MAX_PATHS, 10_000);
}

/// The tree has not moved right after a snapshot; it has after an edit, a
/// new untracked file, a deletion, and a file put back as it was is still
/// the snapshot's.
#[tokio::test]
async fn moved_answers_whether_the_tree_differs_from_the_snapshot() {
    let repo = repository();
    let root = repo.path();
    write(root, "base.txt", "edited before\n");
    write(root, "untracked.txt", "untracked\n");
    let store = store();
    let snapshot = take(root, &store.store).await;
    let moved = || {
        snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted,
        )
    };
    assert!(!moved().await.unwrap());

    write(root, "base.txt", "edited after\n");
    assert!(moved().await.unwrap());
    write(root, "base.txt", "edited before\n");
    assert!(!moved().await.unwrap());

    write(root, "another.txt", "new\n");
    assert!(moved().await.unwrap());
    std::fs::remove_file(root.join("another.txt")).unwrap();
    assert!(!moved().await.unwrap());

    std::fs::remove_file(root.join("untracked.txt")).unwrap();
    assert!(moved().await.unwrap());
    write(root, "untracked.txt", "untracked\n");
    assert!(!moved().await.unwrap());

    let stored = std::fs::read_dir(store.store.dir())
        .unwrap()
        .map(|fanout| std::fs::read_dir(fanout.unwrap().path()).unwrap().count())
        .sum::<usize>();
    let _ = moved().await.unwrap();
    let after = std::fs::read_dir(store.store.dir())
        .unwrap()
        .map(|fanout| std::fs::read_dir(fanout.unwrap().path()).unwrap().count())
        .sum::<usize>();
    assert_eq!(stored, after, "asking stored objects");
}

/// Dropping keeps every snapshot named readable and removes what only the
/// others held; a commit the store does not hold keeps nothing and is no
/// error.
#[tokio::test]
async fn drop_keeps_named_snapshots_readable() {
    let repo = repository();
    let root = repo.path();
    let store = store();
    let mut taken = Vec::new();
    for round in ["first", "second", "third"] {
        write(root, "work.txt", &format!("{round}\n"));
        write(root, &format!("{round}.txt"), &format!("only in {round}\n"));
        taken.push(take(root, &store.store).await.commit);
        std::fs::remove_file(root.join(format!("{round}.txt"))).unwrap();
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    let removed = snapshot::drop(root, &store.store, [&taken[0], &taken[2], &head])
        .await
        .unwrap();
    assert!(removed >= 3, "removed {removed}");
    for (commit, round) in [(&taken[0], "first"), (&taken[2], "third")] {
        assert_eq!(
            read(root, &store.store, commit, "work.txt").await.unwrap(),
            format!("{round}\n").as_bytes()
        );
        let only = read(root, &store.store, commit, &format!("{round}.txt")).await;
        assert_eq!(only.unwrap(), format!("only in {round}\n").as_bytes());
        assert_eq!(
            read(root, &store.store, commit, "base.txt").await.unwrap(),
            b"one\ntwo\nthree\n"
        );
    }
    assert!(!git_says(
        root,
        Some(store.store.dir()),
        &["cat-file", "-e", &taken[1]]
    ));
    assert!(
        read(root, &store.store, &taken[1], "second.txt")
            .await
            .is_none()
    );
    // Nothing left to drop the second time.
    assert_eq!(
        snapshot::drop(root, &store.store, [&taken[0], &taken[2]])
            .await
            .unwrap(),
        0
    );
    let fsck = store
        .store
        .reading(
            root,
            &[
                "fsck",
                "--connectivity-only",
                "--no-dangling",
                &taken[0],
                &taken[2],
            ],
        )
        .output()
        .await
        .unwrap();
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// The files under `dir` that are packs or parts of one: what a fetch
/// leaves behind.
fn packs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("pack-"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A partial clone, made over `file://` from a bare repository that serves
/// filtered fetches and any object asked for by id, as a hosting service
/// does: only the checkout's blobs are fetched, and git fetches any other
/// object it finds it lacks. With the store as git's only object directory
/// every object of the repository looks lacking, so without a guard a
/// snapshot would fetch: a link whose times changed is compared by reading
/// its blob, and dropping asks after a kept commit the store never held.
/// Taking, asking whether the tree moved and dropping fetch nothing, into
/// the store or the repository.
#[tokio::test]
async fn in_a_partial_clone_nothing_is_fetched() {
    let origin = repository();
    let work = origin.path();
    for round in ["first", "second"] {
        write(work, "base.txt", &format!("{round}\n"));
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(work.join("link"));
            std::os::unix::fs::symlink(format!("{round}-target"), work.join("link")).unwrap();
        }
        git(work, &["add", "--all"]);
        git(work, &["commit", "-q", "-m", round]);
    }
    let served = tempfile::tempdir().unwrap();
    let bare = served.path().join("origin.git");
    git(
        served.path(),
        &[
            "clone",
            "-q",
            "--bare",
            &work.to_string_lossy(),
            "origin.git",
        ],
    );
    git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    git(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    // A `file://` URL, since a plain path clones locally and ignores the
    // filter.
    let path = bare.to_string_lossy().replace('\\', "/");
    let url = match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    };
    git(
        served.path(),
        &["clone", "-q", "--filter=blob:none", &url, "clone"],
    );
    let root = served.path().join("clone");
    let root = root.as_path();
    assert_eq!(
        git(root, &["config", "remote.origin.promisor"]),
        "true",
        "the clone is a partial one"
    );
    write(root, "base.txt", "edited\n");
    write(root, "new.txt", "new\n");
    // The same link made again: its times and inode differ from the
    // index's, so git compares it by its blob.
    #[cfg(unix)]
    {
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("second-target", root.join("link")).unwrap();
    }
    std::thread::sleep(Duration::from_millis(1100));

    let store = store();
    let before = manifest(root);
    let taken = take(root, &store.store).await;
    assert!(
        !snapshot::moved(root, &store.store, &LIMITS, &taken.commit, &taken.omitted)
            .await
            .unwrap()
    );
    let head = git(root, &["rev-parse", "HEAD"]);
    snapshot::drop(root, &store.store, [&taken.commit, &head])
        .await
        .unwrap();
    assert_eq!(packs(store.store.dir()), Vec::<PathBuf>::new());
    unchanged(&before, root, "a snapshot in a partial clone");
    assert_eq!(
        read(root, &store.store, &taken.commit, "base.txt")
            .await
            .unwrap(),
        b"edited\n"
    );
    assert_eq!(
        read(root, &store.store, &taken.commit, "new.txt")
            .await
            .unwrap(),
        b"new\n"
    );
}

/// Mid-merge, the snapshot says a merge is under way and holds the
/// conflicted file with its markers, as the working tree does.
#[tokio::test]
async fn mid_merge_the_snapshot_holds_the_markers() {
    let repo = repository();
    let root = repo.path();
    git(root, &["switch", "-q", "-c", "side"]);
    write(root, "base.txt", "side\n");
    git(root, &["commit", "-q", "-am", "side"]);
    git(root, &["switch", "-q", "main"]);
    write(root, "base.txt", "main\n");
    git(root, &["commit", "-q", "-am", "main"]);
    let merged = Command::new("git")
        .args(["merge", "side"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!merged.status.success());

    let store = store();
    let snapshot = take(root, &store.store).await;
    assert!(snapshot.merging);
    let text = read(root, &store.store, &snapshot.commit, "base.txt")
        .await
        .unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("<<<<<<<") && text.contains(">>>>>>>"),
        "{text}"
    );
    assert_eq!(
        text,
        std::fs::read_to_string(root.join("base.txt")).unwrap()
    );
    assert!(
        !snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted
        )
        .await
        .unwrap()
    );
}

/// Mid-rebase, stopped at a conflict, the snapshot says so too.
#[tokio::test]
async fn mid_rebase_the_snapshot_says_a_merge_is_under_way() {
    let repo = repository();
    let root = repo.path();
    git(root, &["switch", "-q", "-c", "side"]);
    write(root, "base.txt", "side\n");
    git(root, &["commit", "-q", "-am", "side"]);
    git(root, &["switch", "-q", "main"]);
    write(root, "base.txt", "main\n");
    git(root, &["commit", "-q", "-am", "main"]);
    git(root, &["switch", "-q", "side"]);
    let rebased = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
            "rebase",
            "main",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!rebased.status.success());

    let store = store();
    let snapshot = take(root, &store.store).await;
    assert!(snapshot.merging);
    let text = read(root, &store.store, &snapshot.commit, "base.txt")
        .await
        .unwrap();
    assert!(String::from_utf8(text).unwrap().contains("<<<<<<<"));
}

/// A link is stored as a link, its target as written, whether or not
/// anything is there.
#[cfg(unix)]
#[tokio::test]
async fn a_link_is_stored_as_a_link() {
    let repo = repository();
    let root = repo.path();
    std::os::unix::fs::symlink("../outside/secret", root.join("dangling")).unwrap();
    std::os::unix::fs::symlink("base.txt", root.join("to-base")).unwrap();
    let store = store();
    let snapshot = take(root, &store.store).await;
    let files = tree(root, &store.store, &snapshot.commit).await;
    assert_eq!(files["dangling"].0, "120000");
    assert_eq!(files["to-base"].0, "120000");
    assert_eq!(
        read(root, &store.store, &snapshot.commit, "dangling")
            .await
            .unwrap(),
        b"../outside/secret"
    );
    assert!(
        !snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted
        )
        .await
        .unwrap()
    );
}

/// A repository inside the working tree is stored by its commit; one with
/// no commit yet cannot be, and is left out rather than failing the rest.
#[tokio::test]
async fn an_embedded_repository_without_a_commit_is_left_out() {
    let repo = repository();
    let root = repo.path();
    let committed = root.join("committed");
    std::fs::create_dir(&committed).unwrap();
    git(&committed, &["init", "-q"]);
    write(&committed, "inner.txt", "inner\n");
    git(&committed, &["add", "."]);
    git(&committed, &["commit", "-q", "-m", "inner"]);
    let empty = root.join("empty");
    std::fs::create_dir(&empty).unwrap();
    git(&empty, &["init", "-q"]);
    write(&empty, "inner.txt", "inner\n");
    write(root, "beside.txt", "beside\n");

    let store = store();
    let snapshot = take(root, &store.store).await;
    let files = tree(root, &store.store, &snapshot.commit).await;
    assert_eq!(paths(&files), ["base.txt", "beside.txt", "committed"]);
    assert_eq!(files["committed"].0, "160000");
    assert!(
        !snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted
        )
        .await
        .unwrap()
    );
}

/// A folder outside any repository has no snapshot.
#[tokio::test]
async fn a_folder_outside_a_repository_has_no_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = store();
    let error = snapshot::take(dir.path(), &store.store, &LIMITS, SystemTime::now())
        .await
        .unwrap_err();
    assert!(matches!(error, GitError::NotARepository), "{error:?}");
    let error = snapshot::moved(dir.path(), &store.store, &LIMITS, "HEAD", &[])
        .await
        .unwrap_err();
    assert!(matches!(error, GitError::NotARepository), "{error:?}");
}

/// The pointer LFS commits for `content`, as its spec writes one.
fn lfs_pointer(sha256: &str, size: usize) -> String {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{sha256}\nsize {size}\n")
}

/// A repository set up for LFS, as `git lfs install` and `git lfs track`
/// leave one, where git-lfs is not installed: every filter is a program
/// that leaves a mark and fails, and each is required, so a filter run
/// anywhere fails the snapshot or leaves the mark. A changed tracked file
/// and a new one are stored as the pointers LFS would commit, worked out
/// from their content; an unchanged one keeps the pointer committed; and
/// nothing appears in `.git/lfs` or anywhere else in `.git`.
#[tokio::test]
async fn a_changed_lfs_file_is_stored_as_its_pointer_without_lfs() {
    let repo = repository();
    let root = repo.path();
    write(
        root,
        ".gitattributes",
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    );
    // Committed as LFS commits them: the pointer in the repository, the
    // content in the working tree.
    let commit_pointer = |path: &str, content: &str, sha256: &str| {
        let pointer = lfs_pointer(sha256, content.len());
        let pointer_file = root.join(".git/pointer-scratch");
        std::fs::write(&pointer_file, &pointer).unwrap();
        let id = git(
            root,
            &[
                "hash-object",
                "-w",
                "--no-filters",
                pointer_file.to_str().unwrap(),
            ],
        );
        std::fs::remove_file(pointer_file).unwrap();
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{id},{path}"),
            ],
        );
        write(root, path, content);
        id
    };
    let v1 = "88eaddfe1f4c7f1f59dd7b69ec68696897f48f92e00e4f49b564818268b33e80";
    commit_pointer("model.bin", "weights v1\n", v1);
    let other = commit_pointer("other.bin", "weights v1\n", v1);
    git(root, &["update-index", "--add", "--", ".gitattributes"]);
    git(root, &["commit", "-q", "-m", "lfs"]);
    let mark = root.join("filter-ran");
    let filter = format!("touch {} && false", mark.display());
    for key in ["clean", "smudge", "process"] {
        git(root, &["config", &format!("filter.lfs.{key}"), &filter]);
    }
    git(root, &["config", "filter.lfs.required", "true"]);
    write(root, "model.bin", "weights v2\n");
    write(root, "fresh.bin", "fresh weights\n");
    write(root, "empty.bin", "");
    std::thread::sleep(Duration::from_millis(1100));

    let store = store();
    let before = manifest(root);
    let snapshot = take(root, &store.store).await;
    unchanged(&before, root, "a snapshot of LFS files");
    assert!(!mark.exists(), "an LFS filter ran");
    assert!(!root.join(".git/lfs").exists());

    let read_text = async |path: &str| {
        String::from_utf8(
            read(root, &store.store, &snapshot.commit, path)
                .await
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        read_text("model.bin").await,
        lfs_pointer(
            "1b6249c6ae300e3b78ae47b73ee855f4d6d229d70fe056774a0e96f3964d5f76",
            11
        )
    );
    assert_eq!(
        read_text("fresh.bin").await,
        lfs_pointer(
            "bd6b5dda1768fa6166e3479b4129168fb75d600fc8a297a83b63122f8b976044",
            14
        )
    );
    assert_eq!(read_text("empty.bin").await, "");
    assert_eq!(
        tree(root, &store.store, &snapshot.commit).await["other.bin"].1,
        other
    );

    let moved = || {
        snapshot::moved(
            root,
            &store.store,
            &LIMITS,
            &snapshot.commit,
            &snapshot.omitted,
        )
    };
    assert!(!moved().await.unwrap());
    assert!(!mark.exists(), "an LFS filter ran");
    // Made executable, its content as it was: its pointer is the
    // snapshot's, its mode is not, and a snapshot now would store the new
    // mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let set = |mode: u32| {
            std::fs::set_permissions(
                root.join("model.bin"),
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap();
        };
        set(0o755);
        assert!(moved().await.unwrap(), "a mode changed alone");
        set(0o644);
        assert!(!moved().await.unwrap());
    }
    write(root, "model.bin", "weights v3\n");
    assert!(moved().await.unwrap());
    unchanged(&before, root, "asking whether LFS files moved");
}
