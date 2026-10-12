//! A Codex thread carries the agent's name: Codex's own app shows the name
//! amux does.

mod support;

use agent::ExitCause;
use support::*;
use wire::Kind;

/// The names the agent gave the thread, in order, from what the fake read.
fn names(agent: &Agent, thread: &str) -> Vec<String> {
    agent
        .provider_input()
        .iter()
        .filter(|line| line["method"] == "thread/name/set")
        .inspect(|line| assert_eq!(line["params"]["threadId"], thread))
        .map(|line| line["params"]["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_thread_is_named_after_the_agent_at_start_and_on_rename() {
    let agent = Agent::start(Setup {
        kind: Kind::Codex,
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    let thread = agent.provider_session();
    agent
        .until("the thread is named", || names(&agent, &thread) == ["test"])
        .await;

    daemon.rename("renamed").await;
    agent
        .until("the thread is renamed", || {
            names(&agent, &thread) == ["test", "renamed"]
        })
        .await;

    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}
