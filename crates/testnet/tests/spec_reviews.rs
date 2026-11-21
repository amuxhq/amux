//! Reviews across hosts, under the `reviews/1` experiment, with real daemons
//! on a loopback link and real agents on the fakes. Only the desk runs the
//! experiment; a person on the laptop reviews the desk's agent through the
//! carrier, which carries the calls as it carries any host service, and the
//! desk answers them because the agent is its own. The laptop's copy of the
//! agent's row follows the review as every client's does, and the desk
//! closes a round when it accepts the message sending it. Agents are
//! refused wherever they call from, a copy of another host's agent is not
//! found, and a host without the experiment does not offer it.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use client::RpcError;
use node::Launch;
use provider_fakes::script::Step;
use testnet::{AgentDecl, HostDecl, Net, NetOptions, Topology, until};
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor, ReviewEdit, ReviewFileRequest, ReviewGetFilesRequest, ReviewLineKind,
    ReviewLineRange, ReviewPutComment, ReviewRowEntry, ReviewSummary, ReviewView, review_edit,
};
use wire::{AgentRef, ErrorCode, MessageBody};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// The coder on the desk, in the desk's repository: one turn on its first
/// prompt and one on the message a review is sent in.
fn coder() -> AgentDecl {
    AgentDecl {
        repository: Some("site".to_owned()),
        ..AgentDecl::new("coder", "desk")
            .prompt("Tidy the site's readme.")
            .steps(vec![
                text("Tidied."),
                Step::TurnEnd,
                text("Looking at your comments."),
                Step::TurnEnd,
            ])
    }
}

/// A helper on the laptop, in its work folder.
fn helper() -> AgentDecl {
    AgentDecl::new("helper", "laptop")
        .prompt("Wait here.")
        .steps(vec![text("Waiting."), Step::TurnEnd])
}

/// The desk, holding the repository `site`, and the laptop, linked.
fn topology() -> Topology {
    Topology::new()
        .host_decl(HostDecl {
            name: "desk".to_owned(),
            repositories: vec!["site".to_owned()],
            files: [("site/README.md".to_owned(), "# The site\n".to_owned())].into(),
            ..HostDecl::default()
        })
        .host("laptop")
        .link("desk", "laptop")
        .agent(coder())
        .agent(helper())
}

/// The desk runs the experiment and the laptop does not.
fn desk_only() -> NetOptions {
    NetOptions {
        launch: Some(Arc::new(|host: &str, launch: &mut Launch| {
            if host == "desk" {
                launch.experiments =
                    settings::experiments::Enabled::of(&[&settings::experiments::REVIEWS]);
            }
        })),
        ..NetOptions::default()
    }
}

fn site(net: &Net) -> PathBuf {
    let desk = net.host("desk").unwrap();
    desk.work.with_file_name("repositories").join("site")
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Every ref of the repository at `cwd`, and every file under its git
/// directory's objects.
fn repository_state(cwd: &Path) -> (String, Vec<PathBuf>) {
    let objects = cwd.join(".git").join("objects");
    let mut files = Vec::new();
    let mut stack = vec![objects.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path.strip_prefix(&objects).unwrap().to_owned());
            }
        }
    }
    files.sort();
    (git(cwd, &["for-each-ref"]), files)
}

fn put(id: &str, snapshot: &str, path: &str, start: u32, end: u32, words: &str) -> ReviewEdit {
    ReviewEdit {
        of: Some(review_edit::Of::Put(ReviewPutComment {
            id: id.as_bytes().to_vec(),
            anchor: Some(ReviewAnchor {
                commit: snapshot.to_owned(),
                path: path.to_owned(),
                lines: Some(ReviewLineRange { start, end }),
            }),
            text: words.to_owned(),
        })),
    }
}

