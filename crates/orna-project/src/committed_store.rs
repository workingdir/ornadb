//! Committed table rows served through the executor's [`TableStore`] interface.
//!
//! The store is read-only: it serves one committed snapshot of each table. Every
//! write and activation is refused with [`StoreError::ReadOnly`], and `commit` and
//! `rollback` report [`StoreError::NoActivation`], because no activation can open.
//! A forward scan pages through the repository, so it holds at most one page of
//! rows at a time. A reverse scan has to buffer its rows and is bounded by the
//! store's buffer quota.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;

use orna_check::TableSchema;
use orna_exec::{CursorId, Direction, Expect, KeyBounds, RowRef, StoreError, TableId, TableStore};
use orna_repository::{GitCommitRef, KeyRange, Repository, RepositoryFormatContext, TypedKey};
use orna_runtime::TableObjectId;
use orna_value::Value;

use crate::ProjectLoader;
use crate::table_storage::{
    OpenedTable, evaluator_key_from_runtime, open_table, runtime_key_of_row, stored_tuple_of_row,
};

/// Rows fetched from the repository per page of a forward scan.
const PAGE_ROWS: usize = 64;

/// The message for committed storage that does not match its descriptor contract.
const STORAGE_CONTRACT: &str = "committed table storage violates its contract";

/// The message for a repository range read that failed.
const RANGE_READ: &str = "committed table range read failed";

/// One stored row as runtime key bytes and stored-tuple bytes.
type Row = (Vec<u8>, Vec<u8>);

/// One open cursor: its table, bounds and direction, and the rows read ahead of the caller.
struct Cursor {
    table: TableId,
    bounds: KeyBounds,
    /// Rows read but not yet returned, in the order the cursor visits them.
    buffered: VecDeque<Row>,
    /// The row [`TableStore::next`] returned last; the returned [`RowRef`] borrows it.
    current: Option<Row>,
    /// The typed key the next forward page resumes after.
    resume_after: Option<TypedKey>,
    /// The last runtime key read, for the strictly-increasing order check.
    last_key: Option<Vec<u8>>,
    /// Whether no further rows can be read: the repository is exhausted or the upper bound passed.
    finished: bool,
}

/// The committed rows of a set of tables, served as a read-only [`TableStore`].
pub struct CommittedTableStore {
    tables: BTreeMap<TableId, OpenedTable>,
    cursors: BTreeMap<CursorId, Cursor>,
    next_cursor: u64,
    rows_read: u64,
    max_buffered_rows: usize,
}

impl CommittedTableStore {
    /// Opens the committed snapshot at `commit` for every `(table id, table name)` pair.
    /// A reverse scan may buffer at most `max_buffered_rows` rows.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownTable`] when a name is not in `tables` or `table_ids`, or when
    /// two pairs share a table id. [`StoreError::Backend`] when a table's committed
    /// storage cannot be opened.
    pub fn open_committed(
        repository: &Repository,
        format_context: &RepositoryFormatContext,
        commit: &GitCommitRef,
        tables: &BTreeMap<String, TableSchema>,
        table_ids: &BTreeMap<String, TableObjectId>,
        names: impl IntoIterator<Item = (TableId, String)>,
        max_buffered_rows: usize,
    ) -> Result<Self, StoreError> {
        let mut opened = BTreeMap::new();
        for (id, name) in names {
            let schema = tables.get(&name).ok_or(StoreError::UnknownTable(id))?;
            let object_id = table_ids
                .get(&name)
                .copied()
                .ok_or(StoreError::UnknownTable(id))?;
            let resolved = ProjectLoader::default()
                .resolve_committed_source_table_schema(repository, commit, object_id)
                .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))?;
            let table = open_table(format_context, object_id, resolved, schema)
                .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))?;
            if opened.insert(id, table).is_some() {
                return Err(StoreError::UnknownTable(id));
            }
        }
        Ok(Self {
            tables: opened,
            cursors: BTreeMap::new(),
            next_cursor: 0,
            rows_read: 0,
            max_buffered_rows,
        })
    }

    fn table(&self, table: TableId) -> Result<&OpenedTable, StoreError> {
        self.tables
            .get(&table)
            .ok_or(StoreError::UnknownTable(table))
    }
}

impl TableStore for CommittedTableStore {
    fn begin(&mut self) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn commit(&mut self) -> Result<(), StoreError> {
        Err(StoreError::NoActivation)
    }

    fn rollback(&mut self) -> Result<(), StoreError> {
        Err(StoreError::NoActivation)
    }

    fn scan(
        &mut self,
        table: TableId,
        bounds: KeyBounds,
        dir: Direction,
    ) -> Result<CursorId, StoreError> {
        let opened = self.table(table)?;
        let mut cursor = Cursor {
            table,
            bounds,
            buffered: VecDeque::new(),
            current: None,
            resume_after: None,
            last_key: None,
            finished: false,
        };
        if dir == Direction::Reverse {
            // A reverse scan reads the whole range first, so its buffer is the bound.
            while !cursor.finished {
                fill_page(opened, &mut cursor)?;
                if cursor.buffered.len() > self.max_buffered_rows {
                    return Err(StoreError::Quota {
                        rows_seen: cursor.buffered.len(),
                    });
                }
            }
            cursor.buffered.make_contiguous().reverse();
        }
        let id = CursorId(self.next_cursor);
        self.next_cursor += 1;
        self.cursors.insert(id, cursor);
        Ok(id)
    }

