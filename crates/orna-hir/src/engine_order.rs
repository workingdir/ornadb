//! The engine-derived `Order` (D16): which static types carry the canonical total order of the kernel types.
//!
//! Typeck settles a `compare` call on such a type to the `Order` protocol member, and the executor compares its values
//! natively. Both read this one predicate, so the two never disagree about which types derive the order.

use crate::{Program, Ty, TyKind};

/// Whether the static type `ty` has the engine-derived order: the scalar kernel types, and tuples of them.
#[must_use]
pub fn derives_order(program: &Program, ty: Ty) -> bool {
    match program.tys.kind(ty) {
        Some(TyKind::Bool | TyKind::Int | TyKind::Decimal | TyKind::Str | TyKind::Blob) => true,
        Some(TyKind::Instant | TyKind::Duration) => true,
        Some(TyKind::Tuple(items)) => items.iter().all(|item| derives_order(program, *item)),
        _ => false,
    }
}
