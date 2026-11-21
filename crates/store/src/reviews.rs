//! The reviews experiment's tables (`reviews/1`), each named with the
//! prefix `exp_reviews_1_`: a review of each own agent's working tree, the
//! snapshots it took, the rounds sent and the comments. Made and dropped
//! with the experiment by [`crate::experiments`], never by a migration.
//!
//! A review is read whole with [`Sqlite::review`] and changed only inside
//! [`Sqlite::write_review`], one transaction for every change a write
//! makes, so a batch of edits lands all or not at all. What the review's
//! rows mean, and what its agent's row says of it, is the daemon core's:
//! the store keeps the rows as they are given.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use wire::experimental::{ReviewDefinition, ReviewLine};

use crate::rows::{self, Rows};
use crate::{AgentKey, AgentRow, Sqlite, StoreError};

/// The SQL that makes the experiment's tables, run each time a store opens
/// with the experiment on.
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS exp_reviews_1_reviews (
  id          BLOB    NOT NULL PRIMARY KEY, -- made when the review began; a review started over has a new one
  agent_id    BLOB    NOT NULL UNIQUE,      -- one of this host's own agents, which has one review at most
  base_branch TEXT    NOT NULL,             -- the branch its changes are compared with
  began_at    TEXT    NOT NULL,             -- the commit HEAD was at when it began; empty before the first commit
  common_dir  TEXT    NOT NULL,             -- the repository's common git directory
  created_ms  INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS exp_reviews_1_snapshots (
  review_id BLOB    NOT NULL,
  snapshot  TEXT    NOT NULL,               -- the snapshot's commit
  base_commit TEXT  NOT NULL,               -- the commit its all changes run from, as found when it was taken
  base_name TEXT    NOT NULL,               -- the base branch as a summary names it, found with base_commit
  taken_ms  INTEGER NOT NULL,
  omitted   BLOB    NOT NULL,               -- the paths left out for their size, each ended by a NUL
  merging   INTEGER NOT NULL,               -- a merge or rebase was under way when it was taken
  PRIMARY KEY (review_id, snapshot)
) STRICT;
CREATE TABLE IF NOT EXISTS exp_reviews_1_bases (
  review_id   BLOB NOT NULL,
  snapshot    TEXT NOT NULL,                -- a snapshot the review keeps
  base_commit TEXT NOT NULL,                -- a commit a summary showed it against, its current base among them
  PRIMARY KEY (review_id, snapshot, base_commit)
) STRICT;
CREATE TABLE IF NOT EXISTS exp_reviews_1_rounds (
  review_id   BLOB    NOT NULL,
  n           INTEGER NOT NULL,             -- from 1, in the order the agent took them
  snapshot    TEXT    NOT NULL,             -- the snapshot the sent text was written on
  base_commit TEXT    NOT NULL,             -- the commit that snapshot was shown against then
  input_id    BLOB    NOT NULL,             -- the input the agent took that closed it
  sent_ms     INTEGER NOT NULL,
  PRIMARY KEY (review_id, n)
) STRICT;
CREATE TABLE IF NOT EXISTS exp_reviews_1_comments (
  id            BLOB    NOT NULL PRIMARY KEY, -- made by the client that wrote it
  review_id     BLOB    NOT NULL,
  anchor_commit TEXT    NOT NULL,             -- the commit that holds the lines
  path          TEXT    NOT NULL,             -- from the repository's top folder
  start_line    INTEGER NOT NULL,             -- from 1, both ends included; zero to zero is the whole file
  end_line      INTEGER NOT NULL,
  text          TEXT    NOT NULL,
  snippet       BLOB    NOT NULL,             -- amux.experimental.ReviewLine, each length-delimited
  scope         BLOB    NOT NULL,             -- amux.experimental.ReviewDefinition, each length-delimited
  round         INTEGER,                      -- the round it was sent in; NULL while a draft
  revision      INTEGER NOT NULL,             -- from 1, up by one each time a person puts it
  sent_revision INTEGER,                      -- the revision the agent took it at; NULL while a draft
  updated_ms    INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS exp_reviews_1_comments_by_review ON exp_reviews_1_comments (review_id);
"#;

/// An own agent's review, read whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    pub id: Vec<u8>,
    pub agent_id: Vec<u8>,
    pub base_branch: String,
    /// The commit HEAD was at when the review began; empty before the
    /// repository's first commit.
    pub began_at: String,
    /// The repository's common git directory.
    pub common_dir: String,
    pub created_ms: i64,
    /// Oldest first: the last is the latest.
    pub snapshots: Vec<Taken>,
    /// By number, from 1.
    pub rounds: Vec<Round>,
    /// In the order they were first written.
    pub comments: Vec<Comment>,
}

/// A snapshot a review took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taken {
    /// Its commit, by full object id.
    pub commit: String,
    /// The commit its all changes run from, as the review's base rule found
    /// it when the snapshot was taken (or its base was picked again).
    pub base_commit: String,
    /// The base branch as a summary of it names it, found with
    /// `base_commit`: what is shown without asking git again.
    pub base_name: String,
    /// Every base commit a summary has shown it against, `base_commit`
    /// among them: each comparison handed out stays one the review answers.
    pub bases: Vec<String>,
    pub taken_ms: i64,
    /// The paths left out of it for their size, from the repository's top
    /// folder.
    pub omitted: Vec<String>,
    /// A merge or rebase was under way when it was taken.
    pub merging: bool,
}