    fn next(&mut self, cursor: CursorId) -> Result<Option<RowRef<'_>>, StoreError> {
        let Self {
            tables,
            cursors,
            rows_read,
            ..
        } = self;
        let state = cursors
            .get_mut(&cursor)
            .ok_or(StoreError::UnknownCursor(cursor))?;
        let opened = tables
            .get(&state.table)
            .ok_or(StoreError::UnknownTable(state.table))?;
        while state.buffered.is_empty() && !state.finished {
            fill_page(opened, state)?;
        }
        state.current = state.buffered.pop_front();
        if state.current.is_some() {
            *rows_read += 1;
        }
        Ok(state.current.as_ref().map(|(key, row)| RowRef { key, row }))
    }

    fn close(&mut self, cursor: CursorId) {
        self.cursors.remove(&cursor);
    }

    fn get(&mut self, table: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let opened = self.table(table)?;
        // A key that does not decode to a typed key matches no stored row.
        let Some(typed) = typed_key_of_runtime(key, opened.text_primary_key) else {
            return Ok(None);
        };
        let range =
            KeyRange::new(Some(typed), None, 1).map_err(|_| StoreError::Backend(RANGE_READ))?;
        let rows = opened
            .graph
            .range_rows(&range, &opened.read_scope)
            .map_err(|_| StoreError::Backend(RANGE_READ))?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let found = runtime_key_of_row(row, opened.key_count, opened.text_primary_key)
            .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))?;
        if found != key {
            return Ok(None);
        }
        stored_tuple_of_row(opened, row)
            .map(Some)
            .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))
    }

    fn put(
        &mut self,
        _table: TableId,
        _key: &[u8],
        _row: &[u8],
        _expect: Expect,
    ) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn delete(&mut self, _table: TableId, _key: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn next_auto_id(&mut self, _table: TableId) -> Result<i64, StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn rows_read(&self) -> u64 {
        self.rows_read
    }
}

/// Reads the next page of a table in canonical key order into the cursor's buffer,
/// resuming after the last key of the previous page. Rows below the lower bound are
/// read and dropped; the first row at or above the upper bound ends the scan.
fn fill_page(table: &OpenedTable, cursor: &mut Cursor) -> Result<(), StoreError> {
    let range =
        KeyRange::new(None, None, PAGE_ROWS).map_err(|_| StoreError::Backend(RANGE_READ))?;
    let range = match cursor.resume_after.take() {
        Some(key) => range
            .with_resume_after(key)
            .map_err(|_| StoreError::Backend(RANGE_READ))?,
        None => range,
    };
    let page = table
        .graph
        .range_rows(&range, &table.read_scope)
        .map_err(|_| StoreError::Backend(RANGE_READ))?;
    let Some(last) = page.last() else {
        cursor.finished = true;
        return Ok(());
    };
    cursor.resume_after = Some(last.key().clone());
    for row in &page {
        let key = runtime_key_of_row(row, table.key_count, table.text_primary_key)
            .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))?;
        // Runtime bytes must follow canonical key order for the scan to be answerable
        // from the repository's order; a row out of order means they do not.
        if cursor.last_key.as_ref().is_some_and(|last| *last >= key) {
            return Err(StoreError::UnsupportedOrder);
        }
        if !below_upper(&cursor.bounds, &key) {
            cursor.finished = true;
            return Ok(());
        }
        let above_lower = above_lower(&cursor.bounds, &key);
        cursor.last_key = Some(key.clone());
        if above_lower {
            let tuple = stored_tuple_of_row(table, row)
                .map_err(|_| StoreError::Backend(STORAGE_CONTRACT))?;
            cursor.buffered.push_back((key, tuple));
        }
    }
    Ok(())
}

fn above_lower(bounds: &KeyBounds, key: &[u8]) -> bool {
    match &bounds.lower {
        Bound::Unbounded => true,
        Bound::Included(lower) => key >= lower.as_slice(),
        Bound::Excluded(lower) => key > lower.as_slice(),
    }
}

fn below_upper(bounds: &KeyBounds, key: &[u8]) -> bool {
    match &bounds.upper {
        Bound::Unbounded => true,
        Bound::Included(upper) => key <= upper.as_slice(),
        Bound::Excluded(upper) => key < upper.as_slice(),
    }
}

/// The typed key a runtime key stores under, or `None` when the bytes are not a key.
fn typed_key_of_runtime(key: &[u8], text_primary_key: bool) -> Option<TypedKey> {
    let evaluator = evaluator_key_from_runtime(key, text_primary_key).ok()?;
    let value = Value::decode(&evaluator).ok()?;
    TypedKey::from_value(&value).ok()
}
