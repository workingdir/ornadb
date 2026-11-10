//! The H1 classification proof (`docs/design/hir.md` section 6.3).
//!
//! For every call the old evaluator executes over the corpus, the HIR `Callee`
//! at that callee's span must map to the recorded route. Differences are
//! accepted only through `tests/differential/expected-differences.toml`, each
//! with a reason and the bead that closes it.
//!
//! The test also proves the two invariants acceptance (c) and (d) name: every
//! lowered corpus program passes `Program::validate`, and every case lowers
//! (or is accounted for by a stage).

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::{compare, observe_new, observe_old};

/// The route's name, for the evidence line.
fn route_name(route: orna_differential::Route) -> &'static str {
    match route {
        orna_differential::Route::DecimalDivide => "decimal-divide",
        orna_differential::Route::CodecDecode => "codec-decode",
        orna_differential::Route::UiAction => "ui-action",
        orna_differential::Route::RootIntrinsic => "root-intrinsic",
        orna_differential::Route::RelationSource => "relation-source",
        orna_differential::Route::RelationStage => "relation-stage",
        orna_differential::Route::SysHost => "sys-host",
        orna_differential::Route::Native => "native",
        orna_differential::Route::SourceFunction => "source-function",
    }
}

#[test]
fn hir_call_classification_matches_the_route_trace() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let all_expected = compare::load_expected_differences().expect("the ratchet file parses");
    for entry in &all_expected {
        assert!(
            entry.field.starts_with("call")
                || entry.field.starts_with("resolution")
                || entry.field == "public-signature"
                || entry.field == "effects"
                || entry.field == "rules",
            "{}: `{}` is not a compared field; the classification, resolution and signature tests own \
             the call, resolution and signature fields, tests/effects.rs owns effects, and \
             tests/rules.rs owns rules",
            entry.case,
            entry.field
        );
    }
    let expected = all_expected
        .into_iter()
        .filter(|entry| entry.field.starts_with("call"))
        .collect::<Vec<_>>();
    let mut compared = 0usize;
    let mut old_calls = 0usize;
    let mut uncompared = Vec::new();
    let mut differences = Vec::new();
    let mut unobservable = 0usize;
    let mut routes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_case: BTreeMap<String, usize> = BTreeMap::new();
    for case in &corpus.cases {
        let old = match observe_old::observe(case) {
            Ok(observation) => observation,
            Err(error) => {
                uncompared.push(format!("{}: {error}", case.id));
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
        if let Err(errors) = lowered.program.validate() {
            panic!(
                "{}: lowering produced an invalid program: {} violations, first {:?}",
                case.id,
                errors.len(),
                errors.first()
            );
        }
        compared += 1;
        old_calls += old.calls.len();
        for call in &old.calls {
            *routes.entry(route_name(call.route)).or_insert(0) += 1;
        }
        let new_calls = observe_new::calls(&lowered);
        let (case_differences, skipped) = compare::compare_calls(&case.id, &old.calls, &new_calls);
        unobservable += skipped;
        *by_case.entry(case.id.clone()).or_insert(0) += case_differences.len();
        differences.extend(case_differences);
    }
    let mut categories: BTreeMap<String, usize> = BTreeMap::new();
    for difference in &differences {
        let category = if difference.right == "no HIR call" {
            "no HIR call at span".to_owned()
        } else {
            format!(
                "route {} (HIR {})",
                difference.left.rsplit('=').next().unwrap_or(""),
                difference.right
            )
        };
        *categories.entry(category).or_insert(0) += 1;
    }
    println!(
        "classification: {compared} cases compared, {old_calls} executed calls, \
         {unobservable} calls whose class the old trace cannot observe, \
         {} differences in {} cases, {} cases uncompared",
        differences.len(),
        by_case.len(),
        uncompared.len(),
    );
    for (route, count) in &routes {
        println!("  {count:>5} executed {route}");
    }
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
        "classification differences must be recorded or fixed:\n{}",
        failures.join("\n")
    );
}
