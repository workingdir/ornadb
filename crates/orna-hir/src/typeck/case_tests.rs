//! Case coverage on HIR: each `case` is checked against its scrutinee type
//! once its body is typed, with the checker's classification (ORNA-S021-TYPE).

use super::diagnostics_tests::{lower_source, spanned, with_code};
use super::{BodyOutcome, typeck};
use crate::diag::CASE_COVERAGE;

/// The coverage diagnostics of one source: each message, with the source text
/// its span covers.
fn coverage_of(source: &str) -> Vec<(String, String)> {
    let mut program = lower_source(source);
    typeck(&mut program);
    with_code(&program, CASE_COVERAGE)
        .into_iter()
        .map(|(_, diagnostic)| {
            (
                diagnostic.message.to_string(),
                spanned(source, diagnostic).to_string(),
            )
        })
        .collect()
}

const STATUS: &str = "pub enum Status { draft, published }\n";

#[test]
fn an_enum_case_covering_every_variant_has_no_coverage_diagnostic() {
    let source = format!(
        "{STATUS}pub fn label(status: Status): Int = case status {{\n    Status.draft: 1,\n    Status.published: 2,\n}};\n"
    );
    assert_eq!(coverage_of(&source), Vec::new());
}

#[test]
fn a_missing_enum_variant_is_reported_on_the_case() {
    let source = format!(
        "{STATUS}pub fn label(status: Status): Int = case status {{\n    Status.draft: 1,\n}};\n"
    );
    let found = coverage_of(&source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("every enum variant"), "{found:?}");
    assert!(found[0].1.starts_with("case status"), "{found:?}");
}

#[test]
fn a_duplicated_enum_variant_is_reported() {
    let source = format!(
        "{STATUS}pub fn label(status: Status): Int = case status {{\n    Status.draft: 1,\n    Status.draft: 3,\n    Status.published: 2,\n}};\n"
    );
    let found = coverage_of(&source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("duplicated"), "{found:?}");
}

#[test]
fn a_guarded_variant_does_not_cover_its_variant() {
    let source = format!(
        "{STATUS}pub fn label(status: Status, flag: Bool): Int = case status {{\n    Status.draft: 1,\n    Status.published if flag: 2,\n}};\n"
    );
    let found = coverage_of(&source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("every enum variant"), "{found:?}");
}

#[test]
fn a_wildcard_arm_does_not_cover_an_enum() {
    let source = format!(
        "{STATUS}pub fn label(status: Status): Int = case status {{\n    Status.draft: 1,\n    _: 2,\n}};\n"
    );
    let found = coverage_of(&source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("every enum variant"), "{found:?}");
}

#[test]
fn a_boolean_case_covering_true_and_false_has_no_coverage_diagnostic() {
    let source = "pub fn pick(flag: Bool): Int = case flag {\n    true: 1,\n    false: 2,\n};\n";
    assert_eq!(coverage_of(source), Vec::new());
}

#[test]
fn a_boolean_catch_all_covers_both_values() {
    let source = "pub fn pick(flag: Bool): Int = case flag {\n    _: 1,\n};\n";
    assert_eq!(coverage_of(source), Vec::new());
}

#[test]
fn a_boolean_case_missing_false_is_reported() {
    let source = "pub fn pick(flag: Bool): Int = case flag {\n    true: 1,\n};\n";
    let found = coverage_of(source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("true and false"), "{found:?}");
}

#[test]
fn a_duplicated_boolean_literal_is_reported() {
    let source = "pub fn pick(flag: Bool): Int = case flag {\n    true: 1,\n    true: 3,\n    false: 2,\n};\n";
    let found = coverage_of(source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("duplicated"), "{found:?}");
}

#[test]
fn a_coverage_violation_counts_as_a_conflict_and_leaves_the_body_untyped() {
    let source = format!(
        "{STATUS}pub fn label(status: Status): Int = case status {{\n    Status.draft: 1,\n}};\n"
    );
    let mut program = lower_source(&source);
    let summary = typeck(&mut program);
    let module = program
        .diagnostics()
        .find(|(_, diagnostic)| diagnostic.code == CASE_COVERAGE)
        .map(|(module, _)| module)
        .expect("a coverage diagnostic");
    let outcomes: Vec<&BodyOutcome> = summary
        .outcomes
        .iter()
        .filter(|outcome| outcome.module == module)
        .collect();
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.conflicts == 1 && !outcome.typed),
        "{outcomes:?}"
    );
}
