//! An in-memory [`TableStore`]: tests, the browser profile and the
//! differential harness. Rows live in one ordered map per table, so a cursor
//! visits them in canonical key order without any per-scan sort.
//!
//! Writes are undone through an undo log that lives only while an activation
//! is open. The auto-id counter is outside the log: an allocated id is never
//! handed out again after a rollback (ORNA-AUTOID-002).

use std::collections::BTreeMap;
use std::ops::Bound;

use super::{CursorId, Direction, Expect, KeyBounds, RowRef, StoreError, TableId, TableStore};

/// One write as it must be undone: the key's state before the write.
#[derive(Debug)]
struct Undo {
    table: TableId,
    key: Vec<u8>,
    previous: Option<Vec<u8>>,
}

/// An open cursor: its range, its direction and the last key it returned.
#[derive(Debug)]
struct Cursor {
    table: TableId,
    bounds: KeyBounds,
    dir: Direction,
    last: Option<Vec<u8>>,
}

/// The in-memory store. [`MemoryStore::snapshot`] makes a read-only copy at
/// the current committed state, which is how `as_of` reads an earlier state.
#[derive(Debug, Default)]
pub struct MemoryStore {
    tables: BTreeMap<TableId, BTreeMap<Vec<u8>, Vec<u8>>>,
    auto_ids: BTreeMap<TableId, i64>,
    undo: Option<Vec<Undo>>,
    cursors: BTreeMap<CursorId, Cursor>,
    next_cursor: u64,
    read_only: bool,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A read-only copy of the store's committed rows. Its writes, and its
    /// activations, fail with [`StoreError::ReadOnly`].
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            tables: self.tables.clone(),
            auto_ids: self.auto_ids.clone(),
            read_only: true,
            ..Self::default()
        }
    }

    /// Writes the row at `key` directly, outside any activation. This seeds a
    /// store with committed rows, as a prior session would have left them.
    pub fn seed(&mut self, table: TableId, key: &[u8], row: &[u8]) {
        self.tables
            .entry(table)
            .or_default()
            .insert(key.to_vec(), row.to_vec());
    }

    fn writable(&self) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        if self.undo.is_none() {
            return Err(StoreError::NoActivation);
        }
        Ok(())
    }

    fn current(&self, table: TableId, key: &[u8]) -> Option<&Vec<u8>> {
        self.tables.get(&table).and_then(|rows| rows.get(key))
    }

    /// Replaces or removes the row at `key`, recording the previous state.
    fn write(&mut self, table: TableId, key: &[u8], row: Option<Vec<u8>>) {
        let rows = self.tables.entry(table).or_default();
        let previous = match row {
            Some(row) => rows.insert(key.to_vec(), row),
            None => rows.remove(key),
        };
        if let Some(undo) = self.undo.as_mut() {
            undo.push(Undo {
                table,
                key: key.to_vec(),
                previous,
            });
        }
    }

    /// The key of the first row of `cursor` after its last key, in its direction.
    fn advance(&self, cursor: CursorId) -> Result<Option<Vec<u8>>, StoreError> {
        let state = self
            .cursors
            .get(&cursor)
            .ok_or(StoreError::UnknownCursor(cursor))?;
        let Some(rows) = self.tables.get(&state.table) else {
            return Ok(None);
        };
        let found = match state.dir {
            Direction::Forward => {
                let lower = match &state.last {
                    Some(last) => Bound::Excluded(last.clone()),
                    None => state.bounds.lower.clone(),
                };
                if range_is_empty(&lower, &state.bounds.upper) {
                    None
                } else {
                    rows.range((lower, state.bounds.upper.clone())).next()
                }
            }
            Direction::Reverse => {
                let upper = match &state.last {
                    Some(last) => Bound::Excluded(last.clone()),
                    None => state.bounds.upper.clone(),
                };
                if range_is_empty(&state.bounds.lower, &upper) {
                    None
                } else {
                    rows.range((state.bounds.lower.clone(), upper)).next_back()
                }
            }
        };
        Ok(found.map(|(key, _)| key.clone()))
    }
}

/// Whether `(lower, upper)` has no key at all. `BTreeMap::range` panics on
/// such bounds, so the cursor checks first.
fn range_is_empty(lower: &Bound<Vec<u8>>, upper: &Bound<Vec<u8>>) -> bool {
    match (lower, upper) {
        (Bound::Unbounded, _) | (_, Bound::Unbounded) => false,
        (Bound::Included(lo), Bound::Included(hi)) => lo > hi,
        (Bound::Included(lo) | Bound::Excluded(lo), Bound::Excluded(hi))
        | (Bound::Excluded(lo), Bound::Included(hi)) => lo >= hi,
    }
}

impl TableStore for MemoryStore {
    fn begin(&mut self) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        if self.undo.is_some() {
            return Err(StoreError::ActivationOpen);
        }
        self.undo = Some(Vec::new());
        Ok(())
    }

    fn commit(&mut self) -> Result<(), StoreError> {
        self.undo.take().ok_or(StoreError::NoActivation)?;
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), StoreError> {
        let log = self.undo.take().ok_or(StoreError::NoActivation)?;
        for undo in log.into_iter().rev() {
            let rows = self.tables.entry(undo.table).or_default();
            match undo.previous {
                Some(row) => {
                    rows.insert(undo.key, row);
                }
                None => {
                    rows.remove(&undo.key);
                }
            }
        }
        self.cursors.clear();
        Ok(())
    }

    fn scan(
        &mut self,
        table: TableId,
        bounds: KeyBounds,
        dir: Direction,
    ) -> Result<CursorId, StoreError> {
        let id = CursorId(self.next_cursor);
        self.next_cursor += 1;
        self.cursors.insert(
            id,
            Cursor {
                table,
                bounds,
                dir,
                last: None,
            },
        );
        Ok(id)
    }

    fn next(&mut self, cursor: CursorId) -> Result<Option<RowRef<'_>>, StoreError> {
        let Some(key) = self.advance(cursor)? else {
            // The cursor stays open at its end; `close` releases it.
            return Ok(None);
        };
        let state = self
            .cursors
            .get_mut(&cursor)
            .ok_or(StoreError::UnknownCursor(cursor))?;
        let table = state.table;
        state.last = Some(key.clone());
        Ok(self
            .tables
            .get(&table)
            .and_then(|rows| rows.get_key_value(key.as_slice()))
            .map(|(key, row)| RowRef {
                key: key.as_slice(),
                row: row.as_slice(),
            }))
    }

    fn close(&mut self, cursor: CursorId) {
        self.cursors.remove(&cursor);
    }

    fn get(&mut self, table: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.current(table, key).cloned())
    }

    fn put(
        &mut self,
        table: TableId,
        key: &[u8],
        row: &[u8],
        expect: Expect,
    ) -> Result<(), StoreError> {
        self.writable()?;
        let exists = self.current(table, key).is_some();
        match expect {
            Expect::Absent if exists => return Err(StoreError::Expectation("absent")),
            Expect::Present if !exists => return Err(StoreError::Expectation("present")),
            _ => {}
        }
        self.write(table, key, Some(row.to_vec()));
        Ok(())
    }

    fn delete(&mut self, table: TableId, key: &[u8]) -> Result<(), StoreError> {
        self.writable()?;
        self.write(table, key, None);
        Ok(())
    }

    fn next_auto_id(&mut self, table: TableId) -> Result<i64, StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        let counter = self.auto_ids.entry(table).or_insert(0);
        *counter += 1;
        Ok(*counter)
    }
}
