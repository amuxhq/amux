//! The replica block invariant: a host's rows for another host's agent are
//! empty, or one contiguous block that ends at the origin's newest row.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use store::{AgentKey, PageEnd};
use wire::Item;

/// How a replica's rows broke the invariant, with both sides for the
/// failure report.
#[derive(Debug, thiserror::Error)]
#[error("{problem}\n  replica: {replica}\n  origin:  {origin}")]
pub struct BlockViolation {
    pub problem: String,
    pub replica: String,
    pub origin: String,
}

/// Every row of an agent's block in `store`, oldest first.
pub(crate) fn block(store: &store::Sqlite, agent: &AgentKey) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();
    // A host that has never heard of the agent holds no rows for it.
    if store
        .agent(agent)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(items);
    }
    let mut before = None;
    loop {
        let page = store
            .page(agent, before, 256)
            .map_err(|error| error.to_string())?;
        before = page.items.last().map(|item| item.order);
        items.extend(page.items);
        if page.end != PageEnd::More || before.is_none() {
            break;
        }
    }
    items.reverse();
    Ok(items)
}

/// Checks `replica` against `origin`, both oldest first, at a settled
/// moment: the block must hold exactly the origin's rows from its lowest
/// order up, at the origin's revisions.
pub(crate) fn check(replica: &[Item], origin: &[Item]) -> Result<(), BlockViolation> {
    let Some(floor) = replica.first().map(|item| item.order) else {
        return Ok(());
    };
    let expected: Vec<&Item> = origin.iter().filter(|item| item.order >= floor).collect();
    let violation = |problem: String| BlockViolation {
        problem,
        replica: orders(replica.iter()),
        origin: orders(origin.iter()),
    };
    let held: BTreeMap<u64, &Item> = replica.iter().map(|item| (item.order, item)).collect();
    let wanted: BTreeMap<u64, &Item> = expected.iter().map(|item| (item.order, *item)).collect();
    if let Some(order) = wanted.keys().find(|order| !held.contains_key(order)) {
        let newest = origin.last().map_or(0, |item| item.order);
        return Err(violation(
            if *order > held.keys().last().copied().unwrap_or(0) {
                format!("the block ends below the origin's newest row (order {newest})")
            } else {
                format!("the block has a hole at order {order}")
            },
        ));
    }
    if let Some(order) = held.keys().find(|order| !wanted.contains_key(order)) {
        return Err(violation(format!(
            "the block holds order {order}, which the origin does not"
        )));
    }
    for (order, item) in &held {
        let origin_item = wanted[order];
        if item.key != origin_item.key || item.revision != origin_item.revision {
            return Err(violation(format!(
                "order {order} holds {} at revision {}, the origin {} at revision {}",
                item.key, item.revision, origin_item.key, origin_item.revision
            )));
        }
    }
    Ok(())
}

fn orders<'a>(items: impl Iterator<Item = &'a Item>) -> String {
    let mut out = String::from("[");
    for (at, item) in items.enumerate() {
        if at > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{}@{}", item.order, item.revision);
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(order: u64, revision: u64) -> Item {
        Item {
            key: format!("k{order}"),
            order,
            revision,
            ..Item::default()
        }
    }

    #[test]
    fn empty_and_whole_tails_pass_and_holes_stale_rows_and_short_tops_fail() {
        let origin = vec![item(1, 1), item(2, 4), item(3, 3)];
        check(&[], &origin).unwrap();
        check(&[item(2, 4), item(3, 3)], &origin).unwrap();
        check(&origin, &origin).unwrap();

        let hole = check(&[item(1, 1), item(3, 3)], &origin).unwrap_err();
        assert!(hole.problem.contains("hole at order 2"), "{hole}");
        let short = check(&[item(1, 1), item(2, 4)], &origin).unwrap_err();
        assert!(short.problem.contains("ends below"), "{short}");
        let stale = check(&[item(2, 2), item(3, 3)], &origin).unwrap_err();
        assert!(stale.problem.contains("revision 2"), "{stale}");
        let extra = check(&[item(3, 3), item(4, 5)], &origin).unwrap_err();
        assert!(extra.problem.contains("order 4"), "{extra}");
    }
}
