//! Classifies every registered spelling that the pinned source does not
//! declare.
//!
//! The pre-reform table registers 51 spellings that no declaration in the
//! module's own pinned source backs. Each must be a documented re-export, a
//! historical-pin leaf, the Rust addendum, an evaluator-only name, or a leaf
//! declared in the sibling `std.collection` module. A new undeclared spelling
//! fails this test until it is classified.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use support::{module_of_prefix, registered_spellings};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum UndeclaredClass {
    /// The six private leaves that exist only for the v4/v5 module-upgrade pins.
    HistoricalPin,
    /// The `std.query` private leaves are declared in `std.collection`.
    SiblingDeclaration,
}

fn pinned_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/std")
}

fn module_sources(prefix: &str) -> &'static [&'static str] {
    match prefix {
        "std.collection." => &["collection.orna"],
        "std.query." => &["query.orna"],
        "std.math." => &["math.orna"],
        "std.text." => &["text.orna"],
        "std.bits." => &["bits.orna"],
        "std.stream." => &["stream.orna"],
        "std.concurrent." => &["concurrent/main.orna"],
        "std.ui." => &["ui.orna"],
        "std.stats." => &["stats.orna"],
        "std.time." => &[
            "time.orna",
            "time/duration/compact.orna",
            "time/duration/clock.orna",
            "time/duration/words.orna",
            "time/duration/iso.orna",
        ],
        "std.hash." => &["hash.orna"],
        "std.random." => &["random.orna"],
        "std.encoding.base64." => &["encoding/base64.orna"],
        "std.encoding.json." => &["encoding/json.orna"],
        "std.encoding.orna." => &["encoding/orna.orna"],
        "std.encoding.ovb." => &["encoding/ovb.orna"],
        "std.money." => &["money.orna"],
        other => panic!("unclassified module prefix {other}"),
    }
}

fn local_name<'a>(prefix: &str, operation: &'a str) -> &'a str {
    if prefix == "std.time." && operation.starts_with("duration.") {
        operation.rsplit('.').next().expect("a final segment")
    } else {
        operation
    }
}

fn declares(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        let line = line.strip_prefix("pub ").unwrap_or(line);
        let Some(rest) = line.strip_prefix("fn ") else {
            return false;
        };
        rest.strip_prefix(name)
            .is_some_and(|tail| tail.starts_with('(') || tail.starts_with('<'))
    })
}

fn declared(prefix: &str, operation: &str) -> bool {
    let name = local_name(prefix, operation);
    let root = pinned_root();
    module_sources(prefix).iter().any(|source| {
        let path = root.join(*source);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        declares(&text, name)
    })
}

fn classification() -> BTreeMap<(&'static str, &'static str), UndeclaredClass> {
    let mut classes = BTreeMap::new();
    for prefix in ["std.collection.", "std.query."] {
        for operation in [
            "__list_length",
            "__list_concat",
            "__numeric_sum",
            "__stable_sort",
            "__minimum",
            "__maximum",
        ] {
            classes.insert((prefix, operation), UndeclaredClass::HistoricalPin);
        }
    }
    for operation in ["__group_by", "__rank", "__asof_join", "__bucket_by"] {
        classes.insert(
            ("std.query.", operation),
            UndeclaredClass::SiblingDeclaration,
        );
    }
    classes
}

fn undeclared_spellings(
    spellings: &[(&'static str, &'static str)],
) -> BTreeSet<(&'static str, &'static str)> {
    spellings
        .iter()
        .copied()
        .filter(|(prefix, operation)| {
            assert!(
                module_of_prefix(prefix).is_some(),
                "unknown prefix {prefix}"
            );
            !declared(prefix, operation)
        })
        .collect()
}

#[test]
fn the_remaining_undeclared_spellings_are_exactly_classified() {
    let undeclared = undeclared_spellings(&registered_spellings());
    let classes = classification();
    let classified = classes.keys().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        undeclared, classified,
        "the undeclared spelling set and its documented classes drifted"
    );
    assert_eq!(undeclared.len(), 16);
    assert_eq!(
        undeclared
            .iter()
            .filter(|(prefix, _)| *prefix == "std.collection.")
            .count(),
        6
    );
    assert_eq!(
        undeclared
            .iter()
            .filter(|(prefix, _)| *prefix == "std.query.")
            .count(),
        10
    );
    assert_eq!(
        undeclared
            .iter()
            .filter(|(prefix, _)| *prefix == "std.stream.")
            .count(),
        0
    );
}

#[test]
fn every_class_is_nonempty_and_named() {
    let classes = classification();
    let mut counts = BTreeMap::new();
    for class in classes.values() {
        *counts.entry(*class).or_insert(0usize) += 1;
    }
    assert_eq!(counts[&UndeclaredClass::HistoricalPin], 12);
    assert_eq!(counts[&UndeclaredClass::SiblingDeclaration], 4);
}

#[test]
fn declaration_scanner_rejects_prefix_and_suffix_matches() {
    assert!(declares("fn trim(value: Str): Str = value;", "trim"));
    assert!(declares("pub fn trim(value: Str): Str = value;", "trim"));
    assert!(declares("fn __asof_join<T, Time, Key>(", "__asof_join"));
    assert!(!declares("fn trim_extra(value: Str): Str = value;", "trim"));
    assert!(!declares("// fn trim(value: Str): Str = value;", "trim"));
    assert!(!declares("fn value_trim(value: Str): Str = value;", "trim"));
}

#[test]
fn adding_an_undeclared_spelling_is_detected() {
    let mut spellings = registered_spellings();
    spellings.push(("std.text.", "not_declared"));
    let undeclared = undeclared_spellings(&spellings);
    assert!(undeclared.contains(&("std.text.", "not_declared")));
    assert!(!classification().contains_key(&("std.text.", "not_declared")));
}

#[test]
fn time_submodule_names_are_mapped_to_the_declaration_leaf() {
    assert_eq!(local_name("std.time.", "duration.compact.format"), "format");
    assert_eq!(local_name("std.text.", "trim"), "trim");
    assert!(declared("std.time.", "duration.compact.format"));
    assert!(declared("std.time.", "duration.iso.format"));
}
