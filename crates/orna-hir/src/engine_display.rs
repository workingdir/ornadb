//! The engine-derived `Display` (kernel-and-library.md 4.5, P5): which static types text an interpolation without a source
//! instance.
//!
//! Typeck settles an interpolated `Display.display` call on such a type to the protocol member, and the executor renders
//! the value natively. Both read this one predicate, so the two never disagree about which types derive the text.

use crate::{Program, Ty, TyKind};

/// Whether the static type `ty` has the engine-derived `Display`: the scalar kernel types `Bool`, `Int`, `Decimal` and
/// `Str`. Every other type needs an `impl Display` in its own module.
#[must_use]
pub fn derives_display(program: &Program, ty: Ty) -> bool {
    matches!(
        program.tys.kind(ty),
        Some(TyKind::Bool | TyKind::Int | TyKind::Decimal | TyKind::Str)
    )
}