/// A round sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Round {
    /// From 1, in the order the agent took the messages that sent them.
    pub n: u32,
    /// The snapshot the sent text was written on.
    pub snapshot: String,
    /// The commit that snapshot was shown against when the text was
    /// written.
    pub base_commit: String,
    /// The input the agent took that closed it: the id of the prompt or
    /// message carrying the sent review.
    pub input_id: Vec<u8>,
    pub sent_ms: i64,
}

/// The lines a comment was written on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// The commit that holds them.
    pub commit: String,
    /// From the repository's top folder.
    pub path: String,
    /// From 1, both ends included; zero to zero is the whole file.
    pub start: u32,
    pub end: u32,
}

/// A comment, draft or sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: Vec<u8>,
    pub anchor: Anchor,
    pub text: String,
    /// The commented lines and those above them, as stored when it was
    /// saved.
    pub snippet: Vec<ReviewLine>,
    /// The definitions enclosing the lines, outermost first.
    pub scope: Vec<ReviewDefinition>,
    /// The round it was sent in; none while it is a draft.
    pub round: Option<u32>,
    /// From 1, up by one each time a person puts it: a sent review names
    /// the revision it was written from.
    pub revision: u64,
    /// The revision the agent took it at, which closed its round; none
    /// while a draft.
    pub sent_revision: Option<u64>,
    pub updated_ms: i64,
}

impl Review {
    /// The snapshot taken last.
    pub fn latest(&self) -> Option<&Taken> {
        self.snapshots.last()
    }

    /// The comments not yet sent.
    pub fn drafts(&self) -> impl Iterator<Item = &Comment> {
        self.comments
            .iter()
            .filter(|comment| comment.round.is_none())
    }

    /// The snapshot taken before the latest, which a client may still be
    /// showing: it is kept until the next one is taken.
    pub fn previous(&self) -> Option<&Taken> {
        self.snapshots.iter().rev().nth(1)
    }

    /// The snapshot `commit`, while the review keeps it.
    pub fn snapshot(&self, commit: &str) -> Option<&Taken> {
        self.snapshots.iter().find(|taken| taken.commit == commit)
    }

    /// The snapshots the review still needs: each one a round or a
    /// comment's anchor names, each one `waiting` names (the snapshots of
    /// sent reviews the agent has not taken yet, whose rounds will be
    /// recorded on them and whose text tells the agent to diff against
    /// them), the latest, and the one before it, which a page opened before
    /// the latest was taken still shows. Any other may go.
    pub fn kept<'a>(&'a self, waiting: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
        let named: BTreeSet<&str> = self
            .rounds
            .iter()
            .map(|round| round.snapshot.as_str())
            .chain(
                self.comments
                    .iter()
                    .map(|comment| comment.anchor.commit.as_str()),
            )
            .chain(waiting)
            .collect();
        self.snapshots
            .iter()
            .filter(|taken| named.contains(taken.commit.as_str()))
            .chain(self.latest())
            .chain(self.previous())
            .map(|taken| taken.commit.clone())
            .collect()
    }
}

