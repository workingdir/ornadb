//! Contract tests every [`TableStore`] implementation must pass. The read-side
//! cases ([`run_read_contract`]) take a [`SeedableStore`] and run against any
//! store that can be seeded with rows; the write-side cases ([`run_write_contract`])
//! take a constructor and stay memory-only until a writable committed store exists.
//! The in-memory store runs both in this crate.

use std::ops::Bound;

use orna_hir::DefId;

#[cfg(test)]
use super::CursorId;
use super::{Direction, Expect, KeyBounds, MemoryStore, StoreError, TableId, TableStore};

fn table(raw: u32) -> TableId {
    DefId::new(raw)
}

/// A [`TableStore`] the read-side cases can build with rows. A seeded store holds
/// valid rows for its own encoding, so the cases never assume a row shape.
pub trait SeedableStore: TableStore {
    /// A store whose `table(1)` holds one row for each index in `indices`, at [`SeedableStore::key`].
    fn seeded(indices: &[u32]) -> Self;

    /// The canonical key of `index`. Ascending indices give ascending keys.
    fn key(index: u32) -> Vec<u8>;
}

impl SeedableStore for MemoryStore {
    fn seeded(indices: &[u32]) -> Self {
        let mut store = MemoryStore::new();
        store.begin().expect("begin");
        for &index in indices {
            store
                .put(table(1), &Self::key(index), b"row", Expect::Absent)
                .expect("seed");
        }
        store.commit().expect("commit");
        store
    }

    fn key(index: u32) -> Vec<u8> {
        index.to_be_bytes().to_vec()
    }
}

fn keys(
    store: &mut impl TableStore,
    table: TableId,
    bounds: KeyBounds,
    dir: Direction,
) -> Vec<Vec<u8>> {
    let cursor = store.scan(table, bounds, dir).expect("scan opens");
    let mut out = Vec::new();
    while let Some(row) = store.next(cursor).expect("next advances") {
        out.push(row.key.to_vec());
    }
    store.close(cursor);
    out
}

fn committed_row(store: &mut impl TableStore, table: TableId, key: &[u8], row: &[u8]) {
    store.begin().expect("begin");
    store.put(table, key, row, Expect::Any).expect("put");
    store.commit().expect("commit");
}

/// Writes outside an activation are refused and leave nothing behind.
pub fn writes_need_an_activation<S: TableStore>(mut store: S) {
    let t = table(1);
    assert_eq!(
        store.put(t, b"a", b"1", Expect::Any),
        Err(StoreError::NoActivation)
    );
    assert_eq!(store.delete(t, b"a"), Err(StoreError::NoActivation));
    assert_eq!(store.get(t, b"a").expect("get"), None);
}

/// A second `begin` while one is open is refused, and `commit` without one is too.
pub fn activations_do_not_nest<S: TableStore>(mut store: S) {
    store.begin().expect("begin");
    assert_eq!(store.begin(), Err(StoreError::ActivationOpen));
    store.commit().expect("commit");
    assert_eq!(store.commit(), Err(StoreError::NoActivation));
    assert_eq!(store.rollback(), Err(StoreError::NoActivation));
}

/// Committed writes are readable; rolled-back writes are not.
pub fn commit_keeps_and_rollback_undoes<S: TableStore>(mut store: S) {
    let t = table(1);
    committed_row(&mut store, t, b"a", b"one");

    store.begin().expect("begin");
    store
        .put(t, b"a", b"changed", Expect::Present)
        .expect("update");
    store.put(t, b"b", b"new", Expect::Absent).expect("insert");
    store.delete(t, b"a").expect("delete");
    store.rollback().expect("rollback");

    assert_eq!(store.get(t, b"a").expect("get"), Some(b"one".to_vec()));
    assert_eq!(store.get(t, b"b").expect("get"), None);
}

