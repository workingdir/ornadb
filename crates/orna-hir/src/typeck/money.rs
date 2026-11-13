//! Money operator and construction checks (hir-host.md B5): `E5003`, `E5004` and `E5103` are decided here, before the
//! generic no-instance diagnostic, so the published code stays kernel-owned.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{ArgValue, BinOp, Body, DefId, HirDiagnostic, Ty};

use super::{BodyCheck, NotYetReason};

impl<'a> BodyCheck<'a> {
    /// Records the published code for a `Money` operator rejection and returns the error type, or returns `None` when
    /// the operands are not a rejected Money shape.
    pub(super) fn money_operator_rejection(
        &mut self,
        op: BinOp,
        left: Ty,
        right: Ty,
        span: SyntaxSpan,
    ) -> Option<Ty> {
        let resolved_left = self.resolved(left);
        let resolved_right = self.resolved(right);
        let code = crate::engine_codes::operator_rejection(
            self.program,
            op,
            resolved_left,
            resolved_right,
        )?;
        self.conflicts += 1;
        let message = format!(
            "the operator is not defined for {} and {}",
            crate::print::print_type(self.program, resolved_left),
            crate::print::print_type(self.program, resolved_right)
        );
        self.diagnostics
            .push(HirDiagnostic::new(code, Box::<str>::from(message), span));
        Some(self.error())
    }

    /// Types the amount of `Money<C>(value)`. A `Float` amount is the `E5004` class (hir-host.md B5); any other
    /// amount is checked against the field as `check_slot` checks it.
    pub(super) fn check_money_amount(
        &mut self,
        body: &mut Body,
        def: DefId,
        call: &crate::Call,
        field_ty: Ty,
        span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        let Some(argument) = call.args.first() else {
            return Ok(());
        };
        let ArgValue::Expr(id) = argument.value else {
            return Ok(());
        };
        let ty = self.infer_expr(body, id, None)?;
        if let Some(code) = crate::engine_codes::construction_rejection(self.program, def, ty) {
            self.conflicts += 1;
            self.diagnostics.push(HirDiagnostic::new(
                code,
                Box::<str>::from("a Money amount must be a Decimal, not a Float"),
                span,
            ));
            return Ok(());
        }
        self.coerce_at(body, id, ty, field_ty, span);
        Ok(())
    }
}
