//! Deterministic step parity for the typed intrinsic dispatch.
//!
//! The baseline was captured from the pre-S2 evaluator at the stage base
//! (`origin/main` before the catalogue dispatch). Each program exercises a
//! native operation through the unresolved qualified spelling, so both the old
//! string-table route and the new id route reach the same handler.

use orna_evaluator::{
    Environment, Functions, Limits, StepBudget, evaluate_with_functions_and_budget,
};
use orna_syntax::parse_expression;

const N_SMALL: usize = 256;
const N_MEDIUM: usize = 1024;
const N_LARGE: usize = 4096;

/// Pre-stage step counts, in `PROGRAMS` order.
const BASELINE: &[u64] = &[
    259, 2050, 4098, 514, 2050, 20484, 1797, 7172, 12292, 292, 3044, 16389, 1283, 1059, 16389,
    1795, 3077, 8709, 770, 7173, 20485, 2052, 3075, 12292, 258, 2049, 8194, 516, 3074, 8194, 9, 9,
    3, 3, 2, 3, 2, 3, 902, 10247,
];

/// One program per entry. `{L}` is the input list and `{M}` the second list.
const PROGRAMS: &[(&str, usize, &str)] = &[
    ("count-256", N_SMALL, "std.collection.count({L})"),
    ("sum-1024", N_MEDIUM, "std.collection.sum({L})"),
    ("first-4096", N_LARGE, "std.collection.first({L})"),
    ("min-256", N_SMALL, "std.collection.min({L})"),
    ("max-1024", N_MEDIUM, "std.collection.max({L})"),
    (
        "map-sum-4096",
        N_LARGE,
        "std.collection.map({L}, x => x + 1) | std.collection.sum",
    ),
    (
        "filter-count-256",
        N_SMALL,
        "std.collection.filter({L}, x => x % 2 == 0) | std.collection.count",
    ),
    (
        "sort-first-1024",
        N_MEDIUM,
        "std.collection.sort_by({L}, x => x) | std.collection.first",
    ),
    (
        "distinct-count-4096",
        N_LARGE,
        "std.collection.distinct({L}) | std.collection.count",
    ),
    (
        "take-sum-256",
        N_SMALL,
        "std.collection.take({L}, 16) | std.collection.sum",
    ),
    (
        "drop-sum-1024",
        N_MEDIUM,
        "std.collection.drop({L}, 16) | std.collection.sum",
    ),
    (
        "union-count-4096",
        N_LARGE,
        "std.collection.union({L}, {M}) | std.collection.count",
    ),
    (
        "every-256",
        N_SMALL,
        "std.collection.every({L}, x => x >= 0)",
    ),
    (
        "exists-1024",
        N_MEDIUM,
        "std.collection.exists({L}, x => x == 7)",
    ),
    (
        "flat-map-count-4096",
        N_LARGE,
        "std.collection.flat_map({L}, x => [x, x]) | std.collection.count",
    ),
    (
        "partition-count-256",
        N_SMALL,
        "std.collection.partition({L}, x => x % 2 == 0)",
    ),
    (
        "zip-count-1024",
        N_MEDIUM,
        "std.collection.zip({L}, {M}) | std.collection.count",
    ),
    (
        "chunk-count-4096",
        N_LARGE,
        "std.collection.chunk({L}, 8) | std.collection.count",
    ),
    (
        "window-count-256",
        N_SMALL,
        "std.collection.window({L}, 4) | std.collection.count",
    ),
    (
        "split-when-count-1024",
        N_MEDIUM,
        "std.collection.split_when({L}, x => x % 5 == 0) | std.collection.count",
    ),
    (
        "group-by-count-4096",
        N_LARGE,
        "std.collection.group_by({L}, x => x % 3) | std.collection.count",
    ),
    (
        "rank-count-256",
        N_SMALL,
        "std.collection.rank({L}, x => x) | std.collection.count",
    ),
    (
        "pairs-count-1024",
        N_MEDIUM,
        "std.collection.pairs({L}) | std.collection.count",
    ),
    (
        "unique-count-4096",
        N_LARGE,
        "std.collection.unique({L}) | std.collection.count",
    ),
    ("stats-mean-256", N_SMALL, "std.stats.mean({L})"),
    ("stats-median-1024", N_MEDIUM, "std.stats.median({L})"),
    ("stats-sum-4096", N_LARGE, "std.stats.sum({L})"),
    (
        "stats-mode-256",
        N_SMALL,
        "std.stats.mode({L}) | std.collection.count",
    ),
    ("stats-range-1024", N_MEDIUM, "std.stats.range({L})"),
    ("stats-variance-4096", N_LARGE, "std.stats.variance({L})"),
    (
        "text-split-count-256",
        N_SMALL,
        "std.text.split(\"a,b,c,d\", \",\") | std.collection.count",
    ),
    (
        "text-join-1024",
        N_MEDIUM,
        "std.text.join([\"a\", \"b\", \"c\"], \"-\")",
    ),
    ("bits-or-4096", N_LARGE, "std.bits.bit_or(255, 15)"),
    (
        "hash-sha256-1024",
        N_MEDIUM,
        "std.hash.sha256_text(\"orna\")",
    ),
    (
        "base64-encode-4096",
        N_LARGE,
        "std.encoding.base64.decode(\"aGVsbG8=\")",
    ),
    ("bits-and-256", N_SMALL, "std.bits.bit_and(255, 15)"),
    ("money-quantize-1024", N_MEDIUM, "std.text.trim(\"  x  \")"),
    ("json-encode-4096", N_LARGE, "std.encoding.json.__encode(1)"),
    (
        "chunk-flatten-count-256",
        N_SMALL,
        "std.collection.chunk({L}, 4) | std.collection.flatten | std.collection.count",
    ),
    (
        "map-filter-count-1024",
        N_MEDIUM,
        "std.collection.map({L}, x => x + 1) | std.collection.filter(x => x % 2 == 0) | std.collection.count",
    ),
];

fn list(n: usize) -> String {
    let mut source = String::with_capacity(n * 4 + 2);
    source.push('[');
    for value in 0..n {
        if value != 0 {
            source.push_str(", ");
        }
        source.push_str(&value.to_string());
    }
    source.push(']');
    source
}

fn limits() -> Limits {
    Limits {
        max_collection_items: N_LARGE * 2,
        max_steps: 50_000_000,
        max_string_bytes: 1 << 20,
        ..Limits::default()
    }
}

fn steps(source: &str) -> u64 {
    let limits = limits();
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    let mut budget = StepBudget::new(limits.max_steps);
    evaluate_with_functions_and_budget(
        &parsed.value,
        &Environment::new(),
        &Functions::new(),
        limits,
        &mut budget,
    )
    .unwrap_or_else(|error| panic!("{source}: {}", error.code()));
    limits.max_steps - budget.remaining()
}

#[test]
fn step_counts_match_the_pre_stage_baseline() {
    assert_eq!(PROGRAMS.len(), BASELINE.len());
    let print = std::env::var_os("ORNA_STEP_BASELINE").is_some();
    for (index, (name, n, template)) in PROGRAMS.iter().enumerate() {
        let source = template.replace("{L}", &list(*n)).replace("{M}", &list(*n));
        let measured = steps(&source);
        if print {
            println!("BASELINE[{index}] = {measured}; // {name}");
        } else {
            assert_eq!(measured, BASELINE[index], "{name}");
        }
    }
}
