//! Calls between paired hosts.
//!
//! A paired host's call arrives as [`Input::PeerCall`](crate::Input) and is
//! answered only about this host's own agents: what this host holds of
//! another host's agents is that host's to tell, so a call naming one is
//! not found here rather than passed on.
//!
//! A call this host makes of a peer for itself leaves as [`Output::Call`]
//! under an id the core mints, and waits in the pending set until what came
//! back arrives as [`Input::PeerAnswer`](crate::Input). A client's call on
//! an agent another host owns never reaches the core: the node carries it
//! to that host unread.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wire::follow_session_request;

use crate::ids::{CallId, HostId};
use crate::io::Output;
use crate::payload::{Answer, CallError, Opening, PeerRead};
use crate::serve::Key;
use crate::{Daemon, Pending, codec};

/// The ids the core mints for its own calls carry this bit; the driver
/// names arriving calls from a counter that never reaches it, so the two
/// never meet.
const MINTED: u64 = 1 << 63;

/// The calls the core made of peers, awaiting what comes back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PeerCalls {
    /// The last call id minted, below [`MINTED`].
    last: u64,
    pub(crate) waiting: BTreeMap<CallId, Waiting>,
}

/// What a call made of a peer is for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Waiting {
    /// A host's inventory, followed.
    Inventory { host: HostId },
    /// The stream that feeds a replica agent.
    Source { agent: Key },
    /// A page below a replica's block, for a client's Fetch.
    Page(Box<Page>),
    /// A child's report, sent to its parent's host as a message from the
    /// child: the outbox row its envelope id names.
    Report {
        #[serde(with = "codec::agent_key")]
        parent: store::AgentKey,
        id: Vec<u8>,
    },
}

/// A client's Fetch that went below a replica's block: the call it
/// answers, where the origin's page joins the block, the host session it
/// was asked under, the held items that lead the origin's, and whether a
/// Reset has replaced the block since.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Page {
    pub(crate) call: CallId,
    pub(crate) agent: Key,
    pub(crate) before_order: Option<u64>,
    pub(crate) session: u64,
    #[serde(with = "codec::messages")]
    pub(crate) held: Vec<wire::Item>,
    pub(crate) replaced: bool,
}

impl PeerCalls {
    pub(crate) fn mint(&mut self) -> CallId {
        self.last += 1;
        minted(self.last)
    }

    /// The inventory sessions the calls waiting name.
    pub(crate) fn sessions(&self, out: &mut Vec<u64>) {
        out.extend(self.waiting.values().filter_map(|waiting| match waiting {
            Waiting::Page(page) => Some(page.session),
            _ => None,
        }));
    }

    /// A Reset replaced the agent's block. A page asked below the old one
    /// may still meet the new one at the same order, but the origin read it
    /// before the rows the Reset's cursor now vouches for, and a row it
    /// revised since lies below the new tail, where no stream sends it
    /// again: such a page is answered and never absorbed.
    pub(crate) fn block_replaced(&mut self, agent: &Key) {
        for waiting in self.waiting.values_mut() {
            if let Waiting::Page(page) = waiting
                && page.agent == *agent
            {
                page.replaced = true;
            }
        }
    }

    /// The calls waiting under their canonical ids, with no count of the
    /// ones minted before.
    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> PeerCalls {
        let waiting = self
            .waiting
            .iter()
            .map(|(call, waiting)| {
                let mut waiting = waiting.clone();
                if let Waiting::Page(page) = &mut waiting {
                    page.session = ids.sessions.get(page.session);
                }
                (ids.calls.get(*call), waiting)
            })
            .collect();
        PeerCalls { last: 0, waiting }
    }
}

/// The `n`th call id the core mints.
pub(crate) fn minted(n: u64) -> CallId {
    CallId(MINTED | n)
}

impl Pending {
    /// The calls made of peers that have not come back.
    pub fn peer_calls(&self) -> usize {
        self.peers.waiting.len()
    }
}