/// Rollback restores a row that later writes in the same activation changed.
pub fn rollback_restores_the_first_state<S: TableStore>(mut store: S) {
    let t = table(1);
    committed_row(&mut store, t, b"a", b"base");

    store.begin().expect("begin");
    store
        .put(t, b"a", b"first", Expect::Any)
        .expect("first write");
    store
        .put(t, b"a", b"second", Expect::Any)
        .expect("second write");
    store.delete(t, b"a").expect("delete");
    store.rollback().expect("rollback");

    assert_eq!(store.get(t, b"a").expect("get"), Some(b"base".to_vec()));
}

/// `Expect` is checked against the key's current state before any write.
pub fn expectations_are_checked<S: TableStore>(mut store: S) {
    let t = table(1);
    committed_row(&mut store, t, b"a", b"one");

    store.begin().expect("begin");
    assert_eq!(
        store.put(t, b"a", b"x", Expect::Absent),
        Err(StoreError::Expectation("absent"))
    );
    assert_eq!(
        store.put(t, b"b", b"x", Expect::Present),
        Err(StoreError::Expectation("present"))
    );
    store.rollback().expect("rollback");
    assert_eq!(store.get(t, b"a").expect("get"), Some(b"one".to_vec()));
}

/// A scan visits keys in canonical byte order, forward and reverse, within bounds.
/// The seeded store holds indices 0 to 4.
pub fn scans_are_ordered_and_bounded<S: SeedableStore>(mut store: S) {
    let t = table(1);
    let all = keys(&mut store, t, KeyBounds::all(), Direction::Forward);
    let expected: Vec<Vec<u8>> = (0..5).map(S::key).collect();
    assert_eq!(all, expected);

    let reversed = keys(&mut store, t, KeyBounds::all(), Direction::Reverse);
    assert_eq!(reversed, all.iter().rev().cloned().collect::<Vec<_>>());

    let inner = KeyBounds {
        lower: Bound::Excluded(S::key(0)),
        upper: Bound::Included(S::key(3)),
    };
    assert_eq!(
        keys(&mut store, t, inner.clone(), Direction::Forward),
        (1..=3).map(S::key).collect::<Vec<_>>()
    );
    assert_eq!(
        keys(&mut store, t, inner, Direction::Reverse),
        (1..=3).rev().map(S::key).collect::<Vec<_>>()
    );
}

/// Empty or inverted bounds yield no rows and never panic.
pub fn empty_bounds_yield_nothing<S: SeedableStore>(mut store: S) {
    let t = table(1);
    let inverted = KeyBounds {
        lower: Bound::Included(S::key(5)),
        upper: Bound::Included(S::key(0)),
    };
    assert!(keys(&mut store, t, inverted, Direction::Forward).is_empty());
    let touching = KeyBounds {
        lower: Bound::Excluded(S::key(1)),
        upper: Bound::Excluded(S::key(1)),
    };
    assert!(keys(&mut store, t, touching, Direction::Reverse).is_empty());
}

/// A cursor sees rows written after it opened, when they lie ahead of its position.
pub fn cursors_read_their_own_writes<S: TableStore>(mut store: S) {
    let t = table(1);
    committed_row(&mut store, t, b"a", b"row");
    committed_row(&mut store, t, b"c", b"row");

    store.begin().expect("begin");
    let cursor = store
        .scan(t, KeyBounds::all(), Direction::Forward)
        .expect("scan");
    let first = store
        .next(cursor)
        .expect("next")
        .map(|row| row.key.to_vec());
    assert_eq!(first, Some(b"a".to_vec()));
    store
        .put(t, b"b", b"staged", Expect::Absent)
        .expect("insert ahead");
    store.delete(t, b"c").expect("delete ahead");
    let second = store
        .next(cursor)
        .expect("next")
        .map(|row| row.key.to_vec());
    assert_eq!(second, Some(b"b".to_vec()));
    assert_eq!(
        store
            .next(cursor)
            .expect("next")
            .map(|row| row.key.to_vec()),
        None
    );
    store.close(cursor);
    store.rollback().expect("rollback");
}