impl Sqlite {
    /// The own agent `agent`'s review; none without one, and none with the
    /// experiment off.
    pub fn review(&self, agent: &[u8]) -> Result<Option<Review>, StoreError> {
        if !self.reviews_1 {
            return Ok(None);
        }
        self.read(|rows| read_review(rows, agent))
    }

    /// The latest snapshot of each own agent's review that has taken one,
    /// by agent id; none with the experiment off.
    pub fn latest_snapshots(&self) -> Result<Vec<(Vec<u8>, Taken)>, StoreError> {
        if !self.reviews_1 {
            return Ok(Vec::new());
        }
        self.read(|rows| {
            let mut statement = rows.conn.prepare_cached(
                "SELECT r.agent_id, s.snapshot, s.base_commit, s.base_name, s.taken_ms, s.omitted, \
                 s.merging FROM exp_reviews_1_reviews r JOIN exp_reviews_1_snapshots s \
                 ON s.review_id = r.id \
                 WHERE s.rowid = (SELECT max(rowid) FROM exp_reviews_1_snapshots \
                                  WHERE review_id = r.id) \
                 ORDER BY r.agent_id",
            )?;
            let mut found = statement
                .query_map([], |row| Ok((row.get::<_, Vec<u8>>(0)?, taken(row, 1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            for (agent, taken) in &mut found {
                if let Some(review) = review_id(rows, agent)? {
                    taken.bases = bases(rows, &review, &taken.commit)?;
                }
            }
            Ok(found)
        })
    }

    /// Runs `write` over the own agent `agent`'s review in one transaction:
    /// committed when it returns its value, rolled back whole when it
    /// returns an error. Refused with [`StoreError::ExperimentOff`] while
    /// the experiment is off, and with [`StoreError::NotOwn`] for another
    /// host's agent.
    pub fn write_review<T, E: From<StoreError>>(
        &mut self,
        agent: &AgentKey,
        write: impl FnOnce(&ReviewWriter<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        if !self.reviews_1 {
            return Err(StoreError::ExperimentOff(crate::experiments::REVIEWS_1.version).into());
        }
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn.into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let rows = Rows {
            conn: &tx,
            own_host: &self.own_host,
            suspension_1: self.suspension_1,
            reviews_1: self.reviews_1,
        };
        let value = write(&ReviewWriter::new(&rows, agent))?;
        tx.commit().map_err(StoreError::from)?;
        Ok(value)
    }
}

/// One agent's review inside [`Sqlite::write_review`]'s transaction. Each
/// change takes the rows as given; whether it makes sense is the caller's
/// to judge, reading [`ReviewWriter::review`] first.
pub struct ReviewWriter<'a> {
    rows: &'a Rows<'a>,
    agent: &'a AgentKey,
}

impl<'a> ReviewWriter<'a> {
    pub(crate) fn new(rows: &'a Rows<'a>, agent: &'a AgentKey) -> Self {
        ReviewWriter { rows, agent }
    }
}

impl ReviewWriter<'_> {
    /// The review as the transaction sees it so far.
    pub fn review(&self) -> Result<Option<Review>, StoreError> {
        read_review(self.rows, &self.agent.agent)
    }

    /// The agent's row as the transaction sees it so far.
    pub fn row(&self) -> Result<Option<AgentRow>, StoreError> {
        self.rows.agent(self.agent)
    }

    /// Writes the agent's row's experimental map whole; false without a
    /// row.
    pub fn set_experimental(&self, map: &BTreeMap<String, Vec<u8>>) -> Result<bool, StoreError> {
        let Some(mut row) = self.rows.agent(self.agent)? else {
            return Ok(false);
        };
        row.experimental = map.clone();
        self.rows
            .upsert_agent(&row, &rows::update_of(rows::EXPERIMENTAL))?;
        Ok(true)
    }

    /// Begins the agent's review, with nothing in it yet.
    pub fn begin(
        &self,
        id: &[u8],
        base_branch: &str,
        began_at: &str,
        common_dir: &str,
        created_ms: i64,
    ) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "INSERT INTO exp_reviews_1_reviews \
             (id, agent_id, base_branch, began_at, common_dir, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                self.agent.agent,
                base_branch,
                began_at,
                common_dir,
                created_ms
            ],
        )?;
        Ok(())
    }

