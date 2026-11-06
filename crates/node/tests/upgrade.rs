//! A profile as a released build left it, opened by this one. What outlives
//! a build is read as this build reads it, so a change that stops it reading
//! what really shipped fails here: the profile and its store, the device
//! key, and each agent's spec, journal, facts and provider session.
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

use prost::Message as _;
use provider_fakes::script::Step;
use replay_support::replay::{RESUME_CONTINUES, Verdict, replay_facts};
use support::synthetic::quiet_launch;
use support::*;

use crate::support;

const AGENT: &str = "keeper";
const REPLY: &str = "Kept for the next build.";

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
        let daemon = install.start("upgraded", quiet_launch()).await;
        let runtime = runtime(&daemon, &install);
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

/// Writes this build's profile: an agent has run a turn and ended. Run at a
/// release's tag by `just upgrade-fixture`, and the result committed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a fixture; `just upgrade-fixture` runs it at a release's tag"]
async fn write_this_releases_profile() {
    let out = fixtures().join(env!("CARGO_PKG_VERSION"));
    assert!(!out.exists(), "{} already exists", out.display());
    let install = Install::new();
    let launch = install.launch(AGENT, vec![text(REPLY), Step::TurnEnd]);
    let daemon = install.start("released", launch).await;
    let runtime = runtime(&daemon, &install);
    let agent = runtime
        .spawn(create(&install.work, AGENT, Some("Say something.")), None)
        .await
        .expect("the agent spawns");
    let key = store::AgentKey::new(runtime.host().as_bytes().to_vec(), agent.agent_id);
    until("the agent's reply to reach the store", || {
        let key = key.clone();
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
    copy_files(&install.data_dir, &out);
}
