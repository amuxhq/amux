//! Diff: the host compares an agent's folder through git-facts and answers
//! with the changed files; it builds the patch and stores it as the agent's
//! blob only when asked. The branch comparison runs from where the branch
//! left its base to the working tree, and an exited agent's folder is
//! compared as it is now. A diff that disagrees with the totals on the
//! agent's row corrects the row, and one agent's turn end has the others in
//! its checkout read their folders' facts again.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use node::{BlobError, PATCH_MIME};
use support::synthetic::*;
use support::*;
use wire::{
    ChangeTotals, DiffBase, DiffFile, DiffFileChange, ErrorCode, GetDiffRequest, Lifecycle,
    diff_base,
};

use crate::support;

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

fn request(agent: &SyntheticAgent, base: diff_base::Base, with_patch: bool) -> GetDiffRequest {
    GetDiffRequest {
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
    store.put_agent(&row).unwrap();
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
        .get_diff(request(&agent, working_tree(), false))
        .await
        .unwrap();
    println!("uncommitted: {uncommitted:#?}");
    assert_eq!(uncommitted.patch, None);
    assert_eq!(uncommitted.head, repo.head);
    assert_eq!(uncommitted.merge_base, None);
    assert_eq!(
        uncommitted.own_commits,
        Some(1),
        "the branch's one commit, which no other ref holds"
    );
    assert_eq!(
        uncommitted.files,
        vec![
            file("kept.txt", 1, 0, DiffFileChange::Changed),
            file("new.txt", 1, 0, DiffFileChange::Created),
        ]
    );

    let on_branch = runtime
        .get_diff(request(
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
        .get_diff(request(&agent, working_tree(), true))
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
        .get_diff(request(
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
        .get_diff(request(&agent, working_tree(), false))
        .await
        .unwrap_err();
    println!("outside a repository: {outside}");
    assert!(matches!(outside, BlobError::Git(_)));
    assert_eq!(outside.to_wire().code, ErrorCode::FailedPrecondition as i32);

    repository(&install.work);
    let unknown = runtime
        .get_diff(request(
            &agent,
            diff_base::Base::Branch("nowhere".into()),
            false,
        ))
        .await
        .unwrap_err();
    println!("an unknown base: {unknown}");
    assert_eq!(unknown.to_wire().code, ErrorCode::FailedPrecondition as i32);

    let no_base = runtime
        .get_diff(GetDiffRequest {
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

fn totals(files: u32, added: u32, removed: u32) -> ChangeTotals {
    ChangeTotals {
        files,
        added,
        removed,
    }
}

/// A turn's end, its snapshot carrying `git` as the agent's read at that
/// end found the folder.
fn turn_ended_with(git: Option<wire::Git>) -> wire::Step {
    let mut step = snapshot(wire::Phase::Idle, &[], 4_000);
    step.snapshot.as_mut().unwrap().git = git;
    step.turn_end = Some(wire::TurnEnd {
        turn_id: 1,
        outcome: wire::TurnOutcome::Completed as i32,
        ..wire::TurnEnd::default()
    });
    step
}

/// What the agent's row says of its folder now.
async fn row_git(runtime: &node::ProfileRuntime, agent: &SyntheticAgent) -> Option<wire::Git> {
    runtime.agent(agent.id).await.unwrap().git
}

/// The row's totals are the agent's own read at its last turn end, and a
/// commit made after it, by a person or another agent in the checkout,
/// leaves them stale. A diff that shows them wrong corrects the row at once,
/// for whichever comparison it made, and asks the agent to read its facts
/// again so its next snapshot agrees.
#[tokio::test(flavor = "multi_thread")]
async fn a_diff_that_disagrees_with_the_row_corrects_it() {
    let install = Install::new();
    repository(&install.work);
    let mut agent = SyntheticAgent::new(&install, "coder", SEGMENTS);
    agent.register(&install);
    agent.go_live();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let read_at_turn_end = wire::Git {
        branch: Some("feature".into()),
        base_branch: Some("main".into()),
        uncommitted: Some(totals(2, 2, 0)),
        on_branch: Some(totals(2, 3, 0)),
        remote: None,
    };
    agent.append(&turn_ended_with(Some(read_at_turn_end.clone())));
    agent.nudge().await;
    until("the turn end's facts on the row", || async {
        let git = row_git(&runtime, &agent).await;
        (git.as_ref() == Some(&read_at_turn_end))
            .then_some(())
            .ok_or_else(|| format!("{git:?}"))
    })
    .await
    .unwrap();

    // Someone else commits everything the agent's row counts.
    git(&install.work, &["add", "-A"]);
    git(&install.work, &["commit", "-q", "-m", "someone else's"]);

    let tree = runtime
        .get_diff(request(&agent, working_tree(), true))
        .await
        .unwrap();
    assert!(tree.files.is_empty(), "{tree:#?}");
    let git_now = row_git(&runtime, &agent).await.unwrap();
    assert_eq!(
        git_now.uncommitted,
        Some(totals(0, 0, 0)),
        "the row agrees with the diff once it is answered"
    );
    assert_eq!(
        git_now.on_branch,
        Some(totals(2, 3, 0)),
        "a comparison the diff did not make is left as it was"
    );

    let branch = runtime
        .get_diff(request(
            &agent,
            diff_base::Base::Branch("main".into()),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(branch.files.len(), 2, "{branch:#?}");
    // The branch now holds both commits: kept.txt gained two lines on it
    // and new.txt one, the same totals counted another way.
    assert_eq!(
        row_git(&runtime, &agent).await.unwrap().on_branch,
        Some(totals(2, 3, 0)),
        "a diff that agrees changes nothing"
    );
    until(
        "the disagreeing diff to ask the agent to read again",
        || async {
            let asked = agent.git_reads_asked();
            (asked == 1).then_some(()).ok_or_else(|| format!("{asked}"))
        },
    )
    .await
    .unwrap();

    let other = runtime
        .get_diff(request(
            &agent,
            diff_base::Base::Branch("feature".into()),
            false,
        ))
        .await
        .unwrap();
    assert!(other.files.is_empty());
    assert_eq!(
        row_git(&runtime, &agent).await.unwrap(),
        wire::Git {
            uncommitted: Some(totals(0, 0, 0)),
            ..read_at_turn_end
        },
        "a branch other than the row's base is not a comparison the row counts"
    );
    assert_eq!(agent.git_reads_asked(), 1);

    agent.die().await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Agents sharing a checkout all count it against HEAD, so one agent's turn
/// end, which may have committed or reverted, has every other live agent
/// whose folder is the same, or one inside the other, read its facts again.
/// An agent in another folder is left alone, and so is the one whose turn
/// ended: it reads at its own turn end.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_end_has_the_others_in_its_checkout_read_their_facts_again() {
    let install = Install::new();
    repository(&install.work);
    let inside = install.work.join("crates");
    std::fs::create_dir_all(&inside).unwrap();
    let elsewhere = install.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut ending = SyntheticAgent::new(&install, "ending", SEGMENTS);
    let mut beside = SyntheticAgent::new(&install, "beside", SEGMENTS);
    let mut nested = SyntheticAgent::new(&install, "nested", SEGMENTS).in_folder(inside);
    let mut apart = SyntheticAgent::new(&install, "apart", SEGMENTS).in_folder(elsewhere);
    let agents = [&mut ending, &mut beside, &mut nested, &mut apart];
    for agent in agents {
        agent.register(&install);
        agent.go_live();
    }
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    // An idle snapshot from each, ingested only after its Hello on the same
    // connection: once every row shows it, the daemon knows each is live.
    for agent in [&mut ending, &mut beside, &mut nested, &mut apart] {
        agent.append(&snapshot(wire::Phase::Idle, &[], 2_000));
        agent.nudge().await;
    }
    for agent in [&ending, &beside, &nested, &apart] {
        until("each agent's idle snapshot", || async {
            let phase = runtime.agent(agent.id).await.unwrap().phase;
            (phase == wire::Phase::Idle as i32)
                .then_some(())
                .ok_or_else(|| format!("{phase}"))
        })
        .await
        .unwrap();
    }
    assert_eq!(beside.git_reads_asked(), 0, "nothing has ended yet");

    ending.append(&turn_ended_with(None));
    ending.nudge().await;
    until("the agents in the checkout to be asked", || async {
        let asked = (beside.git_reads_asked(), nested.git_reads_asked());
        (asked == (1, 1))
            .then_some(())
            .ok_or_else(|| format!("{asked:?}"))
    })
    .await
    .unwrap();
    holds_for(
        "the agent elsewhere and the one that ended to be left alone",
        Duration::from_millis(300),
        || async { apart.git_reads_asked() == 0 && ending.git_reads_asked() == 0 },
    )
    .await
    .unwrap();

    for agent in [&mut ending, &mut beside, &mut nested, &mut apart] {
        agent.die().await;
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
