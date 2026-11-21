//! The promises a daemon keeps about provider subagents: the agents
//! Claude's Agent tool and Codex's child threads start inside one of its
//! agents' own provider process, each with a row and a chat and no process
//! of its own.
//!
//! They are judged at every step a core takes wherever one runs: a dump's
//! replay and the composed net both call [`judge`]. Two more are the core's
//! own end-of-step checks, so a production core reports them with a dump and
//! every judge of a step reports that dump:
//! [`daemon_core::SUBAGENT_AFTER_ITS_END`] (a subagent's end comes after its
//! last item) and [`daemon_core::PART_OF_ANOTHER_AGENT`] (each frame's parts
//! name agents the host runs).

use daemon_core::{Daemon, Output};
use wire::Lifecycle;

use crate::single_daemon::Broken;

pub const ROW_BEFORE_ITEMS: &str = "a provider subagent's row exists before its first item: the \
     store holds no item of an agent it holds no row for";
pub const NONE_OUTLIVES_ITS_HOST: &str = "no provider subagent is live after its host's process \
     ends: a live subagent's host is live, and the subagent ends in the same write as its host";
pub const NO_REPORTS: &str = "a provider subagent never gets a delivery or owes a report: its \
     answer goes back to its parent through the provider";

/// The first of the promises above that the daemon's state after a step
/// breaks. Read only after a step that ingested or exited something, since
/// nothing else moves a subagent.
pub fn judge(daemon: &Daemon, out: &[Output]) -> Option<Broken> {
    let moved = out.iter().any(|output| {
        matches!(
            output,
            Output::Ingested { .. } | Output::ForgetAgent { .. } | Output::Retained(_)
        )
    });
    if !moved {
        return None;
    }
    let store = daemon.store();
    let orphan: Option<Vec<u8>> = store
        .connection()
        .query_row(
            "SELECT agent_id FROM items WHERE NOT EXISTS (SELECT 1 FROM agents \
             WHERE agents.origin_host = items.origin_host AND agents.agent_id = items.agent_id) \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(agent) = orphan {
        return Some(Broken {
            promise: ROW_BEFORE_ITEMS,
            detail: format!("items of {} are held with no row", hex(&agent)),
        });
    }
    let own = store.own_host();
    let rows = store.agents().ok()?;
    for row in rows.iter().filter(|row| row.agent.host == own) {
        if row.native.is_none() {
            continue;
        }
        if row.owes_report {
            return Some(Broken {
                promise: NO_REPORTS,
                detail: format!("subagent {} owes a report", hex(&row.agent.agent)),
            });
        }
        if row.lifecycle != Lifecycle::Live as i32 {
            continue;
        }
        let host = row
            .parent
            .as_ref()
            .and_then(|parent| rows.iter().find(|held| held.agent == *parent));
        if host.is_none_or(|host| host.lifecycle != Lifecycle::Live as i32) {
            return Some(Broken {
                promise: NONE_OUTLIVES_ITS_HOST,
                detail: format!(
                    "subagent {} is live and its host is {:?}",
                    hex(&row.agent.agent),
                    host.map(|host| host.lifecycle)
                ),
            });
        }
    }
    let deliveries = store.deliveries().ok()?;
    let native = |id: &[u8]| {
        rows.iter()
            .any(|row| row.agent.agent == id && row.native.is_some())
    };
    if let Some(delivery) = deliveries
        .iter()
        .find(|delivery| native(&delivery.child_id) || native(&delivery.parent.agent))
    {
        return Some(Broken {
            promise: NO_REPORTS,
            detail: format!(
                "a delivery from {} to {} names a subagent",
                hex(&delivery.child_id),
                hex(&delivery.parent.agent)
            ),
        });
    }
    None
}

fn hex(id: &[u8]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use daemon_core::{AgentId, IngestEnd, Ingested, StartOptions};
    use store::{AgentKey, AgentRow, Delivery, Sqlite};

    use super::*;

    const OWN: [u8; 16] = *b"own-host-0000000";

    fn key(name: &[u8; 16]) -> AgentKey {
        AgentKey::new(OWN, name.to_vec())
    }

    /// A store with a live host and its live provider subagent.
    fn store() -> Sqlite {
        let mut store = Sqlite::open_in_memory(OWN).unwrap();
        let mut host = AgentRow::new(key(b"host-00000000000"), wire::Kind::ClaudeSdk, "/src");
        host.name = "host".into();
        store.put_agent(&host).unwrap();
        let mut sub = AgentRow::new(key(b"sub-000000000000"), wire::Kind::ClaudeSdk, "/src");
        sub.name = "sub-73756221".into();
        sub.parent = Some(host.agent.clone());
        sub.owes_report = false;
        sub.native = Some(wire::NativeChild {
            provider_child_id: "task".into(),
        });
        store.put_agent(&sub).unwrap();
        store
    }

    fn judged(store: Sqlite) -> Option<&'static str> {
        let daemon = Daemon::new(store, StartOptions::default());
        let ingested = Output::Ingested {
            agent: AgentId(*b"host-00000000000"),
            report: Ingested {
                frames: 1,
                turn_ends: 0,
                kind: 0,
                provider: String::new(),
                end: IngestEnd::Read,
                seen: 0,
                skipped: 0,
            },
        };
        judge(&daemon, &[ingested]).map(|broken| broken.promise)
    }

    fn with_row(mut store: Sqlite, name: &[u8; 16], change: impl FnOnce(&mut AgentRow)) -> Sqlite {
        let mut row = store.agent(&key(name)).unwrap().unwrap();
        change(&mut row);
        store.put_agent(&row).unwrap();
        store
    }

    #[test]
    fn a_host_with_a_live_subagent_breaks_nothing() {
        assert_eq!(judged(store()), None);
    }

    #[test]
    fn each_planted_break_is_named() {
        let host = |store| {
            with_row(store, b"host-00000000000", |row| {
                row.lifecycle = Lifecycle::Exited as i32;
            })
        };
        assert_eq!(judged(host(store())), Some(NONE_OUTLIVES_ITS_HOST));

        // Only a commit writes what an own row owes, so it is planted below
        // the store's own writes.
        let owes = store();
        owes.connection()
            .execute(
                "UPDATE agents SET owes_report = 1 WHERE agent_id = ?1",
                [b"sub-000000000000".as_slice()],
            )
            .unwrap();
        assert_eq!(judged(owes), Some(NO_REPORTS));

        let mut delivered = store();
        delivered
            .put_delivery(&Delivery {
                child_id: b"sub-000000000000".to_vec(),
                incarnation: 1,
                turn_id: 1,
                parent: key(b"host-00000000000"),
                parent_incarnation: 1,
                end: wire::child_report::End::TurnEnded(wire::TurnEnded::default()),
            })
            .unwrap();
        assert_eq!(judged(delivered), Some(NO_REPORTS));

        let mut orphaned = store();
        let sub = key(b"sub-000000000000");
        let step = wire::Step {
            items: vec![wire::Item {
                key: "said".into(),
                ..wire::Item::default()
            }],
            ..wire::Step::default()
        };
        let clock = store::CommitClock {
            now_ms: 0,
            notify_delay_ms: 0,
        };
        orphaned.commit(&sub, &[(1, step)], clock).unwrap();
        orphaned
            .connection()
            .execute(
                "DELETE FROM agents WHERE agent_id = ?1",
                [sub.agent.as_slice()],
            )
            .unwrap();
        assert_eq!(judged(orphaned), Some(ROW_BEFORE_ITEMS));
    }
}
