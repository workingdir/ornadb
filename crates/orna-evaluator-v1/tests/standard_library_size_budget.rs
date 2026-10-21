//! Size proofs for the pinned standard library (ORNA-LIB-003; 09 "Resource
//! failure is explicit"). Every case runs the captured `.orna` bodies through
//! an admitted session and checks the exact result at 1,000 elements and at
//! the default 2,048-item limit under `Limits::default()`, and at 10,000
//! elements under explicit limits.
//!
//! The 10,000-element cases raise the item budget explicitly and size the step
//! budget with the default derivation of 200 steps per item plus a fixed
//! allowance (ORNA-CLOSURE-004: limits vary only where explicitly configured).

use std::time::Instant;

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 5] = [
    "std/collection.orna",
    "std/text.orna",
    "std/text/slicing.orna",
    "std/text/lines.orna",
    "std/format/strings.orna",
];

const SMALL: usize = 1_000;
const FULL: usize = 2_048;
const LARGE: usize = 10_000;

fn limits(size: usize) -> Limits {
    let defaults = Limits::default();
    if size <= defaults.max_collection_items {
        return defaults;
    }
    Limits {
        max_collection_items: size,
        max_steps: size as u64 * 200 + 10_000,
        ..defaults
    }
}

/// Loads the pinned modules, binds the shared inputs and returns the session.
/// `xs` holds `0..size`, `letters` holds `size` scalars and `page` holds
/// `size / 10` lines of nine scalars, each ending in LF.
fn session(size: usize) -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources("orna.std/size-budget", sources.clone())
        .expect("pinned collection and text modules form a source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("pinned text helpers resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), limits(size))
            .unwrap_or_else(|error| panic!("pinned modules fail to load: {}", error.code()));
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed source matches its pinned profile");
    }
    for input in [
        "use std.collection as collection;".to_owned(),
        "use std.text as text;".to_owned(),
        "use std.text.slicing as slicing;".to_owned(),
        "use std.text.lines as lines;".to_owned(),
        "use std.format.strings as strings;".to_owned(),
        format!("let xs = {};", ints(0..size)),
        format!("let letters = {};", text(&"a".repeat(size))),
        format!("let page = {};", text(&"aaaaaaaaa\\n".repeat(size / 10))),
    ] {
        assert_eq!(session.submit(&input), Ok(None), "{}", preview(&input));
    }
    session
}

fn ints(range: impl Iterator<Item = usize>) -> String {
    let items = range.map(|value| value.to_string()).collect::<Vec<_>>();
    format!("[{}]", items.join(","))
}

fn text(body: &str) -> String {
    format!("\"{body}\"")
}

fn preview(source: &str) -> &str {
    &source[..source.len().min(80)]
}

/// One timed proof: optional `let` statements followed by a Bool check.
struct Proof {
    label: &'static str,
    bindings: Vec<String>,
    check: String,
}

fn check(label: &'static str, check: String) -> Proof {
    Proof {
        label,
        bindings: Vec::new(),
        check,
    }
}

fn bound(label: &'static str, binding: String, check: String) -> Proof {
    Proof {
        label,
        bindings: vec![binding],
        check,
    }
}

/// Runs each proof and reports its wall time. Any evaluation failure,
/// including `ORNA-EVAL-LIMIT`, fails the test with its code.
fn prove(size: usize, proofs: Vec<Proof>) {
    let mut session = session(size);
    let truth = CanonicalValue::new(Raw::Bool(true)).expect("true is canonical");
    for proof in proofs {
        let started = Instant::now();
        for binding in &proof.bindings {
            match session.submit(binding) {
                Ok(None) => {}
                other => panic!(
                    "{} at {size} elements: {} -> {}",
                    proof.label,
                    preview(binding),
                    outcome(&other),
                ),
            }
        }
        let result = session.submit(&proof.check);
        eprintln!(
            "size {size:>6} {:<30} {:>10.3?}",
            proof.label,
            started.elapsed()
        );
        assert!(
            result
                .as_ref()
                .is_ok_and(|value| value.as_ref() == Some(&truth)),
            "{} at {size} elements: {} -> {}",
            proof.label,
            preview(&proof.check),
            outcome(&result),
        );
    }
}

fn outcome(result: &Result<Option<CanonicalValue>, orna_evaluator_v1::ReplError>) -> String {
    match result {
        Ok(value) => format!("{value:?}"),
        Err(error) => error.code().to_owned(),
    }
}

fn slicing(size: usize) {
    let half = size / 2;
    let quarter = size / 4;
    prove(
        size,
        vec![
            check(
                "text.slicing.take",
                format!(
                    "slicing.take(letters, {half}) == {}",
                    text(&"a".repeat(half))
                ),
            ),
            check(
                "text.slicing.drop",
                format!(
                    "slicing.drop(letters, {half}) == {}",
                    text(&"a".repeat(size - half))
                ),
            ),
            check(
                "text.slicing.slice",
                format!(
                    "slicing.slice(letters, {quarter}, {half}) == {}",
                    text(&"a".repeat(half))
                ),
            ),
            check(
                "collection.take",
                format!("collection.take(xs, {half}) == {}", ints(0..half)),
            ),
            check(
                "collection.drop",
                format!("collection.drop(xs, {half}) == {}", ints(half..size)),
            ),
        ],
    );
}

