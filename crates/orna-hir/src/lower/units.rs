//! Unit families (units.md section 2, D15).
//!
//! A marked enum is a unit family: each variant is a unit, a bare variant is the base unit, and a
//! `= unit_expr` variant is derived. Lowering records a derived variant's expression as resolved
//! factors. Nothing is evaluated here: coefficients and exponents stay as written, and the dimension
//! of a derived family is computed by the checker.

use orna_syntax::{EnumVariant, UnitExpr, UnitTerm};

use super::declare::{Builder, PendingKind};
use crate::diag::UNIT_UNRESOLVED;
use crate::{
    DefId, DefKind, DimensionDef, Idx, ModuleId, Name, NameTarget, UnitDef, UnitFactor, Visibility,
};

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

impl<'a> Builder<'a> {
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
            let factors = job
                .expr
                .terms
                .iter()
                .filter_map(|term| self.unit_factor(&job, term))
                .collect::<Vec<_>>();
            self.pending[job.unit.index()].kind = PendingKind::Built(DefKind::Unit(UnitDef {
                family: Some(job.family),
                factors,
            }));
        }
    }

    /// Resolves one factor of a derived unit. A qualified name (`Length.kilometre`) names a unit of
    /// that family; a bare name is a sibling variant first, otherwise a unit in scope.
    fn unit_factor(&mut self, job: &UnitJob, term: &UnitTerm) -> Option<UnitFactor> {
        let Some(unit) = self.resolve_unit_term(job, term) else {
            self.diagnose(
                job.module,
                UNIT_UNRESOLVED,
                format!("unit `{}` is not declared in scope", term.name),
                term.span.clone(),
            );
            return None;
        };
        Some(UnitFactor {
            divide: term.divide,
            coefficient: term.coefficient.as_deref().map(Box::from),
            unit,
            exponent: term.exponent.as_deref().map(Box::from),
        })
    }

    /// The unit a factor names, or `None` when no unit of that name is in scope.
    fn resolve_unit_term(&self, job: &UnitJob, term: &UnitTerm) -> Option<DefId> {
        let name = Name::new(term.name.as_str());
        let unit = match &term.family {
            Some(family) => match self.resolve_bare(job.module, &Name::new(family.as_str())) {
                NameTarget::Def(base) => self.member_def(base, &name),
                _ => None,
            },
            None => self
                .member_def(job.family, &name)
                .or_else(|| self.resolve_unit(job.module, &name)),
        };
        unit.filter(|def| {
            matches!(
                self.pending[def.index()].kind,
                PendingKind::Built(DefKind::Unit(_))
            )
        })
    }
}
