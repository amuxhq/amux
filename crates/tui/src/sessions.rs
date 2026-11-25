//! What the TUI's tests and the storybook share: the agent and the catch-up
//! a session built from an interpreter's fixtures starts from, and a drawn
//! buffer read back as text.

use ratatui::buffer::Buffer;
use ui_state::Msg;
use wire::{Kind, SessionEvent, session_event};

pub fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::Idle as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

/// What the shared agent's host offers: Claude, here headless, and Codex,
/// each signed in, with the sign-in words their hosts give.
pub fn offered_providers() -> Vec<wire::ProviderOnHost> {
    vec![
        hinted(
            offered("claude", "Claude", Kind::ClaudeSdk),
            "Run claude and sign in with /login.",
        ),
        hinted(offered("codex", "Codex", Kind::Codex), "Run codex login."),
    ]
}

/// One agent a host offers, signed in, with what its kind's agents can do.
pub fn offered(id: &str, name: &str, kind: Kind) -> wire::ProviderOnHost {
    wire::ProviderOnHost {
        provider_id: id.into(),
        catalogue: Some(vec![0; 32]),
        signed_in: true,
        descriptor: Some(wire::ProviderDescriptor {
            name: name.into(),
            kind: kind as i32,
            ..Default::default()
        }),
        capabilities: Some(
            wire::AgentCapabilities::of_kind(kind, wire::Platform::here())
                .unwrap_or_else(|| wire::AgentCapabilities::acp(true, true, false)),
        ),
    }
}

/// `offer` with its host's words for signing in.
pub fn hinted(mut offer: wire::ProviderOnHost, hint: &str) -> wire::ProviderOnHost {
    if let Some(descriptor) = offer.descriptor.as_mut() {
        descriptor.sign_in_hint = hint.into();
    }
    offer
}

/// The shared agent's host as its session knows it: online, offering
/// `providers`.
pub fn host_entry(providers: Vec<wire::ProviderOnHost>) -> wire::HostEntry {
    wire::HostEntry {
        providers,
        ..wire::HostEntry::trusted_host(
            b"host".to_vec(),
            "studio",
            wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
        )
    }
}

pub fn caught_up(revision: u64) -> Msg {
    Msg::Event(SessionEvent {
        of: Some(session_event::Of::CaughtUp(wire::CaughtUp { revision })),
    })
}

/// The buffer's text, one line per row, trailing blanks trimmed.
pub fn text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
