//! Unit keys and dimensions of a program (units.md sections 3 and 6, ORNA-UNIT-001 to ORNA-UNIT-019).
//!
//! [`key`] holds the exact algebra and [`eval`] computes every unit's key once, after lowering. The results live in
//! [`UnitTable`]: each distinct key is interned to one [`UnitKeyId`], so equal keys compare by id, and each unit
//! family records its dimension.

use std::collections::{BTreeMap, HashMap};

use crate::ids::UnitKeyId;
use crate::{DefId, IndexVec};

pub mod eval;
pub mod key;

pub use key::{Exponents, KeyError, MAX_EXPONENT, Rational, UnitKey};

#[cfg(test)]
mod tests;

/// The evaluated units of one program: interned keys, each unit's key and each family's dimension.
#[derive(Clone, Debug, Default)]
pub struct UnitTable {
    keys: IndexVec<UnitKeyId, UnitKey>,
    interned: HashMap<UnitKey, UnitKeyId>,
    unit_keys: BTreeMap<DefId, UnitKeyId>,
    dimensions: BTreeMap<DefId, Exponents>,
}

impl UnitTable {
    /// The interned key `id`.
    #[must_use]
    pub fn key(&self, id: UnitKeyId) -> &UnitKey {
        &self.keys[id]
    }

    /// The key of a unit, when it has one. A unit whose definition is broken has none; its diagnostic is already
    /// recorded by lowering or evaluation.
    #[must_use]
    pub fn unit_key(&self, unit: DefId) -> Option<UnitKeyId> {
        self.unit_keys.get(&unit).copied()
    }

    /// The dimension of a unit family: the axis of its base unit, or the common dimension of its variants.
    #[must_use]
    pub fn family_dimension(&self, family: DefId) -> Option<&Exponents> {
        self.dimensions.get(&family)
    }

    /// Interns `key`, returning the id every equal key shares.
    pub(crate) fn intern(&mut self, key: UnitKey) -> UnitKeyId {
        if let Some(id) = self.interned.get(&key) {
            return *id;
        }
        let id = self.keys.push(key.clone());
        self.interned.insert(key, id);
        id
    }

    pub(crate) fn bind_unit(&mut self, unit: DefId, key: UnitKeyId) {
        self.unit_keys.insert(unit, key);
    }

    pub(crate) fn bind_family(&mut self, family: DefId, dimension: Exponents) {
        self.dimensions.insert(family, dimension);
    }
}
