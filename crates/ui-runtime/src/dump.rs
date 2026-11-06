//! What the fleet writes into a dump bundle: the structure of its model
//! and of its trace, never a record's content. Keys, ids, states and the
//! driver's own acts go in; an inventory row's names, paths and status text
//! stay out. What the fleet adds is the one thing the daemon cannot
//! reproduce, the order this client saw things in. A chat's session goes
//! to the daemon as its record instead (see `ui_state::record`).

use std::fmt::Write as _;

use ui_state::{FleetMsg, FleetState};
use wire::{Agent, HostEntry, InventoryEvent, host_entry, inventory_event, trusted};

use crate::session::hex;
use crate::trace::Structure;

impl Structure for FleetState {
    fn structure(&self) -> String {
        let mut out = format!(
            "connection={:?} caught_up={}\n",
            self.connection(),
            self.caught_up()
        );
        for host in self.hosts() {
            let _ = writeln!(out, "host {}", host_entry(host));
        }
        for entry in self.agents() {
            let _ = writeln!(out, "agent {}", agent(entry));
        }
        out
    }
}

impl Structure for FleetMsg {
    fn structure(&self) -> String {
        match self {
            FleetMsg::Event(event) => inventory(event),
            FleetMsg::Connection(connection) => format!("Connection {connection:?}"),
            FleetMsg::HoldSettle { agent, listing } => {
                format!("HoldSettle {} listing {listing}", hex(&agent.agent))
            }
            FleetMsg::DropSettle(key) => format!("DropSettle {}", hex(&key.agent)),
        }
    }
}

fn inventory(event: &InventoryEvent) -> String {
    use inventory_event::Of;
    match &event.of {
        Some(Of::Host(host)) => format!("Host {}", host_entry(host)),
        Some(Of::HostRemoved(removed)) => format!("HostRemoved {}", hex(&removed.host_id)),
        Some(Of::Agent(entry)) => format!("Agent {}", agent(entry)),
        Some(Of::AgentRemoved(removed)) => format!(
            "AgentRemoved {}/{}",
            hex(&removed.host_id),
            hex(&removed.agent_id)
        ),
        Some(Of::CaughtUp(caught_up)) => format!("CaughtUp rev={}", caught_up.revision),
        Some(Of::Lagged(_)) => "Lagged".into(),
        None => "Event none".into(),
    }
}

fn agent(agent: &Agent) -> String {
    let mut out = format!(
        "{}/{} {:?} {:?} {:?} incarnation={}",
        hex(&agent.host_id),
        hex(&agent.agent_id),
        agent.kind(),
        agent.lifecycle(),
        agent.phase(),
        agent.incarnation
    );
    if let Some(parent) = &agent.parent {
        let _ = write!(
            out,
            " parent={}/{}",
            hex(&parent.host_id),
            hex(&parent.agent_id)
        );
    }
    out
}

fn host_entry(host: &HostEntry) -> String {
    format!(
        "{} {} generation={}",
        hex(host.host_id()),
        standing(host.standing.as_ref()),
        host.generation
    )
}

/// A host's standing by its kind, flags and counts: never the names of the
/// neighbours that announce it or a dial's error, which are other hosts'
/// words.
fn standing(standing: Option<&host_entry::Standing>) -> String {
    use host_entry::Standing;
    use trusted::Reach;
    match standing {
        Some(Standing::Trusted(trusted)) => {
            let reach = match &trusted.reach {
                Some(Reach::Online(route)) => format!(
                    "online {:?}",
                    wire::RouteKind::try_from(*route).unwrap_or_default()
                ),
                Some(Reach::Away(_)) => "away".to_owned(),
                Some(Reach::Offline(offline)) => format!(
                    "offline dial_error={} revoked={}",
                    offline.last_dial_error.is_some(),
                    offline.revoked
                ),
                None => "unreached".to_owned(),
            };
            format!("trusted {reach} current={}", trusted.current)
        }
        Some(Standing::Candidate(candidate)) => format!(
            "candidate addrs={} announced_by={}",
            candidate.addrs.len(),
            candidate.announced_by.len()
        ),
        None => "no standing".to_owned(),
    }
}
