//! Retention: keeping own rows and replica rows under their budgets.
//!
//! Own rows are the truth, so they go in the order that loses least:
//! finished history first (exited agents, whole, least recently active
//! first), and only then the oldest rows of whatever live work is largest,
//! a chunk at a time, never below the newest rows a stream may still be
//! appending to. An exited child whose parent's row says live is live work,
//! since its parent can still continue it. Replica rows are a cache, so
//! agents nobody is following lose all their rows, least recently used
//! first, keeping only their agents row, and the rest keep their newest rows
//! as one contiguous block.

use std::collections::{HashMap, HashSet};

use crate::rows::{self, BLOCK, EVICTED, Rows};
use crate::{AgentKey, AgentRow, StoreError};

/// What a sweep did, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    pub pool_before: u64,
    pub pool_after: u64,
    /// Agents removed whole, in removal order: the daemon deletes their
    /// directories. A replica keeps its agents row; only its rows and blobs
    /// go.
    pub removed: Vec<AgentKey>,
    pub steps: Vec<SweepStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SweepStep {
    /// An agent removed whole, with its rows' bytes; a replica keeps its
    /// agents row with an empty block.
    Removed { agent: AgentKey, bytes: u64 },
    /// An agent's oldest rows removed; its block now starts at `from_order`.
    Trimmed {
        agent: AgentKey,
        rows: u64,
        bytes: u64,
        from_order: u64,
    },
    /// Every candidate is at its protected rows and the pool is still over:
    /// from here the budget is soft.
    Floor,
}

fn is_live(row: &AgentRow) -> bool {
    row.lifecycle == wire::Lifecycle::Live as i32
}

pub(crate) fn sweep_own(
    rows: &Rows<'_>,
    budget: u64,
    chunk: u64,
    floor_k: u32,
) -> Result<Sweep, StoreError> {
    let mut pool = rows.pool_bytes(true)?;
    let mut sweep = Sweep {
        pool_before: pool,
        ..Sweep::default()
    };
    if pool > budget {
        pool = remove_finished(rows, budget, pool, &mut sweep)?;
    }
    if pool > budget {
        let candidates = rows
            .agents()?
            .into_iter()
            .filter(|row| rows.is_own(&row.agent) && is_live(row))
            .map(|row| row.agent)
            .collect::<Vec<_>>();
        pool = trim_rounds(
            rows,
            &candidates,
            budget,
            pool,
            Some(chunk),
            floor_k,
            true,
            &mut sweep,
        )?;
    }
    sweep.pool_after = pool;
    Ok(sweep)
}

/// Removes exited agents whole, least recently active first, until the pool
/// fits. A parent goes with its descendants, as delete takes them; one with
/// a live descendant is still live work and stays.
fn remove_finished(
    tables: &Rows<'_>,
    budget: u64,
    mut pool: u64,
    sweep: &mut Sweep,
) -> Result<u64, StoreError> {
    let rows = tables.agents()?;
    let by_ref = rows
        .iter()
        .map(|row| (row.agent.clone(), row))
        .collect::<HashMap<_, _>>();
    let mut children: HashMap<&AgentKey, Vec<&AgentRow>> = HashMap::new();
    for row in &rows {
        if let Some(parent) = &row.parent {
            children.entry(parent).or_default().push(row);
        }
    }
    let descendants = |root: &AgentRow| {
        let mut all = Vec::new();
        let mut stack = vec![root];
        while let Some(row) = stack.pop() {
            for child in children.get(&row.agent).into_iter().flatten() {
                if tables.is_own(&child.agent) {
                    all.push(*child);
                    stack.push(child);
                }
            }
        }
        all
    };
    let protected = |row: &AgentRow| {
        row.parent
            .as_ref()
            .and_then(|parent| by_ref.get(parent))
            .is_some_and(|parent| is_live(parent))
    };
    let mut candidates = rows
        .iter()
        .filter(|row| tables.is_own(&row.agent) && !is_live(row) && !protected(row))
        .filter(|row| descendants(row).iter().all(|child| !is_live(child)))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|row| {
        (
            row.last_activity_ms().unwrap_or(i64::MIN),
            row.agent.clone(),
        )
    });

    let mut gone = HashSet::new();
    for row in candidates {
        if pool <= budget {
            break;
        }
        if gone.contains(&row.agent) {
            continue;
        }
        let mut family = vec![row];
        family.extend(descendants(row));
        for member in family {
            if !gone.insert(member.agent.clone()) {
                continue;
            }
            let (bytes, _) = tables.agent_bytes(&member.agent)?;
            tables.remove_agent(&member.agent)?;
            pool = pool.saturating_sub(bytes);
            sweep.removed.push(member.agent.clone());
            sweep.steps.push(SweepStep::Removed {
                agent: member.agent.clone(),
                bytes,
            });
        }
    }
    Ok(pool)
}

