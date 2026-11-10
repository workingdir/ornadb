//! The H3a effect-set comparison (`docs/design/hir.md` section 2.5).
//!
//! For every public function the old checker and the HIR both accept, the HIR
//! effect pass's effect set equals the checker's `EffectSummary`, or the
//! difference is recorded under the `effects` field of the shared ratchet.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::{compare, effects, observe_checker, observe_new};

#[test]
fn public_effect_sets_match_the_checker() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let expected = compare::load_expected_differences()
        .expect("the ratchet file parses")
        .into_iter()
        .filter(|entry| entry.field == "effects")
        .collect::<Vec<_>>();
    let mut compared = 0usize;
    let mut functions = 0usize;
    let mut unpublished = 0usize;
    let mut differences = Vec::new();
    for case in &corpus.cases {
        let old: BTreeMap<String, String> = match effects::checker_effects(case) {
            Ok(old) => old,
            Err(_) => continue,
        };
        if !observe_checker::accepts(case).unwrap_or(false) {
            continue;
        }
        let mut lowered = match observe_new::lower_case(case) {
            Ok(lowered) => lowered,
            Err(_) => continue,
        };
        let new = effects::hir_effects(&lowered);
        let observation = observe_new::type_observation(&mut lowered);
        if !observation.accepted {
            continue;
        }
        compared += 1;
        functions += old.len();
        unpublished += new.keys().filter(|key| !old.contains_key(*key)).count();
        differences.extend(effects::compare_effects(&case.id, &old, &new));
    }
    println!(
        "public effect sets: {compared} cases compared, {functions} checker functions, \
         {} differences, {unpublished} HIR-only functions the checker does not publish",
        differences.len(),
    );
    for difference in &differences {
        println!("{}", difference.describe());
    }
    let failures = compare::ratchet(&expected, &differences);
    assert!(
        failures.is_empty(),
        "public effect set differences must be recorded or fixed:\n{}",
        failures.join("\n")
    );
}
