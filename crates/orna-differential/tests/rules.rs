//! The H3b rule-family comparison (`docs/design/hir.md` section 3.1).
//!
//! For every module case the old checker analyses, the namespace and
//! assertion rules that HIR now runs report the same family diagnostics
//! (code and message) on the case's source modules, or the difference is
//! recorded under the `rules` field of the shared ratchet.

use std::collections::BTreeMap;

use orna_differential::case::{Case, CaseKind, Corpus};
use orna_differential::{compare, observe_checker, observe_new};
use orna_hir::ModuleOrigin;

/// The codes the HIR rules pass owns: namespace (E1008, E1009, S002, S003)
/// and assertion (A091-002, A091-003, A091-007, A091-012).
const FAMILY: [&str; 8] = [
    "E1008",
    "E1009",
    "ORNA-S002-NAMESPACE",
    "ORNA-S003-RESERVED",
    "ORNA-A091-002",
    "ORNA-A091-003",
    "ORNA-A091-007",
    "ORNA-A091-012",
];

fn in_family(code: &str) -> bool {
    FAMILY.contains(&code)
}

/// The sorted `code: message` lines of the old checker's family diagnostics.
fn checker_rules(case: &Case) -> Option<Vec<String>> {
    let analysis = observe_checker::analysis(case).ok()?;
    Some(sorted(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| in_family(diagnostic.code()))
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message())),
    ))
}

/// The sorted `code: message` lines of the HIR rules on source modules.
fn hir_rules(program: &orna_hir::Program) -> Vec<String> {
    sorted(
        program
            .modules
            .iter()
            .filter(|module| module.origin == ModuleOrigin::Project)
            .flat_map(|module| module.diagnostics.iter())
            .filter(|diagnostic| in_family(diagnostic.code))
            .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message)),
    )
}

fn sorted(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut lines: Vec<String> = lines.collect();
    lines.sort();
    lines
}

#[test]
fn rule_families_match_the_checker() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let expected = compare::load_expected_differences()
        .expect("the ratchet file parses")
        .into_iter()
        .filter(|entry| entry.field == "rules")
        .collect::<Vec<_>>();
    let mut compared = 0usize;
    let mut agreed = 0usize;
    let mut skipped = 0usize;
    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    let mut differences = Vec::new();
    for case in &corpus.cases {
        if matches!(case.kind, CaseKind::Repl | CaseKind::StandardBundle) {
            continue;
        }
        let Some(old) = checker_rules(case) else {
            skipped += 1;
            continue;
        };
        let Ok(lowered) = observe_new::lower_case(case) else {
            skipped += 1;
            continue;
        };
        let new = hir_rules(&lowered.program);
        compared += 1;
        for line in &new {
            let code = line.split(':').next().unwrap_or_default();
            *diagnostics.entry(code.to_owned()).or_insert(0) += 1;
        }
        if old == new {
            agreed += 1;
            continue;
        }
        differences.push(compare::Difference {
            case: case.id.clone(),
            field: "rules",
            left: old.join("; "),
            right: new.join("; "),
        });
    }
    println!(
        "rule families: {compared} cases compared, {agreed} agreed, {} differences, {skipped} skipped; \
         HIR diagnostics by code: {diagnostics:?}",
        differences.len(),
    );
    for difference in &differences {
        println!("{}", difference.describe());
    }
    let failures = compare::ratchet(&expected, &differences);
    assert!(
        failures.is_empty(),
        "rule-family differences must be recorded or fixed:\n{}",
        failures.join("\n")
    );
}

/// Cases whose S012/S021 code sets differ, measured when this test landed.
/// Most are `valid/` programs where HIR reports `ORNA-S012-UNRESOLVED` for std
/// or sys names the H1 resolver does not export yet (closed by H2 std export
/// resolution), and checker-only S021 comes from type mismatches HIR typeck
/// does not report yet. The count may only go down.
const RECORDED_CODE_DIFFERENCES: usize = 90;

/// The families compared by code presence: unresolved names (S012) and
/// call-site type or arity mismatches (S021).
const CODE_FAMILIES: [&str; 2] = ["ORNA-S012-UNRESOLVED", "ORNA-S021-TYPE"];

/// Maps one HIR lowering diagnostic onto the checker's family code.
///
/// Call-argument binding reports arity mismatches at lowering with the
/// evaluator's `ORNA-EVAL-ARGUMENT`; the checker reports the same call-site
/// mismatch as `ORNA-S021-TYPE`. Only the two arity messages map, so the other
/// `ORNA-EVAL-ARGUMENT` uses (named-argument and duplicate-argument errors)
/// stay unmapped.
fn family_code(code: &str, message: &str) -> Option<&'static str> {
    match code {
        "ORNA-S012-UNRESOLVED" => Some("ORNA-S012-UNRESOLVED"),
        "ORNA-EVAL-ARGUMENT"
            if message == "the call omits a required parameter"
                || message == "the call supplies more positional arguments than parameters" =>
        {
            Some("ORNA-S021-TYPE")
        }
        _ => None,
    }
}

/// The sorted family codes the checker reports for a case.
fn checker_codes(case: &Case) -> Option<Vec<String>> {
    let analysis = observe_checker::analysis(case).ok()?;
    Some(sorted(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| CODE_FAMILIES.contains(&diagnostic.code()))
            .map(|diagnostic| diagnostic.code().to_owned()),
    ))
}

/// The sorted family codes HIR reports on source modules, after mapping.
fn hir_codes(program: &orna_hir::Program) -> Vec<String> {
    sorted(
        program
            .modules
            .iter()
            .filter(|module| module.origin == ModuleOrigin::Project)
            .flat_map(|module| module.diagnostics.iter())
            .filter_map(|diagnostic| family_code(diagnostic.code, &diagnostic.message))
            .map(str::to_owned),
    )
}

#[test]
fn unresolved_and_arity_codes_match_the_checker() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let mut compared = 0usize;
    let mut skipped = 0usize;
    let mut per_code: BTreeMap<&str, [usize; 3]> =
        CODE_FAMILIES.iter().map(|code| (*code, [0; 3])).collect();
    let mut differences = Vec::new();
    for case in &corpus.cases {
        if matches!(case.kind, CaseKind::Repl | CaseKind::StandardBundle) {
            continue;
        }
        let (Some(old), Ok(lowered)) = (checker_codes(case), observe_new::lower_case(case)) else {
            skipped += 1;
            continue;
        };
        let new = hir_codes(&lowered.program);
        compared += 1;
        for code in CODE_FAMILIES {
            let in_old = old.iter().any(|found| found == code);
            let in_new = new.iter().any(|found| found == code);
            let row = per_code.get_mut(code).expect("every family code has a row");
            match (in_old, in_new) {
                (true, true) | (false, false) => row[0] += 1,
                (true, false) => row[1] += 1,
                (false, true) => row[2] += 1,
            }
        }
        if old != new {
            differences.push(compare::difference(
                &case.id,
                "codes",
                old.join("; "),
                new.join("; "),
            ));
        }
    }
    println!(
        "code agreement over {compared} cases ({skipped} skipped); columns: agree, checker-only, hir-only"
    );
    for (code, row) in &per_code {
        println!(
            "{code}: agree {} checker-only {} hir-only {}",
            row[0], row[1], row[2]
        );
    }
    for difference in &differences {
        println!("{}", difference.describe());
    }
    assert!(
        differences.len() <= RECORDED_CODE_DIFFERENCES,
        "code-family differences grew from {RECORDED_CODE_DIFFERENCES} to {}",
        differences.len()
    );
}
