//! The executor's share of the three HIR benchmarks (stage H4 of
//! `docs/design/hir.md`, section 7.4).
//!
//! The sources are the H0 benchmarks of `crates/orna-evaluator/src/bin/orna_bench.rs`,
//! lowered through the differential harness and run by `orna-exec`. One JSON record is
//! printed on stdout. The step and call counters are deterministic and gated by
//! `scripts/check_benchmark_baseline.py` against `crates/orna-differential/benchmarks/exec.json`;
//! heap allocations made during each run are counted by the allocator below and gated the
//! same way; wall time is recorded and not gated. A benchmark the executor cannot run yet
//! is listed under `not_yet` with the construct that stopped it, not guessed at.
//!
//! ```sh
//! cargo run --release -p orna-differential --bin hir_exec_bench > /tmp/exec.json
//! python3 scripts/check_benchmark_baseline.py \
//!     --baseline crates/orna-differential/benchmarks/exec.json --results /tmp/exec.json
//! ```

use std::{
    alloc::{GlobalAlloc, Layout, System},
    fmt::Write as _,
    process::ExitCode,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use orna_differential::{
    case::{Case, Family, Source},
    observe_new::lower_case_admitted,
};
use orna_exec::{ExecLimits, Measured, MemoryStore, TableStore, Value, measure_entry_with_store};
use orna_hir::{DefId, DefKind, Program};
use orna_value::{CanonicalValue, Raw};

/// Counts every heap allocation the process makes, so a run's allocations are the
/// difference of two reads around it. The harness runs on one thread, so the count
/// is exact for the measured run.
struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's layout and pointer
// unchanged, and only adds a relaxed counter increment on the way.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// B1, recursion: `fib(25)`, 242,785 source-level calls.
const B1_SOURCE: &str = "\
pub fn fib(n: Int): Int {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}
";
const B1_ARGUMENT: i64 = 25;
const B1_RESULT: i64 = 75_025;

/// B2, a fold over 10^6 integers through a `while` loop.
const B2_SOURCE: &str = "\
pub fn total(n: Int): Int {
    let sum = 0;
    let i = 1;
    while i <= n {
        sum += i;
        i += 1;
    }
    sum
}
";
const B2_ARGUMENT: i64 = 1_000_000;
const B2_RESULT: i64 = 500_000_500_000;

/// B3, a filter over 10^5 rows of an in-memory table, and the pull-tree property: `first_ten` stops the scan
/// after ten rows, so the store reports ten rows read where `hot` reads all of them.
const B3_SOURCE: &str = "\
pub table Reading(id: Int) {
    value: Int,
}

pub fn hot(): Int {
    Reading | filter(r => r.value > 50) | count
}

pub fn first_ten(): Int {
    Reading | take(10) | count
}
";
const B3_ROWS: i64 = 100_000;
const B3_HOT_RESULT: i64 = 49_000;
const B3_FIRST_TEN_RESULT: i64 = 10;

/// The limits of every benchmark: the H0 step budget, with the executor's
/// depth and width bounds set well above what the benchmarks reach.
fn benchmark_limits() -> ExecLimits {
    ExecLimits {
        max_steps: 50_000_000,
        max_depth: 10_000,
        max_collection_items: 1 << 20,
        max_string_bytes: 1 << 20,
        max_integer_digits: 10_000,
    }
}

/// Lowers one module and returns its program with the entry function `name`.
fn lower_entry(id: &str, file: &str, text: &str, name: &str) -> Result<(Program, DefId), String> {
    let source = Source {
        path: file.to_owned(),
        logical_path: file.to_owned(),
        text: text.to_owned(),
    };
    let case = Case::module(id.to_owned(), Family::Expression, source);
    let lowered = lower_case_admitted(&case)?;
    let entry = lowered
        .program
        .defs
        .iter_enumerated()
        .find(|(_, def)| def.name.to_string() == name && matches!(def.kind, DefKind::Fn(_)))
        .map(|(id, _)| id)
        .ok_or_else(|| format!("{id}: no function {name}"))?;
    Ok((lowered.program, entry))
}

/// The outcome of one benchmark on the executor.
struct Outcome {
    measured: Measured,
    wall_ms: f64,
    allocations: usize,
}

/// Encodes `raw` in the canonical form the store keeps rows and keys in.
fn canonical(raw: Raw) -> Result<Vec<u8>, String> {
    CanonicalValue::new(raw)
        .and_then(|value| value.encode())
        .map_err(|error| format!("canonical encoding failed: {error:?}"))
}

/// The table store with the `Reading` rows of B3 committed, as a prior session would leave them.
fn seeded_reading_store(program: &Program) -> Result<Box<dyn TableStore>, String> {
    let table = program
        .defs
        .iter_enumerated()
        .find(|(_, def)| def.name.to_string() == "Reading" && matches!(def.kind, DefKind::Table(_)))
        .map(|(id, _)| id)
        .ok_or_else(|| "b3-filter: no table Reading".to_owned())?;
    let mut store = MemoryStore::new();
    for id in 0..B3_ROWS {
        let key = canonical(Raw::Int(id.into()))?;
        let row = canonical(Raw::Map(vec![
            (Raw::Text("id".to_owned()), Raw::Int(id.into())),
            (Raw::Text("value".to_owned()), Raw::Int((id % 100).into())),
        ]))?;
        store.seed(table, &key, &row);
    }
    Ok(Box::new(store))
}

