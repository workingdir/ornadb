//! Rule families ported from the old checker onto HIR (`docs/design/hir.md`
//! section 3.1, stage H3b).
//!
//! The pass runs after the effect pass, so every function's effect set is
//! final when an assertion is judged. It covers the namespace rules (S002,
//! S003, E1008, E1009) and the owner-pipe, effect and scope rules of
//! assertions (A091-002, A091-003, A091-007, A091-012). Only source modules
//! (`ModuleOrigin::Project`) are checked: `core`, `sys` and the pinned `std`
//! are implementation declarations.
//!
//! Path validation (S001) runs before lowering, when the caller derives a
//! module path from its file name, so it has no HIR form here. Assertions on
//! types need the type pass and stay "not yet on HIR" (A091-004).

mod assertions;
mod namespace;

use crate::{HirDiagnostic, ModuleId, Program};

/// Records every rule diagnostic on the module it concerns.
pub(crate) fn run(program: &mut Program) {
    let mut found = namespace::check(program);
    found.extend(assertions::check(program));
    for (module, diagnostic) in found {
        program.modules[module].diagnostics.push(diagnostic);
    }
}

/// One rule diagnostic and the module it is recorded on.
pub(super) type Found = (ModuleId, HirDiagnostic);
