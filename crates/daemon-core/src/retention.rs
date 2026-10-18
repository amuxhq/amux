//! Retention: the store's sweeps, as one step.
//!
//! The store decides what goes (exited agents whole, least recently active
//! first, never an exited child whose parent is live; then the largest live
//! agent trimmed a chunk at a time, never below its newest rows; replicas
//! with no source least recently used first, the rest trimmed to their
//! tail with the block kept whole) and says which agents it removed. The
//! core then does what the store cannot: ends the removed agents' streams,
//! tells the fleet they are gone and has their children here follow. Their
//! directories, which take their blobs with them, are the driver's to
//! delete before it reports the pass, as is every replica blob file over
//! its budget; the core says which directories go, and which catalogues a
//! remaining replica's snapshot still names, which no sweep may take.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::{AgentKey, Sweep};
use wire::{AgentRemoved, RemovalReason, inventory_event};

use crate::blobs::{BLOBS, hex};
use crate::ids::AgentId;
use crate::io::{Output, ParentSays};
use crate::serve::inventory;
use crate::{Daemon, codec};

/// One retention pass: the budgets it keeps the store under, and when
/// clients last used each replica, which the replica sweep evicts by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retain {
    /// The bytes this host's own rows may take.
    pub own_budget_bytes: u64,
    /// How much a live agent over the budget is trimmed by at a time.
    pub chunk_bytes: u64,
    /// The bytes replica rows may take.
    pub replica_budget_bytes: u64,
    #[serde(with = "codec::last_used")]
    pub last_used: HashMap<AgentKey, i64>,
}

/// What a retention pass did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retained {
    /// The own sweep, or why it failed.
    pub own: Result<Sweep, String>,
    /// The replica sweep, or why it failed.
    pub replicas: Result<Sweep, String>,
    /// The directories of the own agents removed, rows first: a crash
    /// before one goes leaves a directory with no row, which the next
    /// start removes.
    pub removed_dirs: Vec<PathBuf>,
    /// The catalogue blob files remaining replicas' snapshots name.
    pub pinned: HashSet<PathBuf>,
}

impl Daemon {
    /// Runs both sweeps, own rows first.
    pub(crate) fn retain(&mut self, retain: Retain, out: &mut Vec<Output>) {
        let own = self.sweep_own(&retain, out);
        let removed_dirs = match &own {
            Ok(sweep) => sweep
                .removed
                .iter()
                .filter_map(|key| AgentId::from_slice(&key.agent))
                .map(|agent| self.agent_dir(agent))
                .collect(),
            Err(_) => Vec::new(),
        };
        let replicas = self.sweep_replicas(&retain);
        let pinned = self.pinned_catalogues();
        out.push(Output::Retained(Box::new(Retained {
            own,
            replicas,
            removed_dirs,
            pinned,
        })));
    }

    fn sweep_own(&mut self, retain: &Retain, out: &mut Vec<Output>) -> Result<Sweep, String> {
        let sweep = self
            .store
            .sweep_own(
                retain.own_budget_bytes,
                retain.chunk_bytes,
                self.settings.tail_rows,
            )
            .map_err(|error| error.to_string())?;
        for key in &sweep.removed {
            self.removed_for_good(key, RemovalReason::Retention, out);
        }
        Ok(sweep)
    }

    fn sweep_replicas(&mut self, retain: &Retain) -> Result<Sweep, String> {
        let sourced: HashSet<AgentKey> = self.sources.open().collect();
        self.store
            .sweep_replicas(
                retain.replica_budget_bytes,
                self.settings.tail_rows,
                &sourced,
                &retain.last_used,
            )
            .map_err(|error| error.to_string())
    }

