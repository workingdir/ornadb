//! One interned `Ty` (design section 2.3).
//!
//! Types are interned per [`crate::Program`]: equal types are equal [`Ty`]
//! values, so a type comparison is an integer comparison. The interner is the
//! only owner of `Ty` allocation; a `Ty` from one program must not be used in
//! another.
//!
//! Inference variables ([`TyVar`]) appear only while a body is being checked.
//! A finished body contains no `TyKind::Infer`; the validator rejects one.

use std::collections::HashMap;

use crate::{DefId, EffectRow, Name};

/// An interned type. `Copy`, and equality is identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Ty(u32);

impl Ty {
    /// The raw interner index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// One generic parameter position of the enclosing definition.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ParamIdx(u32);

impl ParamIdx {
    /// Wraps a parameter position.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the parameter position.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// One inference variable, solved by `InferCtxt` during checking.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TyVar(u32);

impl TyVar {
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

/// The structure of a type.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TyKind {
    /// `Bool`.
    Bool,
    /// `Int`.
    Int,
    /// `Decimal`.
    Decimal,
    /// `Float`.
    Float,
    /// `Str` (the specification spelling; the 1.0 tree called it `Text`).
    Str,
    /// `Blob`.
    Blob,
    /// `Uuid`.
    Uuid,
    /// `Date`.
    Date,
    /// `Instant`.
    Instant,
    /// `Duration`.
    Duration,
    /// `Period`.
    Period,
    /// `Digest`.
    Digest,
    /// The unit type of an expression with no value.
    Unit,
    /// The type of an expression that cannot complete (`fail`, `return`,
    /// `break`, `continue`).
    Never,
    /// A diagnostic was already emitted for this expression. Never a dynamic
    /// `Any`.
    Error,
    /// `T?`.
    Option(Ty),
    /// `List<T>`.
    List(Ty),
    /// `Map<K, V>`: immutable and persistent, keys under the canonical total
    /// order (decision D6).
    Map(Ty, Ty),
    /// `Set<T>`.
    Set(Ty),
    /// `(T, U, ..)`.
    Tuple(Box<[Ty]>),
    /// A structural record, sorted by field name.
    Record(Box<[(Name, Ty)]>),
    /// `Range<T>`.
    Range(Ty),
    /// A table or query value; the row type is inside.
    Relation(Ty),
    /// `Stream<T>`.
    Stream(Ty),
    /// A declared nominal: a table row type, a `type`, an `enum`, or a `sys`
    /// type. The definition is the identity; the box is the generic arguments.
    Adt(DefId, Box<[Ty]>),
    /// A function value.
    Fn(FnTy),
    /// A generic parameter of the enclosing definition.
    Param(ParamIdx),
    /// An inference variable, absent from a finished body.
    Infer(TyVar),
}

/// A function type.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FnTy {
    /// Parameter types, in declaration order.
    pub params: Box<[Ty]>,
    /// The result type.
    pub ret: Ty,
    /// How the function's effects are described in its type, per decision D2.
    pub effects: EffectRow,
}

/// Interns types for one [`crate::Program`].
#[derive(Clone, Debug, Default)]
pub struct TyInterner {
    kinds: Vec<TyKind>,
    interned: HashMap<TyKind, Ty>,
}

impl TyInterner {
    /// An interner with no types.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the interned type for `kind`, adding it when it is new.
    pub fn intern(&mut self, kind: TyKind) -> Ty {
        if let Some(ty) = self.interned.get(&kind) {
            return *ty;
        }
        let ty = Ty(u32::try_from(self.kinds.len()).expect("HIR holds at most u32::MAX types"));
        self.kinds.push(kind.clone());
        self.interned.insert(kind, ty);
        ty
    }

    /// The structure of `ty`, or `None` when `ty` belongs to another program.
    #[must_use]
    pub fn kind(&self, ty: Ty) -> Option<&TyKind> {
        self.kinds.get(ty.0 as usize)
    }

    /// Whether `ty` was interned here.
    #[must_use]
    pub fn contains(&self, ty: Ty) -> bool {
        (ty.0 as usize) < self.kinds.len()
    }

    /// The number of distinct interned types.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether no type has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The unit type.
    pub fn unit(&mut self) -> Ty {
        self.intern(TyKind::Unit)
    }

    /// The diagnostic-error type.
    pub fn error(&mut self) -> Ty {
        self.intern(TyKind::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::{TyInterner, TyKind};

    #[test]
    fn interning_the_same_structure_returns_the_same_type() {
        let mut types = TyInterner::new();
        let int = types.intern(TyKind::Int);
        let list = types.intern(TyKind::List(int));
        let again = types.intern(TyKind::Int);
        let other = types.intern(TyKind::List(again));
        assert_eq!(list, other);
        assert_eq!(types.kind(list), Some(&TyKind::List(int)));
        assert_eq!(types.len(), 2);
    }
}
