//! A profile as a released build left it, opened by this one. What outlives
//! a build is read as this build reads it, so a change that stops it reading
//! what really shipped fails here: the profile, its account and its store,
//! the device key, and each agent's spec, journal, facts and provider
//! session.
//!
//! The store is opened first, on its own, which is where this build's
//! migrations run on what the release wrote. It holds something in every
//! table a release fills over time: an agent of its own with its history, a
//! paired host with the address it answered at and the agent it listed, a
//! child's report waiting for a parent that has exited, a notification still
//! owed to the person, and a draft.
//!
//! The agent is not resumed: its working directory was the writer's
//! temporary one. Blobs are bytes named by their hash, with no format to
//! stop reading, and the core's ring and the session tickets are records a
//! build makes afresh, so nothing here reads those.
//!
//! Each directory under `tests/fixtures/upgrade/` is one release's, made at
//! its tag by `just upgrade-fixture`. Until the first is committed there is
//! nothing to open.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use node::{Cloud, LanOptions, ProfileRuntime};
use prost::Message as _;
use provider_fakes::script::Step;
use replay_support::replay::{RESUME_CONTINUES, Verdict, replay_facts};
use support::synthetic::{SyntheticAgent, item, quiet_launch, snapshot, turn_end};
use support::*;
use wire::{AskNotice, Notice, Phase, RunWant, ask_notice, notice};

use crate::support;

/// The release's own agent, run by its real agent process.
const AGENT: &str = "keeper";
const REPLY: &str = "Kept for the next build.";
/// Its child, whose finished turn it has not been told of.
const CHILD: &str = "helper";
const REPORT: &str = "The tests pass.";
/// An agent waiting on the person, who has not been notified yet.
const ASKER: &str = "asker";
const ASKED: &str = "cargo test";
/// The paired host's agent, which the release lists.
const AFAR: &str = "afar";
const DRAFT: &str = "Not sent yet.";
const SERVICE: &str = "https://account.example";
const SUBJECT: &str = "the-release's-account";
const EMAIL: &str = "keeper@example.com";

const SEGMENTS: u64 = 1 << 20;
/// Longer than the writer runs, so the notification is still owed when it
/// stops.
const NOTIFY_DELAY_MS: i64 = 3_600_000;

/// Edge options listening on loopback, on any port.
fn lan() -> node::EdgeOptions {
    node::EdgeOptions {
        lan: Some(LanOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            socket: None,
        }),
        dial: true,
        ..Default::default()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade")
}

/// Every release's profile starts under this build, with its identity, its
/// agent and what that agent said still there.
#[tokio::test(flavor = "multi_thread")]
async fn a_profile_a_release_wrote_opens_with_its_agent_and_history() {
    let Ok(releases) = std::fs::read_dir(fixtures()) else {
        return;
    };
    for release in releases {
        let release = release.unwrap().path();
        let install = Install::holding(&release);
        let profile = install.profile_dir();
        let device_key = node::harness::device_key_path(&profile);
        let written = std::fs::read(&device_key).expect("the release's device key");
        the_store_holds_what_the_release_left(&install);
        let daemon = install.start("upgraded", quiet_launch()).await;
        let runtime = runtime(&daemon, &install);
        let listed = daemon.registry().list().expect("the profiles list");
        let account = listed[0].account.as_ref().expect("the release's account");
        assert_eq!(
            (account.subject.as_str(), account.email.as_deref()),
            (SUBJECT, Some(EMAIL)),
            "{}'s account",
            release.display()
        );
        assert_eq!(listed[0].cloud, Cloud::SignedOut);
        let (agent, items) = runtime
            .read_store(|store| {
                let agents = store.agents().unwrap();
                let row = agents.iter().find(|row| row.name == AGENT);
                let key = &row.expect("the release's agent is listed").agent;
                (
                    key.agent.clone(),
                    store.page(key, None, 1_000).unwrap().items,
                )
            })
            .await
            .unwrap();
        assert!(
            items.iter().any(|item| item.text == REPLY),
            "{} lost what its agent said: {items:?}",
            release.display()
        );
        node::harness::load_or_create_device_identity_in(&profile, runtime.host())
            .expect("the release's device key reads");
        assert_eq!(
            std::fs::read(&device_key).unwrap(),
            written,
            "the host keeps the identity its peers know it by"
        );
        let agent = uuid::Uuid::from_slice(&agent).unwrap();
        an_agents_directory_reads(&install.agent_dir(agent), agent);
        // A later build may make different steps of the same facts, so an
        // old ring need not replay as it was journaled. It has to read, and
        // to break none of the interpreter's checks.
        match replay_facts(&profile.join(node::AGENTS)) {
            Verdict::Holds(_) => {}
            Verdict::Fails(failing) if failing.property == RESUME_CONTINUES => {}
            verdict => panic!("{}'s facts: {verdict:?}", release.display()),
        }
        daemon.shutdown().await.unwrap();
    }
}