    /// Compares the review with `base_branch` from now on.
    pub fn set_base(&self, review: &[u8], base_branch: &str) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "UPDATE exp_reviews_1_reviews SET base_branch = ?2 WHERE id = ?1",
            params![review, base_branch],
        )?;
        Ok(())
    }

    /// Records `taken` as the review's latest snapshot, shown against its
    /// base commit; one it took before becomes the latest again, keeping
    /// the bases it was shown against.
    pub fn add_snapshot(&self, review: &[u8], taken: &Taken) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "DELETE FROM exp_reviews_1_snapshots WHERE review_id = ?1 AND snapshot = ?2",
            params![review, taken.commit],
        )?;
        self.rows.conn.execute(
            "INSERT INTO exp_reviews_1_snapshots \
             (review_id, snapshot, base_commit, base_name, taken_ms, omitted, merging) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                review,
                taken.commit,
                taken.base_commit,
                taken.base_name,
                taken.taken_ms,
                encode_paths(&taken.omitted),
                taken.merging
            ],
        )?;
        self.add_base(review, &taken.commit, &taken.base_commit)
    }

    /// Records `base_commit`, named `base_name`, as what the snapshot
    /// `snapshot` is compared against for all changes from now on; the
    /// bases it was shown against before stay its.
    pub fn set_snapshot_base(
        &self,
        review: &[u8],
        snapshot: &str,
        base_commit: &str,
        base_name: &str,
    ) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "UPDATE exp_reviews_1_snapshots SET base_commit = ?3, base_name = ?4 \
             WHERE review_id = ?1 AND snapshot = ?2",
            params![review, snapshot, base_commit, base_name],
        )?;
        self.add_base(review, snapshot, base_commit)
    }

    fn add_base(&self, review: &[u8], snapshot: &str, base_commit: &str) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "INSERT OR IGNORE INTO exp_reviews_1_bases (review_id, snapshot, base_commit) \
             VALUES (?1, ?2, ?3)",
            params![review, snapshot, base_commit],
        )?;
        Ok(())
    }

    /// Forgets every snapshot of the review not in `kept`.
    pub fn keep_snapshots(&self, review: &[u8], kept: &BTreeSet<String>) -> Result<(), StoreError> {
        let held = self
            .rows
            .conn
            .prepare_cached("SELECT snapshot FROM exp_reviews_1_snapshots WHERE review_id = ?1")?
            .query_map(params![review], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for commit in held.iter().filter(|commit| !kept.contains(*commit)) {
            for table in ["exp_reviews_1_snapshots", "exp_reviews_1_bases"] {
                self.rows.conn.execute(
                    &format!("DELETE FROM {table} WHERE review_id = ?1 AND snapshot = ?2"),
                    params![review, commit],
                )?;
            }
        }
        Ok(())
    }

    /// Writes `comment` into the review: a new one, or one the review holds
    /// changed in place.
    pub fn put_comment(&self, review: &[u8], comment: &Comment) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "INSERT INTO exp_reviews_1_comments \
             (id, review_id, anchor_commit, path, start_line, end_line, text, snippet, scope, \
              round, revision, sent_revision, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT (id) DO UPDATE SET \
             anchor_commit = excluded.anchor_commit, path = excluded.path, \
             start_line = excluded.start_line, end_line = excluded.end_line, \
             text = excluded.text, snippet = excluded.snippet, scope = excluded.scope, \
             round = excluded.round, revision = excluded.revision, \
             sent_revision = excluded.sent_revision, updated_ms = excluded.updated_ms",
            params![
                comment.id,
                review,
                comment.anchor.commit,
                comment.anchor.path,
                comment.anchor.start,
                comment.anchor.end,
                comment.text,
                encode_each(&comment.snippet),
                encode_each(&comment.scope),
                comment.round,
                // SQLite's integers are signed; no comment is put 2^63 times.
                comment.revision as i64,
                comment.sent_revision.map(|revision| revision as i64),
                comment.updated_ms
            ],
        )?;
        Ok(())
    }

    /// The review a comment id belongs to, whichever agent's it is.
    pub fn comment_owner(&self, comment: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .rows
            .conn
            .prepare_cached("SELECT review_id FROM exp_reviews_1_comments WHERE id = ?1")?
            .query_row(params![comment], |row| row.get(0))
            .optional()?)
    }

    /// Deletes one of the review's comments, draft or sent; false when the
    /// review holds none by that id.
    pub fn delete_comment(&self, review: &[u8], comment: &[u8]) -> Result<bool, StoreError> {
        let deleted = self.rows.conn.execute(
            "DELETE FROM exp_reviews_1_comments WHERE review_id = ?1 AND id = ?2",
            params![review, comment],
        )?;
        Ok(deleted > 0)
    }

    /// Deletes every draft of the review, leaving the sent comments; how
    /// many went.
    pub fn discard_drafts(&self, review: &[u8]) -> Result<usize, StoreError> {
        Ok(self.rows.conn.execute(
            "DELETE FROM exp_reviews_1_comments WHERE review_id = ?1 AND round IS NULL",
            params![review],
        )?)
    }

    /// Records a round the review sent.
    pub fn add_round(&self, review: &[u8], round: &Round) -> Result<(), StoreError> {
        self.rows.conn.execute(
            "INSERT INTO exp_reviews_1_rounds \
             (review_id, n, snapshot, base_commit, input_id, sent_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                review,
                round.n,
                round.snapshot,
                round.base_commit,
                round.input_id,
                round.sent_ms
            ],
        )?;
        Ok(())
    }

    /// Removes the review and everything in it.
    pub fn remove(&self) -> Result<(), StoreError> {
        remove_agent(self.rows, &self.agent.agent)
    }
}

