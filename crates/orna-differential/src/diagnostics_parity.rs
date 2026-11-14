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

/// The recorded reason of a code in one case: the case-specific reason, else the per-code reason.
#[must_use]
pub fn reason_for(case: &str, code: &str) -> Option<&'static str> {
    case_reason(case, code).or_else(|| recorded_reason(code))
}

/// The recorded reasons of a case's differing codes, one per distinct text, joined with ` | `.
#[must_use]
pub fn reason_of(case: &str, old: &BTreeSet<String>, new: &BTreeSet<String>) -> String {
    let mut reasons: Vec<&str> = Vec::new();
    for code in old.symmetric_difference(new) {
        let reason = reason_for(case, code).unwrap_or("UNRECORDED");
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
        reason_of(case, old, new)
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
/// This is the per-code reason. A case whose finding is more specific carries its own reason in
/// [`case_reason`], which takes precedence in [`reason_for`]. A code without a reason here must not
/// differ in any case. The typing codes (`ORNA-INFER-*`, `ORNA-S021-TYPE`, `ORNA-S020-ANNOTATION`)
/// are not yet mapped to each other: HIR reports constraint conflicts under `ORNA-INFER-011` with
/// both types printed, and the checker reports the call-site mismatch under `ORNA-S021-TYPE`; the
/// pairing is unproven per message.
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
        "ORNA-UNIT-UNRESOLVED" => Some(
            "HIR reports a numeric literal's suffix that no unit of the unit fixture declares (ORNA-UNIT-008 step 3); \
             the affine unit C is a library type on HIR (units.md section 8.5), not a declared unit, so its \
             E5002/E5003 come from the affine rule the checker runs and HIR does not port",
        ),
        "ORNA-S012-UNRESOLVED" => Some(
            "OPEN HIR gap: HIR reports a name as not in scope, not an export, or not a declared type, where \
             the checker resolves it from its observer catalogue or reports a different error; HIR's \
             environment does not yet model these names",
        ),
        "ORNA-S021-TYPE" => Some(
            "checker-only static type mismatch; HIR reports a different first error, or none, for the same \
             site, so the mismatch is not reconciled in HIR (OPEN unless the case reason records a spec verdict)",
        ),
        "ORNA-INFER-011" => Some(
            "HIR constraint conflict: HIR's typing rejects the site with both types printed; the checker \
             reports or accepts it under another code (see the case reason)",
        ),
        "ORNA-INFER-007" => {
            Some("HIR unsolved-variable diagnostic; the checker has no code for it")
        }
        "ORNA-S020-ANNOTATION" => Some(
            "D1 strict, INFER-004 SHOULD allows the old behaviour; HIR requires annotated public parameters \
             (docs/design/hir.md, H2a subset), so the difference is expected, not a spec-required error",
        ),
        "ORNA-S022-UNSUPPORTED" => Some(
            "HIR does not support this construct yet: sys declarations and module calls (stage H5, sys.orna \
             lowering), top-level offset or affine units (H2c), or an unknown unary operator (see the case reason)",
        ),
        "ORNA-EVAL-ARGUMENT" => Some(
            "HIR binds named arguments against the callee's parameter labels and rejects a call that names no \
             parameter or omits a required one; the checker's verdict on the same call differs (see the case reason)",
        ),
        "ORNA091-E-RESULT" => Some(
            "OPEN: HIR does not port ERR-009 (Result/Ok/Err control plumbing was removed; return the success \
             type directly); HIR rejects the file earlier with ORNA-S012 (Result is not declared)",
        ),
        "ORNA091-E-TRYFROM" => Some(
            "OPEN: HIR does not port TRYFROM (Orna-1.1.0.md:5104: use From<Source>; From may not fail)",
        ),
        "ORNA091-E-CURRENCY-SYMBOL" => Some(
            "OPEN: HIR does not port CURRENCY-SYMBOL (Orna-1.1.0.md:5100: currency symbols belong to \
             locale-aware formatting, not Currency identity)",
        ),
        "ORNA-A091-002" => {
            Some("OPEN: HIR does not port ASSERT-016/017 (a legacy self pipe in a table assertion)")
        }
        "ORNA-A091-004" => Some(
            "OPEN: HIR does not port ASSERT-038 (a table assertion must be a predicate over its owner's relation)",
        ),
        "ORNA-A091-007" => Some(
            "OPEN: HIR does not port ASSERT-039 (a declaration assertion may not use a forbidden network effect)",
        ),
        code if code.starts_with("ORNA100-") => Some(
            "OPEN: HIR does not port the legacy system-row method rejection (ORNA100-E-SYS-ADMIN-METHOD: system \
             rows are read-only; only the checker's message states it)",
        ),
        "E5001" => Some(
            "HIR types `90.days` through the builtin Duration postfix, which is not a declared Quantity, so the E5001 dimension rule (HB5 engine_codes operator_rejection) does not see two Quantities; follow-up: route builtin Duration through the same dimension check, bead ornadb-2bga0",
        ),
        "E5002" => Some(
            "HIR types the corpus literal (20.C) as a plain number, not Temperature, so the E5002 affine-point rule never sees two points",
        ),
        "E5003" => Some(
            "HIR types the corpus operands as plain numbers, not Temperature, so the E5003 affine-sum rule never fires; the currency case is checked as an ORNA-INFER-011 constraint instead",
        ),
        "E2001" => Some("OPEN: HIR does not port the table-write field type check (E2001)"),
        "E2002" => Some(
            "HIR reports E2002 for a table declaration's required fields (the Audit case, see its case reason); the \
             checker's E2002 on valid/finite-stream is not reproduced because HIR does not resolve that fixture's names",
        ),
        "E2003" => Some(
            "HIR reports E2003 for a field the table declaration does not have (the Audit case, see its case reason)",
        ),
        "E3010" => Some(
            "OPEN: HIR does not port FIELD-006 (a Range<T> is not a primary-key type in version 1.0)",
        ),
        "E3012" => Some(
            "OPEN: HIR does not port FIELD-008 (a computed field must be deterministic and row-local)",
        ),
        "E4107" => Some(
            "OPEN: HIR does not port the relation-equality ambiguity check (E4107; only the checker's message states it)",
        ),
        "E6001" => Some(
            "OPEN: HIR does not port TIME-002 (calendar bucketing of an Instant requires a time zone; Orna-1.1.0.md:2103)",
        ),
        "E7002" => Some("OPEN: HIR does not port SECRET-002 (secret values cannot be displayed)"),
        "E7201" => Some(
            "OPEN: HIR does not port PRES-008 (Display and Present implementations must be read-only)",
        ),
        "E9001" => Some(
            "OPEN: HIR does not port ATTACH-002 (cross-database writes are not atomic or supported)",
        ),
        "E9101" => Some(
            "OPEN: HIR does not port CONSUMER-001 (a durable source may not be consumed twice without distinct consumer identity)",
        ),
        "E9102" => Some(
            "OPEN: HIR does not port CONSUMER-005 (a durable consumer function may own only one checkpointed source root)",
        ),
        _ => None,
    }
}