/// Opens the release's store with this build, which runs the migrations it
/// lacks, and reads a row of every kind the release left in it.
fn the_store_holds_what_the_release_left(install: &Install) {
    let store = install.store();
    let own = store.own_host().to_vec();
    let agents = store.agents().unwrap();
    let named = |name: &str| {
        agents
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("the release's {name} is listed"))
    };
    let (keeper, child, asker, afar) = (named(AGENT), named(CHILD), named(ASKER), named(AFAR));
    assert_eq!(child.parent.as_ref(), Some(&keeper.agent));

    // The paired host: its pinned key, where it answered, and its agent.
    let hosts = store.trusted_hosts().unwrap();
    let [peer] = hosts.as_slice() else {
        panic!("one paired host, not {hosts:?}");
    };
    assert_eq!(peer.pubkey.len(), 32);
    let direct: serde_json::Value = serde_json::from_str(&peer.direct).unwrap();
    assert!(
        direct
            .as_array()
            .is_some_and(|addresses| !addresses.is_empty()),
        "the paired host answered somewhere: {}",
        peer.direct
    );
    assert_eq!(afar.agent.host, peer.host_id, "its agent is a replica");
    assert_ne!(afar.agent.host, own);
    assert!(store.host_generation(&peer.host_id).unwrap().is_some());

    // The child's report, waiting for its exited parent.
    let deliveries = store.deliveries().unwrap();
    let [delivery] = deliveries.as_slice() else {
        panic!("one report waits, not {deliveries:?}");
    };
    assert_eq!(delivery.child_id, child.agent.agent);
    assert_eq!(delivery.parent, keeper.agent);
    match &delivery.end {
        wire::child_report::End::TurnEnded(ended) => assert_eq!(ended.text, REPORT),
        other => panic!("the report is a finished turn, not {other:?}"),
    }

    // The notification owed for the agent that asks.
    let owed = store.notifications().unwrap();
    let owed = owed
        .iter()
        .find(|row| row.agent_id == asker.agent.agent)
        .expect("a notification is owed for the agent that asks");
    assert_eq!(owed.name, ASKER);
    match Notice::decode(owed.notice.as_slice()).unwrap().of {
        Some(notice::Of::Ask(ask)) => match ask.wants {
            Some(ask_notice::Wants::Run(run)) => assert_eq!(run.command, ASKED),
            other => panic!("the ask wants to run a command, not {other:?}"),
        },
        other => panic!("the notice is an ask, not {other:?}"),
    }

    let draft = store
        .draft(&keeper.agent.agent)
        .unwrap()
        .expect("the release's draft");
    assert_eq!(wire::Draft::decode(draft.as_slice()).unwrap().text, DRAFT);
}