/// Operations on a closed cursor fail, and closing twice does nothing.
pub fn closed_cursors_are_unknown<S: TableStore>(mut store: S) {
    let cursor = store
        .scan(table(1), KeyBounds::all(), Direction::Forward)
        .expect("scan");
    store.close(cursor);
    store.close(cursor);
    assert_eq!(
        store.next(cursor).map(|_| ()),
        Err(StoreError::UnknownCursor(cursor))
    );
}

/// Automatic ids increase and are not reused after a rollback.
pub fn auto_ids_survive_rollback<S: TableStore>(mut store: S) {
    let t = table(1);
    assert_eq!(store.next_auto_id(t).expect("first"), 1);
    store.begin().expect("begin");
    assert_eq!(store.next_auto_id(t).expect("second"), 2);
    store.rollback().expect("rollback");
    assert_eq!(store.next_auto_id(t).expect("third"), 3);
}

/// Rows the large-table case seeds: more than the retired 2,048-row cap.
const LARGE_ROWS: u32 = 3_000;

/// Scanning more than 2,048 rows is not capped: the whole table is visited.
pub fn large_tables_are_not_capped<S: SeedableStore>(mut store: S) {
    let visited = keys(&mut store, table(1), KeyBounds::all(), Direction::Forward);
    assert_eq!(visited.len(), LARGE_ROWS as usize);
}

/// The read-side cases. Each one receives a store seeded with the rows it reads.
pub fn run_read_contract<S: SeedableStore>() {
    scans_are_ordered_and_bounded(S::seeded(&[0, 1, 2, 3, 4]));
    empty_bounds_yield_nothing(S::seeded(&[1]));
    closed_cursors_are_unknown(S::seeded(&[]));
    large_tables_are_not_capped(S::seeded(&(0..LARGE_ROWS).collect::<Vec<_>>()));
}

/// The write-side cases, which need a store that accepts writes. `make` builds a fresh, empty store for each case.
pub fn run_write_contract<S: TableStore>(make: impl Fn() -> S) {
    writes_need_an_activation(make());
    activations_do_not_nest(make());
    commit_keeps_and_rollback_undoes(make());
    rollback_restores_the_first_state(make());
    expectations_are_checked(make());
    cursors_read_their_own_writes(make());
    auto_ids_survive_rollback(make());
}

#[cfg(test)]
#[test]
fn memory_store_meets_the_contract() {
    run_read_contract::<MemoryStore>();
    run_write_contract(MemoryStore::new);
}

#[cfg(test)]
#[test]
fn snapshots_read_committed_rows_and_refuse_writes() {
    let t = table(1);
    let mut live = MemoryStore::new();
    committed_row(&mut live, t, b"a", b"one");
    let mut snapshot = live.snapshot();

    committed_row(&mut live, t, b"a", b"two");
    assert_eq!(snapshot.get(t, b"a").expect("get"), Some(b"one".to_vec()));
    assert_eq!(snapshot.begin(), Err(StoreError::ReadOnly));
    assert_eq!(
        snapshot.put(t, b"b", b"x", Expect::Any),
        Err(StoreError::ReadOnly)
    );
    assert_eq!(snapshot.delete(t, b"a"), Err(StoreError::ReadOnly));
    assert_eq!(
        keys(&mut snapshot, t, KeyBounds::all(), Direction::Forward),
        vec![b"a".to_vec()]
    );
}

#[cfg(test)]
#[test]
fn cursor_ids_are_distinct() {
    let mut store = MemoryStore::new();
    let first: CursorId = store
        .scan(table(1), KeyBounds::all(), Direction::Forward)
        .expect("first");
    let second: CursorId = store
        .scan(table(1), KeyBounds::all(), Direction::Forward)
        .expect("second");
    assert_ne!(first, second);
}
