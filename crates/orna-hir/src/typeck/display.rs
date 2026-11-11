//! String interpolation (kernel-and-library.md 4.5, P5): each interpolated expression becomes a call of the `Display`
//! protocol member on its own value. The engine derives the instance for the scalar kernel types (`engine_display`);
//! any other type needs an `impl Display` in its own module. An interpolation of a type with no instance is a
//! diagnostic that names the type.

use orna_syntax::SyntaxSpan;

use crate::diag::GENERIC_OVERLAP;
use crate::{Arg, ArgValue, Body, Call, Callee, DefId, DefKind, Expr, ExprId, ExprKind, InterpPart, ModulePath, Name, ParamIdx, Ty, TyKind};

use super::{BodyCheck, NotYetReason};

impl BodyCheck<'_> {
    /// Types an interpolation. Each expression part is rewritten to a `Display.display` call on its value, so the
    /// executor runs the instance the value's type selects. The interpolation itself is `Str`.
    pub(super) fn interpolation_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        parts: &[InterpPart],
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let mut rewritten = Vec::with_capacity(parts.len());
        for part in parts {
            rewritten.push(match part {
                InterpPart::Text(text) => InterpPart::Text(text.clone()),
                InterpPart::Expr(value) => {
                    let ty = self.infer_expr(body, *value, None)?;
                    let ty = self.resolved(ty);
                    InterpPart::Expr(self.display_part(body, id, *value, ty, span.clone())?)
                }
            });
        }
        body.exprs[id].kind = ExprKind::Interp(rewritten.into_boxed_slice());
        Ok(self.intern(TyKind::Str))
    }

    /// The expression that displays `value` of type `ty`: a `Display.display` call on it. When no single instance
    /// applies, the conflict is recorded and `value` is kept, since the body cannot run anyway.
    fn display_part(
        &mut self,
        body: &mut Body,
        id: ExprId,
        value: ExprId,
        ty: Ty,
        span: SyntaxSpan,
    ) -> Result<ExprId, NotYetReason> {
        let Some(callee) = self.display_callee(ty, span.clone())? else {
            return Ok(value);
        };
        let node_span = body.exprs[id].span.clone();
        let ty_str = self.intern(TyKind::Str);
        Ok(body.exprs.push(Expr {
            kind: ExprKind::Call(Box::new(Call {
                callee,
                callee_span: span,
                args: Box::new([Arg {
                    slot: ParamIdx::new(0),
                    value: ArgValue::Expr(value),
                }]),
                piped: false,
            })),
            ty: ty_str,
            span: node_span,
            // The legacy evaluator rendered an interpolated scalar without a call step, so this call adds none.
            weight: 0,
        }))
    }

    /// The `Display.display` member that renders a value of type `ty`. `Ok(None)` means no single instance applies,
    /// and the conflict is recorded.
    fn display_callee(&mut self, ty: Ty, span: SyntaxSpan) -> Result<Option<Callee>, NotYetReason> {
        let name = Name::new("display");
        let protocol = self
            .core_display_protocol()
            .ok_or_else(|| NotYetReason::Unsupported("the `core.display` protocol is not declared".into()))?;
        let member = self.protocol_member(protocol, 0, &name).ok_or_else(|| {
            NotYetReason::Unsupported("the `Display` protocol has no `display` member".into())
        })?;
        if crate::engine_display::derives_display(self.program, ty) {
            return self.protocol_callee(member, vec![ty], span).map(Some);
        }
        let implementations = match self.kind_of(ty) {
            Some(TyKind::Adt(owner, _)) => self.display_implementations(owner, protocol),
            Some(TyKind::Param(index)) => self
                .bounds_of(index)
                .iter()
                .filter(|bound| bound.protocol == protocol)
                .count(),
            Some(TyKind::Infer(_)) | None => {
                return Err(NotYetReason::Unsupported(
                    "interpolation of a value whose type is not inferred yet".into(),
                ));
            }
            Some(_) => 0,
        };
        match implementations {
            0 => {
                let shown = crate::print::print_type(self.program, ty);
                self.conflict(
                    format!("no `Display` instance for {shown}: interpolating it needs `impl Display`"),
                    span,
                );
                Ok(None)
            }
            1 => self.protocol_callee(member, vec![ty], span).map(Some),
            _ => {
                self.conflicts += 1;
                self.diagnostics.push(crate::HirDiagnostic::new(
                    GENERIC_OVERLAP,
                    format!(
                        "two `Display` instances supply the interpolation of {}",
                        crate::print::print_type(self.program, ty)
                    ),
                    span,
                ));
                Ok(None)
            }
        }
    }

    /// How many `Display` implementations a nominal owner declares.
    fn display_implementations(&self, owner: DefId, protocol: DefId) -> usize {
        self.program.instances.get(&owner).map_or(0, |implementations| {
            implementations
                .iter()
                .filter(|implementation| {
                    matches!(
                        &self.program.defs[**implementation].kind,
                        DefKind::Impl(declared) if declared.protocol == Some(protocol)
                    )
                })
                .count()
        })
    }

    /// The `Display` protocol, exported by the embedded `core.display` module.
    fn core_display_protocol(&self) -> Option<DefId> {
        let path = ModulePath::new([Name::new("core"), Name::new("display")]);
        let module = self.program.modules.iter().find(|module| module.path == path)?;
        module.exports.get(&Name::new("Display")).copied()
    }
}