/// What an agent's next incarnation starts from: its spec, the journal it
/// goes on writing, the facts it resumes from, and the session the provider
/// resumes.
fn an_agents_directory_reads(dir: &Path, agent: uuid::Uuid) {
    let spec = std::fs::read(dir.join("spec.1")).expect("the release's spec");
    let spec = wire::AgentSpec::decode(spec.as_slice()).unwrap();
    assert_eq!(spec.agent_id, agent.as_bytes());
    let journal = journal::Reader::new(dir.join(agent_dir::JOURNAL), 0)
        .read_to_end()
        .unwrap();
    assert!(
        journal
            .frames
            .iter()
            .any(|(_, step)| step.items.iter().any(|item| item.text == REPLY)),
        "the journal lost what the agent said: {:?}",
        journal.frames
    );
    // The replay finds no fault in a ring it finds nothing in, and an agent
    // with no ring silently starts over.
    let facts = dir.join(agent_dir::PRIVATE).join("facts");
    let saved = core_ring::saved::<interpret::ring::Entry>(&facts)
        .unwrap()
        .expect("the release's facts ring");
    assert!(!saved.inputs.is_empty(), "the facts ring reads as empty");
    let session = dir.join(agent_dir::PRIVATE).join("provider-session");
    let session = std::fs::read_to_string(session).expect("the release's provider session");
    assert!(!session.is_empty());
}

