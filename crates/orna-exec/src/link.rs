//! The link step: binds each engine native declaration of a program to its host implementation.
//!
//! A declaration binds by its key (module path and name) against the engine registry in [`crate::natives`]. A program
//! whose recorded native ABI this engine does not implement is refused before it runs (`docs/design/hir.md` 4.5).

use std::collections::HashMap;

use orna_hir::{DefId, DefKind, ENGINE_NATIVE_ABI, FnBody, NativeBinding, NativeDecl, Program};

use crate::natives::{self, NativeFn};

/// The native declaration a definition carries, if it is one.
fn native_decl(kind: &DefKind) -> Option<&NativeDecl> {
    match kind {
        DefKind::Fn(function) => match &function.body {
            FnBody::Native(decl) => Some(decl),
            _ => None,
        },
        _ => None,
    }
}

/// Refuses a program admitted with a native ABI this engine does not implement.
///
/// # Errors
///
/// A message naming the refusal, when the program's recorded ABI is not the engine's.
pub(crate) fn check_abi(program: &Program) -> Result<(), &'static str> {
    if program.native_abi == ENGINE_NATIVE_ABI {
        Ok(())
    } else {
        Err("engine native ABI the program was admitted with is not implemented by this engine")
    }
}

/// Binds every engine native declaration the registry implements, keyed by its definition. A declaration the
/// registry does not implement stays unbound; calling it reports that it is not ported yet.
pub(crate) fn bind(program: &Program) -> HashMap<DefId, NativeFn> {
    program
        .defs
        .iter_enumerated()
        .filter_map(|(id, def)| {
            let decl = native_decl(&def.kind)?;
            if !matches!(decl.binding, NativeBinding::Engine { .. }) {
                return None;
            }
            natives::lookup(&decl.key).map(|native| (id, native))
        })
        .collect()
}
