//! The H1 resolution proof (`docs/design/hir.md` section 6.3 acceptance (b)).
//!
//! For every name in every corpus program, the HIR resolver and the old
//! checker must agree on resolved or unresolved, and on `ORNA-S011` ambiguity.
//! The checker's public answer carries a name for an unresolved use but no span,
//! so the comparison is by spelling and occurrence count; ambiguity is a count.
//!
//! Differences are accepted only through
//! `tests/differential/expected-differences.toml`, each with a reason and the
//! bead that closes it.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::{compare, observe_checker, observe_new};

#[test]
fn hir_resolution_matches_the_checker() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    // The classification test owns the file's vocabulary check; this test
    // ratchets only the resolution fields so one shared file can hold both.
    let expected = compare::load_expected_differences()
        .expect("the ratchet file parses")
        .into_iter()
        .filter(|entry| entry.field.starts_with("resolution"))
        .collect::<Vec<_>>();
    let mut compared = 0usize;
    let mut names = 0usize;
    let mut uncompared = Vec::new();
    let mut differences = Vec::new();
    let mut categories: BTreeMap<&'static str, usize> = BTreeMap::new();
    for case in &corpus.cases {
        let old = match observe_checker::resolution(case) {
            Ok(observation) => observation,
            Err(error) => {
                uncompared.push(error);
                continue;
            }
        };
        let lowered = match observe_new::lower_case(case) {
            Ok(lowered) => lowered,
            Err(error) => {
                uncompared.push(error);
                continue;
            }
        };
        let new = observe_new::resolution(&lowered);
        compared += 1;
        names += old.unresolved.values().sum::<usize>();
        for difference in compare::compare_resolution(&case.id, &old, &new) {
            *categories.entry(difference.field).or_insert(0) += 1;
            differences.push(difference);
        }
    }
    println!(
        "resolution: {compared} cases compared, {names} checker-unresolved names, \
         {} differences, {} cases uncompared",
        differences.len(),
        uncompared.len()
    );
    for (category, count) in &categories {
        println!("  {count:>5} {category}");
    }
    for error in &uncompared {
        println!("  uncompared: {error}");
    }
    for difference in differences.iter() {
        println!("{}", difference.describe());
    }
    let failures = compare::ratchet(&expected, &differences);
    assert!(
        failures.is_empty(),
        "resolution differences must be recorded or fixed:\n{}",
        failures.join("\n")
    );
}