/// The review's entry on `agent`'s row as `host` holds it: the desk's own
/// row, or the laptop's copy.
async fn entry(net: &Net, host: &str, agent: &str) -> Option<ReviewRowEntry> {
    let key = net.agent(agent).unwrap().key();
    let row = net
        .runtime(host)
        .unwrap()
        .read_store(move |store| store.agent(&key).unwrap())
        .await
        .unwrap()?;
    REVIEW_ROW.get(&row.experimental)
}

/// Waits for the laptop's copy of the coder's row to carry the entry the
/// desk's own row does, and answers it.
async fn copied(net: &Net, what: &str) -> ReviewRowEntry {
    until(what, || async {
        let own = entry(net, "desk", "coder").await;
        let copy = entry(net, "laptop", "coder").await;
        match (own, copy) {
            (Some(own), Some(copy)) if own == copy => Ok(copy),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap()
}

fn refused(answer: Result<ReviewSummary, RpcError>) -> (Option<ErrorCode>, String) {
    let error = answer.expect_err("refused");
    (error.code(), error.to_string())
}

/// A person on the laptop, where the experiment is off, reviews the desk's
/// agent: opens it, reads its files and comments, each edit reaching the
/// laptop's copy of the row, and sends the comments in a message, which the
/// desk marks sent as it accepts it. Nothing is written to the repository.
/// Deleting the agent removes its review with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_person_on_another_host_reviews_an_agent_through_the_carrier() {
    let mut net = Net::start_with(topology(), desk_only()).await.unwrap();
    assert!(
        !net.runtime("laptop")
            .unwrap()
            .launch()
            .experiments
            .is_on(&settings::experiments::REVIEWS)
    );
    net.settle("coder").await.unwrap();
    let site = site(&net);
    std::fs::write(site.join("README.md"), "# The site\n\nNow tidy.\n").unwrap();
    std::fs::write(site.join("notes.md"), "untracked\n").unwrap();
    let before = repository_state(&site);

    let laptop = net.person("laptop").await.unwrap();
    let coder = net.agent("coder").unwrap().clone();
    let reviews = laptop.agent(&coder).reviews();
    let opened = reviews.open(None, false).await.unwrap();
    let paths: Vec<&str> = opened.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["README.md", "notes.md"]);
    let first = copied(&net, "the laptop's copy to carry the review").await;
    assert_eq!(first.drafts, 0);

    let files = reviews
        .get_files(ReviewGetFilesRequest {
            from: opened.base_commit.clone(),
            snapshot: opened.snapshot.clone(),
            view: ReviewView::All as i32,
            files: vec![ReviewFileRequest {
                path: "README.md".to_owned(),
                ..ReviewFileRequest::default()
            }],
            ..ReviewGetFilesRequest::default()
        })
        .await
        .unwrap();
    let added: Vec<&str> = files
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind() == ReviewLineKind::Added)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(added, ["", "Now tidy."]);

    let snapshot = opened.snapshot.as_str();
    reviews
        .edit(vec![put(
            "tidy",
            snapshot,
            "README.md",
            3,
            3,
            "Tidier still.",
        )])
        .await
        .unwrap();
    let one = copied(&net, "the copy to follow the first comment").await;
    assert_eq!(one.drafts, 1);
    assert!(one.counter > first.counter);
    let commented = reviews
        .edit(vec![put(
            "notes",
            snapshot,
            "notes.md",
            0,
            0,
            "Leave this out.",
        )])
        .await
        .unwrap();
    let two = copied(&net, "the copy to follow the second comment").await;
    assert_eq!(two.drafts, 2);
    assert!(two.counter > one.counter);

    let sending = ui_view::sent_review::attachment(&commented, &commented.comments);
    laptop
        .agent(&coder)
        .send_message(
            uuid::Uuid::new_v4().as_bytes().to_vec(),
            None,
            MessageBody {
                text: "Two small things.".to_owned(),
                attachments: vec![sending],
            },
        )
        .await
        .unwrap();
    let sent = copied(&net, "the copy to say the drafts went").await;
    assert_eq!(sent.drafts, 0);
    let after = reviews.open(None, false).await.unwrap();
    assert_eq!(after.rounds.len(), 1, "{:?}", after.rounds);
    assert_eq!(after.rounds[0].snapshot, opened.snapshot);
    assert_eq!(after.rounds[0].base_commit, opened.base_commit);
    assert!(
        after
            .comments
            .iter()
            .all(|comment| comment.round == Some(1)),
        "{:?}",
        after.comments
    );
    assert_eq!(
        repository_state(&site),
        before,
        "the repository is untouched"
    );
    println!(
        "laptop reviewed coder on desk: {} comments sent in round 1 on {}",
        after.comments.len(),
        after.snapshot
    );

    let agent_dir = net.agent_dir("coder").unwrap();
    assert!(agent_dir.join(daemon_core::reviews::REVIEW).is_dir());
    let id = coder.id;
    net.delete("coder").await.unwrap();
    let review = net
        .runtime("desk")
        .unwrap()
        .read_store(move |store| store.review(id.as_bytes()).unwrap())
        .await
        .unwrap();
    assert!(review.is_none(), "the review's rows go with the agent");
    assert!(!agent_dir.exists(), "its store goes with its directory");
    let key = coder.key();
    until("the laptop's copy to go", || async {
        let key = key.clone();
        let copy = net
            .runtime("laptop")
            .unwrap()
            .read_store(move |store| store.agent(&key).unwrap())
            .await
            .unwrap();
        match copy {
            None => Ok(()),
            Some(row) => Err(format!("{:?}", REVIEW_ROW.get(&row.experimental))),
        }
    })
    .await
    .unwrap();
    net.shutdown().await.unwrap();
}