/// Trims the largest candidate with rows above its floor, a round at a
/// time, until the pool fits or every candidate is at its floor. With no
/// chunk a round trims the agent straight down to its floor.
#[allow(clippy::too_many_arguments)]
fn trim_rounds(
    tables: &Rows<'_>,
    candidates: &[AgentKey],
    budget: u64,
    mut pool: u64,
    chunk: Option<u64>,
    floor_k: u32,
    own: bool,
    sweep: &mut Sweep,
) -> Result<u64, StoreError> {
    while pool > budget {
        let mut largest: Option<(u64, &AgentKey, u64)> = None;
        for agent in candidates {
            let (bytes, rows) = tables.agent_bytes(agent)?;
            if rows > u64::from(floor_k) && largest.is_none_or(|(size, ..)| bytes > size) {
                largest = Some((bytes, agent, rows));
            }
        }
        let Some((_, agent, rows)) = largest else {
            sweep.steps.push(SweepStep::Floor);
            break;
        };
        let spare = rows - u64::from(floor_k);
        let oldest = tables.oldest_items(agent, spare.min(u64::from(u32::MAX)) as u32)?;
        let mut removed_rows = 0;
        let mut removed_bytes = 0;
        for (_, bytes) in &oldest {
            removed_rows += 1;
            removed_bytes += bytes;
            if chunk.is_some_and(|chunk| removed_bytes >= chunk) {
                break;
            }
        }
        let from_order = match oldest.get(removed_rows as usize) {
            Some((order, _)) => *order,
            None => tables
                .oldest_items(agent, removed_rows as u32 + 1)?
                .last()
                .map_or(0, |(order, _)| *order),
        };
        tables.remove_items_below(agent, from_order)?;
        let mut row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        row.complete_from_order = Some(from_order);
        // Own history below the boundary is gone for good; a replica's is
        // still at the origin.
        row.exhausted = own;
        tables.upsert_agent(&row, &rows::update_of(BLOCK))?;
        pool = pool.saturating_sub(removed_bytes);
        sweep.steps.push(SweepStep::Trimmed {
            agent: agent.clone(),
            rows: removed_rows,
            bytes: removed_bytes,
            from_order,
        });
    }
    Ok(pool)
}

/// Drops a replica's rows and empties its block, keeping its agents row:
/// the row is the origin's registry entry, which says whether a parent is
/// live and which incarnation a child's delivery names. It goes only when
/// the origin stops listing the agent or its host rewinds.
fn evict_replica(tables: &Rows<'_>, row: &AgentRow) -> Result<(), StoreError> {
    tables.remove_items(&row.agent)?;
    let mut row = row.clone();
    row.complete_from_order = None;
    row.exhausted = false;
    row.source_cursor = 0;
    row.source_generation = 0;
    tables.upsert_agent(&row, &rows::update_of(EVICTED))
}

pub(crate) fn sweep_replicas(
    tables: &Rows<'_>,
    budget: u64,
    floor_k: u32,
    sourced: &HashSet<AgentKey>,
    last_used: &HashMap<AgentKey, i64>,
) -> Result<Sweep, StoreError> {
    let mut pool = tables.pool_bytes(false)?;
    let mut sweep = Sweep {
        pool_before: pool,
        ..Sweep::default()
    };
    let replicas = tables
        .agents()?
        .into_iter()
        .filter(|row| !tables.is_own(&row.agent))
        .collect::<Vec<_>>();
    let mut unsourced = replicas
        .iter()
        .filter(|row| !sourced.contains(&row.agent))
        .collect::<Vec<_>>();
    // One never read here goes by when its phase began, which every
    // replica row holds from its origin's inventory. Last activity is not
    // used: it is written only when a snapshot is absorbed, so a replica
    // nobody opened has none.
    unsourced.sort_by_key(|row| {
        (
            last_used
                .get(&row.agent)
                .copied()
                .or(row.phase_since_ms)
                .unwrap_or(i64::MIN),
            row.agent.clone(),
        )
    });
    for row in unsourced {
        if pool <= budget {
            break;
        }
        let (bytes, rows) = tables.agent_bytes(&row.agent)?;
        if rows == 0 {
            // Evicted already, or never held anything.
            continue;
        }
        evict_replica(tables, row)?;
        pool = pool.saturating_sub(bytes);
        sweep.removed.push(row.agent.clone());
        sweep.steps.push(SweepStep::Removed {
            agent: row.agent.clone(),
            bytes,
        });
    }
    if pool > budget {
        let candidates = replicas
            .iter()
            .filter(|row| sourced.contains(&row.agent))
            .map(|row| row.agent.clone())
            .collect::<Vec<_>>();
        // Rows a Reset left below the block are served by Get but are not
        // history anyone pages through, so they go first, and whole. What
        // the rounds then trim is the block alone, and its boundary can
        // only move up to a row inside it: counting a stale row as the
        // block's oldest would leave the boundary claiming contiguity over
        // the hole between it and the block.
        for agent in &candidates {
            if pool <= budget {
                break;
            }
            let row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            let floor = match row.complete_from_order {
                Some(floor) => floor,
                // No block yet: nothing held is part of one.
                None => tables.max_position(agent)?.map_or(0, |order| order + 1),
            };
            let (before_bytes, before_rows) = tables.agent_bytes(agent)?;
            tables.remove_items_below(agent, floor)?;
            let (after_bytes, after_rows) = tables.agent_bytes(agent)?;
            if before_rows > after_rows {
                let bytes = before_bytes - after_bytes;
                pool = pool.saturating_sub(bytes);
                sweep.steps.push(SweepStep::Trimmed {
                    agent: agent.clone(),
                    rows: before_rows - after_rows,
                    bytes,
                    from_order: floor,
                });
            }
        }
        pool = trim_rounds(
            tables,
            &candidates,
            budget,
            pool,
            None,
            floor_k,
            false,
            &mut sweep,
        )?;
    }
    sweep.pool_after = pool;
    Ok(sweep)
}
