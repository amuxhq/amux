//! The store tables of experiments, outside the promised migrations.
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

/// The table [`WORKSPACES_1`] keeps each agent's workspace in.
pub const WORKSPACES_1_MEMBERS: &str = "exp_workspaces_1_members";

/// What an agent's workspace is, for the host's own agents and replicas
/// alike, joined when a row is read: a row read while the table is there
/// carries its entry in [`AgentRow::experimental`](crate::AgentRow), and a
/// row removed takes its entry with it. For own agents it is a cache: the
/// folder is the truth.
pub const WORKSPACES_1: ExperimentTables = ExperimentTables {
    version: "workspaces/1",
    schema: r#"
CREATE TABLE IF NOT EXISTS exp_workspaces_1_members (
  origin_host BLOB NOT NULL,  -- the host that runs the agent
  agent_id    BLOB NOT NULL,  -- the agent's id
  workspace   BLOB NOT NULL,  -- amux.experimental.AgentWorkspace, encoded
  PRIMARY KEY (origin_host, agent_id)
) STRICT;
"#,
};

/// The incarnation each of this host's own agents began its current run
/// at: a resume after a suspension or a restart continues the run, any
/// other resume starts a new one.
pub const SUSPENSION_1: ExperimentTables = ExperimentTables {
    version: "suspension/1",
    schema: r#"
CREATE TABLE IF NOT EXISTS exp_suspension_1_runs (
  agent_id    BLOB    NOT NULL PRIMARY KEY, -- one of this host's own agents
  incarnation INTEGER NOT NULL              -- the spec.<n> its current run began at
) STRICT;
"#,
};

/// Every experiment version this build keeps tables for.
pub const EXPERIMENT_TABLES: &[&ExperimentTables] = &[&WORKSPACES_1, &SUSPENSION_1];

/// Makes the tables of the experiment versions in `on` and drops every
/// other `exp_` table, in one transaction. A version in `on` with no tables
/// declared has nothing to make.
pub(crate) fn settle(conn: &mut Connection, on: &[&str]) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let kept: Vec<String> = on.iter().map(|version| prefix(version)).collect();
    drop_tables(&tx, &kept)?;
    for tables in EXPERIMENT_TABLES {
        if on.contains(&tables.version) {
            tx.execute_batch(tables.schema)?;
        }
    }
    tx.commit()
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
