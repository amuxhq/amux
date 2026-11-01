//! The store as a value, for exploring its transitions: a canonical dump of
//! every row that restores to an equal in-memory store, and read-only views
//! of an agent's block that checks judge.
//!
//! A dump lists every table, each row's columns
//! as stored and the rows sorted by their columns, so two stores holding
//! the same rows dump equal whatever order they were written in. The
//! markers and reset counts the store keeps in memory travel with it.

use rusqlite::types::ValueRef;

use crate::sqlite::{OpenError, Sqlite, block_floor};
use crate::{AgentKey, Item, Marker, StoreError};

/// Every row of a store, canonically ordered: equal for two stores that
/// hold the same rows, markers and reset counts. The default is no store
/// at all, with no tables.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dump {
    own_host: Vec<u8>,
    tables: Vec<Table>,
    markers: Vec<(AgentKey, Marker)>,
    resets: Vec<(AgentKey, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Table {
    name: String,
    rows: Vec<Vec<Cell>>,
}

/// One stored value. A real is kept as its bits, so a dump can be hashed
/// and its rows sorted.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Cell {
    Null,
    Integer(i64),
    Real(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    fn read(value: ValueRef<'_>) -> Result<Self, StoreError> {
        Ok(match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(integer) => Self::Integer(integer),
            ValueRef::Real(real) => Self::Real(real.to_bits()),
            ValueRef::Text(text) => Self::Text(
                std::str::from_utf8(text)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?
                    .to_owned(),
            ),
            ValueRef::Blob(blob) => Self::Blob(blob.to_vec()),
        })
    }

    fn value(&self) -> rusqlite::types::Value {
        use rusqlite::types::Value;
        match self {
            Self::Null => Value::Null,
            Self::Integer(integer) => Value::Integer(*integer),
            Self::Real(bits) => Value::Real(f64::from_bits(*bits)),
            Self::Text(text) => Value::Text(text.clone()),
            Self::Blob(blob) => Value::Blob(blob.clone()),
        }
    }
}

impl Dump {
    pub fn own_host(&self) -> &[u8] {
        &self.own_host
    }
}

/// The orders of an agent's block, oldest first, and the order its row says
/// the block runs from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockOrders {
    /// The row's `complete_from_order`: for a replica, none means no block;
    /// for an own row, none means all history is held.
    pub from: Option<u64>,
    pub orders: Vec<u64>,
}

impl Sqlite {
    /// Every row this store holds, with its markers and reset counts.
    pub fn dump(&self) -> Result<Dump, StoreError> {
        let tables = self.read(|rows| {
            let names = rows
                .conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' \
                     AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            names
                .into_iter()
                .map(|name| {
                    let mut statement = rows.conn.prepare(&format!("SELECT * FROM \"{name}\""))?;
                    let width = statement.column_count();
                    let mut table = Vec::new();
                    let mut query = statement.query([])?;
                    while let Some(row) = query.next()? {
                        table.push(
                            (0..width)
                                .map(|column| Cell::read(row.get_ref(column)?))
                                .collect::<Result<Vec<_>, _>>()?,
                        );
                    }
                    table.sort();
                    Ok(Table { name, rows: table })
                })
                .collect::<Result<Vec<_>, StoreError>>()
        })?;
        let mut markers: Vec<_> = self
            .markers
            .iter()
            .map(|(agent, marker)| (agent.clone(), *marker))
            .collect();
        markers.sort_by(|a, b| a.0.cmp(&b.0));
        let mut resets: Vec<_> = self
            .resets
            .iter()
            .map(|(agent, count)| (agent.clone(), *count))
            .collect();
        resets.sort();
        Ok(Dump {
            own_host: self.own_host.clone(),
            tables,
            markers,
            resets,
        })
    }

    /// An in-memory store holding exactly the dump's rows, markers and reset
    /// counts: its own dump equals `dump`.
    pub fn restore(dump: &Dump) -> Result<Self, StoreError> {
        let mut store =
            Self::open_in_memory(dump.own_host.clone()).map_err(|error| match error {
                OpenError::Sqlite(error) => StoreError::Sqlite(error),
                other => StoreError::Corrupt(other.to_string()),
            })?;
        store.load(dump)?;
        Ok(store)
    }

