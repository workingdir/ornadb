//! Type-argument completion for generic callees: inference variables for omitted type arguments (D1/D5) and the
//! zonking of solved ones.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{Callee, DefKind};

use super::BodyCheck;

impl<'a> BodyCheck<'a> {
    /// Completes the type arguments of a generic callee (D1/D5). A call that writes none gets one inference variable
    /// per generic parameter. A call that writes some supplies the declared parameters only (GENERIC-001); the
    /// implicit parameters that omitted annotations added (ORNA-INFER-003) are inferred, so they get fresh
    /// variables. The variables are recorded in the callee, so the finishing pass replaces them with the solved
    /// types. Returns `false` after recording the conflict when the written count is not the declared count.
    pub(super) fn complete_targs(&mut self, call: &mut crate::Call, span: SyntaxSpan) -> bool {
        let (def, targs) = match &mut call.callee {
            Callee::Source { def, targs }
            | Callee::Native { def, targs }
            | Callee::Sys { def, targs } => (*def, targs),
            _ => return true,
        };
        let (count, declared) = match self.program.defs.get(def).map(|d| &d.kind) {
            Some(DefKind::Fn(function)) => (
                function.generics.len(),
                function
                    .generics
                    .iter()
                    .filter(|generic| !generic.implicit)
                    .count(),
            ),
            _ => return true,
        };
        if targs.is_empty() {
            if count > 0 {
                *targs = (0..count)
                    .map(|_| self.infer.new_var(&mut self.program.tys, span.clone()))
                    .collect();
            }
            return true;
        }
        if targs.len() != declared {
            self.conflict("the call supplies the wrong number of type arguments", span);
            return false;
        }
        let mut complete = targs.to_vec();
        while complete.len() < count {
            complete.push(self.infer.new_var(&mut self.program.tys, span.clone()));
        }
        *targs = complete.into_boxed_slice();
        true
    }

    /// Replaces solved inference variables in the type arguments of a generic callee.
    pub(super) fn zonk_callee_targs(&mut self, callee: &mut Callee) {
        if let Callee::Source { targs, .. }
        | Callee::Native { targs, .. }
        | Callee::Sys { targs, .. } = callee
        {
            for targ in targs.iter_mut() {
                *targ = self.zonk(*targ);
            }
        }
    }
}