impl Daemon {
    /// A paired host's read. A report it hands over reaches the core as a
    /// message.
    pub(crate) fn peer_call(&mut self, call: CallId, request: PeerRead, out: &mut Vec<Output>) {
        let own = self.store.own_host().to_vec();
        let answered = match request {
            PeerRead::FollowAgents => self.subscribe_inventory(call, Some(own), out),
            PeerRead::FollowSession(request) => {
                let from = match request.from {
                    Some(follow_session_request::From::After(after)) => Opening::After {
                        revision: after.revision,
                        cap: after.cap,
                        generation: after.generation,
                    },
                    Some(follow_session_request::From::Tail(tail)) => Opening::Tail(tail),
                    None => Opening::Tail(0),
                };
                self.own(&request.agent_id)
                    .and_then(|()| self.subscribe_session(call, &request.agent_id, from, out))
            }
            PeerRead::ListItems(request) => self
                .own(&request.agent_id)
                .and_then(|()| self.list_items(call, &request, out)),
        };
        self.end_unanswered(call, answered, out);
    }

    /// Refuses a call on an agent this host does not run.
    fn own(&self, agent_id: &[u8]) -> Result<(), CallError> {
        let key = store::AgentKey::new(self.store.own_host(), agent_id);
        match self.store.agent(&key)? {
            Some(_) => Ok(()),
            None => Err(CallError::NoAgent),
        }
    }

    /// What came back of a call the core made of a peer.
    pub(crate) fn peer_answered(&mut self, made: CallId, answer: Answer, out: &mut Vec<Output>) {
        let waiting = if answer.is_last() {
            self.pending.peers.waiting.remove(&made)
        } else {
            self.pending.peers.waiting.get(&made).cloned()
        };
        let Some(waiting) = waiting else {
            return;
        };
        match waiting {
            Waiting::Inventory { host } => self.inventory_answered(host, made, answer, out),
            Waiting::Source { agent } => self.source_answered(&agent, made, answer, out),
            Waiting::Page(page) => self.page_answered(*page, answer, out),
            Waiting::Report { parent, id } => self.report_answered(&parent, &id, answer, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use store::{AgentKey, AgentRow, Sqlite};

    use super::*;
    use crate::payload::CarrierKind;
    use crate::{Input, LinkId, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const PEER: [u8; 16] = *b"far-host-0000000";
    const MINE: [u8; 16] = *b"agent-0000000000";
    const THEIRS: [u8; 16] = *b"agent-1111111111";

    fn store() -> Sqlite {
        let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
        for key in [AgentKey::new(OWN, MINE), AgentKey::new(PEER, THEIRS)] {
            let row = AgentRow::new(key, wire::Kind::Codex, "/src");
            store.put_agent(&row).expect("the store takes its agent");
        }
        store
    }

    fn daemon() -> Daemon {
        Daemon::new(store(), StartOptions::default())
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    #[test]
    fn a_replayed_early_read_changes_nothing_the_store_holds() {
        let mut daemon = daemon();
        let before = daemon.store().dump().expect("the store dumps");
        let read = || {
            crate::payload::PeerRead::FollowSession(wire::FollowSessionRequest {
                agent_id: MINE.to_vec(),
                from: Some(follow_session_request::From::Tail(8)),
            })
        };
        for (link, call) in [(1, minted(1)), (2, minted(2))] {
            step(
                &mut daemon,
                Input::LinkUp {
                    link: LinkId(link),
                    peer: HostId(PEER),
                    kind: CarrierKind::Direct,
                    lan: false,
                    resumed: None,
                },
            );
            let out = step(
                &mut daemon,
                Input::PeerCall {
                    from: HostId(PEER),
                    call,
                    request: read(),
                },
            );
            assert!(
                out.iter()
                    .any(|output| matches!(output, Output::Answer { call: to, .. } if *to == call)),
                "{out:?}"
            );
        }
        step(
            &mut daemon,
            Input::LinkDown {
                link: LinkId(2),
                reason: wire::LinkCloseReason::UserShutdown,
            },
        );
        assert_eq!(daemon.store().dump().expect("the store dumps"), before);
    }
}
