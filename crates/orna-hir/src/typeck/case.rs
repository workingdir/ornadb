//! Case coverage on HIR. Each `case` is recorded with the shape of its arms
//! while its arms are typed; once the body finishes, the coverage rules run on
//! the recorded sites against the zonked scrutinee type.
//!
//! The rules mirror the old checker's enum and boolean `case` rules
//! (ORNA-S021-TYPE). A wildcard arm covers no enum variant: the checker rejects
//! a wildcard on an enum outright, so HIR counts it as covering nothing.
//! Option `case` coverage is not ported yet.

use std::collections::BTreeSet;

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::diag::CASE_COVERAGE;
use crate::{Arm, Body, DefId, DefKind, EnumDef, HirDiagnostic, Lit, Pat, Program, Ty, TyKind};

use super::BodyCheck;

const VARIANT_DUPLICATED: &str = "case enum variant is duplicated";
const VARIANT_MISSING: &str = "case expression does not cover every enum variant";
const BOOL_DUPLICATED: &str = "boolean case arm is duplicated";
const BOOL_MISSING: &str = "case expression does not cover true and false";

/// What one arm's pattern covers, as far as coverage is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArmShape {
    /// An enum variant constructor, by its definition.
    Variant(DefId),
    /// A boolean literal.
    Bool(bool),
    /// A wildcard or a binding, which matches every value.
    Any,
    /// Any other pattern; it covers no value by itself.
    Other,
}

/// One arm of a recorded `case`.
#[derive(Clone, Copy, Debug)]
pub(super) struct CaseArm {
    shape: ArmShape,
    /// Whether a guard applies; a guarded arm establishes no coverage.
    guarded: bool,
}

impl CaseArm {
    /// Records one arm from its lowered pattern.
    fn of(body: &Body, arm: &Arm) -> Self {
        let shape = match body.pats.get(arm.pat) {
            Some(Pat::Wild | Pat::Bind { .. }) => ArmShape::Any,
            Some(Pat::Lit(Lit::Bool(value))) => ArmShape::Bool(*value),
            Some(Pat::Ctor { def, .. }) => ArmShape::Variant(*def),
            _ => ArmShape::Other,
        };
        Self {
            shape,
            guarded: arm.guard.is_some(),
        }
    }
}

/// One `case` expression, recorded while its arms are typed.
#[derive(Clone, Debug)]
pub(super) struct CaseSite {
    span: SyntaxSpan,
    /// The scrutinee type, resolved when the body finishes.
    scrutinee: Ty,
    arms: Vec<CaseArm>,
}

impl<'a> BodyCheck<'a> {
    /// Records one `case` for the coverage rules that run when the body
    /// finishes.
    pub(super) fn record_case(
        &mut self,
        body: &Body,
        scrutinee: Ty,
        arms: &[Arm],
        span: SyntaxSpan,
    ) {
        let arms = arms.iter().map(|arm| CaseArm::of(body, arm)).collect();
        self.cases.push(CaseSite {
            span,
            scrutinee,
            arms,
        });
    }

    /// Runs the coverage rules on every recorded `case`. A violation is a
    /// diagnostic and fails the body the same way a type conflict does.
    pub(super) fn check_cases(&mut self) {
        for site in std::mem::take(&mut self.cases) {
            let scrutinee = self.zonk(site.scrutinee);
            let kind = self.kind_of(scrutinee);
            for message in coverage_violations(self.program, kind.as_ref(), &site.arms) {
                self.conflicts += 1;
                self.diagnostics.push(HirDiagnostic::new(
                    CASE_COVERAGE,
                    message,
                    site.span.clone(),
                ));
            }
        }
    }
}

/// The coverage messages for one `case` against its scrutinee type. Only enum
/// and boolean scrutinees have coverage rules here.
fn coverage_violations(
    program: &Program,
    scrutinee: Option<&TyKind>,
    arms: &[CaseArm],
) -> Vec<&'static str> {
    match scrutinee {
        Some(TyKind::Adt(def, _)) => match &program.defs[*def].kind {
            DefKind::Enum(EnumDef { variants, .. }) => enum_violations(variants, arms),
            _ => Vec::new(),
        },
        Some(TyKind::Bool) => bool_violations(arms),
        _ => Vec::new(),
    }
}

/// Enum coverage: every variant covered by an unguarded arm, each at most once.
fn enum_violations(variants: &[DefId], arms: &[CaseArm]) -> Vec<&'static str> {
    let mut violations = Vec::new();
    let mut covered = BTreeSet::new();
    for arm in arms.iter().filter(|arm| !arm.guarded) {
        if let ArmShape::Variant(def) = arm.shape
            && !covered.insert(def)
        {
            violations.push(VARIANT_DUPLICATED);
        }
    }
    if variants.iter().any(|variant| !covered.contains(variant)) {
        violations.push(VARIANT_MISSING);
    }
    violations
}

/// Boolean coverage: `true` and `false` each covered once by unguarded arms. A
/// catch-all covers both values at once.
fn bool_violations(arms: &[CaseArm]) -> Vec<&'static str> {
    let mut violations = Vec::new();
    let mut covered = BTreeSet::new();
    for arm in arms.iter().filter(|arm| !arm.guarded) {
        match arm.shape {
            ArmShape::Bool(value) => cover_bool(value, &mut covered, &mut violations),
            ArmShape::Any => {
                cover_bool(false, &mut covered, &mut violations);
                cover_bool(true, &mut covered, &mut violations);
            }
            ArmShape::Variant(_) | ArmShape::Other => {}
        }
    }
    if covered.len() != 2 {
        violations.push(BOOL_MISSING);
    }
    violations
}

fn cover_bool(value: bool, covered: &mut BTreeSet<bool>, violations: &mut Vec<&'static str>) {
    if !covered.insert(value) {
        violations.push(BOOL_DUPLICATED);
    }
}
