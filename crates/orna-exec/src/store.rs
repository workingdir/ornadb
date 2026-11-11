//! The table store interface the executor reads and writes through.
//!
//! This is the shape `docs/design/hir.md` 5.2 defines. Rows and keys are the
//! canonical encodings of `orna-value`, so the store never decodes them. The
//! store knows tables by [`TableId`]. Writes happen only inside one activation:
//! [`TableStore::begin`] opens it, [`TableStore::commit`] keeps its writes and
//! [`TableStore::rollback`] undoes every one of them (ORNA-TXN-002, D4).

use std::ops::Bound;

use orna_hir::DefId;

pub use memory::MemoryStore;

mod memory;
#[cfg(any(test, feature = "test-support"))]
pub mod contract;

/// A table, named by its definition in the program.
pub type TableId = DefId;

/// The position of an open cursor in its store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CursorId(pub u64);

/// The order a cursor visits its rows in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Ascending canonical key order.
    Forward,
    /// Descending canonical key order.
    Reverse,
}

/// The canonical-key range a cursor covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBounds {
    /// The lower end of the range.
    pub lower: Bound<Vec<u8>>,
    /// The upper end of the range.
    pub upper: Bound<Vec<u8>>,
}

impl KeyBounds {
    /// Every key of the table.
    #[must_use]
    pub fn all() -> Self {
        Self {
            lower: Bound::Unbounded,
            upper: Bound::Unbounded,
        }
    }
}

/// One row as borrowed canonical key and row bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRef<'a> {
    /// The row's canonical key.
    pub key: &'a [u8],
    /// The row's canonical encoding.
    pub row: &'a [u8],
}

/// What a [`TableStore::put`] requires of the key's current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// The key must hold no row.
    Absent,
    /// The key must hold a row.
    Present,
    /// No requirement.
    Any,
}

/// A store failure. None of these is a language error: the executor maps them
/// to a canonical code at the call that reached them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The store was asked to write outside an activation.
    NoActivation,
    /// The store was asked to begin an activation while one is open.
    ActivationOpen,
    /// The store is a read-only snapshot.
    ReadOnly,
    /// The cursor is not open.
    UnknownCursor(CursorId),
    /// An `Expect` requirement did not hold for the key.
    Expectation(&'static str),
}

/// The table store. Its methods take `&mut self` even for reads, because a
/// cursor advances and a store may read ahead.
pub trait TableStore {
    /// Opens an activation. Writes are legal only inside one.
    ///
    /// # Errors
    ///
    /// [`StoreError::ActivationOpen`] when one is already open.
    fn begin(&mut self) -> Result<(), StoreError>;

    /// Keeps every write of the open activation.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoActivation`] when none is open.
    fn commit(&mut self) -> Result<(), StoreError>;

    /// Undoes every write of the open activation.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoActivation`] when none is open.
    fn rollback(&mut self) -> Result<(), StoreError>;

    /// Opens a cursor over `table` within `bounds`. It sees the activation's
    /// own writes made before each advance.
    ///
    /// # Errors
    ///
    /// Store-specific failures, for example a read-only snapshot that lacks the table.
    fn scan(
        &mut self,
        table: TableId,
        bounds: KeyBounds,
        dir: Direction,
    ) -> Result<CursorId, StoreError>;

    /// The next row of the cursor, or `None` at its end.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownCursor`] when the cursor is not open.
    fn next(&mut self, cursor: CursorId) -> Result<Option<RowRef<'_>>, StoreError>;

    /// Closes the cursor. Closing an unknown cursor does nothing.
    fn close(&mut self, cursor: CursorId);

    /// The row at `key`, if any.
    ///
    /// # Errors
    ///
    /// Store-specific failures.
    fn get(&mut self, table: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;

    /// Writes the row at `key`, checking `expect` against the key's current state first.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoActivation`] outside an activation, [`StoreError::Expectation`]
    /// when `expect` does not hold.
    fn put(
        &mut self,
        table: TableId,
        key: &[u8],
        row: &[u8],
        expect: Expect,
    ) -> Result<(), StoreError>;

    /// Removes the row at `key`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoActivation`] outside an activation.
    fn delete(&mut self, table: TableId, key: &[u8]) -> Result<(), StoreError>;

    /// Allocates the next automatic `Int` key of `table`. An allocated value is
    /// never handed out again, even when its activation rolls back (ORNA-AUTOID-002).
    ///
    /// # Errors
    ///
    /// Store-specific failures.
    fn next_auto_id(&mut self, table: TableId) -> Result<i64, StoreError>;

    /// Rows [`TableStore::next`] has returned since the store was created. A run's reads are the difference of two reads.
    fn rows_read(&self) -> u64;
}
