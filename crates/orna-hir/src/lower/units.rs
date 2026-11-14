//! Unit families (units.md section 2, D15).
//!
//! A marked enum is a unit family: each variant is a unit, a bare variant is the base unit, and a
//! `= unit_expr` variant is derived. Lowering records a derived variant's expression as resolved
//! factors. Nothing is evaluated here: coefficients and exponents stay as written, and the dimension
//! of a derived family is computed by the checker.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use orna_syntax_ast::{EnumVariant, Expr as AstExpr, LiteralKind, UnitExpr, UnitTerm};

use super::BodyState;
use super::declare::{Builder, PendingKind, UnitLookup};
use super::exprs::{error_expr, lower_expr, push};
use crate::diag::{AMBIGUOUS, UNIT_UNRESOLVED};
use crate::{
    Arg, ArgValue, Call, Callee, DefId, DefKind, DimensionDef, ExprId, ExprKind, Idx, ModuleId,
    Name, NameTarget, ParamIdx, UnitDef, UnitFactor, UseContext, Visibility,
};

/// Whether `base` is a numeric literal, the only base a unit suffix applies to (ORNA-UNIT-008).
pub(crate) fn is_numeric_literal(base: &AstExpr) -> bool {
    matches!(
        base,
        AstExpr::Literal {
            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
            ..
        }
    )
}

/// Lowers `literal.name`, a numeric literal with a unit suffix, as a call of that unit's constructor, so the
/// checker types it as the unit. `None` when no unit of that name is in scope; the caller then tries the
/// builtin postfix table. Several families in scope defining the name are ambiguous and reported by name.
pub(crate) fn lower_suffix_unit(
    builder: &mut Builder,
    state: &mut BodyState,
    base: &AstExpr,
    name: &str,
    span: &SyntaxSpan,
) -> Option<ExprId> {
    if !is_numeric_literal(base) {
        return None;
    }
    let suffix = Name::new(name);
    let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
    match builder.resolve_unit(state.module, &suffix) {
        UnitLookup::None => None,
        UnitLookup::Ambiguous(candidates) => {
            let families = candidates
                .iter()
                .map(|unit| family_name(builder, *unit))
                .collect::<Vec<_>>()
                .join(" and ");
            builder.diagnose(
                state.module,
                AMBIGUOUS,
                format!("unit suffix `{name}` is ambiguous: it names a unit of {families}"),
                name_span,
            );
            Some(error_expr(builder, state, span.clone()))
        }
        UnitLookup::Found(unit) => {
            super::record_body_use(
                &mut builder.program,
                state.module,
                state.id,
                name_span.clone(),
                suffix,
                UseContext::Value,
                NameTarget::Def(unit),
            );
            let literal = lower_expr(builder, state, base);
            let call = Call {
                callee: Callee::Ctor { def: unit },
                callee_span: name_span,
                args: Box::new([Arg {
                    slot: ParamIdx::new(0),
                    value: ArgValue::Expr(literal),
                }]),
                piped: false,
            };
            Some(push(
                builder,
                state,
                ExprKind::Call(Box::new(call)),
                span.clone(),
            ))
        }
    }
}

/// The name of the family a unit belongs to, for a diagnostic.
fn family_name(builder: &Builder, unit: DefId) -> String {
    match &builder.pending[unit.index()].kind {
        PendingKind::Built(DefKind::Unit(UnitDef {
            family: Some(family),
            ..
        })) => builder.def_name(*family).to_string(),
        _ => String::from("an unnamed family"),
    }
}

/// A derived unit whose expression is resolved once every definition is built.
#[derive(Clone, Debug)]
pub(crate) struct UnitJob {
    /// The unit the expression defines.
    pub(crate) unit: DefId,
    /// The family the unit is a variant of.
    pub(crate) family: DefId,
    /// The module the expression is written in.
    pub(crate) module: ModuleId,
    /// The unit expression as written.
    pub(crate) expr: UnitExpr,
}

/// The dimension of a unit family. A base family's dimension is the axis of its bare unit; a derived
/// family has no bare unit and its dimension is computed by the checker.
pub(crate) fn family_dimension(variants: &[EnumVariant], lowered: &[DefId]) -> DimensionDef {
    let unit = variants
        .iter()
        .zip(lowered)
        .find(|(variant, _)| variant.unit.is_none())
        .map(|(_, def)| *def);
    DimensionDef { unit }
}

