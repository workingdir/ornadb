//! The per-code parity of every diagnostic (`docs/design/hir.md` section 6.3, H3).
//!
//! For every module case the old checker analyses, the set of diagnostic codes
//! the checker reports is compared with the set HIR reports on the case's
//! source modules after lowering and typing. Each code is counted per case as
//! agreeing, checker-only or HIR-only. The number of cases whose code sets
//! differ is ratcheted: it may only go down, and every code that differs must
//! carry a recorded reason.

use std::collections::{BTreeMap, BTreeSet};

use orna_differential::case::{Case, CaseKind, Corpus};
use orna_differential::{compare, observe_checker, observe_new};
use orna_hir::ModuleOrigin;

/// Cases whose diagnostic code sets differ, measured when this test landed.
/// The count may only go down.
const RECORDED_CODE_DIFFERENCES: usize = 156;

/// The old checker's diagnostic codes for a case.
fn checker_codes(case: &Case) -> Option<BTreeSet<String>> {
    let analysis = observe_checker::analysis(case).ok()?;
    Some(
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code().to_owned())
            .collect(),
    )
}

/// HIR's diagnostic codes on the case's source modules, after lowering and typing.
fn hir_codes(case: &Case) -> Option<BTreeSet<String>> {
    let mut lowered = observe_new::lower_case(case).ok()?;
    orna_hir::typeck(&mut lowered.program);
    let program = &lowered.program;
    Some(
        program
            .diagnostics()
            .filter(|(module, _)| program.modules[*module].origin == ModuleOrigin::Project)
            .map(|(_, diagnostic)| diagnostic.code.to_owned())
            .collect(),
    )
}

fn joined(codes: &BTreeSet<String>) -> String {
    codes.iter().cloned().collect::<Vec<_>>().join("; ")
}

/// Why a code's per-case sets differ, recorded with the measurement.
///
/// A code without a reason here must not differ in any case. The typing
/// codes (`ORNA-INFER-*`, `ORNA-S021-TYPE`, `ORNA-S020-ANNOTATION`) are not
/// yet mapped to each other: HIR reports constraint conflicts under
/// `ORNA-INFER-011` with both types printed, and the checker reports the
/// call-site mismatch under `ORNA-S021-TYPE`; the pairing is unproven per message.
fn recorded_reason(code: &str) -> Option<&'static str> {
    match code {
        "ORNA-S000-PARSE" => {
            Some("the checker reports parse errors; HIR lowering does not re-report them")
        }
        "ORNA-S010-IMPORT" => {
            Some("HIR import diagnostic the checker does not report under this code; unclassified")
        }
        "ORNA-S012-UNRESOLVED" => {
            Some("std and sys names the H1 resolver does not export yet (H2 std export)")
        }
        "ORNA-S021-TYPE" => {
            Some("checker call-site type mismatch; HIR reports the conflict as ORNA-INFER-011")
        }
        "ORNA-INFER-011" => {
            Some("HIR constraint conflict; the checker reports the same site as ORNA-S021-TYPE")
        }
        "ORNA-INFER-007" => {
            Some("HIR unsolved-variable diagnostic; the checker has no code for it")
        }
        "ORNA-S020-ANNOTATION" => {
            Some("public annotation: HIR and checker disagree on which bodies they reach")
        }
        "ORNA-S022-UNSUPPORTED" => {
            Some("HIR reports forms the H2a subset does not type; unclassified")
        }
        "ORNA-EVAL-ARGUMENT" => {
            Some("HIR call-binding arity error; the checker maps it only in rules.rs")
        }
        code if code.starts_with("ORNA100-")
            || code.starts_with("ORNA091-")
            || code.starts_with("ORNA-A091-") =>
        {
            Some("checker-only system or assertion check not ported to HIR")
        }
        code if code.starts_with('E') && code.len() == 5 => {
            Some("checker-only lowering or checking family not ported to HIR")
        }
        _ => None,
    }
}

#[test]
fn every_diagnostic_code_matches_the_checker_per_case() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let mut compared = 0usize;
    let mut skipped = 0usize;
    let mut table: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    let mut differences = Vec::new();
    for case in &corpus.cases {
        if matches!(case.kind, CaseKind::Repl | CaseKind::StandardBundle) {
            continue;
        }
        let (Some(old), Some(new)) = (checker_codes(case), hir_codes(case)) else {
            skipped += 1;
            continue;
        };
        compared += 1;
        for code in old.union(&new) {
            let row = table.entry(code.clone()).or_insert([0; 3]);
            match (old.contains(code), new.contains(code)) {
                (true, true) => row[0] += 1,
                (true, false) => row[1] += 1,
                (false, true) => row[2] += 1,
                (false, false) => unreachable!("a code in the union is on one side"),
            }
        }
        if old != new {
            differences.push(compare::difference(
                &case.id,
                "codes",
                joined(&old),
                joined(&new),
            ));
        }
    }
    println!(
        "diagnostic codes over {compared} cases ({skipped} skipped); columns: agree, checker-only, hir-only; \
         {} cases differ (recorded {RECORDED_CODE_DIFFERENCES})",
        differences.len()
    );
    for (code, row) in &table {
        let reason = recorded_reason(code).unwrap_or("UNRECORDED");
        println!(
            "{code}: agree {} checker-only {} hir-only {} :: {reason}",
            row[0], row[1], row[2]
        );
    }
    for difference in &differences {
        println!("{}", difference.describe());
    }
    let unrecorded: Vec<&String> = table
        .iter()
        .filter(|(code, row)| (row[1] + row[2] > 0) && recorded_reason(code).is_none())
        .map(|(code, _)| code)
        .collect();
    assert!(
        unrecorded.is_empty(),
        "differing codes without a recorded reason: {unrecorded:?}"
    );
    assert!(
        differences.len() <= RECORDED_CODE_DIFFERENCES,
        "code differences grew from {RECORDED_CODE_DIFFERENCES} to {}",
        differences.len()
    );
}