    /// Replaces everything this store holds with the dump's rows, markers
    /// and reset counts, and takes its host as its own: its dump then
    /// equals `dump`. Faster than [`Sqlite::restore`] when a store is
    /// reused, since the schema is already read.
    pub fn load(&mut self, dump: &Dump) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for table in &dump.tables {
            tx.execute(&format!("DELETE FROM \"{}\"", table.name), [])?;
            let Some(width) = table.rows.first().map(Vec::len) else {
                continue;
            };
            let slots = (1..=width)
                .map(|slot| format!("?{slot}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut insert =
                tx.prepare(&format!("INSERT INTO \"{}\" VALUES ({slots})", table.name))?;
            for row in &table.rows {
                insert.execute(rusqlite::params_from_iter(row.iter().map(Cell::value)))?;
            }
        }
        tx.commit()?;
        self.own_host = dump.own_host.clone();
        self.markers = dump.markers.iter().cloned().collect();
        self.resets = dump.resets.iter().cloned().collect();
        Ok(())
    }

    /// The orders of the agent's block, oldest first.
    pub fn block_orders(&self, agent: &AgentKey) -> Result<BlockOrders, StoreError> {
        let from = self
            .agent(agent)?
            .ok_or(StoreError::UnknownAgent)?
            .complete_from_order;
        let orders = self.block(agent)?.iter().map(|item| item.order).collect();
        Ok(BlockOrders { from, orders })
    }

    /// The agent's block, oldest first: the rows a reader is served.
    pub fn block(&self, agent: &AgentKey) -> Result<Vec<Item>, StoreError> {
        self.suffix_from(agent, 0)
    }

    /// The block's rows whose order is at least `order`, oldest first.
    pub fn suffix_from(&self, agent: &AgentKey, order: u64) -> Result<Vec<Item>, StoreError> {
        let own = self.is_own(agent);
        self.read(|rows| {
            let row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            let Some(floor) = block_floor(&row, own) else {
                return Ok(Vec::new());
            };
            let mut items = rows.items_desc(agent, None, floor.max(order), u32::MAX)?;
            items.reverse();
            Ok(items)
        })
    }

    /// Every row stored for the agent, in the block or below it, oldest
    /// order first.
    pub fn rows(&self, agent: &AgentKey) -> Result<Vec<Item>, StoreError> {
        self.read(|rows| {
            rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            let mut items = rows.items_desc(agent, None, 0, u32::MAX)?;
            items.reverse();
            Ok(items)
        })
    }

    /// The newest revision the store holds for the agent: of any stored row
    /// or of its snapshot; 0 when it holds neither.
    pub fn newest_revision_held(&self, agent: &AgentKey) -> Result<u64, StoreError> {
        let snapshot = self
            .agent(agent)?
            .ok_or(StoreError::UnknownAgent)?
            .snapshot
            .map_or(0, |snapshot| snapshot.revision);
        let rows = self
            .rows(agent)?
            .iter()
            .map(|item| item.revision)
            .max()
            .unwrap_or(0);
        Ok(snapshot.max(rows))
    }

    /// A replica row's source cursor: the origin revision its source is
    /// complete through.
    pub fn source_cursor(&self, agent: &AgentKey) -> Result<u64, StoreError> {
        Ok(self
            .agent(agent)?
            .ok_or(StoreError::UnknownAgent)?
            .source_cursor)
    }

    /// The origin generation a replica's block was taken under; none while
    /// the row holds no block.
    pub fn source_generation(&self, agent: &AgentKey) -> Result<Option<u64>, StoreError> {
        let row = self.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        Ok(row.complete_from_order.map(|_| row.source_generation))
    }

    /// Whether this replica holds what `origin` holds of the agent: the
    /// same snapshot, and every row the origin's block has from the
    /// replica's boundary up, identical. A replica without a block is not
    /// current; rows the replica keeps below what the origin still holds
    /// are not compared.
    pub fn current_with(&self, agent: &AgentKey, origin: &Sqlite) -> Result<bool, StoreError> {
        let replica = self.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        let theirs = origin.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        let Some(boundary) = replica.complete_from_order else {
            return Ok(false);
        };
        if replica.snapshot != theirs.snapshot {
            return Ok(false);
        }
        let Some(their_floor) = block_floor(&theirs, origin.is_own(agent)) else {
            return Ok(false);
        };
        let from = boundary.max(their_floor);
        Ok(self.suffix_from(agent, from)? == origin.suffix_from(agent, from)?)
    }

    /// How many resets the agent's replica row has absorbed since the store
    /// opened or was restored from a dump.
    pub fn reset_count(&self, agent: &AgentKey) -> Result<u64, StoreError> {
        self.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        Ok(self.resets.get(agent).copied().unwrap_or(0))
    }
}