fn union_accumulate(size: usize) {
    let mut expected_lines = vec![text("aaaaaaaaa"); size / 10];
    expected_lines.push(text(""));
    prove(
        size,
        vec![
            check(
                "format.strings.repeat",
                format!("strings.repeat(\"a\", {size}) == letters"),
            ),
            check(
                "format.strings.pad_left",
                format!(
                    "strings.pad_left(\"x\", {size}, \".\") == {}",
                    text(&format!("{}x", ".".repeat(size - 1)))
                ),
            ),
            check(
                "text.lines.split",
                format!("lines.split(page) == [{}]", expected_lines.join(",")),
            ),
            check(
                "text.lines.normalise",
                "lines.normalise(page) == page".to_owned(),
            ),
        ],
    );
}

fn text_split(size: usize) {
    let lines = size / 10;
    prove(
        size,
        vec![
            check(
                "text.split scalars",
                format!("collection.count(text.split(letters, \"\")) == {size}"),
            ),
            check(
                "text.split separator",
                format!(
                    "collection.count(text.split(page, \"\\n\")) == {}",
                    lines + 1
                ),
            ),
            check(
                "text.lines.count",
                format!("lines.count(page) == {}", lines + 1),
            ),
            check(
                "text.join round trip",
                "text.join(text.split(page, \"\\n\"), \"\\n\") == page".to_owned(),
            ),
        ],
    );
}

fn collection_helpers(size: usize) {
    let half = size / 2;
    let last = size - 1;
    let chunks = size.div_ceil(10);
    let last_chunk = (size - 1) / 10 * 10;
    let even = ints((0..size).step_by(2));
    let odd = ints((1..size).step_by(2));
    prove(
        size,
        vec![
            check(
                "collection.filter",
                format!("collection.filter(xs, value => value % 2 == 0) == {even}"),
            ),
            check(
                "collection.map",
                format!(
                    "collection.map(xs, value => value + 1) == {}",
                    ints(1..size + 1)
                ),
            ),
            check(
                "collection.flat_map",
                "collection.flat_map(xs, value => [value]) == xs".to_owned(),
            ),
            check(
                "collection.partition",
                format!("collection.partition(xs, value => value % 2 == 0) == ({even}, {odd})"),
            ),
            bound(
                "collection.chunk",
                "let chunked = collection.chunk(xs, 10);".to_owned(),
                format!(
                    "collection.count(chunked) == {chunks} \
                     && collection.take(chunked, 1) == [{}] \
                     && collection.drop(chunked, {}) == [{}]",
                    ints(0..10),
                    chunks - 1,
                    ints(last_chunk..size)
                ),
            ),
            check(
                "collection.flatten",
                "collection.flatten(chunked) == xs".to_owned(),
            ),
            bound(
                "collection.window",
                "let windows = collection.window(xs, 3);".to_owned(),
                format!(
                    "collection.count(windows) == {} && collection.drop(windows, {}) == [{}]",
                    size - 2,
                    size - 3,
                    ints(size - 3..size)
                ),
            ),
            bound(
                "collection.zip",
                "let zipped = collection.zip(xs, xs);".to_owned(),
                format!(
                    "collection.count(zipped) == {size} \
                     && collection.drop(zipped, {last}) == [({last}, {last})]"
                ),
            ),
            bound(
                "collection.pairs",
                "let paired = collection.pairs(xs);".to_owned(),
                format!(
                    "collection.count(paired) == {last} \
                     && collection.drop(paired, {}) == [({}, {last})]",
                    size - 2,
                    size - 2
                ),
            ),
            bound(
                "collection.split_when",
                "let groups = collection.split_when(xs, value => value % 10 == 0);".to_owned(),
                format!(
                    "collection.count(groups) == {chunks} && collection.drop(groups, {}) == [{}]",
                    chunks - 1,
                    ints(last_chunk..size)
                ),
            ),
            check(
                "collection.every/exists/one",
                format!(
                    "collection.every(xs, value => value >= 0) \
                     && collection.exists(xs, value => value == {last}) \
                     && collection.one(xs, value => value == {half}) == {half}"
                ),
            ),
            check(
                "collection.union",
                format!(
                    "collection.union(collection.take(xs, {half}), collection.drop(xs, {half})) == xs"
                ),
            ),
        ],
    );
}

#[test]
fn pinned_slicing_completes_over_one_thousand_elements() {
    slicing(SMALL);
}

#[test]
fn pinned_slicing_completes_at_the_default_item_limit() {
    slicing(FULL);
}

#[test]
fn pinned_slicing_completes_over_ten_thousand_elements() {
    slicing(LARGE);
}

#[test]
fn pinned_union_accumulation_completes_over_one_thousand_elements() {
    union_accumulate(SMALL);
}

#[test]
fn pinned_union_accumulation_completes_at_the_default_item_limit() {
    union_accumulate(FULL);
}

#[test]
fn pinned_union_accumulation_completes_over_ten_thousand_elements() {
    union_accumulate(LARGE);
}

#[test]
fn pinned_text_split_completes_over_one_thousand_elements() {
    text_split(SMALL);
}

#[test]
fn pinned_text_split_completes_at_the_default_item_limit() {
    text_split(FULL);
}

#[test]
fn pinned_text_split_completes_over_ten_thousand_elements() {
    text_split(LARGE);
}

#[test]
fn pinned_collection_helpers_complete_over_one_thousand_elements() {
    collection_helpers(SMALL);
}

#[test]
fn pinned_collection_helpers_complete_at_the_default_item_limit() {
    collection_helpers(FULL);
}

#[test]
fn pinned_collection_helpers_complete_over_ten_thousand_elements() {
    collection_helpers(LARGE);
}
