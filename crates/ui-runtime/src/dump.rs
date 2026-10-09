//! What a client writes into a dump bundle: the structure of its model and
//! of its trace, never a record's content. Keys, orders, revisions, input
//! ids and states, markers and the driver's own acts go in; item text and
//! bodies, snapshot bodies, input payloads and an inventory row's names,
//! paths and status text stay out. The daemon's part of the same bundle
//! carries the content, redacted per kind by the only code that can decode
//! it; what the client adds is the one thing the daemon cannot reproduce,
//! the order this client saw things in.

use std::fmt::Write as _;

use ui_state::{FleetMsg, FleetState, InputOutcome, InputWhat, Msg, SessionState};
use wire::{
    Agent, HostEntry, InventoryEvent, Item, SendInputResponse, SessionEvent, inventory_event,
    send_input_response, session_event,
};

use crate::session::hex;
use crate::trace::Structure;

impl Structure for SessionState {
    fn structure(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "agent {}", agent(self.agent()));
        if let Some(host) = self.host() {
            let _ = writeln!(out, "host {}", host_entry(host));
        }
        let _ = writeln!(
            out,
            "connection={:?} caught_up={} epoch={} reset_pending={}",
            self.connection(),
            self.caught_up(),
            self.epoch(),
            self.reset_pending()
        );
        let state = self.agent_state();
        if self.has_snapshot() {
            let _ = writeln!(
                out,
                "snapshot rev={} phase={:?} at_ms={} queue=[{}] asks=[{}]",
                state.revision,
                state.phase,
                state.at_ms,
                state
                    .queue
                    .iter()
                    .map(|queued| hex(&queued.input_id))
                    .collect::<Vec<_>>()
                    .join(" "),
                state
                    .asks
                    .iter()
                    .map(|ask| ask.key())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        } else {
            out.push_str("snapshot none\n");
        }
        let transcript = self.transcript();
        let _ = writeln!(
            out,
            "items {} has_older={} cap={} following={} held={} moved_on={}",
            transcript.len(),
            transcript.has_older(),
            self.cap(),
            self.following(),
            self.held_arrivals(),
            self.head_moved_on()
        );
        for held in transcript.iter() {
            let _ = writeln!(out, "  {} class={}", item(&held.item), variant(&held.class));
        }
        let _ = writeln!(out, "inputs {}", self.inputs().iter().count());
        for sent in self.inputs().iter() {
            let _ = writeln!(
                out,
                "  {} {} {:?}",
                hex(&sent.id),
                what(&sent.what),
                sent.state
            );
        }
        out
    }
}

impl Structure for Msg {
    fn structure(&self) -> String {
        match self {
            Msg::Event(event) => session_event(event),
            Msg::Page {
                items,
                exhausted,
                epoch,
            } => format!(
                "Page epoch={epoch} exhausted={exhausted} [{}]",
                items.iter().map(item).collect::<Vec<_>>().join(", ")
            ),
            Msg::Connection(connection) => format!("Connection {connection:?}"),
            Msg::Send(input) => format!(
                "Send {} {}",
                hex(&input.input_id),
                what(&InputWhat::of(input))
            ),
            Msg::Sent(id, outcome) => format!("Sent {} {}", hex(id), input_outcome(outcome)),
            Msg::Discard(id) => format!("Discard {}", hex(id)),
            Msg::Entry(entry) => format!("Entry {}", agent(entry)),
            Msg::Host(host) => format!("Host {}", host_entry(host)),
            Msg::Blob { hash, status } => format!("Blob {} {}", hex(hash), variant(status)),
            Msg::Following(following) => format!("Following {following}"),
            Msg::Reloading => "Reloading".into(),
            Msg::Window(cap) => format!("Window {cap}"),
            Msg::Catalogue { hash, .. } => format!("Catalogue {}", hex(hash)),
        }
    }
}

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
        }
    }
}

fn session_event(event: &SessionEvent) -> String {
    use session_event::Of;
    match &event.of {
        Some(Of::Snapshot(snapshot)) => format!(
            "Snapshot rev={} phase={:?} queue=[{}]",
            snapshot.revision,
            snapshot.phase(),
            snapshot
                .queue
                .iter()
                .map(|queued| hex(&queued.input_id))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Some(Of::Item(held)) => format!("Item {}", item(held)),
        Some(Of::Append(append)) => format!(
            "Append {} base={} rev={}",
            append.key, append.base_revision, append.revision
        ),
        Some(Of::CaughtUp(caught_up)) => format!("CaughtUp rev={}", caught_up.revision),
        Some(Of::Lagged(_)) => "Lagged".into(),
        Some(Of::Reset(_)) => "Reset".into(),
        Some(Of::Detached(_)) => "Detached".into(),
        Some(Of::Opening(opening)) => format!("Opening gen={}", opening.generation),
        None => "Event none".into(),
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
        None => "Event none".into(),
    }
}

fn item(item: &Item) -> String {
    let mut out = format!(
        "{} order={} rev={} kind={}",
        item.key, item.order, item.revision, item.kind
    );
    if !item.input_id.is_empty() {
        let _ = write!(out, " input={}", hex(&item.input_id));
    }
    out
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
        "{} {:?} {:?} {:?} generation={}",
        hex(&host.host_id),
        host.presence(),
        host.via(),
        host.trust(),
        host.generation
    )
}

fn what(what: &InputWhat) -> String {
    match what {
        InputWhat::Prompt { attachments, .. } => {
            format!("prompt attachments={}", attachments.len())
        }
        InputWhat::Answer { ask_key } => format!("answer ask={ask_key}"),
        InputWhat::Withdraw { target } => format!("withdraw {}", hex(target)),
        InputWhat::SendNow { target } => format!("send_now {}", hex(target)),
        InputWhat::Interrupt => "interrupt".into(),
        InputWhat::Other => "other".into(),
    }
}

fn input_outcome(outcome: &InputOutcome) -> String {
    match outcome {
        InputOutcome::Reply(SendInputResponse {
            of: Some(send_input_response::Of::Accepted(accepted)),
        }) => format!("accepted queued={}", accepted.queued),
        InputOutcome::Reply(SendInputResponse {
            of: Some(send_input_response::Of::Rejected(rejected)),
        }) => format!("rejected {:?}", rejected.refusal()),
        InputOutcome::Reply(SendInputResponse { of: None }) => "no verdict".into(),
        InputOutcome::Lost => "lost".into(),
    }
}

/// An enum value's variant name alone, without the fields it carries.
fn variant(value: &impl std::fmt::Debug) -> String {
    let text = format!("{value:?}");
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    text[..end].to_owned()
}
