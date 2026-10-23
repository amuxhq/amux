//! A store file made fit for a dump: every value that may carry a secret
//! redacted in place, and the file rebuilt so nothing it replaced stays in
//! a free page.
//!
//! The store decodes no agent's bodies and knows no redaction rules, so the
//! caller brings a [`Redactor`]; the store only says which values are
//! which. Ids, cursors, revisions, orders and enumerations are kept, so the
//! copy reads, and serves, as the original does. The daemon's core walks
//! its own checkpoint and recorded inputs through the same redactor.

use std::path::Path;

use prost::Message as _;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{AgentKey, Item, Snapshot, Step, StoreError, decode_attachments, encode_attachments};

/// What a dump's redaction does to a store's and a core's values.
pub trait Redactor {
    /// Free text.
    fn text(&self, text: &str) -> String;
    /// An encoded protocol message, by its full name (`amux.v1.Item`),
    /// redacted and encoded again.
    fn message(&self, name: &str, bytes: &[u8]) -> Vec<u8>;
    /// An agent's row fields of its kind, as one `wire.Agent`.
    fn agent(&self, kind: i32, agent: wire::Agent) -> wire::Agent;
    /// Items or a snapshot of an agent of `kind`, as one `wire.Step`.
    fn step(&self, kind: i32, step: Step) -> Step;
}

/// Redacts the store file at `path`, which nothing else has open: the
/// rows' names, directories, texts, details, snapshots, git states, item
/// bodies and attachments, the owed reports' and notifications' texts.
/// The file is left in rollback mode, with no WAL beside it. Returns each
/// row's agent with its ingest cursor, the journal offset its commits
/// reach.
pub fn redact_file(
    path: &Path,
    redactor: &dyn Redactor,
) -> Result<Vec<(AgentKey, u64)>, StoreError> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    conn.pragma_update(None, "secure_delete", true)?;
    let tx = conn.transaction()?;
    agents(&tx, redactor)?;
    items(&tx, redactor)?;
    let text = |value: Option<String>| value.map(|value| redactor.text(&value));
    {
        let mut read = tx.prepare("SELECT rowid, text, exit_detail FROM deliveries")?;
        let rows = read
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<(i64, Option<String>, Option<String>)>, _>>()?;
        for (rowid, body, detail) in rows {
            tx.execute(
                "UPDATE deliveries SET text = ?2, exit_detail = ?3 WHERE rowid = ?1",
                params![rowid, text(body), text(detail)],
            )?;
        }
        let mut read = tx.prepare("SELECT rowid, name, working_on, text FROM notifications")?;
        let rows = read
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<(i64, Option<String>, Option<String>, String)>, _>>()?;
        for (rowid, name, working_on, body) in rows {
            tx.execute(
                "UPDATE notifications SET name = ?2, working_on = ?3, text = ?4 WHERE rowid = ?1",
                params![rowid, text(name), text(working_on), redactor.text(&body)],
            )?;
        }
    }
    tx.commit()?;
    conn.execute_batch("VACUUM")?;
    let mut read = conn.prepare("SELECT origin_host, agent_id, ingest_cursor FROM agents")?;
    let cursors = read
        .query_map([], |row| {
            Ok((
                AgentKey::new(row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?),
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cursors)
}

fn agents(tx: &rusqlite::Transaction<'_>, redactor: &dyn Redactor) -> Result<(), StoreError> {
    type Row = (
        i64,
        i32,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
    );
    let mut read = tx.prepare(
        "SELECT rowid, kind, name, cwd, exit_detail, working_on, git, snapshot FROM agents",
    )?;
    let rows = read
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .collect::<Result<Vec<Row>, _>>()?;
    for (rowid, kind, name, cwd, exit_detail, working_on, git, snapshot) in rows {
        let git = git
            .map(|bytes| wire::Git::decode(bytes.as_slice()))
            .transpose()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let agent = redactor.agent(
            kind,
            wire::Agent {
                kind,
                name,
                cwd,
                exit_detail,
                working_on,
                git,
                ..Default::default()
            },
        );
        let snapshot = snapshot
            .map(|bytes| Snapshot::decode(bytes.as_slice()))
            .transpose()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?
            .map(|snapshot| {
                redactor
                    .step(
                        kind,
                        Step {
                            snapshot: Some(snapshot),
                            ..Step::default()
                        },
                    )
                    .snapshot
                    .unwrap_or_default()
                    .encode_to_vec()
            });
        tx.execute(
            "UPDATE agents SET name = ?2, cwd = ?3, exit_detail = ?4, working_on = ?5, \
             git = ?6, snapshot = ?7 WHERE rowid = ?1",
            params![
                rowid,
                agent.name,
                agent.cwd,
                agent.exit_detail,
                agent.working_on,
                agent.git.map(|git| git.encode_to_vec()),
                snapshot,
            ],
        )?;
    }
    Ok(())
}

fn items(tx: &rusqlite::Transaction<'_>, redactor: &dyn Redactor) -> Result<(), StoreError> {
    let mut read = tx.prepare("SELECT rowid FROM items")?;
    let rowids = read
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    // One row at a time, so a large store is never held whole.
    for rowid in rowids {
        let Some((kind, key, text, attachments, body)) = tx
            .query_row(
                "SELECT kind, key, text, attachments, body FROM items WHERE rowid = ?1",
                [rowid],
                |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()?
        else {
            continue;
        };
        let had_attachments = attachments.is_some();
        let item = Item {
            key,
            text,
            kind,
            body,
            attachments: decode_attachments(attachments.as_deref().unwrap_or_default())?,
            ..Item::default()
        };
        let item = redactor
            .step(
                kind,
                Step {
                    items: vec![item],
                    ..Step::default()
                },
            )
            .items
            .pop()
            .unwrap_or_default();
        let attachments = had_attachments.then(|| encode_attachments(&item.attachments));
        tx.execute(
            "UPDATE items SET text = ?2, attachments = ?3, body = ?4 WHERE rowid = ?1",
            params![rowid, item.text, attachments, item.body],
        )?;
    }
    Ok(())
}
