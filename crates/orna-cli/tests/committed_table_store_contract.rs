//! The committed table store against the shared `TableStore` read contract, and
//! a bounded-memory scan of a committed table larger than the retired
//! 100,000-row cap.

#[path = "support/committed_int_table.rs"]
mod committed_int_table;
use orna_cli_testkit::format3;

use committed_int_table::{CommittedIntTable, committed_int_table, runtime_key, table_id};
use orna_exec::contract::{SeedableStore, run_read_contract};
use orna_exec::{CursorId, Direction, Expect, KeyBounds, RowRef, StoreError, TableId, TableStore};

/// Rows in the normal-suite scan. The seed writes this many rows in batched
/// `orna run`s, so the normal suite stays within minutes.
const SCAN_ROWS: u32 = 5_000;

/// Rows in the ignored above-cap scan: more than the retired 100,000-row cap.
const LARGE_ROWS: u32 = 100_001;

/// Peak growth the scan may add to the process high-water mark. The scan holds
/// one page at a time, so the bound is far below the size of the table.
const MAX_SCAN_GROWTH_BYTES: u64 = 64 * 1024 * 1024;

/// A committed `Item` table as a `TableStore`. Each method delegates to the
/// production [`orna_project::CommittedTableStore`].
struct Seeded(CommittedIntTable);

impl TableStore for Seeded {
    fn begin(&mut self) -> Result<(), StoreError> {
        self.0.store.begin()
    }

    fn commit(&mut self) -> Result<(), StoreError> {
        self.0.store.commit()
    }

    fn rollback(&mut self) -> Result<(), StoreError> {
        self.0.store.rollback()
    }

    fn scan(
        &mut self,
        table: TableId,
        bounds: KeyBounds,
        dir: Direction,
    ) -> Result<CursorId, StoreError> {
        self.0.store.scan(table, bounds, dir)
    }

    fn next(&mut self, cursor: CursorId) -> Result<Option<RowRef<'_>>, StoreError> {
        self.0.store.next(cursor)
    }

    fn close(&mut self, cursor: CursorId) {
        self.0.store.close(cursor);
    }

    fn get(&mut self, table: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.store.get(table, key)
    }

    fn put(
        &mut self,
        table: TableId,
        key: &[u8],
        row: &[u8],
        expect: Expect,
    ) -> Result<(), StoreError> {
        self.0.store.put(table, key, row, expect)
    }

    fn delete(&mut self, table: TableId, key: &[u8]) -> Result<(), StoreError> {
        self.0.store.delete(table, key)
    }

    fn next_auto_id(&mut self, table: TableId) -> Result<i64, StoreError> {
        self.0.store.next_auto_id(table)
    }

    fn rows_read(&self) -> u64 {
        self.0.store.rows_read()
    }
}

impl SeedableStore for Seeded {
    /// Commits the contiguous rows `indices` (an empty slice commits an empty table).
    fn seeded(indices: &[u32]) -> Self {
        let (first, end) = match (indices.first(), indices.last()) {
            (Some(&first), Some(&last)) => {
                assert!(
                    indices
                        .iter()
                        .zip(first..)
                        .all(|(&index, expected)| index == expected),
                    "the contract seeds contiguous indices"
                );
                (first, last + 1)
            }
            _ => (0, 0),
        };
        Self(committed_int_table(first, end))
    }

    fn key(index: u32) -> Vec<u8> {
        runtime_key(index)
    }
}

/// Peak resident set size of this process in bytes, from its `VmHWM` status line.
fn peak_rss_bytes() -> u64 {
    let kib: u64 = std::fs::read_to_string("/proc/self/status")
        .expect("procfs status")
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("Linux reports VmHWM");
    kib * 1024
}

#[test]
fn committed_store_meets_the_read_contract() {
    run_read_contract::<Seeded>();
}

#[test]
#[ignore = "seeds 100,001 rows in batched runs: about 2 hours; run as an evidence job"]
fn scan_of_a_table_above_the_retired_cap_completes_with_bounded_memory() {
    scan_with_bounded_memory(LARGE_ROWS);
}

#[test]
fn scan_visits_every_row_with_bounded_memory() {
    scan_with_bounded_memory(SCAN_ROWS);
}

fn scan_with_bounded_memory(rows: u32) {
    let mut seeded = Seeded(committed_int_table(0, rows));
    let baseline = peak_rss_bytes();
    let store = &mut seeded.0.store;
    let cursor = store
        .scan(table_id(), KeyBounds::all(), Direction::Forward)
        .expect("the committed table scans");
    let mut visited: u64 = 0;
    while store.next(cursor).expect("the cursor advances").is_some() {
        visited += 1;
    }
    store.close(cursor);
    let growth = peak_rss_bytes().saturating_sub(baseline);
    println!("committed scan of {rows} rows: peak growth {growth} bytes");

    assert_eq!(visited, u64::from(rows));
    assert_eq!(store.rows_read(), u64::from(rows));
    assert!(
        growth < MAX_SCAN_GROWTH_BYTES,
        "the scan grew the peak by {growth} bytes, above {MAX_SCAN_GROWTH_BYTES}"
    );
}

#[test]
fn taking_ten_rows_reads_exactly_ten_rows() {
    let mut seeded = Seeded(committed_int_table(0, SCAN_ROWS));
    let store = &mut seeded.0.store;
    let cursor = store
        .scan(table_id(), KeyBounds::all(), Direction::Forward)
        .expect("the committed table scans");
    for _ in 0..10 {
        assert!(store.next(cursor).expect("the cursor advances").is_some());
    }
    store.close(cursor);
    assert_eq!(store.rows_read(), 10);
}
