//! Effects per function (design section 2.5).
//!
//! One vocabulary for the twelve `sys.EffectKind` values, plus `Opaque` (an
//! effectful callee with no named kind) and `MayFail` (the call can fail).
//! [`EffectSet`] is the bitset the effect pass computes for every function;
//! [`EffectRow`] is how a function *type* describes it, parameterised by
//! decision D2.

/// One host effect kind, in `sys.EffectKind` declaration order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EffectKind {
    /// Reads a table.
    TableRead,
    /// Writes a table.
    TableWrite,
    /// Reaches the network.
    Network,
    /// Reads or writes the filesystem.
    Filesystem,
    /// Spawns or signals a process.
    Process,
    /// Reads the clock.
    Clock,
    /// Reads entropy.
    Randomness,
    /// Reads a secret.
    SecretAccess,
    /// Presents or interprets UI.
    Ui,
    /// Reaches the Git repository.
    Git,
    /// Administers storage.
    StorageAdmin,
    /// Administers the runtime.
    RuntimeAdmin,
}

impl EffectKind {
    /// Every `sys.EffectKind` value, in declaration order.
    pub const ALL: [EffectKind; 12] = [
        EffectKind::TableRead,
        EffectKind::TableWrite,
        EffectKind::Network,
        EffectKind::Filesystem,
        EffectKind::Process,
        EffectKind::Clock,
        EffectKind::Randomness,
        EffectKind::SecretAccess,
        EffectKind::Ui,
        EffectKind::Git,
        EffectKind::StorageAdmin,
        EffectKind::RuntimeAdmin,
    ];

    const fn bit(self) -> u16 {
        // The two non-`EffectKind` bits start after every named kind.
        const _: () = assert!(EffectKind::ALL.len() <= 12);
        assert!((self as u16) < 12, "EffectKind bit collides with Opaque");
        1 << (self as u16)
    }
}

/// The bit for an effectful callee with no named [`EffectKind`].
const OPAQUE: u16 = 1 << 12;
/// The bit for a call that can fail.
const MAY_FAIL: u16 = 1 << 13;

/// The set of effects one function can perform.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EffectSet(u16);

impl EffectSet {
    /// The empty set: a pure function that cannot fail.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// The set holding `kind`.
    #[must_use]
    pub const fn single(kind: EffectKind) -> Self {
        Self(kind.bit())
    }

    /// The set holding an effectful call with no named kind.
    #[must_use]
    pub const fn opaque() -> Self {
        Self(OPAQUE)
    }

    /// The set holding only `MayFail`.
    #[must_use]
    pub const fn failing() -> Self {
        Self(MAY_FAIL)
    }

    /// Whether the set holds `kind`.
    #[must_use]
    pub const fn contains(self, kind: EffectKind) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Whether the set holds an effectful call with no named kind.
    #[must_use]
    pub const fn is_opaque(self) -> bool {
        self.0 & OPAQUE != 0
    }

    /// Whether the set holds `MayFail`.
    #[must_use]
    pub const fn may_fail(self) -> bool {
        self.0 & MAY_FAIL != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the union of both sets.
    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Adds `kind`.
    pub fn insert(&mut self, kind: EffectKind) {
        self.0 |= kind.bit();
    }

    /// Adds every effect of `other`.
    pub fn join_with(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// The raw bitset.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// The named kinds in the set, in declaration order.
    pub fn kinds(self) -> impl Iterator<Item = EffectKind> {
        EffectKind::ALL
            .into_iter()
            .filter(move |kind| self.contains(*kind))
    }
}

/// How a function type describes its effects, per decision D2.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EffectRow {
    /// The effects are written or inferred and are part of the type.
    Known(EffectSet),
    /// The function is polymorphic over a callback's effects.
    Var(EffectVar),
    /// Function types carry no effect information (the status quo made
    /// explicit; a callee of such a type counts as [`EffectSet::opaque`]).
    Hidden,
}

/// One effect-row variable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EffectVar(u32);

impl EffectVar {
    /// Wraps a variable index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the variable index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{EffectKind, EffectRow, EffectSet};

    #[test]
    fn effect_sets_join_and_report_their_named_kinds() {
        let mut effects = EffectSet::single(EffectKind::Clock);
        effects.join_with(EffectSet::single(EffectKind::TableRead));
        effects.join_with(EffectSet::failing());
        assert_eq!(
            effects.kinds().collect::<Vec<_>>(),
            vec![EffectKind::TableRead, EffectKind::Clock]
        );
        assert!(effects.may_fail());
        assert!(!effects.is_opaque());

        let row = EffectRow::Known(effects);
        assert_eq!(row, EffectRow::Known(effects));
    }
}
