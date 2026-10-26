//! Host words shared by the fleet rows and the hosts overlay: how each host
//! is reached, every trusted host whatever its presence and every candidate
//! discovery currently sees, and the one banner line a reachability problem
//! earns.

use ratatui::text::{Line, Span};
use ui_state::FleetState;
use ui_view::{Offline, Reach};
use wire::{HostEntry, RouteKind};

use crate::text::{self, pad_to};
use crate::theme::Theme;

/// How a trusted host is reached now, in one word: the route while it is
/// online, else whether it is away or offline. This machine's own entry has
/// no route.
pub fn route(reach: Reach, local: bool) -> &'static str {
    match reach {
        Reach::Online(RouteKind::Direct) => "direct",
        Reach::Online(RouteKind::Relay) => "relay",
        Reach::Online(RouteKind::Ssh) => "ssh",
        Reach::Online(RouteKind::Unspecified) if local => "local",
        Reach::Online(RouteKind::Unspecified) => "online",
        Reach::Away => "away",
        Reach::Offline(_) => "offline",
    }
}

/// The overlay's words for a trusted host: its route, then what stands in
/// its way. A host that said it no longer trusts this machine says that;
/// one offline because this machine is signed out names this machine rather
/// than the host.
pub fn caption(fleet: &FleetState, entry: &HostEntry, local_host: &[u8]) -> String {
    let reach = ui_view::reach(fleet, local_host, entry.host_id());
    let mut parts = vec![format!("·{}", route(reach, entry.host_id() == local_host))];
    match reach {
        Reach::Offline(Offline::Revoked) => parts.push("no longer trusts this machine".into()),
        Reach::Offline(Offline::SignedOut) => parts.push("this machine is signed out".into()),
        // Why it is away comes before any old error from dialing it
        // directly, which is not the reason.
        Reach::Away => parts.push(crate::chat::FREE_PLAN_AWAY.into()),
        // Signing in matters only for reaching a host through the relay.
        Reach::Online(RouteKind::Relay) | Reach::Offline(Offline::Plain)
            if entry.account_signed_in() == Some(false) =>
        {
            parts.push("not signed in".into());
        }
        Reach::Online(_) | Reach::Offline(Offline::Plain) => {}
    }
    if let Some(error) = entry
        .offline()
        .and_then(|offline| offline.last_dial_error.as_ref())
        .filter(|e| !e.is_empty())
    {
        parts.push(error.clone());
    }
    parts.join(" · ")
}

pub(crate) fn shell_target(target: &str) -> String {
    if target
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        target.to_owned()
    } else {
        format!("'{}'", target.replace('\'', "'\\''"))
    }
}

/// The trusted hosts, then what discovery found, each by name.
pub fn listed(fleet: &FleetState) -> Vec<&HostEntry> {
    let mut hosts: Vec<&HostEntry> = fleet.hosts().collect();
    hosts.sort_by(|a, b| {
        (a.trusted().is_none())
            .cmp(&(b.trusted().is_none()))
            .then(a.name().to_lowercase().cmp(&b.name().to_lowercase()))
            .then(a.host_id().cmp(b.host_id()))
    });
    hosts
}

/// The hosts modal's rows: one line per host, its name (this machine said
/// so), then how it is reached ("direct", "relay") or why not ("away · not
/// signed in"); what discovery found, faint, with the command that pairs
/// it; and a faint footer on pairing another.
pub fn modal_rows(fleet: &FleetState, local_host: &[u8], theme: Theme) -> Vec<Line<'static>> {
    let hosts = listed(fleet);
    let names: Vec<String> = hosts
        .iter()
        .map(|entry| {
            if entry.name().is_empty() {
                "unnamed host".to_owned()
            } else {
                entry.name().to_owned()
            }
        })
        .collect();
    let column = names
        .iter()
        .map(|name| text::str_width(name))
        .max()
        .unwrap_or(0)
        + 3;
    let mut rows = vec![Line::default()];
    if hosts.is_empty() {
        rows.push(Line::from(Span::styled("No hosts known", theme.muted())));
    }
    for (entry, name) in hosts.iter().zip(names) {
        let trusted = entry.trusted().is_some();
        let mut line = Line::default();
        let ink = if trusted { theme.text() } else { theme.faint() };
        line.spans.push(Span::styled(name.clone(), ink));
        pad_to(&mut line, column);
        let words = if entry.host_id() == local_host {
            "this machine".to_owned()
        } else if trusted {
            caption(fleet, entry, local_host)
                .trim_start_matches('·')
                .to_owned()
        } else {
            format!("found nearby · amux pair {}", shell_target(&name))
        };
        line.spans.push(Span::styled(
            words,
            if trusted {
                theme.muted()
            } else {
                theme.faint()
            },
        ));
        rows.push(line);
    }
    rows.push(Line::default());
    rows.push(Line::from(vec![
        Span::styled("Pair another machine with ", theme.faint()),
        Span::styled("amux pair", theme.code()),
    ]));
    rows.push(Line::from(vec![
        Span::styled("esc", theme.muted()),
        Span::styled(" close", theme.faint()),
    ]));
    rows
}
