//! A person's review of an agent's working tree, as the agent's host
//! answers Open and Edit in memory. Opening takes a snapshot on a first look
//! and when the tree has moved with no drafts waiting; with drafts it stays
//! on their snapshot and says the tree moved, until a refresh moves them on.
//! Comments are placed on the snapshot by one diff from the commit that
//! holds their lines: untouched lines move, a sent comment on edited lines
//! is outdated and a draft becomes a comment on its file. Edits are all or
//! none, and each one tells every client through the agent's row.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use node::ProfileRuntime;
use node::harness::{Calling, Served, served_as};
use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::experimental::review_placement::At;
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor, ReviewEdit, ReviewEditRequest, ReviewLineRange, ReviewOpenRequest,
    ReviewPutComment, ReviewRowEntry, ReviewSummary, ReviewerComment, review_edit, reviews1_client,
};
use wire::{Empty, Lifecycle};

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

/// A daemon running the reviews experiment.
fn launch() -> node::Launch {
    let mut launch = quiet_launch();
    launch.experiments = settings::experiments::Enabled::of(&[&settings::experiments::REVIEWS]);
    launch
}

/// An agent that has exited, working in `cwd`, whose spec records
/// `base_branch` as the branch its worktree was made from.
fn agent_in(install: &Install, cwd: &Path, base_branch: Option<&str>) -> SyntheticAgent {
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
        base_branch: base_branch.map(str::to_owned),
        ..wire::AgentSpec::default()
    };
    std::fs::write(
        agent.dir.join("spec.1"),
        prost::Message::encode_to_vec(&spec),
    )
    .unwrap();
    agent
}

/// Thirty numbered lines, with `edits` replacing some by number and
/// `above` lines put before the first.
fn numbered(above: usize, edits: &[(usize, &str)]) -> String {
    let mut text = "new\n".repeat(above);
    for n in 1..=30 {
        match edits.iter().find(|(at, _)| *at == n) {
            Some((_, line)) => text.push_str(line),
            None => text.push_str(&format!("line {n}")),
        }
        text.push('\n');
    }
    text
}

/// A repository on `feature`, made from `main`, whose branch changed
/// `notes.txt` from thirty numbered lines.
fn feature_branch(work: &Path) {
    git(work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("notes.txt"), numbered(0, &[])).unwrap();
    std::fs::write(work.join("base.txt"), "base\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "first"]);
    git(work, &["switch", "-q", "-c", "feature"]);
    std::fs::write(work.join("notes.txt"), numbered(0, &[(25, "feature")])).unwrap();
    git(work, &["commit", "-q", "-am", "feature"]);
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
    }

    async fn open_with(
        &self,
        agent: &SyntheticAgent,
        refresh: bool,
        base_branch: Option<&str>,
    ) -> Result<ReviewSummary, tonic::Status> {
        let request = ReviewOpenRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            base_branch: base_branch.map(str::to_owned),
            refresh,
        };
        Ok(self.client().open(request).await?.into_inner())
    }

    async fn open(&self, agent: &SyntheticAgent) -> ReviewSummary {
        self.open_with(agent, false, None).await.unwrap()
    }

    async fn refresh(&self, agent: &SyntheticAgent) -> ReviewSummary {
        self.open_with(agent, true, None).await.unwrap()
    }

    async fn try_edit(
        &self,
        agent: &SyntheticAgent,
        edits: Vec<review_edit::Of>,
    ) -> Result<ReviewSummary, tonic::Status> {
        let request = ReviewEditRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            edits: edits
                .into_iter()
                .map(|of| ReviewEdit { of: Some(of) })
                .collect(),
        };
        Ok(self.client().edit(request).await?.into_inner())
    }

    async fn edit(&self, agent: &SyntheticAgent, edits: Vec<review_edit::Of>) -> ReviewSummary {
        self.try_edit(agent, edits).await.unwrap()
    }

    /// The review's entry on the agent's row, as every client reads it.
    async fn entry(&self, agent: &SyntheticAgent) -> Option<ReviewRowEntry> {
        let row = self.runtime.agent(agent.id).await.unwrap();
        REVIEW_ROW.get(&row.experimental)
    }

    /// Marks the comments `ids` of `summary` sent in the next round, as
    /// the host does when it accepts the message sending them.
    fn send(&self, agent: &SyntheticAgent, summary: &ReviewSummary, ids: &[&str]) {
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
                        sent_ms: 1,
                    },
                )?;
                for comment in &review.comments {
                    if ids.iter().any(|id| id.as_bytes() == comment.id) {
                        writer.put_comment(
                            &review.id,
                            &store::reviews::Comment {
                                round: Some(n),
                                ..comment.clone()
                            },
                        )?;
                    }
                }
                Ok(())
            })
            .unwrap();
    }
}