/// Removes what the experiment's tables hold of the own agent `agent`, as
/// its row is removed.
pub(crate) fn remove_agent(rows: &Rows<'_>, agent: &[u8]) -> Result<(), StoreError> {
    let Some(review) = review_id(rows, agent)? else {
        return Ok(());
    };
    for table in [
        "exp_reviews_1_comments",
        "exp_reviews_1_rounds",
        "exp_reviews_1_snapshots",
        "exp_reviews_1_bases",
    ] {
        rows.conn
            .prepare_cached(&format!("DELETE FROM {table} WHERE review_id = ?1"))?
            .execute(params![review])?;
    }
    rows.conn
        .prepare_cached("DELETE FROM exp_reviews_1_reviews WHERE id = ?1")?
        .execute(params![review])?;
    Ok(())
}

fn review_id(rows: &Rows<'_>, agent: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(rows
        .conn
        .prepare_cached("SELECT id FROM exp_reviews_1_reviews WHERE agent_id = ?1")?
        .query_row(params![agent], |row| row.get(0))
        .optional()?)
}

fn read_review(rows: &Rows<'_>, agent: &[u8]) -> Result<Option<Review>, StoreError> {
    let Some(mut review) = rows
        .conn
        .prepare_cached(
            "SELECT id, base_branch, began_at, common_dir, created_ms \
             FROM exp_reviews_1_reviews WHERE agent_id = ?1",
        )?
        .query_row(params![agent], |row| {
            Ok(Review {
                id: row.get(0)?,
                agent_id: agent.to_vec(),
                base_branch: row.get(1)?,
                began_at: row.get(2)?,
                common_dir: row.get(3)?,
                created_ms: row.get(4)?,
                snapshots: Vec::new(),
                rounds: Vec::new(),
                comments: Vec::new(),
            })
        })
        .optional()?
    else {
        return Ok(None);
    };
    review.snapshots = rows
        .conn
        .prepare_cached(
            "SELECT snapshot, base_commit, base_name, taken_ms, omitted, merging \
             FROM exp_reviews_1_snapshots WHERE review_id = ?1 ORDER BY rowid",
        )?
        .query_map(params![review.id], |row| taken(row, 0))?
        .collect::<Result<_, _>>()?;
    for taken in &mut review.snapshots {
        taken.bases = bases(rows, &review.id, &taken.commit)?;
    }
    review.rounds = rows
        .conn
        .prepare_cached(
            "SELECT n, snapshot, base_commit, input_id, sent_ms FROM exp_reviews_1_rounds \
             WHERE review_id = ?1 ORDER BY n",
        )?
        .query_map(params![review.id], |row| {
            Ok(Round {
                n: row.get(0)?,
                snapshot: row.get(1)?,
                base_commit: row.get(2)?,
                input_id: row.get(3)?,
                sent_ms: row.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let comments = rows
        .conn
        .prepare_cached(
            "SELECT id, anchor_commit, path, start_line, end_line, text, snippet, scope, round, \
             revision, sent_revision, updated_ms FROM exp_reviews_1_comments \
             WHERE review_id = ?1 ORDER BY rowid",
        )?
        .query_map(params![review.id], |row| {
            Ok((
                Comment {
                    id: row.get(0)?,
                    anchor: Anchor {
                        commit: row.get(1)?,
                        path: row.get(2)?,
                        start: row.get(3)?,
                        end: row.get(4)?,
                    },
                    text: row.get(5)?,
                    snippet: Vec::new(),
                    scope: Vec::new(),
                    round: row.get(8)?,
                    revision: row.get::<_, i64>(9)? as u64,
                    sent_revision: row
                        .get::<_, Option<i64>>(10)?
                        .map(|revision| revision as u64),
                    updated_ms: row.get(11)?,
                },
                row.get::<_, Vec<u8>>(6)?,
                row.get::<_, Vec<u8>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    review.comments = comments
        .into_iter()
        .map(|(mut comment, snippet, scope)| {
            comment.snippet = decode_each(&snippet)?;
            comment.scope = decode_each(&scope)?;
            Ok(comment)
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(Some(review))
}

/// Every base commit the snapshot `snapshot` of `review` has been shown
/// against, in the order it was.
fn bases(rows: &Rows<'_>, review: &[u8], snapshot: &str) -> Result<Vec<String>, StoreError> {
    Ok(rows
        .conn
        .prepare_cached(
            "SELECT base_commit FROM exp_reviews_1_bases WHERE review_id = ?1 AND snapshot = ?2 \
             ORDER BY rowid",
        )?
        .query_map(params![review, snapshot], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

/// A snapshot from the row's columns starting at `at`: commit, base
/// commit, base name, time, omitted paths and the merging flag. Its bases
/// are read apart.
fn taken(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Taken> {
    Ok(Taken {
        commit: row.get(at)?,
        base_commit: row.get(at + 1)?,
        base_name: row.get(at + 2)?,
        bases: Vec::new(),
        taken_ms: row.get(at + 3)?,
        omitted: decode_paths(&row.get::<_, Vec<u8>>(at + 4)?),
        merging: row.get(at + 5)?,
    })
}

/// Paths for their column, each ended by a NUL, which no path holds.
fn encode_paths(paths: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn decode_paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

/// Messages for a column, each length-delimited.
fn encode_each<M: prost::Message>(messages: &[M]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for message in messages {
        message
            .encode_length_delimited(&mut bytes)
            .expect("a Vec grows to fit");
    }
    bytes
}

fn decode_each<M: prost::Message + Default>(mut bytes: &[u8]) -> Result<Vec<M>, StoreError> {
    let mut messages = Vec::new();
    while !bytes.is_empty() {
        messages.push(
            M::decode_length_delimited(&mut bytes)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        );
    }
    Ok(messages)
}