impl Builder {
    /// Declares one variant of a unit family as a unit. A derived variant queues its expression.
    pub(crate) fn build_family_unit(
        &mut self,
        module: ModuleId,
        family: DefId,
        variant: &EnumVariant,
    ) -> DefId {
        let unit = self.declare_nested(
            module,
            &variant.name,
            Visibility::Public,
            variant.span.clone(),
            DefKind::Unit(UnitDef {
                family: Some(family),
                factors: Vec::new(),
                offset: None,
                affine: false,
            }),
        );
        if let Some(expr) = &variant.unit {
            self.unit_jobs.push(UnitJob {
                unit,
                family,
                module,
                expr: expr.clone(),
            });
        }
        unit
    }

    /// Resolves every queued derived-unit expression into factors, once all definitions exist.
    pub(crate) fn resolve_unit_jobs(&mut self) {
        for job in std::mem::take(&mut self.unit_jobs) {
            let factors = self.resolve_factors(&job);
            self.pending[job.unit.index()].kind = PendingKind::Built(DefKind::Unit(UnitDef {
                family: Some(job.family),
                factors,
                offset: None,
                affine: false,
            }));
        }
    }

    /// The factors of a derived unit, or none when any term fails to resolve: a broken definition has no
    /// key (units.md ORNA-UNIT-019), so the remaining factors must not define one.
    fn resolve_factors(&mut self, job: &UnitJob) -> Vec<UnitFactor> {
        let mut factors = Vec::with_capacity(job.expr.terms.len());
        let mut broken = false;
        for term in &job.expr.terms {
            match self.unit_factor(job, term) {
                Some(factor) => factors.push(factor),
                None => broken = true,
            }
        }
        if broken { Vec::new() } else { factors }
    }

    /// Resolves one factor of a derived unit. A qualified name (`Length.kilometre`) names a unit of
    /// that family; a bare name is a sibling variant first, otherwise a unit in scope. Several units in
    /// scope with that name are ambiguous, as a suffix is.
    fn unit_factor(&mut self, job: &UnitJob, term: &UnitTerm) -> Option<UnitFactor> {
        let unit = match self.resolve_unit_term(job, term) {
            UnitLookup::Found(unit) => unit,
            UnitLookup::Ambiguous(candidates) => {
                let families = candidates
                    .iter()
                    .map(|unit| family_name(self, *unit))
                    .collect::<Vec<_>>()
                    .join(" and ");
                self.diagnose(
                    job.module,
                    AMBIGUOUS,
                    format!(
                        "unit `{}` is ambiguous: it names a unit of {families}",
                        term.name
                    ),
                    term.span.clone(),
                );
                return None;
            }
            UnitLookup::None => {
                self.diagnose(
                    job.module,
                    UNIT_UNRESOLVED,
                    format!("unit `{}` is not declared in scope", term.name),
                    term.span.clone(),
                );
                return None;
            }
        };
        Some(UnitFactor {
            divide: term.divide,
            coefficient: term.coefficient.as_deref().map(Box::from),
            unit,
            exponent: term.exponent.as_deref().map(Box::from),
        })
    }

    /// The unit a factor names: a qualified factor names a unit of that family, a bare one a sibling
    /// variant or a unit in scope.
    fn resolve_unit_term(&self, job: &UnitJob, term: &UnitTerm) -> UnitLookup {
        let name = Name::new(term.name.as_str());
        let member = match &term.family {
            Some(family) => match self.resolve_bare(job.module, &Name::new(family.as_str())) {
                NameTarget::Def(base) => self.member_def(base, &name),
                _ => None,
            },
            None => self.member_def(job.family, &name),
        }
        .filter(|def| self.is_unit(*def));
        match (member, &term.family) {
            (Some(unit), _) => UnitLookup::Found(unit),
            (None, Some(_)) => UnitLookup::None,
            (None, None) => self.resolve_unit(job.module, &name),
        }
    }
}

/// The text of a unit declaration's offset when it is a numeric literal, exactly as written. A non-literal offset
/// has no text here and is not yet evaluated, so the unit keeps no offset.
pub(crate) fn literal_offset(offset: Option<&AstExpr>) -> Option<Box<str>> {
    match offset? {
        AstExpr::Literal {
            text,
            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
            ..
        } => Some(Box::from(text.as_str())),
        _ => None,
    }
}