fn put(id: &str, commit: &str, path: &str, start: u32, end: u32) -> review_edit::Of {
    review_edit::Of::Put(ReviewPutComment {
        id: id.as_bytes().to_vec(),
        anchor: Some(ReviewAnchor {
            commit: commit.to_owned(),
            path: path.to_owned(),
            lines: Some(ReviewLineRange { start, end }),
        }),
        text: format!("about {id}"),
    })
}

fn comment<'a>(summary: &'a ReviewSummary, id: &str) -> &'a ReviewerComment {
    summary
        .comments
        .iter()
        .find(|comment| comment.id == id.as_bytes())
        .unwrap_or_else(|| panic!("{id} in {:#?}", summary.comments))
}

fn at(summary: &ReviewSummary, id: &str) -> At {
    comment(summary, id)
        .placement
        .as_ref()
        .and_then(|placement| placement.at)
        .expect("every comment is placed")
}

fn near(summary: &ReviewSummary, id: &str) -> bool {
    comment(summary, id)
        .placement
        .as_ref()
        .is_some_and(|placement| placement.changed_near)
}

fn lines(start: u32, end: u32) -> At {
    At::Lines(ReviewLineRange { start, end })
}

fn ids(summary: &ReviewSummary) -> Vec<String> {
    summary
        .comments
        .iter()
        .map(|comment| String::from_utf8(comment.id.clone()).unwrap())
        .collect()
}

