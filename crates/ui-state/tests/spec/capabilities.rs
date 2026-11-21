//! What an agent can do, as the session reads it: its snapshot's word, else
//! its host's for its provider, else what its kind implies.

use ui_state::{Msg, SessionState};
use wire::{AgentCapabilities, Kind, Phase};

use super::harness::*;

fn host_offering(provider: &str, kind: Kind, capabilities: AgentCapabilities) -> wire::HostEntry {
    let mut host = wire::HostEntry::trusted_host(
        b"host-a".to_vec(),
        "desk",
        wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
    );
    host.providers = vec![wire::ProviderOnHost {
        provider_id: provider.into(),
        descriptor: Some(wire::ProviderDescriptor {
            kind: kind as i32,
            ..Default::default()
        }),
        capabilities: Some(capabilities),
        ..Default::default()
    }];
    host
}

fn host_saying(capabilities: AgentCapabilities) -> wire::HostEntry {
    host_offering("mine", Kind::Acp, capabilities)
}

/// The host says what a new agent of its provider can do, as the kind it
/// starts now: an agent of another kind of the same provider reads its own
/// kind's, so a headless Claude is never offered terminal Claude's switch
/// back once the host starts Claude in its terminal.
#[test]
fn the_hosts_word_stands_only_for_the_kind_it_starts() {
    let mut headless = agent(Kind::ClaudeSdk);
    headless.provider = "claude".into();
    let mut state = SessionState::new(headless, CAP);
    state.update(Msg::Host(host_offering(
        "claude",
        Kind::ClaudePty,
        AgentCapabilities::claude_pty(),
    )));
    assert_eq!(state.capabilities(), AgentCapabilities::claude_sdk());
    assert!(!state.capabilities().switches_back);

    state.update(Msg::Host(host_offering(
        "claude",
        Kind::ClaudeSdk,
        AgentCapabilities::claude_sdk(),
    )));
    assert_eq!(state.capabilities(), AgentCapabilities::claude_sdk());
}

#[test]
fn the_snapshot_says_first_then_the_host_then_the_kind() {
    // An ACP agent before any snapshot: its host's word for its provider.
    let mut acp = agent(Kind::Acp);
    acp.provider = "mine".into();
    let mut state = SessionState::new(acp, CAP);
    assert_eq!(state.capabilities(), AgentCapabilities::default());
    let listed = AgentCapabilities::acp(true, false, false);
    state.update(Msg::Host(host_saying(listed)));
    assert_eq!(state.capabilities(), listed);

    // Its own snapshot outranks the host's.
    let own = AgentCapabilities::acp(true, true, true);
    let mut said = snapshot(Kind::Acp, 1, Phase::Idle, &[], &[]);
    said.capabilities = Some(own);
    apply_checked(&mut state, ev_snapshot(said));
    assert_eq!(state.capabilities(), own);

    // A snapshot an earlier build wrote names none: Codex's kind says.
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    apply_checked(
        &mut codex,
        ev_snapshot(snapshot(Kind::Codex, 1, Phase::Idle, &[], &[])),
    );
    assert_eq!(codex.capabilities(), AgentCapabilities::codex());
}
