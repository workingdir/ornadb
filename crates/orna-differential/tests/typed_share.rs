//! The H2a typed-share ratchet (`docs/design/hir.md` section 6.3, stage H2).
//!
//! "Anything the subset cannot type is reported as 'not yet on HIR' and the
//! harness counts it, so the ratchet is: typed share of the corpus only grows."
//! This test runs the type pass over every corpus case the HIR lowers and
//! checks the count against `tests/differential/typed-share.toml`. It prints
//! why each remaining case is untyped, so the growth work is visible rather than
//! being a number in a file.

use std::collections::BTreeMap;

use orna_differential::case::{CaseKind, Corpus};
use orna_differential::compare::{self, TypedShare};
use orna_differential::observe_new;

#[test]
fn typed_share_of_the_corpus_only_grows() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let recorded = compare::load_typed_share().expect("the typed-share ratchet parses");
    let mut lowered = 0usize;
    let mut typed = 0usize;
    let mut unlowered = Vec::new();
    let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut examples: BTreeMap<&'static str, String> = BTreeMap::new();
    for case in &corpus.cases {
        let mut program = match observe_new::lower_case(case) {
            Ok(program) => program,
            Err(error) => {
                unlowered.push(error);
                continue;
            }
        };
        lowered += 1;
        let observation = observe_new::type_observation(&mut program);
        if observation.typed_case() {
            typed += 1;
        }
        for reason in &observation.not_yet {
            let category = category(reason);
            *reasons.entry(category).or_insert(0) += 1;
            examples.entry(category).or_insert_with(|| reason.clone());
        }
        for diagnostic in &observation.diagnostics {
            *reasons.entry("lowering diagnostic").or_insert(0) += 1;
            examples
                .entry("lowering diagnostic")
                .or_insert_with(|| diagnostic.clone());
        }
    }
    println!(
        "typed share: {typed}/{lowered} lowered cases ({} cases are not lowered)",
        unlowered.len()
    );
    for (category, count) in &reasons {
        println!("  {count:>5} {category}");
        if let Some(example) = examples.get(category) {
            println!("        e.g. {example}");
        }
    }
    for error in &unlowered {
        if case_kind_of(error) != Some(CaseKind::Repl) {
            println!("  unlowered: {error}");
        }
    }
    let failures = compare::typed_share_ratchet(recorded, TypedShare { typed, lowered });
    assert!(
        failures.is_empty(),
        "the typed-share ratchet failed:\n{}",
        failures.join("\n")
    );
}

/// A coarse category for a not-yet reason, for counting in evidence output.
fn category(reason: &str) -> &'static str {
    if reason.contains("omits a parameter annotation") {
        "unannotated parameter (H2b)"
    } else if reason.contains("H2b") {
        "later stage (H2b)"
    } else if reason.contains("Error") || reason.contains("native") {
        "needs the core/sys declarations (H2b/H5)"
    } else if reason.contains("`|?`") || reason.contains("recovery") {
        "recovery needs the core Error type (H2b/H5)"
    } else {
        "outside the H2a subset"
    }
}

/// Whether an unlowered case names a REPL session, whose stage is H8.
fn case_kind_of(error: &str) -> Option<CaseKind> {
    error
        .contains("incremental module")
        .then_some(CaseKind::Repl)
}