fn paths(summary: &ReviewSummary) -> Vec<&str> {
    summary
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// Whether git, reading the snapshot store `objects` beside the repository
/// at `work`, finds `commit`.
fn readable(work: &Path, objects: &str, commit: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects)
        .current_dir(work)
        .status()
        .unwrap()
        .success()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_look_snapshots_and_lists_and_a_second_with_nothing_changed_takes_none() {
    let install = Install::new();
    feature_branch(&install.work);
    std::fs::write(install.work.join("todo.txt"), "untracked\n").unwrap();
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;

    let first = host.open(&agent).await;
    assert!(!first.id.is_empty());
    assert_eq!(first.base_branch, "main");
    assert_eq!(
        first.base_commit,
        git(&host.install.work, &["rev-parse", "main"])
    );
    assert_eq!(
        first.began_at,
        git(&host.install.work, &["rev-parse", "HEAD"])
    );
    assert_eq!(paths(&first), ["notes.txt", "todo.txt"]);
    assert_eq!(first.files[1].status, wire::FileStatus::Created as i32);
    assert_eq!(
        PathBuf::from(&first.root).canonicalize().unwrap(),
        host.install.work.canonicalize().unwrap()
    );
    assert!(readable(
        &host.install.work,
        &first.objects,
        &first.snapshot
    ));
    assert!(!first.stale && first.rounds.is_empty() && first.file_states.is_empty());
    // The snapshot is the store's, not the repository's.
    let in_repository = Command::new("git")
        .args(["cat-file", "-e", &first.snapshot])
        .current_dir(&host.install.work)
        .status()
        .unwrap();
    assert!(!in_repository.success());

    let counter = host.entry(&agent).await.unwrap().counter;
    let again = host.open(&agent).await;
    assert_eq!(again.snapshot, first.snapshot);
    assert_eq!(again.id, first.id);
    assert_eq!(host.entry(&agent).await.unwrap().counter, counter);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_drafts_an_open_stays_on_their_snapshot_and_a_refresh_moves_them() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    let snapshot = first.snapshot.clone();
    host.edit(
        &agent,
        vec![
            put("kept", &snapshot, "notes.txt", 20, 21),
            put("edited", &snapshot, "notes.txt", 5, 5),
            put("whole", &snapshot, "notes.txt", 0, 0),
        ],
    )
    .await;

    // Two lines put in at the top, and line 5 edited.
    std::fs::write(
        host.install.work.join("notes.txt"),
        numbered(2, &[(5, "five"), (25, "feature")]),
    )
    .unwrap();
    let waiting = host.open(&agent).await;
    assert_eq!(waiting.snapshot, snapshot);
    assert!(waiting.stale);
    assert_eq!(waiting.moved_drafts, 0);
    assert!(host.entry(&agent).await.unwrap().moved);

    let refreshed = host.refresh(&agent).await;
    assert_ne!(refreshed.snapshot, snapshot);
    assert!(!refreshed.stale);
    assert_eq!(refreshed.moved_drafts, 3);
    assert_eq!(at(&refreshed, "kept"), lines(22, 23));
    assert_eq!(at(&refreshed, "edited"), At::File(Empty {}));
    assert_eq!(at(&refreshed, "whole"), At::File(Empty {}));
    for id in ["kept", "edited", "whole"] {
        let anchor = comment(&refreshed, id).anchor.as_ref().unwrap();
        assert_eq!(anchor.commit, refreshed.snapshot, "{id}");
    }
    let edited = comment(&refreshed, "edited");
    assert_eq!(
        edited.anchor.as_ref().unwrap().lines,
        Some(ReviewLineRange::default())
    );
    assert_eq!(edited.text, "about edited");
    assert!(!host.entry(&agent).await.unwrap().moved);
    // The drafts left the old snapshot, which nothing names now.
    assert!(!readable(&host.install.work, &refreshed.objects, &snapshot));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_comments_move_go_outdated_and_say_when_code_near_them_changed() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    let snapshot = first.snapshot.clone();
    let written = host
        .edit(
            &agent,
            vec![
                put("untouched", &snapshot, "notes.txt", 20, 21),
                put("edited", &snapshot, "notes.txt", 5, 5),
                put("near", &snapshot, "notes.txt", 12, 12),
                put("draft", &snapshot, "notes.txt", 28, 28),
            ],
        )
        .await;
    host.send(&agent, &written, &["untouched", "edited", "near"]);

    std::fs::write(
        host.install.work.join("notes.txt"),
        numbered(2, &[(5, "five"), (10, "ten"), (25, "feature")]),
    )
    .unwrap();
    // A draft waits, so a refresh takes the snapshot the comments are
    // placed on.
    let after = host.refresh(&agent).await;
    assert_ne!(after.snapshot, snapshot);
    assert_eq!(after.rounds.len(), 1);
    assert_eq!(after.rounds[0].snapshot, snapshot);
    assert_eq!(comment(&after, "untouched").round, Some(1));
    assert_eq!(at(&after, "untouched"), lines(22, 23));
    assert!(!near(&after, "untouched"));
    assert_eq!(at(&after, "edited"), At::Outdated(Empty {}));
    assert_eq!(at(&after, "near"), lines(14, 14));
    assert!(near(&after, "near"));
    assert_eq!(at(&after, "draft"), lines(30, 30));
    assert!(!near(&after, "draft"));
    // The sent comments' snapshot is kept for their round.
    assert!(readable(&host.install.work, &after.objects, &snapshot));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discarding_drafts_leaves_sent_comments_and_a_sent_one_can_be_deleted() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let snapshot = host.open(&agent).await.snapshot;
    let written = host
        .edit(
            &agent,
            vec![
                put("sent", &snapshot, "notes.txt", 3, 4),
                put("other", &snapshot, "notes.txt", 8, 8),
            ],
        )
        .await;
    host.send(&agent, &written, &["sent", "other"]);
    host.edit(&agent, vec![put("draft", &snapshot, "notes.txt", 9, 9)])
        .await;

    let discarded = host
        .edit(&agent, vec![review_edit::Of::DiscardDrafts(Empty {})])
        .await;
    assert_eq!(ids(&discarded), ["sent", "other"]);
    assert_eq!(host.entry(&agent).await.unwrap().drafts, 0);

    let deleted = host
        .edit(&agent, vec![review_edit::Of::Delete(b"sent".to_vec())])
        .await;
    assert_eq!(ids(&deleted), ["other"]);
    assert_eq!(deleted.rounds.len(), 1);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_over_makes_the_next_look_a_first_one_and_removes_the_store() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.edit(
        &agent,
        vec![put("draft", &first.snapshot, "notes.txt", 1, 1)],
    )
    .await;
    let store = agent.dir.join(agent_dir::REVIEW);
    assert!(store.is_dir());

    let over = host
        .edit(&agent, vec![review_edit::Of::StartOver(Empty {})])
        .await;
    assert!(over.id.is_empty() && over.comments.is_empty() && over.snapshot.is_empty());
    assert!(!store.exists());
    assert_eq!(host.entry(&agent).await, None);

    let again = host.open(&agent).await;
    assert_ne!(again.id, first.id);
    assert!(again.comments.is_empty() && again.rounds.is_empty());
    assert!(readable(
        &host.install.work,
        &again.objects,
        &again.snapshot
    ));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_agents_in_one_folder_each_get_their_own_review() {
    let install = Install::new();
    feature_branch(&install.work);
    let one = agent_in(&install, &install.work, Some("main"));
    let two = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&one).await;
    let second = host.open(&two).await;
    assert_ne!(first.id, second.id);
    assert_ne!(first.objects, second.objects);
    host.edit(&one, vec![put("mine", &first.snapshot, "notes.txt", 2, 2)])
        .await;
    assert!(host.open(&two).await.comments.is_empty());
    assert_eq!(ids(&host.open(&one).await), ["mine"]);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn on_the_base_branch_with_no_upstream_the_first_commits_stay_in_all_changes() {
    let install = Install::new();
    let work = &install.work;
    git(work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("notes.txt"), "one\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "first"]);
    let began = git(work, &["rev-parse", "HEAD"]);
    let agent = agent_in(&install, work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    assert_eq!(first.base_commit, began);
    assert!(first.files.is_empty());

    let work = &host.install.work;
    std::fs::write(work.join("notes.txt"), "one\ntwo\n").unwrap();
    git(work, &["commit", "-q", "-am", "the agent's"]);
    let later = host.open(&agent).await;
    assert_eq!(later.base_commit, began);
    assert_eq!(later.began_at, began);
    assert_eq!(paths(&later), ["notes.txt"]);
    // What changed alone falls back to HEAD there, and sees nothing.
    let changes = host.runtime.changes(agent.id, None).await.unwrap();
    assert!(changes.files.is_empty(), "{changes:?}");
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn since_the_last_round_after_a_merge_from_the_base_holds_only_the_agents_files() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;
    host.send(&agent, &first, &[]);

    let work = &host.install.work;
    git(work, &["switch", "-q", "main"]);
    std::fs::write(work.join("arrived.txt"), "from the base\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "on main"]);
    git(work, &["switch", "-q", "feature"]);
    git(work, &["merge", "-q", "--no-edit", "main"]);
    std::fs::write(
        work.join("notes.txt"),
        numbered(0, &[(3, "three"), (25, "feature")]),
    )
    .unwrap();

    let after = host.open(&agent).await;
    assert_ne!(after.snapshot, first.snapshot);
    assert_eq!(paths(&after), ["notes.txt"]);
    let states: Vec<_> = after
        .file_states
        .iter()
        .map(|state| (state.path.as_str(), state.changed_since_last, state.added))
        .collect();
    assert_eq!(states, [("notes.txt", true, 1)]);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_is_no_repository_is_a_failed_precondition() {
    let install = Install::new();
    let agent = agent_in(&install, &install.work, None);
    let host = Host::start(install).await;
    let refused = host.open_with(&agent, false, None).await.unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unkept_snapshots_are_dropped_and_named_ones_kept() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    let first = host.open(&agent).await;

    std::fs::write(host.install.work.join("a.txt"), "a\n").unwrap();
    let second = host.open(&agent).await;
    assert!(!readable(
        &host.install.work,
        &second.objects,
        &first.snapshot
    ));
    let written = host
        .edit(&agent, vec![put("sent", &second.snapshot, "a.txt", 1, 1)])
        .await;
    host.send(&agent, &written, &["sent"]);

    std::fs::write(host.install.work.join("b.txt"), "b\n").unwrap();
    let third = host.open(&agent).await;
    std::fs::write(host.install.work.join("c.txt"), "c\n").unwrap();
    let fourth = host.open(&agent).await;
    assert!(readable(
        &host.install.work,
        &fourth.objects,
        &second.snapshot
    ));
    assert!(!readable(
        &host.install.work,
        &fourth.objects,
        &third.snapshot
    ));
    assert!(readable(
        &host.install.work,
        &fourth.objects,
        &fourth.snapshot
    ));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_opens_at_once_take_one_snapshot() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    host.open(&agent).await;
    let counter = host.entry(&agent).await.unwrap().counter;

    std::fs::write(host.install.work.join("a.txt"), "a\n").unwrap();
    let (one, two) = tokio::join!(host.open(&agent), host.open(&agent));
    assert_eq!(one.snapshot, two.snapshot);
    // One snapshot written: the counter rose once.
    assert_eq!(host.entry(&agent).await.unwrap().counter, counter + 1);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rows_entry_counts_drafts_and_every_edit_raises_its_counter() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;
    assert_eq!(host.entry(&agent).await, None);
    let snapshot = host.open(&agent).await.snapshot;
    let mut last = host.entry(&agent).await.unwrap();
    assert_eq!(last.drafts, 0);

    let edits = [
        (vec![put("a", &snapshot, "notes.txt", 1, 1)], 1),
        (vec![put("b", &snapshot, "notes.txt", 2, 3)], 2),
        // Its words again, on the same lines.
        (vec![put("b", &snapshot, "notes.txt", 2, 3)], 2),
        (vec![review_edit::Of::Delete(b"a".to_vec())], 1),
        (vec![review_edit::Of::DiscardDrafts(Empty {})], 0),
    ];
    for (edit, drafts) in edits {
        let written = host.edit(&agent, edit).await;
        let entry = host.entry(&agent).await.unwrap();
        assert_eq!(entry.drafts, drafts, "{written:?}");
        assert_eq!(entry.counter, last.counter + 1);
        last = entry;
    }
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_all_or_none_and_each_refusal_says_why() {
    let install = Install::new();
    feature_branch(&install.work);
    let agent = agent_in(&install, &install.work, Some("main"));
    let host = Host::start(install).await;

    let before = host
        .try_edit(&agent, vec![put("a", "0000", "notes.txt", 1, 1)])
        .await
        .unwrap_err();
    assert_eq!(before.code(), Code::FailedPrecondition, "{before:?}");

    let snapshot = host.open(&agent).await.snapshot;
    let code = |answer: Result<ReviewSummary, tonic::Status>| answer.unwrap_err().code();
    // A good put beside a bad one: neither is made.
    let unshown = git(&host.install.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        code(
            host.try_edit(
                &agent,
                vec![
                    put("good", &snapshot, "notes.txt", 1, 1),
                    put("bad", &unshown, "notes.txt", 1, 1),
                ],
            )
            .await
        ),
        Code::FailedPrecondition
    );
    assert_eq!(
        code(
            host.try_edit(&agent, vec![put("bad", &snapshot, "notes.txt", 4, 2)])
                .await
        ),
        Code::InvalidArgument
    );
    assert_eq!(
        code(
            host.try_edit(&agent, vec![put("bad", &snapshot, "", 1, 1)])
                .await
        ),
        Code::InvalidArgument
    );
    let long = review_edit::Of::Put(ReviewPutComment {
        id: b"long".to_vec(),
        anchor: Some(ReviewAnchor {
            commit: snapshot.clone(),
            path: "notes.txt".to_owned(),
            lines: Some(ReviewLineRange { start: 1, end: 1 }),
        }),
        text: "x".repeat(16 * 1024 + 1),
    });
    assert_eq!(
        code(host.try_edit(&agent, vec![long]).await),
        Code::InvalidArgument
    );
    let many = (0..501)
        .map(|n| put(&format!("c{n}"), &snapshot, "notes.txt", 0, 0))
        .collect();
    assert_eq!(
        code(host.try_edit(&agent, many).await),
        Code::ResourceExhausted
    );
    assert!(host.open(&agent).await.comments.is_empty());

    // On removed lines the anchor names the base commit.
    let base = host.open(&agent).await.base_commit;
    let removed = host
        .edit(&agent, vec![put("removed", &base, "notes.txt", 25, 25)])
        .await;
    assert_eq!(at(&removed, "removed"), At::File(Empty {}));

    let empty_base = host.open_with(&agent, false, Some("")).await.unwrap_err();
    assert_eq!(empty_base.code(), Code::InvalidArgument);
    host.stop().await;
}

/// Sending a review: the host marks sent exactly the comments the message
/// it accepted names, in a round on the snapshot they were made on.
mod sent {
    use std::collections::BTreeMap;

    use wire::{Attachment, InlineText, MessageBody, SendMessageRequest};

    use super::*;

    /// An agent whose process is running, so it takes messages, working in
    /// the installation's work folder on a branch made from `main`.
    fn live_agent(install: &Install) -> SyntheticAgent {
        let mut agent = SyntheticAgent::new(install, "coder", SEGMENTS);
        agent.register(install);
        let spec = wire::AgentSpec {
            agent_id: agent.id.as_bytes().to_vec(),
            kind: KIND as i32,
            cwd: install.work.to_string_lossy().into_owned(),
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
        agent.go_live();
        agent
    }

    /// The person sends `agent` a message saying `text` with
    /// `attachments`, under an id of its own.
    async fn message(
        host: &Host,
        agent: &SyntheticAgent,
        text: &str,
        attachments: Vec<Attachment>,
    ) -> Result<(), tonic::Status> {
        let mut agents = wire::agent_service_client(served_as(
            &host.runtime,
            Served::Profile,
            Calling::PersonHere(analytics::Client::Terminal),
        ));
        let request = SendMessageRequest {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            agent_id: agent.id.as_bytes().to_vec(),
            context: None,
            message: Some(MessageBody {
                text: text.to_owned(),
                attachments,
                ..MessageBody::default()
            }),
        };
        agents.send_message(request).await.map(drop)
    }

    /// The sent review of the comments `ids` of the review as `summary`
    /// shows it, as a client attaches it.
    fn review(summary: &ReviewSummary, ids: &[&str]) -> Attachment {
        let comments: Vec<ReviewerComment> =
            ids.iter().map(|id| comment(summary, id).clone()).collect();
        attachments::sent_review::attachment(summary, &comments)
    }

    /// The round each comment of `summary` was sent in, by its id.
    fn rounds(summary: &ReviewSummary) -> BTreeMap<String, Option<u32>> {
        summary
            .comments
            .iter()
            .map(|comment| {
                (
                    String::from_utf8(comment.id.clone()).unwrap(),
                    comment.round,
                )
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_accepted_message_marks_exactly_the_drafts_it_names_sent() {
        let install = Install::new();
        feature_branch(&install.work);
        let agent = live_agent(&install);
        let host = Host::start(install).await;
        let snapshot = host.open(&agent).await.snapshot;
        let mine = host
            .edit(
                &agent,
                vec![
                    put("one", &snapshot, "notes.txt", 25, 25),
                    put("two", &snapshot, "notes.txt", 0, 0),
                ],
            )
            .await;
        // A second client writes a draft after the first last read the
        // review, so the first's send does not name it.
        host.edit(&agent, vec![put("theirs", &snapshot, "notes.txt", 2, 2)])
            .await;
        assert_eq!(host.entry(&agent).await.unwrap().drafts, 3);
        let sending = review(&mine, &["one", "two"]);
        let counter = host.entry(&agent).await.unwrap().counter;

        // The agent refuses the message: nothing is sent.
        agent.answer_with(Answer::Reject(wire::Refusal::Unsupported));
        message(&host, &agent, "Two things.", vec![sending.clone()])
            .await
            .unwrap_err();
        assert_eq!(host.entry(&agent).await.unwrap().counter, counter);

        // A message carrying no review sends nothing, nor one carrying a
        // review the agent no longer has.
        agent.answer_with(Answer::Accept);
        let plain = Attachment {
            of: Some(wire::attachment::Of::Text(InlineText {
                name: "notes".to_owned(),
                text: "not a review".to_owned(),
            })),
            ..Attachment::default()
        };
        message(&host, &agent, "Just a note.", vec![plain])
            .await
            .unwrap();
        let mut started_over = mine.clone();
        started_over.id = uuid::Uuid::new_v4().as_bytes().to_vec();
        message(
            &host,
            &agent,
            "From before.",
            vec![review(&started_over, &["one"])],
        )
        .await
        .unwrap();
        assert_eq!(host.entry(&agent).await.unwrap().counter, counter);
        assert!(host.open(&agent).await.rounds.is_empty());

        message(&host, &agent, "Two things.", vec![sending.clone()])
            .await
            .unwrap();
        let entry = host.entry(&agent).await.unwrap();
        assert_eq!(entry.drafts, 1, "the row's draft count drops");
        assert_eq!(entry.counter, counter + 1);
        let sent = host.open(&agent).await;
        assert_eq!(
            rounds(&sent),
            BTreeMap::from([
                ("one".to_owned(), Some(1)),
                ("theirs".to_owned(), None),
                ("two".to_owned(), Some(1)),
            ])
        );
        let [round] = sent.rounds.as_slice() else {
            panic!("one round: {:?}", sent.rounds);
        };
        assert_eq!(round.snapshot, snapshot);
        assert_eq!(round.base_commit, mine.base_commit);
        assert!(round.sent_at_ms > 0);
        // The agent was handed the review as it was sent: once when it
        // refused it, and once when it took it.
        let handed = agent
            .inputs()
            .into_iter()
            .filter_map(|handed| match handed {
                Handed::Message(envelope) => Some(envelope),
                Handed::Input(_) => None,
            })
            .filter_map(|envelope| match envelope.body {
                Some(wire::envelope::Body::Message(message)) => Some(message),
                _ => None,
            })
            .filter(|message| message.attachments.contains(&sending))
            .count();
        assert_eq!(handed, 2);

        // Sent again, as a person resends a message, under a new id: its
        // comments went with the first, so no second round closes.
        message(&host, &agent, "Two things.", vec![sending])
            .await
            .unwrap();
        let again = host.open(&agent).await;
        assert_eq!(again.rounds.len(), 1, "{:?}", again.rounds);
        assert_eq!(host.entry(&agent).await.unwrap().counter, counter + 1);

        // The second client's draft goes in a round of its own.
        message(
            &host,
            &agent,
            "And one more.",
            vec![review(&again, &["theirs"])],
        )
        .await
        .unwrap();
        let second = host.open(&agent).await;
        assert_eq!(second.rounds.len(), 2);
        assert_eq!(rounds(&second)["theirs"], Some(2));
        assert_eq!(host.entry(&agent).await.unwrap().drafts, 0);
        host.stop().await;
    }
}

/// A review's snapshot store, in its agent's directory: it can hold
/// untracked files, secrets among them, so it never goes into a dump and is
/// removed when the daemon opens the profile with the experiment off.
mod stores {
    use std::collections::BTreeSet;

    use wire::DumpRequest;

    use super::*;

    const SECRET: &str = "PLANTEDreviewsecret0001";
    const WORDS: &str = "PLANTEDreviewwords0002";

    fn names(dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// How many of the experiment's tables the profile store holds, read
    /// from what the store says of every table it has.
    fn review_tables(install: &Install) -> usize {
        let dump = format!("{:?}", install.store().dump().unwrap());
        dump.matches("name: \"exp_reviews_1_").count()
    }

    /// Stops `host`'s daemon, handing back its installation.
    async fn stopped(host: Host) -> Install {
        let Host {
            install,
            daemon,
            runtime,
        } = host;
        drop(runtime);
        daemon.unwrap().shutdown().await.unwrap();
        install
    }

    // Unix only for the link: making one on Windows takes a privilege.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn switched_off_the_next_open_removes_every_review_and_nothing_else() {
        let install = Install::new();
        feature_branch(&install.work);
        let agent = agent_in(&install, &install.work, Some("main"));
        let linked = agent_in(&install, &install.work, Some("main"));
        let host = Host::start(install).await;
        let snapshot = host.open(&agent).await.snapshot;
        host.edit(&agent, vec![put("draft", &snapshot, "notes.txt", 25, 25)])
            .await;
        assert!(host.entry(&agent).await.is_some());
        assert_eq!(review_tables(&host.install), 4);
        let install = stopped(host).await;

        let review = agent.dir.join(agent_dir::REVIEW);
        assert!(review.join("objects").is_dir());
        // Another agent's review folder is a link out of its directory:
        // the link goes, and nothing it names.
        let elsewhere = install.work.with_file_name("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("kept.txt"), "kept\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked.dir.join(agent_dir::REVIEW)).unwrap();
        let mut kept = names(&agent.dir);
        kept.remove(agent_dir::REVIEW);

        let daemon = install.start("boot-2", quiet_launch()).await;
        let runtime = runtime(&daemon, &install);
        assert!(!review.exists(), "the agent's review store is removed");
        assert!(
            std::fs::symlink_metadata(linked.dir.join(agent_dir::REVIEW)).is_err(),
            "the link in a review folder's place is removed"
        );
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("kept.txt")).unwrap(),
            "kept\n"
        );
        let after = names(&agent.dir);
        assert!(kept.is_subset(&after), "{kept:?} all stay in {after:?}");
        assert_eq!(review_tables(&install), 0, "the tables are dropped");
        let row = runtime.agent(agent.id).await.unwrap();
        assert_eq!(REVIEW_ROW.get(&row.experimental), None);
        drop(runtime);
        daemon.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dump_holds_nothing_of_an_agents_review() {
        let install = Install::new();
        feature_branch(&install.work);
        std::fs::write(
            install.work.join("secrets.env"),
            format!("TOKEN={SECRET}\n"),
        )
        .unwrap();
        let agent = agent_in(&install, &install.work, Some("main"));
        let host = Host::start(install).await;
        let snapshot = host.open(&agent).await.snapshot;
        let commented = host
            .edit(
                &agent,
                vec![review_edit::Of::Put(ReviewPutComment {
                    id: b"secret".to_vec(),
                    anchor: Some(ReviewAnchor {
                        commit: snapshot,
                        path: "secrets.env".to_owned(),
                        lines: Some(ReviewLineRange { start: 1, end: 1 }),
                    }),
                    text: format!("Never commit {WORDS}."),
                })],
            )
            .await;
        let snippet: Vec<&str> = comment(&commented, "secret")
            .snippet
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            snippet.iter().any(|line| line.contains(SECRET)),
            "the review holds the secret: {snippet:?}"
        );
        assert!(agent.dir.join(agent_dir::REVIEW).join("objects").is_dir());

        let bundle = host
            .runtime
            .dump(DumpRequest {
                reason: "the review page froze".to_owned(),
                ..DumpRequest::default()
            })
            .await
            .unwrap();
        let mut stack = vec![bundle.clone()];
        let mut files = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(&bundle).unwrap();
                assert!(
                    !relative
                        .iter()
                        .any(|part| part == std::ffi::OsStr::new(agent_dir::REVIEW)),
                    "{} is in the dump",
                    relative.display()
                );
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                files += 1;
                let bytes = std::fs::read(&path).unwrap();
                for planted in [SECRET, WORDS] {
                    assert!(
                        !bytes
                            .windows(planted.len())
                            .any(|window| window == planted.as_bytes()),
                        "{planted} is in {}",
                        relative.display()
                    );
                }
            }
        }
        assert!(files > 0);
        host.stop().await;
    }
}