/// Runs one entry with the given integer arguments against `store`. Only the run is timed and
/// counted; building the argument values and the store happens before either starts.
fn run(
    program: &Program,
    entry: DefId,
    arguments: &[i64],
    store: Box<dyn TableStore>,
) -> Result<Outcome, String> {
    let values: Vec<Value> = arguments
        .iter()
        .map(|argument| Value::from(*argument))
        .collect();
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    let started = Instant::now();
    let (measured, _store) =
        measure_entry_with_store(program, entry, values, benchmark_limits(), store);
    let measured = measured.map_err(|error| format!("{error:?}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
    Ok(Outcome {
        measured,
        wall_ms,
        allocations,
    })
}

/// Asserts the benchmark produced the value it must, so a change that skips work
/// cannot masquerade as an improvement.
fn expect(outcome: &Outcome, expected: i64, benchmark: &str) -> Result<(), String> {
    if outcome.measured.value == Value::from(expected) {
        Ok(())
    } else {
        Err(format!(
            "{benchmark} produced {:?}, expected {expected}",
            outcome.measured.value
        ))
    }
}

/// Asserts the store reported exactly `expected` rows read, so a scan that stops early or reads too far fails.
fn expect_rows(outcome: &Outcome, expected: u64, benchmark: &str) -> Result<(), String> {
    if outcome.measured.rows_read == expected {
        Ok(())
    } else {
        Err(format!(
            "{benchmark} read {} rows, expected {expected}",
            outcome.measured.rows_read
        ))
    }
}

/// One benchmark's JSON entry. Only the table benchmarks record `rows_read`; the others read no rows.
fn record(name: &str, outcome: &Outcome, reads_rows: bool) -> String {
    let rows_read = if reads_rows {
        format!(",\"rows_read\":{}", outcome.measured.rows_read)
    } else {
        String::new()
    };
    format!(
        "{name:?}:{{\"steps\":{},\"calls\":{},\"allocations\":{}{rows_read},\"wall_ms\":{:.3}}}",
        outcome.measured.steps, outcome.measured.calls, outcome.allocations, outcome.wall_ms
    )
}

fn measure_benchmarks() -> Result<String, String> {
    let (b1_program, b1_entry) = lower_entry("b1-fib", "b1-fib.orna", B1_SOURCE, "fib")?;
    let b1 = run(
        &b1_program,
        b1_entry,
        &[B1_ARGUMENT],
        Box::new(MemoryStore::new()),
    )?;
    expect(&b1, B1_RESULT, "b1_fib_25")?;

    let (b2_program, b2_entry) = lower_entry("b2-fold", "b2-fold.orna", B2_SOURCE, "total")?;
    let b2 = run(
        &b2_program,
        b2_entry,
        &[B2_ARGUMENT],
        Box::new(MemoryStore::new()),
    )?;
    expect(&b2, B2_RESULT, "b2_fold_1000000")?;

    let (hot_program, hot_entry) = lower_entry("b3-filter", "b3-filter.orna", B3_SOURCE, "hot")?;
    let hot_store = seeded_reading_store(&hot_program)?;
    let b3_hot = run(&hot_program, hot_entry, &[], hot_store)?;
    expect(&b3_hot, B3_HOT_RESULT, "b3_filter_100000")?;
    expect_rows(&b3_hot, B3_ROWS as u64, "b3_filter_100000")?;

    let (take_program, take_entry) =
        lower_entry("b3-take", "b3-take.orna", B3_SOURCE, "first_ten")?;
    let take_store = seeded_reading_store(&take_program)?;
    let b3_take = run(&take_program, take_entry, &[], take_store)?;
    expect(&b3_take, B3_FIRST_TEN_RESULT, "b3_take_10")?;
    expect_rows(&b3_take, 10, "b3_take_10")?;

    let mut text = String::from("{\"pipeline\":\"exec\",\"benchmarks\":{");
    let _ = write!(
        text,
        "{},{},{},{}",
        record("b1_fib_25", &b1, false),
        record("b2_fold_1000000", &b2, false),
        record("b3_filter_100000", &b3_hot, true),
        record("b3_take_10", &b3_take, true)
    );
    text.push_str("}}");
    Ok(text)
}

fn main() -> ExitCode {
    match measure_benchmarks() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hir_exec_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::measure_benchmarks;

    #[test]
    fn record_lists_every_benchmark_with_its_counters() {
        let text = measure_benchmarks().expect("the executor runs B1, B2 and B3");
        assert!(text.starts_with("{\"pipeline\":\"exec\",\"benchmarks\":{\"b1_fib_25\":{"));
        assert!(text.contains("\"b2_fold_1000000\":{"));
        assert!(text.contains("\"b3_filter_100000\":{"));
        assert!(text.contains("\"b3_take_10\":{"));
        assert!(text.contains("\"rows_read\":10,"));
    }
}
