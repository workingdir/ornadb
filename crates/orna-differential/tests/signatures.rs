//! The H2 public-signature comparison (`docs/design/hir.md` section 6.3, H2).
//!
//! "For every program the old checker accepts and the new one accepts, the
//! printed type of every public signature is equal or listed." The comparison
//! is over the signatures the checker publishes: its exported function symbols.
//! A function the checker does not export (its result is inferred, so the
//! header records the diagnostic type) is counted as unpublished rather than
//! treated as a difference, because that type is H2b/H3 work.
//!
//! Accepted differences live in the shared
//! `tests/differential/expected-differences.toml` under the
//! `public-signature` field, with a reason and the bead that closes it.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::{compare, observe_checker, observe_new};

#[test]
fn public_signature_types_match_the_checker() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let expected = compare::load_expected_differences()
        .expect("the ratchet file parses")
        .into_iter()
        .filter(|entry| entry.field == "public-signature")
        .collect::<Vec<_>>();
    let mut compared = 0usize;
    let mut signatures = 0usize;
    let mut unpublished: BTreeMap<String, usize> = BTreeMap::new();
    let mut differences = Vec::new();
    for case in &corpus.cases {
        let old = match observe_checker::signatures(case) {
            Ok(signatures) => signatures,
            Err(_) => continue,
        };
        if !observe_checker::accepts(case).unwrap_or(false) {
            continue;
        }
        let mut lowered = match observe_new::lower_case(case) {
            Ok(lowered) => lowered,
            Err(_) => continue,
        };
        let observation = observe_new::type_observation(&mut lowered);
        if !observation.accepted {
            continue;
        }
        compared += 1;
        signatures += old.len();
        for key in observation
            .signatures
            .keys()
            .filter(|key| !old.contains_key(*key))
        {
            *unpublished.entry((*key).clone()).or_insert(0) += 1;
        }
        differences.extend(compare::compare_signatures(
            &case.id,
            &old,
            &observation.signatures,
        ));
    }
    println!(
        "public signatures: {compared} cases compared, {signatures} checker signatures, \
         {} differences, {} HIR-only signatures the checker does not publish",
        differences.len(),
        unpublished.values().sum::<usize>()
    );
    for (key, count) in &unpublished {
        println!("  not published by the checker ({count}): {key}");
    }
    for difference in &differences {
        println!("{}", difference.describe());
    }
    let failures = compare::ratchet(&expected, &differences);
    assert!(
        failures.is_empty(),
        "public signature differences must be recorded or fixed:\n{}",
        failures.join("\n")
    );
}