/// An agent is refused, on its own host's tools socket and carried from
/// another; a person asking a host about a copy of another host's agent is
/// told which host runs it; a host without the experiment does not offer
/// reviews at all.
#[tokio::test(flavor = "multi_thread")]
async fn agents_copies_and_hosts_without_the_experiment_are_refused() {
    let net = Net::start_with(topology(), desk_only()).await.unwrap();
    net.settle("coder").await.unwrap();
    net.settle("helper").await.unwrap();
    let coder = net.agent("coder").unwrap().clone();
    let helper = net.agent("helper").unwrap().clone();

    // The laptop's agent, its call carried to the desk: refused there.
    let carried = net.tools("helper").await.unwrap();
    let (code, words) = refused(carried.agent(&coder).reviews().open(None, false).await);
    assert_eq!(code, Some(ErrorCode::PermissionDenied), "{words}");
    assert!(words.contains("people"), "{words}");

    // The desk's own agent, on its tools socket: reviews are not served
    // there at all.
    let own = net.tools("coder").await.unwrap();
    let (code, words) = refused(own.agent(&coder).reviews().open(None, false).await);
    assert_eq!(code, Some(ErrorCode::Unimplemented), "{words}");

    // The desk holds a copy of the laptop's helper and runs the experiment,
    // but the helper's tree is on the laptop.
    let desk = net.person("desk").await.unwrap();
    let copy = AgentRef {
        host_id: coder.host_id.as_bytes().to_vec(),
        agent_id: helper.id.as_bytes().to_vec(),
    };
    let (code, words) = refused(desk.agent(&copy).reviews().open(None, false).await);
    assert_eq!(code, Some(ErrorCode::NotFound), "{words}");
    assert!(
        words.contains("runs on") && words.contains(&helper.host_id.to_string()),
        "{words}"
    );

    // Asked of the laptop, which runs the helper without the experiment:
    // not offered.
    let (code, words) = refused(desk.agent(&helper).reviews().open(None, false).await);
    assert_eq!(code, Some(ErrorCode::Unimplemented), "{words}");
    net.shutdown().await.unwrap();
}
