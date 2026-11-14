//! Rejection of legacy methods that write a read-only `sys` row (ORNA100-E-SYS-ADMIN-METHOD).

use crate::{Ty, TyKind};

use super::BodyCheck;

impl<'a> BodyCheck<'a> {
    /// The rejection of a legacy method that writes a read-only `sys` row, when `receiver` is that row (a
    /// `sys.Checkpoint`, `sys.Failure` or `sys.Stream`, or a relation of them). The row changes only through `sys.admin`.
    pub(super) fn legacy_sys_admin_message(
        &self,
        receiver: Ty,
        member: &str,
    ) -> Option<&'static str> {
        let receiver = match self.kind_of(receiver) {
            Some(TyKind::Relation(element)) => self.resolved(element),
            _ => receiver,
        };
        let Some(TyKind::Adt(def, _)) = self.kind_of(receiver) else {
            return None;
        };
        let definition = &self.program.defs[def];
        if self.program.modules[definition.module].origin != crate::ModuleOrigin::Sys {
            return None;
        }
        match (definition.name.as_str(), member) {
            ("Checkpoint", "reset") => Some(
                "system rows are read-only; use `sys.admin.reset_checkpoint` with compare-and-set arguments",
            ),
            ("Failure", "replay") => Some(
                "system rows are read-only; use `sys.admin.replay_failure(failure.reference, ...)`",
            ),
            ("Failure", "resolve") => Some(
                "system rows are read-only; use `sys.admin.resolve_failure(failure.reference, ...)`",
            ),
            ("Stream", "retry") => Some(
                "system rows are read-only; use `sys.admin.retry_failure` on a `sys.FailureRef`",
            ),
            ("Stream", "skip") => Some(
                "system rows are read-only; use `sys.admin.skip_failure` on a `sys.FailureRef`",
            ),
            _ => None,
        }
    }
}
