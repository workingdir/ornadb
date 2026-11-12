//! The per-code parity of every diagnostic (`docs/design/hir.md` section 6.3, H3).
//!
//! For every module case the old checker analyses, the set of diagnostic codes
//! the checker reports is compared with the set HIR reports on the case's
//! source modules after lowering and typing. Each code is counted per case as
//! agreeing, checker-only or HIR-only.
//!
//! The cases whose code sets differ are ratcheted exactly: the measured set must
//! equal the `## expected` section of `diagnostic_codes_differing.txt`. A new
//! difference fails the test, and a listed case that now agrees fails it as
//! stale. Every differing code must carry a recorded reason.

use std::collections::{BTreeMap, BTreeSet};

use orna_differential::case::{Case, CaseKind, Corpus};
use orna_differential::{observe_checker, observe_new};
use orna_hir::{Idx, ModuleOrigin};

/// The expected differences, one `<case> :: <checker codes> => <HIR codes>` line each.
const DIFFERING: &str = include_str!("diagnostic_codes_differing.txt");

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
    let owned = &lowered.owned;
    let mut codes: BTreeSet<String> = program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].origin == ModuleOrigin::Project)
        .filter(|(module, _)| owned.get(module.index()).copied().unwrap_or(false))
        .map(|(_, diagnostic)| diagnostic.code.to_owned())
        .collect();
    // The old checker reports each syntax diagnostic of a project module as
    // `ORNA-S000-PARSE`; HIR lowers the recovered tree and reports the same code.
    if lowered.parse_diagnostics > 0 {
        codes.insert(orna_check::DIAG_PARSE.to_owned());
    }
    Some(codes)
}

fn joined(codes: &BTreeSet<String>) -> String {
    if codes.is_empty() {
        return "(none)".to_owned();
    }
    codes.iter().cloned().collect::<Vec<_>>().join("; ")
}

/// One differing case, in the form `diagnostic_codes_differing.txt` lists it.
fn difference_line(case: &str, old: &BTreeSet<String>, new: &BTreeSet<String>) -> String {
    format!("{case} :: {} => {}", joined(old), joined(new))
}

/// The `## expected` section of `diagnostic_codes_differing.txt`.
fn expected_differences() -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    let mut in_expected = false;
    for line in DIFFERING.lines() {
        if let Some(section) = line.strip_prefix("## ") {
            in_expected = section == "expected";
            continue;
        }
        let line = line.trim_end();
        if in_expected && !line.is_empty() && !line.starts_with('#') {
            expected.insert(line.to_owned());
        }
    }
    expected
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
        "sys.version.incompatible" => Some(
            "HIR applies the production sys admission (ORNA-SYS-008): a catalogued member with no \
             linked provider is rejected at lowering, as the executor requires; the checker compared \
             here analyses on the declared surface (SystemProviders::DeclaredSurface) and does not gate \
             providers",
        ),
        "ORNA-S010-IMPORT" => {
            Some("HIR import diagnostic the checker does not report under this code; unclassified")
        }
        "ORNA-S012-UNRESOLVED" => Some(
            "HIR resolves environment-only names (fixture tables, currency and unit names, connectors, \
             harness names) that the checker resolves from its observer catalogue, and reports missing \
             `use` imports the spec requires (ORNA-IMPORT-005); deferred money, units and secret stages (hir.md); not a std export gap",
        ),
        "ORNA-S021-TYPE" => Some(
            "checker type mismatch; HIR reports arity or constraint conflicts as ORNA-INFER-011 or ORNA-INFER-007, \
                 stops at ORNA-S000 on parse errors, or reports unresolved names first (ORNA-S012); the ledger comments list each case",
        ),
        "ORNA-INFER-011" => Some(
            "HIR constraint conflict. Range membership (`value in range`, ORNA-RANGE-006) is typed; the \
             residual conflict is a legacy fixture passing `0..10` where the spec's `take(rows, count: Int)` \
             takes an Int (09-standard-library.md:31); the checker reports the same site as ORNA-S021-TYPE",
        ),
        "ORNA-INFER-007" => {
            Some("HIR unsolved-variable diagnostic; the checker has no code for it")
        }
        "ORNA-S020-ANNOTATION" => Some(
            "D1 strict, INFER-004 SHOULD allows the old behaviour; HIR requires annotated public parameters \
             (docs/design/hir.md, H2a subset), so the difference is expected, not a spec-required error",
        ),
        "ORNA-S022-UNSUPPORTED" => Some(
            "HIR reports `sys` as a module rather than a value and does not represent sys declarations or \
             module calls (stage H5, sys.orna lowering, docs/design/hir.md); also top-level offset or affine \
             units (H2c, valid/affine-unit) and one unary-operator table artifact",
        ),
        "ORNA-EVAL-ARGUMENT" => Some(
            "HIR binds named arguments only against statically known parameter labels; member, std and \
             relation callees bind after H2 settles them (hir.md call binding), and a lambda or function-value \
             callee has no labels (spec structural function types), so its rejection is the spec verdict",
        ),
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
    let mut measured = BTreeSet::new();
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
            measured.insert(difference_line(&case.id, &old, &new));
        }
    }
    let expected = expected_differences();
    println!(
        "diagnostic codes over {compared} cases ({skipped} skipped); columns: agree, checker-only, hir-only; \
         {} cases differ (expected {})",
        measured.len(),
        expected.len()
    );
    for (code, row) in &table {
        let reason = if row[1] + row[2] == 0 {
            "agrees on every case"
        } else {
            recorded_reason(code).unwrap_or("UNRECORDED")
        };
        println!(
            "{code}: agree {} checker-only {} hir-only {} :: {reason}",
            row[0], row[1], row[2]
        );
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
    let unexpected: Vec<&String> = measured.difference(&expected).collect();
    let stale: Vec<&String> = expected.difference(&measured).collect();
    assert!(
        unexpected.is_empty(),
        "code differences not listed in diagnostic_codes_differing.txt:\n{}",
        unexpected
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        stale.is_empty(),
        "listed code differences that now agree (remove them):\n{}",
        stale
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}
