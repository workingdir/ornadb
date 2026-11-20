//! Evaluation of unit definitions into keys (units.md sections 3.3 and 3.5, ORNA-UNIT-009, -010, -013, -019).
//!
//! Runs during lowering, once every unit declaration and derived unit expression is resolved and before any
//! declaration signature is lowered, so a unit suffix in a signature reads the unit's key. A base unit's key is its
//! own axis. A derived unit's key is the product of its factors, each factor's key raised to its written exponent and
//! scaled by its coefficient, with `/` dividing the whole term. A unit whose definition is broken, or depends on one,
//! has no key; the lowering diagnostic or the cycle diagnostic says why.
//!
//! Evaluation is incremental: [`UnitEval`] keeps every finished unit, so a later pass evaluates only what it has not
//! seen, and each diagnostic is reported once.

use std::collections::{BTreeMap, BTreeSet};

use super::{Exponents, KeyError, MAX_EXPONENT, Rational, UnitKey, UnitTable};
use crate::diag::{UNIT_CYCLE, UNIT_DIM, UNIT_LIMIT};
use crate::ids::Idx;
use crate::lower::Builder;
use crate::{DefId, HirDiagnostic, ModuleId, UnitDef, UnitFactor};

type Found = (ModuleId, HirDiagnostic);

/// The state of one unit during evaluation: `Visiting` while its factors are being evaluated, then `Done`.
#[derive(Clone)]
enum Slot {
    Visiting,
    Done(Option<UnitKey>),
}

/// The evaluation state a lowering keeps across passes: the interned keys and every unit already finished.
#[derive(Clone, Default)]
pub(crate) struct UnitEval {
    slots: BTreeMap<DefId, Slot>,
    reported: BTreeSet<(DefId, &'static str)>,
    pub(crate) table: UnitTable,
}

impl UnitEval {
    /// Keys a top-level base unit by its own axis, `({unit: 1}, 1)`, as soon as its declaration is built: a base
    /// unit has no factors to wait for (units.md 3.3).
    pub(crate) fn bind_base(&mut self, unit: DefId) {
        let key = UnitKey::base(unit);
        let id = self.table.intern(key.clone());
        self.table.bind_unit(unit, id);
        self.slots.insert(unit, Slot::Done(Some(key)));
    }
}

/// Evaluates every unit of `builder` not yet finished, interns the keys and records the diagnostics.
pub(crate) fn evaluate(builder: &mut Builder) {
    let mut state = std::mem::take(&mut builder.unit_eval);
    let found = {
        let mut evaluator = Evaluator {
            builder: &*builder,
            state: &mut state,
            found: Vec::new(),
        };
        evaluator.run();
        evaluator.found
    };
    builder.unit_eval = state;
    for (module, diagnostic) in found {
        builder.program.modules[module].diagnostics.push(diagnostic);
    }
}

struct Evaluator<'a> {
    builder: &'a Builder,
    state: &'a mut UnitEval,
    found: Vec<Found>,
}

