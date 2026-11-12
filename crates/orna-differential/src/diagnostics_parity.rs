//! Diagnostic-code parity between the old checker and HIR (`docs/design/hir.md` section 6.3, H3).
//!
//! The per-case code sets of the old checker and of HIR, the ledger of differing cases and the
//! recorded reason of each differing code. The `diagnostic_codes` ratchet test and the shadow
//! report both read these, so they cannot disagree about what a difference is.

use std::collections::{BTreeMap, BTreeSet};

use orna_hir::{Idx, ModuleOrigin};

use crate::case::{Case, CaseKind, Corpus};
use crate::{observe_checker, observe_new};

/// The expected differences, one `<case> :: <checker codes> => <HIR codes> :: <reason>` line each.
pub const DIFFERING: &str = include_str!("../tests/diagnostic_codes_differing.txt");

/// The old checker's diagnostic codes for a case.
#[must_use]
pub fn checker_codes(case: &Case) -> Option<BTreeSet<String>> {
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
#[must_use]
pub fn hir_codes(case: &Case) -> Option<BTreeSet<String>> {
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

/// A code set as the ledger prints it.
#[must_use]
pub fn joined(codes: &BTreeSet<String>) -> String {
    if codes.is_empty() {
        return "(none)".to_owned();
    }
    codes.iter().cloned().collect::<Vec<_>>().join("; ")
}

/// The recorded reasons of a case's differing codes, one per distinct text, joined with ` | `.
#[must_use]
pub fn reason_of(old: &BTreeSet<String>, new: &BTreeSet<String>) -> String {
    let mut reasons: Vec<&str> = Vec::new();
    for code in old.symmetric_difference(new) {
        let reason = recorded_reason(code).unwrap_or("UNRECORDED");
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    reasons.join(" | ")
}

/// One differing case, in the form `diagnostic_codes_differing.txt` lists it: `<case> :: <checker> => <HIR> :: <reason>`.
#[must_use]
pub fn difference_line(case: &str, old: &BTreeSet<String>, new: &BTreeSet<String>) -> String {
    format!(
        "{case} :: {} => {} :: {}",
        joined(old),
        joined(new),
        reason_of(old, new)
    )
}

/// The reason field of a `diagnostic_codes_differing.txt` row, or `None` when the row has none.
#[must_use]
pub fn row_reason(line: &str) -> Option<&str> {
    let mut fields = line.splitn(3, " :: ");
    let _case = fields.next()?;
    let codes = fields.next()?;
    let reason = fields.next()?.trim();
    (codes.contains(" => ") && !reason.is_empty()).then_some(reason)
}

/// The `## expected` section of a differing-cases ledger.
#[must_use]
pub fn expected_differences(ledger: &str) -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    let mut in_expected = false;
    for line in ledger.lines() {
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
#[must_use]
pub fn recorded_reason(code: &str) -> Option<&'static str> {
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
             `use` imports the spec requires (ORNA-IMPORT); not a std export gap",
        ),
        "ORNA-S021-TYPE" => {
            Some("checker call-site type mismatch; HIR reports the conflict as ORNA-INFER-011")
        }
        "ORNA-INFER-011" => Some(
            "HIR constraint conflict. Range membership (`value in range`, ORNA-RANGE-006) is typed; the \
             residual conflict is a legacy fixture passing `0..10` where the spec's `take(rows, count: Int)` \
             takes an Int (09-standard-library.md:31); the checker reports the same site as ORNA-S021-TYPE",
        ),
        "ORNA-INFER-007" => {
            Some("HIR unsolved-variable diagnostic; the checker has no code for it")
        }
        "ORNA-S020-ANNOTATION" => Some(
            "HIR requires annotated public parameters under D1 (docs/design/hir.md, H2a subset); \
             ORNA-INFER-004 makes annotation a SHOULD, so this is a deviation closed by H2b inference, \
             not a spec-required error",
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

/// The per-case measurement of the diagnostic-code parity over a corpus.
#[derive(Debug, Default)]
pub struct Measure {
    /// Cases both the checker and HIR analysed.
    pub compared: usize,
    /// Cases one of the two pipelines did not analyse.
    pub skipped: usize,
    /// Per code: `[agree, checker-only, hir-only]` case counts.
    pub table: BTreeMap<String, [usize; 3]>,
    /// Every differing case, keyed by case id, with the checker's and HIR's code sets.
    pub differing: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)>,
}

/// Measures the diagnostic-code parity of every module and expression case of `corpus`.
#[must_use]
pub fn measure(corpus: &Corpus) -> Measure {
    let mut measure = Measure::default();
    for case in &corpus.cases {
        if matches!(case.kind, CaseKind::Repl | CaseKind::StandardBundle) {
            continue;
        }
        let (Some(old), Some(new)) = (checker_codes(case), hir_codes(case)) else {
            measure.skipped += 1;
            continue;
        };
        measure.compared += 1;
        for code in old.union(&new) {
            let row = measure.table.entry(code.clone()).or_insert([0; 3]);
            match (old.contains(code), new.contains(code)) {
                (true, true) => row[0] += 1,
                (true, false) => row[1] += 1,
                (false, true) => row[2] += 1,
                (false, false) => unreachable!("a code in the union is on one side"),
            }
        }
        if old != new {
            measure.differing.insert(case.id.clone(), (old, new));
        }
    }
    measure
}
