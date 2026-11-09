//! Deterministic step parity for the typed intrinsic dispatch.
//!
//! Stage S2 replaces the string tables in `orna-evaluator` with a typed
//! `IntrinsicId` dispatch. The routing change must not alter how many
//! evaluation steps a program consumes: `docs/design/std-native.md` section 4
//! (S2) requires this test to record the baseline captured from the
//! pre-stage implementation (`origin/main`) and assert equality.
//!
//! Every program is built from a finite list of `n` values and reaches the
//! native arm through an unresolved `std.<module>.<leaf>` spelling, so the
//! measurement is independent of whether a caller admitted a standard layer.
//! The recorded number is the smallest `max_steps` budget that lets the whole
//! expression evaluate; raising the budget never turns success into failure,
//! so the predicate is monotone and the minimum is well defined.
//!
//! `ORNA_STD_STEP_PARITY_EMIT=1` prints the table instead of asserting it; the
//! constants below were produced by running this same file against
//! `origin/main` (pre-stage) with that variable set.

use orna_evaluator::{Environment, Limits, evaluate_expression};

/// Sizes measured by the design (section 4, stage S2).
const SIZES: [usize; 3] = [256, 1024, 4096];

/// Smallest successful `max_steps` per program at `SIZES`.
const BASELINE: [(&str, [u64; 3]); 27] = [
    ("filter", [1795, 7171, 28675]),
    ("map", [1027, 4099, 16387]),
    ("flat_map", [1027, 4099, 16387]),
    ("sort_by", [2330, 11589, 54102]),
    ("take", [269, 1037, 4109]),
    ("drop", [505, 2041, 8185]),
    ("distinct", [770, 3074, 12290]),
    ("union", [1027, 4099, 16387]),
    ("count", [259, 1027, 4099]),
    ("first", [258, 1026, 4098]),
    ("one", [261, 1029, 4101]),
    ("sum", [514, 2050, 8194]),
    ("min", [514, 2050, 8194]),
    ("max", [514, 2050, 8194]),
    ("every", [1283, 5123, 20483]),
    ("exists", [1283, 5123, 20483]),
    ("group_by", [1283, 5123, 20483]),
    ("stats_mean", [258, 1026, 4098]),
    ("stats_median", [513, 2049, 8193]),
    ("stats_sum", [514, 2050, 8194]),
    ("stats_range", [770, 3074, 12290]),
    ("stats_mode", [514, 2050, 8194]),
    ("stats_variance", [514, 2050, 8194]),
    ("text_split", [1031, 4103, 16391]),
    ("stats_percentile", [260, 1028, 4100]),
    ("text_join", [773, 3077, 12293]),
    ("bits_or", [3, 3, 3]),
];

fn list(size: usize) -> String {
    let mut source = String::from("[");
    for value in 1..=size {
        if value > 1 {
            source.push_str(", ");
        }
        source.push_str(&value.to_string());
    }
    source.push(']');
    source
}

/// One program per catalogue family, parameterised by the list length.
fn programs(size: usize) -> Vec<(&'static str, String)> {
    let values = list(size);
    vec![
        (
            "filter",
            format!("std.collection.filter({values}, value => value % 2 == 0)"),
        ),
        (
            "map",
            format!("std.collection.map({values}, value => value + 1)"),
        ),
        (
            "flat_map",
            format!("std.collection.flat_map({values}, value => [value, value])"),
        ),
        (
            "sort_by",
            format!("std.collection.sort_by({values}, value => value % 97)"),
        ),
        ("take", format!("std.collection.take({values}, 10)")),
        ("drop", format!("std.collection.drop({values}, 10)")),
        ("distinct", format!("std.collection.distinct({values})")),
        ("union", format!("std.collection.union({values}, {values})")),
        ("count", format!("std.collection.count({values})")),
        ("first", format!("std.collection.first({values})")),
        (
            "one",
            format!("std.collection.one(std.collection.take({values}, 1))"),
        ),
        ("sum", format!("std.collection.sum({values})")),
        ("min", format!("std.collection.min({values})")),
        ("max", format!("std.collection.max({values})")),
        (
            "every",
            format!("std.collection.every({values}, value => value > 0)"),
        ),
        (
            "exists",
            format!("std.collection.exists({values}, value => value < 0)"),
        ),
        (
            "group_by",
            format!("std.collection.group_by({values}, value => value % 4)"),
        ),
        ("stats_mean", format!("std.stats.mean({values})")),
        ("stats_median", format!("std.stats.median({values})")),
        ("stats_sum", format!("std.stats.sum({values})")),
        ("stats_range", format!("std.stats.range({values})")),
        ("stats_mode", format!("std.stats.mode({values})")),
        ("stats_variance", format!("std.stats.variance({values})")),
        (
            "text_split",
            format!(
                "std.text.split(std.text.join(std.collection.map({values}, value => \"x\"), \",\"), \",\")"
            ),
        ),
        (
            "stats_percentile",
            format!("std.stats.percentile({values}, 0.25, \"linear\")"),
        ),
        (
            "text_join",
            format!("std.text.join(std.collection.map({values}, value => \"x\"), \",\")"),
        ),
        ("bits_or", String::from("std.bits.bit_or(123, 456)")),
    ]
}

fn evaluates_within(program: &str, max_steps: u64) -> bool {
    evaluate_expression(
        program,
        &Environment::new(),
        Limits {
            max_steps,
            max_collection_items: 16_384,
            ..Limits::default()
        },
    )
    .is_ok()
}

/// The smallest step budget that evaluates `program` completely.
fn consumed_steps(program: &str) -> u64 {
    assert!(
        evaluates_within(program, u64::MAX),
        "program must evaluate without a budget: {program}"
    );
    let mut high = 64u64;
    while !evaluates_within(program, high) {
        high = high
            .checked_mul(2)
            .expect("step budget search does not overflow");
        assert!(high <= 1 << 40, "step budget stays bounded: {program}");
    }
    let mut low = 0u64;
    while low < high {
        let middle = low + (high - low) / 2;
        if evaluates_within(program, middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

#[test]
fn step_counts_match_the_pre_stage_baseline() {
    let emit = std::env::var_os("ORNA_STD_STEP_PARITY_EMIT").is_some();
    let per_size = SIZES
        .into_iter()
        .map(|size| {
            let programs = programs(size);
            assert_eq!(programs.len(), BASELINE.len(), "one program per row");
            programs
        })
        .collect::<Vec<_>>();
    for (index, (expected_name, expected)) in BASELINE.iter().enumerate() {
        let mut measured = [0u64; 3];
        for (size_index, programs) in per_size.iter().enumerate() {
            let (name, program) = &programs[index];
            assert_eq!(
                name, expected_name,
                "program order is part of the recorded table"
            );
            measured[size_index] = consumed_steps(program);
        }
        if emit {
            println!(
                "    (\"{expected_name}\", [{}, {}, {}]),",
                measured[0], measured[1], measured[2]
            );
            continue;
        }
        for (size_index, size) in SIZES.into_iter().enumerate() {
            assert_eq!(
                measured[size_index], expected[size_index],
                "{expected_name} at n = {size} consumed {} steps; the pre-stage baseline is {}",
                measured[size_index], expected[size_index]
            );
        }
    }
}
