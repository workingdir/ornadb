//! The three baseline benchmarks of the HIR design (section 7.4), measured on
//! the old evaluator through its public API.
//!
//! One JSON record is printed on stdout; wall time and the deterministic step
//! and call counters are recorded. `scripts/check_benchmark_baseline.py`
//! compares the counters with `crates/orna-evaluator/benchmarks/baseline.json`
//! and fails on a regression; wall time is printed for the job summary and is
//! not gated, because shared CI runners are noisy.
//!
//! The binary records calls through the `route-trace` feature, so it is only
//! built when that feature is on:
//!
//! ```sh
//! cargo run --release -p orna-evaluator --bin orna_bench --features route-trace \
//!     > /tmp/bench.json
//! python3 scripts/check_benchmark_baseline.py \
//!     --baseline crates/orna-evaluator/benchmarks/baseline.json --results /tmp/bench.json
//! ```
//!
//! B1 recurses (`fib(25)`, 242,785 calls). B2 folds a million integers with a
//! `while` loop so it measures the evaluator, not a materialised list. B3
//! filters 10^5 in-memory rows and counts them: the rows come from the
//! evaluator's relation-source effect boundary, so the operator tree is
//! measured without SQLite or Git.
//!
//! The design's optional `sum(1..=n)` range variant is not measured: the old
//! evaluator refuses it with `ORNA-EVAL-TYPE` (a range is not a collection
//! `sum` accepts), so it has no H0 baseline. H6 measures range iteration.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    process::ExitCode,
    time::{Duration, Instant},
};

use num_bigint::BigInt;
use orna_evaluator::{
    EffectHandler, Environment, Functions, Limits, PureFunction, RelationPage, StepBudget,
    invoke_named_with_effects_and_budget, route_trace,
};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::{Declaration, Expr, Parameter};
use orna_value::{CanonicalValue, Raw};

/// B1, recursion: 242,785 calls at `n = 25`.
const B1_SOURCE: &str = "\
pub fn fib(n: Int): Int {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}
";
const B1_ARGUMENT: i64 = 25;

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

/// B3, a filter over 10^5 rows of an in-memory table.
const B3_SOURCE: &str = "\
pub table Reading(id: Int) {
    value: Int,
}

pub fn hot(): Int {
    Reading | filter(r => r.value > 50) | count
}
";
const B3_ROWS: usize = 100_000;

/// Explicit limits. The defaults (419,600 steps, 2,048 items) refuse all three
/// benchmarks, so every run states its own bound (design section 7.4).
fn b1_limits() -> Limits {
    Limits {
        max_steps: 50_000_000,
        ..Limits::default()
    }
}

fn b2_limits() -> Limits {
    Limits {
        max_steps: 50_000_000,
        ..Limits::default()
    }
}

fn b3_limits() -> Limits {
    Limits {
        max_steps: 50_000_000,
        max_collection_items: 200_000,
        ..Limits::default()
    }
}

/// The evaluator serves nothing on behalf of this handler.
struct NoEffects;

impl EffectHandler for NoEffects {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, orna_evaluator::EvaluationError> {
        Ok(None)
    }
}

/// The in-memory `Reading` table: `rows` rows whose `value` is `index % 100`.
struct ReadingRows {
    rows: usize,
}

impl ReadingRows {
    fn row(&self, index: usize) -> CanonicalValue {
        let value = Raw::Int(BigInt::from(index % 100));
        CanonicalValue::new(Raw::Map(vec![(Raw::Text("value".to_owned()), value)]))
            .expect("a one-field row is canonical")
    }
}

impl EffectHandler for ReadingRows {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, orna_evaluator::EvaluationError> {
        Ok(None)
    }

    fn admitted_relation_source(&self, name: &str) -> Option<String> {
        (name == "Reading").then(|| name.to_owned())
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, orna_evaluator::EvaluationError> {
        assert_eq!(source, "Reading");
        budget.debit(1)?;
        let start = after
            .and_then(|cursor| <[u8; 8]>::try_from(cursor).ok())
            .map_or(0, |cursor| u64::from_be_bytes(cursor) as usize);
        let end = (start + limit.max(1)).min(self.rows);
        let page = (start..end).map(|index| self.row(index)).collect();
        let next = (end < self.rows).then(|| (end as u64).to_be_bytes().to_vec());
        Ok(Some(RelationPage { rows: page, next }))
    }
}

fn function(source: &str, file: &str, name: &str) -> (Vec<Parameter>, Expr) {
    let parsed = parse_module_with_file(source, file);
    assert!(parsed.is_ok(), "{file}: {:#?}", parsed.diagnostics);
    for item in parsed.value.items {
        if let Declaration::Function { signature, body } = item.declaration
            && signature.name == name
        {
            return (signature.parameters, body);
        }
    }
    panic!("{file} declares no function {name}")
}

fn functions(source: &str, file: &str, names: &[&str]) -> Functions {
    let mut functions = Functions::new();
    for name in names {
        let (parameters, body) = function(source, file, name);
        functions.insert(
            (*name).to_owned(),
            PureFunction {
                parameters,
                body,
                environment: Environment::new(),
            },
        );
    }
    functions
}

fn integer(value: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Int(BigInt::from(value))).expect("an integer is canonical")
}

