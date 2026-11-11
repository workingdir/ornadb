//! Evaluation of unit definitions into keys (units.md sections 3.3 and 3.5, ORNA-UNIT-009, -010, -013, -019).
//!
//! Runs once, after lowering. A base unit's key is its own axis. A derived unit's key is the product of its factors,
//! each factor's key raised to its written exponent and scaled by its coefficient, with `/` dividing the whole term.
//! A unit whose definition is broken, or depends on one, has no key; the lowering diagnostic or the cycle diagnostic
//! says why.

use std::collections::BTreeMap;

use super::{Exponents, KeyError, MAX_EXPONENT, Rational, UnitKey, UnitTable};
use crate::diag::{UNIT_CYCLE, UNIT_DIM, UNIT_LIMIT};
use crate::{
    DefId, DefKind, DimensionDef, EnumDef, HirDiagnostic, ModuleId, Program, UnitDef, UnitFactor,
};

type Found = (ModuleId, HirDiagnostic);

/// Computes every unit key and family dimension of `program`, interns the keys and records the diagnostics.
pub(crate) fn run(program: &mut Program) {
    let (table, found) = Evaluator::new(program).evaluate();
    program.units = table;
    for (module, diagnostic) in found {
        program.modules[module].diagnostics.push(diagnostic);
    }
}

/// The state of one unit during evaluation: `Visiting` while its factors are being evaluated, then `Done`.
enum Slot {
    Visiting,
    Done(Option<UnitKey>),
}

struct Evaluator<'p> {
    program: &'p Program,
    slots: BTreeMap<DefId, Slot>,
    found: Vec<Found>,
}

impl<'p> Evaluator<'p> {
    fn new(program: &'p Program) -> Self {
        Self {
            program,
            slots: BTreeMap::new(),
            found: Vec::new(),
        }
    }

    fn evaluate(mut self) -> (UnitTable, Vec<Found>) {
        let program = self.program;
        for unit in program.defs.indices() {
            if matches!(program.defs[unit].kind, DefKind::Unit(_)) {
                self.key_of(unit);
            }
        }
        let dimensions = self.check_families();
        let mut table = UnitTable::default();
        for (unit, slot) in &self.slots {
            if let Slot::Done(Some(key)) = slot {
                let id = table.intern(key.clone());
                table.bind_unit(*unit, id);
            }
        }
        for (family, dimension) in dimensions {
            table.bind_family(family, dimension);
        }
        (table, self.found)
    }

    /// The key of `unit`, computed once. A unit found on the stack is a cycle, reported at that unit.
    fn key_of(&mut self, unit: DefId) -> Option<UnitKey> {
        if matches!(self.slots.get(&unit), Some(Slot::Visiting)) {
            let message = format!("unit `{}` depends on itself", name_of(self.program, unit));
            self.report(unit, UNIT_CYCLE, message);
            return None;
        }
        if let Some(Slot::Done(key)) = self.slots.get(&unit) {
            return key.clone();
        }
        self.slots.insert(unit, Slot::Visiting);
        let key = self.compute(unit);
        self.slots.insert(unit, Slot::Done(key.clone()));
        key
    }

    fn compute(&mut self, unit: DefId) -> Option<UnitKey> {
        let program = self.program;
        let DefKind::Unit(UnitDef {
            family: Some(family),
            factors, ..
        }) = &program.defs[unit].kind
        else {
            return None;
        };
        if base_axis(program, *family) == Some(unit) {
            return Some(UnitKey::base(unit));
        }
        if factors.is_empty() {
            return None;
        }
        match self.derive(factors) {
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

    /// Checks every family against its dimension (ORNA-UNIT-010) and returns the dimensions of the families that
    /// have one. A base family's dimension is its base unit's axis; a derived family's is its variants' common one.
    fn check_families(&mut self) -> Vec<(DefId, Exponents)> {
        let program = self.program;
        let mut families: BTreeMap<DefId, Vec<DefId>> = BTreeMap::new();
        for unit in program.defs.indices() {
            if let DefKind::Unit(UnitDef {
                family: Some(family),
                ..
            }) = &program.defs[unit].kind
            {
                families.entry(*family).or_default().push(unit);
            }
        }
        let mut dimensions = Vec::new();
        for (family, units) in families {
            let keys = units
                .into_iter()
                .filter_map(|unit| self.key_of(unit).map(|key| (unit, key)))
                .collect::<Vec<_>>();
            let dimension = match base_axis(program, family) {
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
                        name_of(program, *unit),
                        show(program, key.exponents()),
                        name_of(program, family),
                        show(program, &dimension),
                    );
                    self.report(*unit, UNIT_DIM, message);
                }
            }
            dimensions.push((family, dimension));
        }
        dimensions
    }

    fn report(&mut self, unit: DefId, code: &'static str, message: String) {
        let program = self.program;
        let def = &program.defs[unit];
        self.found.push((
            def.module,
            HirDiagnostic::new(code, message, def.span.clone()),
        ));
    }
}

/// The axis of a base family: the one bare variant its dimension declares. `None` for a derived family.
fn base_axis(program: &Program, family: DefId) -> Option<DefId> {
    match &program.defs[family].kind {
        DefKind::Enum(EnumDef {
            dimension: Some(DimensionDef { unit: Some(axis) }),
            ..
        }) => Some(*axis),
        _ => None,
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

fn name_of(program: &Program, def: DefId) -> &str {
    program.defs[def].name.as_str()
}

/// A dimension printed over its axes' names: `metre*second^-1`, or `1` when dimensionless.
fn show(program: &Program, exponents: &Exponents) -> String {
    if exponents.is_dimensionless() {
        return "1".to_owned();
    }
    exponents
        .iter()
        .map(|(axis, exponent)| {
            let name = name_of(program, axis);
            if exponent == 1 {
                name.to_owned()
            } else {
                format!("{name}^{exponent}")
            }
        })
        .collect::<Vec<_>>()
        .join("*")
}
