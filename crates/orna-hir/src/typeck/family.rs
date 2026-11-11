//! Typing of a unit family used as a type (units.md ORNA-UNIT-020; 05-units.md "Using a unit").
//!
//! A family name in a type position accepts the quantities of its own variants and no bare number. A quantity of
//! another variant is converted to the family's base unit where it enters the position, by the exact ratio of the two
//! scales. A derived family has no base unit, so its variants are accepted as written.

use std::mem;

use orna_syntax::SyntaxSpan;

use super::BodyCheck;
use super::quantity::{Fail, Num, budget, scaled};
use crate::diag::UNIT_ANNOT;
use crate::ids::UnitKeyId;
use crate::units::Rational;
use crate::units::print::key_text;
use crate::{Body, DefId, DefKind, EnumDef, Expr, ExprId, ExprKind, Ty, TyKind};

impl<'a> BodyCheck<'a> {
    /// Coerces the value `id`, typed `actual`, to `expected`. A unit family in `expected` accepts its own quantities
    /// only, and a quantity of another variant converts to the base unit. Every other type coerces as before.
    pub(super) fn coerce_at(
        &mut self,
        body: &mut Body,
        id: ExprId,
        actual: Ty,
        expected: Ty,
        span: SyntaxSpan,
    ) -> Ty {
        let Some(family) = self.unit_family(expected) else {
            return self.coerce(actual, expected, span);
        };
        let actual = self.resolved(actual);
        match self.kind_of(actual) {
            Some(TyKind::Quantity(value, key)) => {
                let Some(num) = self
                    .kind_of(self.resolved(value))
                    .as_ref()
                    .and_then(Num::of)
                else {
                    return self.coerce(actual, expected, span);
                };
                match self.family_factor(family, num, key) {
                    Ok(factor) => {
                        if let Some(factor) = factor {
                            wrap_convert(body, id, actual, factor);
                        }
                        expected
                    }
                    Err(fail) => self.report(fail, span),
                }
            }
            Some(kind) if Num::of(&kind).is_some() => {
                let message = format!(
                    "a bare number is not a value of the unit family `{}`; write it with one of its units",
                    self.program.defs[family].name
                );
                self.report(Fail::new(UNIT_ANNOT, message), span)
            }
            _ => self.coerce(actual, expected, span),
        }
    }

    /// The factor that converts a quantity of `key` to the base unit of `family`, when the position needs one. `None`
    /// when the quantity already is the base unit, or when the family has no base unit.
    fn family_factor(
        &self,
        family: DefId,
        num: Num,
        key: UnitKeyId,
    ) -> Result<Option<Rational>, Fail> {
        if !self.family_units(family).contains(&key) {
            let message = format!(
                "`{}` is not a unit of the family `{}`",
                key_text(self.program, key),
                self.program.defs[family].name
            );
            return Err(Fail::new(UNIT_ANNOT, message));
        }
        let Some(base) = self.family_base(family) else {
            return Ok(None);
        };
        if base == key {
            return Ok(None);
        }
        let factor = self
            .program
            .units
            .key(key)
            .ratio_to(self.program.units.key(base))
            .map_err(budget)?;
        scaled(num, &factor)?;
        Ok(Some(factor))
    }

    /// The enum definition of `family`, when the definition is an enum.
    fn family_def(&self, family: DefId) -> Option<&EnumDef> {
        match &self.program.defs.get(family)?.kind {
            DefKind::Enum(definition) => Some(definition),
            _ => None,
        }
    }

    /// The definition of the unit family `ty` names in a type position, when it is one.
    fn unit_family(&self, ty: Ty) -> Option<DefId> {
        let Some(TyKind::Adt(def, _)) = self.kind_of(self.resolved(ty)) else {
            return None;
        };
        self.family_def(def)?.dimension.as_ref().map(|_| def)
    }

    /// The keys of the units of `family`, in declaration order.
    fn family_units(&self, family: DefId) -> Vec<UnitKeyId> {
        self.family_def(family).map_or_else(Vec::new, |definition| {
            definition
                .variants
                .iter()
                .filter_map(|variant| self.program.units.unit_key(*variant))
                .collect()
        })
    }

    /// The key of the base unit of a base family; `None` for a derived family.
    fn family_base(&self, family: DefId) -> Option<UnitKeyId> {
        let unit = self.family_def(family)?.dimension.as_ref()?.unit?;
        self.program.units.unit_key(unit)
    }
}

/// Rewrites `id` as a conversion of its own value. The original node moves to a child typed `operand_ty` and keeps its
/// legacy weight. The conversion is synthetic and carries none, as lowering gives synthetic nodes.
fn wrap_convert(body: &mut Body, id: ExprId, operand_ty: Ty, factor: Rational) {
    let span = body.exprs[id].span.clone();
    let weight = mem::replace(&mut body.exprs[id].weight, 0);
    let kind = mem::replace(&mut body.exprs[id].kind, ExprKind::Continue);
    let operand = body.exprs.push(Expr {
        kind,
        ty: operand_ty,
        span,
        weight,
    });
    body.exprs[id].kind = ExprKind::UnitConvert { operand, factor };
}