/// Writes this build's profile, with something in every table of its store.
/// Run at a release's tag by `just upgrade-fixture`, and the result
/// committed.
///
/// The release's own agent is a real agent process on the fake provider.
/// Its child, the agent that asks and the paired host's agent are journals
/// written by hand, as the daemon's own tests write them: what the store
/// keeps of them is what this build's daemon made of those journals.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a fixture; `just upgrade-fixture` runs it at a release's tag"]
async fn write_this_releases_profile() {
    let out = fixtures().join(env!("CARGO_PKG_VERSION"));
    assert!(!out.exists(), "{} already exists", out.display());
    let install = Install::new();

    // An agent runs a turn and is stopped.
    let launch = install.launch(AGENT, vec![text(REPLY), Step::TurnEnd]);
    let daemon = install.start("released", launch).await;
    let runtime = runtime(&daemon, &install);
    let agent = runtime
        .spawn(create(&install.work, AGENT, Some("Say something.")), None)
        .await
        .expect("the agent spawns");
    let keeper = store::AgentKey::new(runtime.host().as_bytes().to_vec(), agent.agent_id);
    until("the agent's reply to reach the store", || {
        let key = keeper.clone();
        let runtime = runtime.clone();
        async move {
            let items = runtime
                .read_store(move |store| store.page(&key, None, 1_000).unwrap().items)
                .await
                .unwrap();
            match items.iter().any(|item| item.text == REPLY) {
                true => Ok(()),
                false => Err(format!("{} items", items.len())),
            }
        }
    })
    .await
    .unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();

    // While no daemon runs, a child of that agent finishes a turn, and
    // another agent starts.
    let mut child = SyntheticAgent::new(&install, CHILD, SEGMENTS);
    child.parent = Some(keeper.clone());
    child.register(&install);
    child.append(&snapshot(Phase::Working, &[], 2_000));
    child.append(&item("last", REPORT));
    child.append(&turn_end(1, "last"));
    let mut asker = SyntheticAgent::new(&install, ASKER, SEGMENTS);
    asker.register(&install);
    asker.go_live();

    // A second machine, with an agent of its own.
    let peer = Install::new();
    SyntheticAgent::new(&peer, AFAR, SEGMENTS).register(&peer);
    let mut options = peer.options("peer", quiet_launch());
    options.edge = lan();
    let peer_daemon = node::start(options, None).await.unwrap();
    let peer_edge = support::runtime(&peer_daemon, &peer).edge().unwrap();

    // The release's daemon again, on the network and slow to notify.
    let mut launch = quiet_launch();
    launch.core.notify_delay_ms = NOTIFY_DELAY_MS;
    let mut options = install.options("released", launch);
    options.edge = lan();
    let daemon = node::start(options, None).await.unwrap();
    let runtime = support::runtime(&daemon, &install);
    let edge = runtime.edge().unwrap();

    // The two pair, and the release links to the other.
    edge.trust(&peer_edge).await.unwrap();
    peer_edge.trust(&edge).await.unwrap();
    edge.dial(peer_edge.host_id(), peer_edge.lan_addr().unwrap());
    assert!(
        wait_for_route(&edge, peer_edge.host_id(), PATIENCE).await,
        "the release links to its peer"
    );
    let peer_id = peer_edge.host_id().as_bytes().to_vec();
    until_store(&runtime, "the peer's address and its agent", {
        let peer_id = peer_id.clone();
        move |store| {
            let answered = store
                .trusted_hosts()
                .unwrap()
                .iter()
                .any(|host| host.host_id == peer_id && host.direct != "[]");
            let listed = store
                .agents()
                .unwrap()
                .iter()
                .any(|row| row.agent.host == peer_id && row.name == AFAR);
            let generation = store.host_generation(&peer_id).unwrap().is_some();
            answered && listed && generation
        }
    })
    .await;

    // The child's report waits: its parent has exited.
    until_store(&runtime, "the child's report to wait", |store| {
        store.deliveries().unwrap().len() == 1
    })
    .await;
    assert_eq!(runtime.drain_deliveries().await.kept, 1);

    // The other agent asks to run a command.
    let mut asks = snapshot(Phase::NeedsYou, &[], 3_000);
    asks.snapshot.as_mut().unwrap().notice = Some(Box::new(Notice {
        of: Some(notice::Of::Ask(AskNotice {
            key: "ask:1".to_owned(),
            wants: Some(ask_notice::Wants::Run(RunWant {
                command: ASKED.to_owned(),
            })),
            why: String::new(),
            more: 0,
        })),
    }));
    asker.append(&asks);
    asker.nudge().await;
    let asking = asker.id.as_bytes().to_vec();
    until_store(&runtime, "the notification to be owed", move |store| {
        store
            .notifications()
            .unwrap()
            .iter()
            .any(|row| row.agent_id == asking)
    })
    .await;

    runtime
        .update_draft(
            &keeper.agent,
            &wire::Draft {
                text: DRAFT.into(),
                attachments: Vec::new(),
            },
        )
        .await;
    let drafted = keeper.agent.clone();
    until_store(&runtime, "the draft", move |store| {
        store.draft(&drafted).unwrap().is_some()
    })
    .await;

    let closing = Arc::downgrade(&runtime);
    drop((edge, runtime, peer_edge));
    daemon.shutdown().await.unwrap();
    peer_daemon.shutdown().await.unwrap();
    drop(asker);
    // The store is closed, and its log folded into its file, once the last
    // task holding the profile has ended.
    until("the release's daemon to let go of its store", || {
        let held = closing.strong_count();
        async move {
            match held {
                0 => Ok(()),
                held => Err(format!("{held} references held")),
            }
        }
    })
    .await
    .unwrap();
    // Signed in once and signed out since: tied to its account, dialling
    // nothing.
    node::harness::write_signed_out_account(&install.profile_dir(), SERVICE, SUBJECT, EMAIL)
        .unwrap();
    copy_files(&install.data_dir, &out);
}

/// Waits until the profile's store answers `holds`.
async fn until_store(
    runtime: &Arc<ProfileRuntime>,
    what: &str,
    holds: impl Fn(&store::Sqlite) -> bool + Clone + Send + 'static,
) {
    until(what, || {
        let (runtime, holds) = (runtime.clone(), holds.clone());
        async move {
            match runtime.read_store(move |store| holds(store)).await {
                Ok(true) => Ok(()),
                Ok(false) => Err("not yet".to_owned()),
                Err(error) => Err(error.to_string()),
            }
        }
    })
    .await
    .unwrap();
}
