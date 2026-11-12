//! The typed catalogue of Orna's native intrinsic environment.
//!
//! This crate is the single declaration site for the native operations the
//! evaluator and the semantic checker share. It depends on nothing from
//! `orna-check` or `orna-evaluator`: the closed [`SigType`] language is
//! mapped to the checker's `Type` by the checker itself.
//!
//! The catalogue is proved complete against the historical
//! `STANDARD_BINDING_MODULES` table by `tests/oracle_standard_binding_modules.rs`
//! before any stage deletes that table.

mod dispatch;
mod effect;
mod id;
mod modules;
mod pin;
mod signature;

pub use dispatch::{Dispatch, Justification};
pub use effect::Effect;
pub use id::IntrinsicId;
pub use modules::{Intrinsic, Module, REEXPORTS, ROOT_NAMES, Reexport, catalogue};
pub use pin::{CURRENT_STD_PIN, PinRange};
pub use signature::{Param, SigType, Signature};

/// Returns the descriptor for `id`.
///
/// The function is total: `tests/catalogue_is_total.rs` proves that
/// `catalogue()[i].id == IntrinsicId::ALL[i]` and that every id appears exactly
/// once, so a mismatch can only be a build bug.
pub fn by_id(id: IntrinsicId) -> &'static Intrinsic {
    let index = usize::from(id.as_u16());
    let intrinsic = &catalogue()[index];
    debug_assert_eq!(
        intrinsic.id, id,
        "catalogue order disagrees with IntrinsicId::ALL"
    );
    intrinsic
}

/// Returns the declared parameters of `id`.
pub fn parameters(id: IntrinsicId) -> &'static [Param] {
    by_id(id).signature.params
}

/// Finds the intrinsic registered under `module` and `name`.
///
/// `name` may be the public export, the registered private leaf, or a
/// documented re-export spelling. A `None` result means no descriptor claims
/// that spelling.
pub fn lookup(module: Module, name: &str) -> Option<&'static Intrinsic> {
    let catalogue = catalogue();
    if let Some(intrinsic) = catalogue
        .iter()
        .find(|intrinsic| intrinsic.module == module && intrinsic.name == name)
    {
        return Some(intrinsic);
    }
    if let Some(intrinsic) = catalogue.iter().find(|intrinsic| {
        intrinsic.module == module && intrinsic.leaf.is_some_and(|leaf| leaf == name)
    }) {
        return Some(intrinsic);
    }
    REEXPORTS
        .iter()
        .find(|(reexport_module, reexport_name, _)| {
            *reexport_module == module && *reexport_name == name
        })
        .map(|(_, _, id)| by_id(*id))
}

/// Maps a root name to its `Core*` identity.
///
/// Only the intrinsic environment appears here (`spec 09:5-7`). The
/// `std.collection`/`std.query` helpers that also work at the root are reached
/// through their admitted declarations.
pub fn root_intrinsic(name: &str) -> Option<IntrinsicId> {
    ROOT_NAMES
        .iter()
        .find(|(root, _)| *root == name)
        .map(|(_, id)| *id)
}