    /// The catalogue a remaining replica's current snapshot names: without
    /// it the agent's picker would lose its offers while the origin is
    /// away.
    fn pinned_catalogues(&self) -> HashSet<PathBuf> {
        let own = self.store.own_host();
        self.store
            .agents()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.agent.host != own)
            .filter_map(|row| {
                let hash = row.snapshot.as_ref()?.catalogue.as_ref()?;
                Some(self.files_of(&row).join(BLOBS).join(hex(hash)))
            })
            .collect()
    }

    /// The agent is gone from the store for good, as a delete leaves it: its
    /// streams end, the inventory says why, its children here follow and
    /// its draft is forgotten.
    pub(crate) fn removed_for_good(
        &mut self,
        agent: &AgentKey,
        reason: RemovalReason,
        out: &mut Vec<Output>,
    ) {
        let removed = AgentRemoved {
            host_id: agent.host.clone(),
            agent_id: agent.agent.clone(),
            reason: reason as i32,
        };
        let removed = Arc::new(inventory(inventory_event::Of::AgentRemoved(removed)));
        self.agent_removed(agent, removed, out);
        self.forget_draft(&agent.agent, out);
        if let Err(error) = self.parent_changed(agent, ParentSays::Deleted, out) {
            tracing::warn!(%error, "following a removed parent failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use store::{AgentRow, CommitClock, Sqlite};
    use wire::{Item, Lifecycle, Step, inventory_event};

    use super::*;
    use crate::payload::{ClientRequest, Status};
    use crate::{CallId, Input, StartOptions};

    const OWN: &[u8; 16] = b"own-host-0000000";
    const AGENT: &[u8; 16] = b"agent-0000000000";

    /// A daemon whose one own agent has exited with a row of history.
    fn daemon() -> Daemon {
        let mut store = Sqlite::open_in_memory(*OWN).expect("a store opens");
        let key = AgentKey::new(*OWN, *AGENT);
        let mut row = AgentRow::new(key.clone(), wire::Kind::Codex, "/src");
        row.lifecycle = Lifecycle::Exited as i32;
        store.put_agent(&row).expect("the store takes its agent");
        let step = Step {
            items: vec![Item {
                key: "said".into(),
                text: "hello".into(),
                ..Item::default()
            }],
            ..Step::default()
        };
        store
            .commit(
                &key,
                &[(1, step)],
                CommitClock {
                    now_ms: 0,
                    notify_delay_ms: 0,
                },
            )
            .expect("the store takes its row");
        Daemon::new(
            store,
            StartOptions {
                agents: "/agents".into(),
                ..StartOptions::default()
            },
        )
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    #[test]
    fn an_exited_agent_over_the_budget_is_removed_with_its_streams_and_its_directory_named() {
        let mut daemon = daemon();
        let key = AgentKey::new(*OWN, *AGENT);
        let subscribe = ClientRequest::Subscribe {
            agent_id: AGENT.to_vec(),
            tail: 10,
        };
        step(
            &mut daemon,
            Input::ClientCall {
                call: CallId(1),
                request: subscribe,
            },
        );
        step(
            &mut daemon,
            Input::ClientCall {
                call: CallId(2),
                request: ClientRequest::SubscribeInventory,
            },
        );

        let out = step(&mut daemon, Input::Retain(Retain::default()));

        assert!(
            out.contains(&Output::EndCall {
                call: CallId(1),
                status: Status::Ok,
            }),
            "its stream ends: {out:#?}"
        );
        let told = out.iter().any(|output| match output {
            Output::Answer { call, response } if *call == CallId(2) => matches!(
                response,
                crate::payload::Response::Inventory(event)
                    if matches!(&event.of, Some(inventory_event::Of::AgentRemoved(removed))
                        if removed.reason == RemovalReason::Retention as i32)
            ),
            _ => false,
        });
        assert!(told, "the inventory says why it went: {out:#?}");
        let retained = out.iter().find_map(|output| match output {
            Output::Retained(retained) => Some(retained),
            _ => None,
        });
        let retained = retained.expect("the pass says what it did");
        assert_eq!(retained.own.as_ref().unwrap().removed, vec![key.clone()]);
        assert_eq!(
            retained.removed_dirs,
            vec![daemon.agent_dir(AgentId(*AGENT))],
            "its directory goes"
        );
        assert!(retained.replicas.is_ok());
        assert_eq!(daemon.store().agent(&key).unwrap(), None, "and its row");
    }

    #[test]
    fn a_pass_under_its_budget_removes_nothing() {
        let mut daemon = daemon();
        let retain = Retain {
            own_budget_bytes: u64::MAX,
            replica_budget_bytes: u64::MAX,
            ..Retain::default()
        };
        let out = step(&mut daemon, Input::Retain(retain));
        assert_eq!(out.len(), 1, "only the report: {out:#?}");
        let Output::Retained(retained) = &out[0] else {
            panic!("a report, not {out:#?}");
        };
        assert_eq!(
            retained.own.as_ref().unwrap().removed,
            Vec::<AgentKey>::new()
        );
    }
}
