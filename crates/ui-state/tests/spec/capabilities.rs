//! What an agent can do, as the session reads it: its snapshot's word, else
//! its host's for its provider, else what its kind implies.

use ui_state::{Msg, SessionState};
use wire::{AgentCapabilities, Kind, Phase, Platform};

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

    // A snapshot an earlier build wrote names none: Codex's kind says, on
    // a host whose platform this session has not heard, so not Windows.
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    apply_checked(
        &mut codex,
        ev_snapshot(snapshot(Kind::Codex, 1, Phase::Idle, &[], &[])),
    );
    assert_eq!(
        codex.capabilities(),
        AgentCapabilities::codex(Platform::Unspecified)
    );
}

/// A host from before hosts said what their agents can do lists Codex
/// with no capabilities, on its platform.
fn older_host(platform: Platform) -> wire::HostEntry {
    let mut host = wire::HostEntry::trusted_host(
        b"host-a".to_vec(),
        "desk",
        wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
    );
    if let Some(described) = host.host.as_mut() {
        described.platform = platform as i32;
    }
    host.providers = vec![wire::ProviderOnHost {
        provider_id: "codex".into(),
        descriptor: Some(wire::ProviderDescriptor {
            kind: Kind::Codex as i32,
            ..Default::default()
        }),
        capabilities: None,
        ..Default::default()
    }];
    host
}

/// What Codex's kind implies is read for the platform of the host it runs
/// on, never the reader's: a Mac reads a Windows host's Codex as having no
/// terminal to attach, and a Windows reader a Mac's as having one. A host
/// whose platform is unknown reads as readers before this read every host
/// but their own Windows: attachable.
#[test]
fn an_older_hosts_codex_is_read_for_that_hosts_platform() {
    let mut codex = agent(Kind::Codex);
    codex.provider = "codex".into();
    for (platform, attachable) in [
        (Platform::Windows, false),
        (Platform::Macos, true),
        (Platform::Linux, true),
        (Platform::Unspecified, true),
    ] {
        let host = older_host(platform);
        assert_eq!(
            ui_state::offered_capabilities(Some(&host), &codex).attachable,
            attachable,
            "{platform:?}"
        );
        let mut state = SessionState::new(codex.clone(), CAP);
        state.update(Msg::Host(host));
        assert_eq!(state.capabilities().attachable, attachable, "{platform:?}");
    }
}
