//! Diff: the host compares an agent's folder through git-facts and answers
//! with the changed files; it builds the patch and stores it as the agent's
//! blob only when asked. The branch comparison runs from where the branch
//! left its base to the working tree, and an exited agent's folder is
//! compared as it is now.

mod support;

use std::path::Path;
use std::process::Command;

use node::{BlobError, PATCH_MIME};
use support::synthetic::*;
use support::*;
use wire::{DiffBase, DiffFile, DiffFileChange, DiffRequest, ErrorCode, Lifecycle, diff_base};

const SEGMENTS: u64 = 1 << 20;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Every file under `dir`, recursively; empty when it does not exist.
fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path.to_string_lossy().into_owned());
        }
    }
    found.sort();
    found
}

/// The work folder on branch `feature`, which left `main` at `fork`: one
/// commit on the branch, then an uncommitted change and an untracked file.
struct Repository {
    head: String,
    fork: String,
}

fn repository(work: &Path) -> Repository {
    git(work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("kept.txt"), "one\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "first"]);
    let fork = git(work, &["rev-parse", "HEAD"]);
    git(work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("kept.txt"), "one\ntwo\n").unwrap();
    git(work, &["commit", "-q", "-am", "second"]);
    std::fs::write(work.join("kept.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(work.join("new.txt"), "untracked\n").unwrap();
    let head = git(work, &["rev-parse", "HEAD"]);
    Repository { head, fork }
}

fn request(agent: &SyntheticAgent, base: diff_base::Base, with_patch: bool) -> DiffRequest {
    DiffRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        base: Some(DiffBase { base: Some(base) }),
        with_patch,
    }
}

fn working_tree() -> diff_base::Base {
    diff_base::Base::Head(wire::Empty {})
}

fn file(path: &str, added: u32, removed: u32, change: DiffFileChange) -> DiffFile {
    DiffFile {
        path: path.to_owned(),
        added,
        removed,
        change: change as i32,
        binary: false,
    }
}

/// An agent that has exited, its folder still there.
fn exited(install: &Install) -> SyntheticAgent {
    let agent = SyntheticAgent::new(install, "coder", SEGMENTS);
    let mut row = agent.row(install);
    row.lifecycle = Lifecycle::Exited as i32;
    row.exit_cause = wire::ExitCause::Finished as i32;
    let mut store = store::Sqlite::open(
        &install.profile_dir().join(node::STORE),
        host(install).as_bytes().to_vec(),
    )
    .unwrap();
    store::Store::put_agent(&mut store, &row).unwrap();
    agent
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_patch_the_host_lists_the_files_and_stores_nothing() {
    let install = Install::new();
    let repo = repository(&install.work);
    let agent = exited(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let before = files_under(&agent.dir);

    let uncommitted = runtime
        .diff(request(&agent, working_tree(), false))
        .await
        .unwrap();
    println!("uncommitted: {uncommitted:#?}");
    assert_eq!(uncommitted.patch, None);
    assert_eq!(uncommitted.head, repo.head);
    assert_eq!(uncommitted.merge_base, None);
    assert_eq!(
        uncommitted.files,
        vec![
            file("kept.txt", 1, 0, DiffFileChange::Changed),
            file("new.txt", 1, 0, DiffFileChange::Created),
        ]
    );

    let on_branch = runtime
        .diff(request(
            &agent,
            diff_base::Base::Branch("main".into()),
            false,
        ))
        .await
        .unwrap();
    println!("on the branch: {on_branch:#?}");
    assert_eq!(on_branch.patch, None);
    assert_eq!(on_branch.merge_base.as_deref(), Some(repo.fork.as_str()));
    assert_eq!(
        on_branch.files,
        vec![
            file("kept.txt", 2, 0, DiffFileChange::Changed),
            file("new.txt", 1, 0, DiffFileChange::Created),
        ],
        "the branch's commit and its uncommitted work"
    );

    assert_eq!(
        files_under(&agent.dir),
        before,
        "nothing is built or stored without a patch"
    );
    assert!(files_under(&agent.dir.join(agent_dir::BLOBS)).is_empty());
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_patch_the_host_stores_it_as_the_agents_blob() {
    let install = Install::new();
    let work = install.work.clone();
    let repo = repository(&work);
    let agent = exited(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let blob_text = |hash: &[u8]| {
        std::fs::read_to_string(agent.dir.join(agent_dir::BLOBS).join(hex(hash))).unwrap()
    };

    let tree = runtime
        .diff(request(&agent, working_tree(), true))
        .await
        .unwrap();
    assert_eq!(tree.head, repo.head);
    assert_eq!(tree.files.len(), 2);
    let patch = tree.patch.unwrap();
    assert_eq!(patch.mime, PATCH_MIME);
    assert_eq!(patch.name, "working-tree.diff");
    let text = blob_text(&patch.hash);
    println!("working tree:\n{text}");
    assert!(text.contains("+three"), "the uncommitted change");
    assert!(
        text.contains("new.txt") && text.contains("+untracked"),
        "untracked files"
    );
    assert!(!text.contains("+two"), "committed changes are HEAD's");
    assert!(
        text.lines()
            .any(|line| line.starts_with("index ") && line.len() > 80),
        "index lines carry full object ids"
    );
    assert_eq!(
        git(&work, &["status", "--porcelain"]),
        "M kept.txt\n?? new.txt",
        "the person's own index is untouched"
    );

    let branch = runtime
        .diff(request(
            &agent,
            diff_base::Base::Branch("main".into()),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(branch.merge_base.as_deref(), Some(repo.fork.as_str()));
    let patch = branch.patch.unwrap();
    assert_eq!(patch.name, "main.diff");
    let text = blob_text(&patch.hash);
    println!("against main:\n{text}");
    assert!(
        text.contains("+two") && text.contains("+three") && text.contains("+untracked"),
        "from where the branch left main to the working tree"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_git_cannot_make_says_why() {
    let install = Install::new();
    let agent = exited(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let outside = runtime
        .diff(request(&agent, working_tree(), false))
        .await
        .unwrap_err();
    println!("outside a repository: {outside}");
    assert!(matches!(outside, BlobError::Git(_)));
    assert_eq!(outside.to_wire().code, ErrorCode::FailedPrecondition as i32);

    repository(&install.work);
    let unknown = runtime
        .diff(request(
            &agent,
            diff_base::Base::Branch("nowhere".into()),
            false,
        ))
        .await
        .unwrap_err();
    println!("an unknown base: {unknown}");
    assert_eq!(unknown.to_wire().code, ErrorCode::FailedPrecondition as i32);

    let no_base = runtime
        .diff(DiffRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            base: None,
            with_patch: false,
        })
        .await
        .unwrap_err();
    assert_eq!(no_base.to_wire().code, ErrorCode::InvalidArgument as i32);
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
