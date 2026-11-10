//! The executor's share of the three HIR benchmarks (stage H4 of
//! `docs/design/hir.md`, section 7.4).
//!
//! The sources are the H0 benchmarks of `crates/orna-evaluator/src/bin/orna_bench.rs`,
//! lowered through the differential harness and run by `orna-exec`. One JSON record is
//! printed on stdout. The step and call counters are deterministic and gated by
//! `scripts/check_benchmark_baseline.py` against `crates/orna-differential/benchmarks/exec.json`;
//! wall time is recorded and not gated. A benchmark the executor cannot run yet is
//! listed under `not_yet` with the construct that stopped it, not guessed at.
//!
//! ```sh
//! cargo run --release -p orna-differential --bin hir_exec_bench > /tmp/exec.json
//! python3 scripts/check_benchmark_baseline.py \
//!     --baseline crates/orna-differential/benchmarks/exec.json --results /tmp/exec.json
//! ```

use std::{fmt::Write as _, process::ExitCode, time::Instant};

use orna_differential::{
    case::{Case, Family, Source},
    observe_new::lower_case,
};
use orna_exec::{ExecLimits, Measured, Value, measure_entry};
use orna_hir::{DefId, DefKind, Program};

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

/// B3, a filter over 10^5 rows of an in-memory table. The executor has no table
/// scan yet, so it is reported under `not_yet`, never measured.
const B3_SOURCE: &str = "\
pub table Reading(id: Int) {
    value: Int,
}

pub fn hot(): Int {
    Reading | filter(r => r.value > 50) | count
}
";

/// The limits of every benchmark: the H0 step budget, with the executor's
/// depth and width bounds set well above what the benchmarks reach.
fn benchmark_limits() -> ExecLimits {
    ExecLimits {
        max_steps: 50_000_000,
        max_depth: 10_000,
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
    let lowered = lower_case(&case)?;
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
}

/// Runs one entry with the given integer arguments, timing only the run.
fn run(program: &Program, entry: DefId, arguments: &[i64]) -> Result<Outcome, String> {
    let values = arguments.iter().map(|argument| Value::from(*argument)).collect();
    let started = Instant::now();
    let measured = measure_entry(program, entry, values, benchmark_limits()).map_err(|error| format!("{error:?}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(Outcome { measured, wall_ms })
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

fn record(name: &str, outcome: &Outcome) -> String {
    format!(
        "{name:?}:{{\"steps\":{},\"calls\":{},\"wall_ms\":{:.3}}}",
        outcome.measured.steps, outcome.measured.calls, outcome.wall_ms
    )
}

fn measure_benchmarks() -> Result<String, String> {
    let (b1_program, b1_entry) = lower_entry("b1-fib", "b1-fib.orna", B1_SOURCE, "fib")?;
    let b1 = run(&b1_program, b1_entry, &[B1_ARGUMENT])?;
    expect(&b1, B1_RESULT, "b1_fib_25")?;

    let (b2_program, b2_entry) = lower_entry("b2-fold", "b2-fold.orna", B2_SOURCE, "total")?;
    let b2 = run(&b2_program, b2_entry, &[B2_ARGUMENT])?;
    expect(&b2, B2_RESULT, "b2_fold_1000000")?;

    let b3_not_yet = match lower_entry("b3-filter", "b3-filter.orna", B3_SOURCE, "hot") {
        Ok((program, entry)) => match run(&program, entry, &[]) {
            Ok(_) => return Err("b3_filter_100000 ran although the executor has no table scan".to_owned()),
            Err(reason) => reason,
        },
        Err(reason) => reason,
    };

    let mut text = String::from("{\"pipeline\":\"exec\",\"benchmarks\":{");
    let _ = write!(text, "{},{}", record("b1_fib_25", &b1), record("b2_fold_1000000", &b2));
    let _ = write!(text, "}},\"not_yet\":{{\"b3_filter_100000\":{b3_not_yet:?}}}}}");
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
