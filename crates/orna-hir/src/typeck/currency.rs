//! The currency bound on a function's declared types (hir-host.md B3).
//!
//! A type argument of a nominal type whose parameter is bounded by the currency protocol must implement it, as
//! `Money<GBP>` requires of `GBP`. The check reads `engine_money`, so the Q-B1 decision lives in one place.

use crate::diag::INFER_CONFLICT;
use crate::{DefId, DefKind, HirDiagnostic, Ty, engine_money, print_type};

use super::{Attempt, TypeChecker};

impl TypeChecker<'_> {
    /// The failed attempt for a function whose parameter or result type applies a currency bound to a type
    /// that does not implement the currency protocol; `None` when every such bound holds.
    pub(super) fn currency_signature_failure(&self, owner: DefId) -> Option<Attempt> {
        let DefKind::Fn(function) = &self.program.defs[owner].kind else {
            return None;
        };
        let declared: Vec<Ty> = function
            .params
            .iter()
            .map(|parameter| parameter.ty)
            .chain(std::iter::once(function.ret))
            .collect();
        let span = self.program.defs[owner].span.clone();
        let mut diagnostics = Vec::new();
        for ty in declared {
            for argument in engine_money::currency_bound_failures(self.program, ty) {
                let message = format!(
                    "`{}` does not implement the `Currency` protocol that its position requires",
                    print_type(self.program, argument)
                );
                let diagnostic = HirDiagnostic::new(INFER_CONFLICT, message, span.clone());
                if !diagnostics.contains(&diagnostic) {
                    diagnostics.push(diagnostic);
                }
            }
        }
        if diagnostics.is_empty() {
            return None;
        }
        Some(Attempt::Failed {
            bodies: Vec::new(),
            conflicts: diagnostics.len(),
            diagnostics,
            detail: "a declared type applies a currency bound to a type that is not a currency"
                .into(),
        })
    }
}