/// The reason of one code in one case, for a case whose finding is more specific than the code's
/// per-code reason. Keyed by the case's file name, which is unique within the corpus.
#[must_use]
pub fn case_reason(case: &str, code: &str) -> Option<&'static str> {
    let name = case.rsplit('/').next().unwrap_or(case);
    match (name, code) {
        ("too_many_arguments.orna", "ORNA-INFER-011") => Some(
            "both reject: the call supplies more arguments than parameters (HIR ORNA-INFER-011); the checker \
             reports the same site as ORNA-S020-ANNOTATION and ORNA-S021-TYPE",
        ),
        ("too_many_arguments.orna", "ORNA-S021-TYPE") => Some(
            "both reject: the checker reports the arity mismatch against the static signature as ORNA-S021-TYPE; \
             HIR reports it as ORNA-INFER-011",
        ),
        ("zero_parameter_argument.orna", "ORNA-S021-TYPE") => Some(
            "both reject: a zero-parameter function receives an argument; the checker reports ORNA-S021-TYPE, \
             HIR reports ORNA-INFER-011",
        ),
        ("zero_parameter_argument.orna", "ORNA-INFER-011") => Some(
            "both reject: the call supplies more arguments than parameters; HIR reports ORNA-INFER-011, the \
             checker ORNA-S021-TYPE",
        ),
        ("tuple-destructure-wrong-arity.orna", "ORNA-S012-UNRESOLVED") => Some(
            "checker-only: the checker reports two names unresolved in this fixture where HIR resolves them; \
             both sides reject it, and HIR's first error is the arity conflict",
        ),
        ("tuple-destructure-wrong-arity.orna", "ORNA-INFER-011") => Some(
            "both reject: the pattern `((a, b))` needs a pair and the argument is Int; HIR reports Int and \
             (?4, ?5) cannot both hold; the checker reports ORNA-S012 first",
        ),
        ("comparison-chain.orna", "ORNA-S021-TYPE") => Some(
            "both reject: the comparison chain mixes Bool with Int; the checker reports ORNA-S021-TYPE (static \
             types are incompatible), HIR reports ORNA-INFER-011",
        ),
        ("comparison-chain.orna", "ORNA-INFER-011") => Some(
            "both reject: Bool and Int cannot both hold (ORNA-INFER-011); the checker reports the same site as \
             ORNA-S021-TYPE",
        ),
        ("question-coalesce-adjacent.orna", "ORNA-S021-TYPE") => Some(
            "both reject: the operands are Int? and Int; the checker reports ORNA-S021-TYPE (static types are \
             incompatible), HIR reports ORNA-INFER-011",
        ),
        ("question-coalesce-adjacent.orna", "ORNA-INFER-011") => Some(
            "both reject: Int? and Int cannot both hold (ORNA-INFER-011); the checker reports ORNA-S021-TYPE",
        ),
        ("static-protocol-function.orna", "ORNA-S021-TYPE") => Some(
            "both reject: the types () and Str are incompatible; the checker reports ORNA-S021-TYPE, HIR \
             reports ORNA-INFER-011",
        ),
        ("static-protocol-function.orna", "ORNA-INFER-011") => Some(
            "both reject: () and Str cannot both hold (ORNA-INFER-011); the checker reports ORNA-S021-TYPE",
        ),
        ("incompatible-dimensions.orna", "ORNA-INFER-011") => Some(
            "both reject: HIR reports the operator is not defined for Duration and Quantity<Int, Energy.kWh>; \
             the checker reports E5001 for the same site. OPEN: builtin Duration is not routed through the \
             dimension check (bead ornadb-2bga0)",
        ),
        ("currency-locale-format.orna", "ORNA-EVAL-ARGUMENT") => Some(
            "OPEN: HIR reports the call omits a required parameter; the checker accepts it. The spec's stdlib \
             format signature does not state which parameter is required, so the verdict is not decidable from \
             the reference text",
        ),
        ("currency-locale-format.orna", "ORNA-INFER-011") => Some(
            "OPEN: HIR reports Locale and Str cannot both hold at the format argument; the checker accepts it \
             (the same stdlib format signature question)",
        ),
        ("lambda-empty-record.orna", "ORNA-INFER-011") => Some(
            "FIXTURE SPEC-INVALID: the fixture passes Unit where the lambda takes the empty record {}; ARROW-001(1) \
             and RECORD-002 do not admit it, so HIR's rejection is the spec verdict. This slice does not change \
             the fixture",
        ),
        ("money-rate.orna", "ORNA-INFER-011") => Some(
            "OPEN REAL HIR GAP: Decimal * Money<GBP> is admitted by ORNA-MONEY-003 and accepted by the checker; HIR \
             reports the operator is not defined for Decimal and Money<GBP>",
        ),
        ("ranges.orna", "ORNA-INFER-011") => Some(
            "FIXTURE SPEC-INVALID: the fixture passes a range where take(rows, count: Int) takes Int \
             (09-standard-library.md:31); HIR's rejection is the spec verdict and the checker accepts it",
        ),
        ("effectful-display.orna", "E2002") => Some(
            "spec-correct: the Audit table declaration requires `action` and `order` on insertion; the Display \
             implementation's Audit.insert supplies only `message`, so HIR reports the omitted fields (Main \
             decision, header L7)",
        ),
        ("effectful-display.orna", "E2003") => Some(
            "spec-correct: the Audit table declaration has no `message` field, so HIR reports the unknown field \
             (Main decision, header L7)",
        ),
        ("finite-stream.orna", "E2002") => Some(
            "OPEN, unverified: the checker reports missing required field `name` on this valid/ fixture, and HIR \
             cannot resolve `rows`, `read` or `for_each` (ORNA-S012), so it never reaches the insertion; which side \
             is right is not established",
        ),
        ("legacy-checkpoint-reset-method.orna", "ORNA-EVAL-ARGUMENT") => Some(
            "both reject: HIR reports no parameter is named `to`; the checker reports ORNA100-E-SYS-ADMIN-METHOD \
             (system rows are read-only; use sys.admin.reset_checkpoint)",
        ),
        ("legacy-stream-skip-method.orna", "ORNA-EVAL-ARGUMENT") => Some(
            "both reject: HIR reports no parameter is named `reason`; the checker reports ORNA100-E-SYS-ADMIN-METHOD \
             (system rows are read-only; use sys.admin.skip_failure)",
        ),
        ("legacy-tryfrom.orna", "ORNA-S021-TYPE") => Some(
            "both reject: the checker reports a generic protocol instantiation it cannot validate (ORNA-S021-TYPE); \
             HIR reports the D1 annotation requirement on `value`",
        ),
        ("assert-effectful-table.orna", "ORNA-S012-UNRESOLVED") => Some(
            "OPEN HIR gap: `get` is not an export of the module, which the checker resolves; the checker reports \
             the forbidden network effect (ASSERT-039)",
        ),
        ("assert-owner-type-mismatch.orna", "E2001") => Some(
            "OPEN: the checker reports operator `>=` is not defined for Int in a table assertion; HIR does not \
             port table-assertion typing",
        ),
        ("assert-owner-type-mismatch.orna", "ORNA-S022-UNSUPPORTED") => Some(
            "OPEN: HIR reports an unknown unary operator on the assertion, where the checker reports E2001 and \
             ORNA-A091-004",
        ),
        ("two-durable-sources.orna", "ORNA-S021-TYPE") => Some(
            "OPEN: the checker reports the named arguments do not match the consumer's static signature; HIR cannot \
             resolve `google`, `openbanking`, `for_each` or `process` (ORNA-S012), so it never reaches the signature check",
        ),
        ("implicit-conversion-chain.orna", "ORNA-S021-TYPE") => Some(
            "spec-correct: ORNA-CONVERT-007 forbids the A -> B -> C conversion chain this fixture needs; its types are \
             undeclared, so HIR's ORNA-S012 is the first error and the checker's ORNA-S021 is extra (header L9)",
        ),
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
