//! The store tables of experiments, outside the promised migrations, and
//! the clearing of own rows' entries of experiments a host does not run.
//!
//! An experiment's data is never promised: its shape may change in any
//! release, and losing it is the price of shipping it early. So each
//! version of an experiment has tables of its own, declared here rather
//! than in [`MIGRATIONS`](crate::schema), each named with the prefix its
//! version gives (`exp_workspaces_1_` for `workspaces/1`). When the daemon
//! opens a store it creates the tables of the experiments it runs and drops
//! every `exp_` table that does not belong to one of them at the version
//! this build implements. So a new version never reads an old version's
//! rows, switching an experiment off frees its space, and an older build,
//! which knows nothing of `exp_` tables, leaves them alone. No promised
//! migration may name an `exp_` table.
//!
//! Graduating an experiment does not convert its rows: the settled design
//! gets a migration of its own, and the experiment's tables are dropped the
//! next time a store opens, like any version this build does not run.
//!
//! Tables hold a host's own working state, which nobody else reads. What a
//! row says to everyone under an experiment rides on the row instead, in its
//! `experimental` column, so a host with the experiment off still stores and
//! passes on a replica's entries. As the store opens a host clears its own
//! rows of the entries of every version it does not run, since a host speaks
//! only for its own rows; replicas keep what their origins wrote.

use rusqlite::{Connection, TransactionBehavior};

/// One experiment version's tables.
#[derive(Debug)]
pub struct ExperimentTables {
    /// The version whose tables these are, as the settings' registry names
    /// it: `workspaces/1`.
    pub version: &'static str,
    /// The SQL that makes them: `CREATE ... IF NOT EXISTS` statements, each
    /// table and index named with the version's prefix, every table
    /// `STRICT`. Run each time a store opens with the experiment on.
    pub schema: &'static str,
}

impl ExperimentTables {
    /// The prefix every table of this version carries.
    pub fn prefix(&self) -> String {
        prefix(self.version)
    }
}

/// The table prefix of an experiment version: `exp_<name>_<n>_`.
pub fn prefix(version: &str) -> String {
    format!("exp_{}_", version.replace('/', "_"))
}

/// The table [`SUSPENSION_1`] keeps each own agent's run in.
pub const SUSPENSION_1_RUNS: &str = "exp_suspension_1_runs";

/// The incarnation each of this host's own agents began its current run
/// at: a resume after a suspension or a restart continues the run, any
/// other resume starts a new one.
pub const SUSPENSION_1: ExperimentTables = ExperimentTables {
    version: wire::experimental::SUSPENSION.version(),
    schema: r#"
CREATE TABLE IF NOT EXISTS exp_suspension_1_runs (
  agent_id    BLOB    NOT NULL PRIMARY KEY, -- one of this host's own agents
  incarnation INTEGER NOT NULL              -- the spec.<n> its current run began at
) STRICT;
"#,
};

/// Each own agent's review: its snapshots, rounds and comments.
pub const REVIEWS_1: ExperimentTables = ExperimentTables {
    version: wire::experimental::REVIEW_ROW.version(),
    schema: crate::reviews::SCHEMA,
};

/// Every experiment version this build keeps tables for.
pub const EXPERIMENT_TABLES: &[&ExperimentTables] = &[&SUSPENSION_1];

/// Makes the tables of the experiment versions in `on` and drops every
/// other `exp_` table, and keeps on the rows of `own_host` only the
/// experimental entries of the versions in `on`, in one transaction. A
/// version in `on` with no tables declared has nothing to make.
pub(crate) fn settle(conn: &mut Connection, own_host: &[u8], on: &[&str]) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let kept: Vec<String> = on.iter().map(|version| prefix(version)).collect();
    drop_tables(&tx, &kept)?;
    for tables in EXPERIMENT_TABLES {
        if on.contains(&tables.version) {
            tx.execute_batch(tables.schema)?;
        }
    }
    keep_own_entries(&tx, own_host, on)?;
    tx.commit()
}

/// Drops from each of `own_host`'s rows the experimental entries of every
/// version not in `on`; a column that does not decode goes whole, since no
/// experiment's entry is worth failing to open over. Replica rows are left
/// as their origins wrote them.
fn keep_own_entries(conn: &Connection, own_host: &[u8], on: &[&str]) -> rusqlite::Result<()> {
    let held = conn
        .prepare(
            "SELECT rowid, experimental FROM agents \
             WHERE origin_host = ?1 AND experimental IS NOT NULL",
        )?
        .query_map([own_host], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (rowid, column) in held {
        let kept = match crate::rows::decode_experimental(Some(column.as_slice())) {
            Ok(mut map) => {
                let before = map.len();
                map.retain(|version, _| on.contains(&version.as_str()));
                if map.len() == before {
                    continue;
                }
                crate::rows::encode_experimental(&map)
            }
            Err(_) => None,
        };
        conn.execute(
            "UPDATE agents SET experimental = ?2 WHERE rowid = ?1",
            rusqlite::params![rowid, kept],
        )?;
    }
    Ok(())
}

/// Drops every `exp_` table whose name starts with none of `kept`; its
/// indexes and triggers go with it.
pub(crate) fn drop_tables(conn: &Connection, kept: &[String]) -> rusqlite::Result<()> {
    let names = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 4) = 'exp_'",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for name in names {
        if !kept.iter().any(|prefix| name.starts_with(prefix.as_str())) {
            conn.execute_batch(&format!("DROP TABLE \"{}\"", name.replace('"', "\"\"")))?;
        }
    }
    Ok(())
}