impl Evaluator<'_> {
    fn run(&mut self) {
        for unit in self.builder.unit_defs() {
            self.key_of(unit);
        }
        self.check_families();
    }

    /// The key of `unit`, computed once. A unit found on the stack is a cycle, reported at that unit.
    fn key_of(&mut self, unit: DefId) -> Option<UnitKey> {
        if matches!(self.state.slots.get(&unit), Some(Slot::Visiting)) {
            let message = format!("unit `{}` depends on itself", name_of(self.builder, unit));
            self.report(unit, UNIT_CYCLE, message);
            return None;
        }
        if let Some(Slot::Done(key)) = self.state.slots.get(&unit) {
            return key.clone();
        }
        self.state.slots.insert(unit, Slot::Visiting);
        let key = self.compute(unit);
        if let Some(key) = &key {
            let id = self.state.table.intern(key.clone());
            self.state.table.bind_unit(unit, id);
        }
        self.state.slots.insert(unit, Slot::Done(key.clone()));
        key
    }

    fn compute(&mut self, unit: DefId) -> Option<UnitKey> {
        let builder = self.builder;
        let UnitDef {
            family,
            factors,
            affine,
            ..
        } = builder.unit_def(unit)?;
        if family.is_some_and(|family| builder.family_axis(family) == Some(unit)) {
            return Some(UnitKey::base(unit));
        }
        if factors.is_empty() {
            return None;
        }
        match self.derive(factors) {
            Ok(Some(key)) if *affine => Some(key.with_affine()),
            Ok(key) => key,
            Err(message) => {
                self.report(unit, UNIT_LIMIT, message);
                None
            }
        }
    }

    /// The product of a derived unit's factors. `Ok(None)` when a factor has no key of its own.
    fn derive(&mut self, factors: &[UnitFactor]) -> Result<Option<UnitKey>, String> {
        let mut key = UnitKey::dimensionless();
        for factor in factors {
            let Some(term) = self.term(factor)? else {
                return Ok(None);
            };
            let combined = if factor.divide {
                key.div(&term)
            } else {
                key.mul(&term)
            };
            key = combined.map_err(describe)?;
        }
        Ok(Some(key))
    }

    /// One factor as a key: its unit's key to the written exponent, scaled by the written coefficient.
    fn term(&mut self, factor: &UnitFactor) -> Result<Option<UnitKey>, String> {
        let Some(unit) = self.key_of(factor.unit) else {
            return Ok(None);
        };
        let mut term = unit.pow(exponent_of(factor)?).map_err(describe)?;
        if let Some(text) = &factor.coefficient {
            term = term.scaled_by(&coefficient_of(text)?).map_err(describe)?;
        }
        Ok(Some(term))
    }

    /// Checks every family against its dimension (ORNA-UNIT-010) and binds the dimensions of the families that have
    /// one. A base family's dimension is its base unit's axis; a derived family's is its variants' common one.
    fn check_families(&mut self) {
        let builder = self.builder;
        let mut families: BTreeMap<DefId, Vec<DefId>> = BTreeMap::new();
        for unit in builder.unit_defs() {
            if let Some(UnitDef {
                family: Some(family),
                ..
            }) = builder.unit_def(unit)
            {
                families.entry(*family).or_default().push(unit);
            }
        }
        for (family, units) in families {
            let keys = units
                .into_iter()
                .filter_map(|unit| self.key_of(unit).map(|key| (unit, key)))
                .collect::<Vec<_>>();
            let dimension = match builder.family_axis(family) {
                Some(axis) => match self.key_of(axis) {
                    Some(key) => key.exponents().clone(),
                    None => continue,
                },
                None => match keys.first() {
                    Some((_, key)) => key.exponents().clone(),
                    None => continue,
                },
            };
            for (unit, key) in &keys {
                if key.exponents() != &dimension {
                    let message = format!(
                        "unit `{}` has dimension {} but its family `{}` has dimension {}",
                        name_of(builder, *unit),
                        show(builder, key.exponents()),
                        name_of(builder, family),
                        show(builder, &dimension),
                    );
                    self.report(*unit, UNIT_DIM, message);
                }
            }
            self.state.table.bind_family(family, dimension);
        }
    }

    /// Records a diagnostic on `unit` once: a later pass re-checks the same units and must not repeat it.
    fn report(&mut self, unit: DefId, code: &'static str, message: String) {
        if !self.state.reported.insert((unit, code)) {
            return;
        }
        let def = &self.builder.pending[unit.index()];
        self.found.push((
            def.module,
            HirDiagnostic::new(code, message, def.span.clone()),
        ));
    }
}

fn exponent_of(factor: &UnitFactor) -> Result<i32, String> {
    let Some(text) = &factor.exponent else {
        return Ok(1);
    };
    text.parse::<i32>()
        .ok()
        .filter(|power| power.unsigned_abs() <= MAX_EXPONENT.unsigned_abs())
        .ok_or_else(|| format!("exponent `{text}` is outside [-32, 32]"))
}

fn coefficient_of(text: &str) -> Result<Rational, String> {
    match Rational::parse_decimal(text) {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(describe(error)),
        None => Err(format!("coefficient `{text}` is not a decimal literal")),
    }
}

fn describe(error: KeyError) -> String {
    match error {
        KeyError::Exponent => "an exponent leaves [-32, 32]".to_owned(),
        KeyError::Budget => "a unit rational exceeds the integer budget".to_owned(),
        KeyError::ZeroDivision => "a unit scale divides by zero".to_owned(),
        KeyError::NonPositiveScale => "a unit scale must be positive".to_owned(),
    }
}

fn name_of(builder: &Builder, def: DefId) -> &str {
    builder.pending[def.index()].name.as_str()
}

/// A dimension printed over its axes' names: `metre*second^-1`, or `1` when dimensionless.
fn show(builder: &Builder, exponents: &Exponents) -> String {
    if exponents.is_dimensionless() {
        return "1".to_owned();
    }
    exponents
        .iter()
        .map(|(axis, exponent)| {
            let name = name_of(builder, axis);
            if exponent == 1 {
                name.to_owned()
            } else {
                format!("{name}^{exponent}")
            }
        })
        .collect::<Vec<_>>()
        .join("*")
}