/// The outcome of one benchmark.
struct Measured {
    steps: u64,
    calls: usize,
    wall: Duration,
    value: CanonicalValue,
}

/// Runs one named function under a caller-owned budget and a call collector.
fn measure(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<Measured, String> {
    let mut budget = StepBudget::new(limits.max_steps);
    let started = Instant::now();
    let (result, records) = route_trace::collect(|| {
        invoke_named_with_effects_and_budget(
            name,
            functions,
            arguments,
            limits,
            effects,
            &mut budget,
        )
    });
    let wall = started.elapsed();
    let value = result.map_err(|error| format!("{name} failed: {}", error.code()))?;
    Ok(Measured {
        steps: limits.max_steps - budget.remaining(),
        calls: records.len(),
        wall,
        value,
    })
}

/// Asserts the benchmarked program produced the value it is supposed to
/// produce, so a change that makes the evaluator skip work cannot masquerade as
/// an improvement.
fn expect(value: &CanonicalValue, expected: i64, benchmark: &str) {
    assert_eq!(
        value.raw(),
        &Raw::Int(BigInt::from(expected)),
        "{benchmark} produced an unexpected value"
    );
}

fn json(object: &[(&str, String)]) -> String {
    let mut text = String::from("{");
    for (index, (name, value)) in object.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        let _ = write!(&mut text, "{name:?}:{value}");
    }
    text.push('}');
    text
}

fn record(measured: &Measured) -> String {
    json(&[
        ("steps", measured.steps.to_string()),
        ("calls", measured.calls.to_string()),
        (
            "wall_ms",
            format!("{:.3}", measured.wall.as_secs_f64() * 1_000.0),
        ),
    ])
}

fn main() -> ExitCode {
    let b1_functions = functions(B1_SOURCE, "b1-fib.orna", &["fib"]);
    let b2_functions = functions(B2_SOURCE, "b2-fold.orna", &["total"]);
    let b3_functions = functions(B3_SOURCE, "b3-filter.orna", &["hot"]);

    let mut arguments = Environment::new();
    arguments.insert("n".into(), integer(B1_ARGUMENT));
    let b1 = match measure(
        "fib",
        &b1_functions,
        &arguments,
        b1_limits(),
        &mut NoEffects,
    ) {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("b1_fib_25: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut arguments = Environment::new();
    arguments.insert("n".into(), integer(B2_ARGUMENT));
    let b2 = match measure(
        "total",
        &b2_functions,
        &arguments,
        b2_limits(),
        &mut NoEffects,
    ) {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("b2_fold_1000000: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut rows = ReadingRows { rows: B3_ROWS };
    let b3 = match measure(
        "hot",
        &b3_functions,
        &Environment::new(),
        b3_limits(),
        &mut rows,
    ) {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("b3_filter_100000: {error}");
            return ExitCode::FAILURE;
        }
    };

    expect(&b1.value, 75_025, "b1_fib_25");
    expect(&b2.value, 500_000_500_000, "b2_fold_1000000");
    expect(&b3.value, 49_000, "b3_filter_100000");

    let benchmarks = BTreeMap::from([
        ("b1_fib_25".to_owned(), record(&b1)),
        ("b2_fold_1000000".to_owned(), record(&b2)),
        ("b3_filter_100000".to_owned(), record(&b3)),
    ]);
    let body = benchmarks
        .iter()
        .map(|(name, value)| json_pair(name, value))
        .collect::<Vec<_>>()
        .join(",");
    println!("{{\"evaluator\":\"old\",\"benchmarks\":{{{body}}}}}");
    ExitCode::SUCCESS
}

fn json_pair(name: &str, value: &str) -> String {
    format!("{name:?}:{value}")
}
