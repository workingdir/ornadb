//! Explicit conversions (CONVERT-001..003): `Target.from(source)` selects the `From<Source>`
//! implementation nested in `Target` whose source type accepts the value. A conversion is never
//! applied implicitly (CONVERT-006); this module is reached only from a call written `Target.from(...)`.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{Arg, ArgValue, Body, Call, DefId, DefKind, ExprId, ExprKind, Name, Ty, TyKind};

use super::{BodyCheck, NotYetReason};

impl BodyCheck<'_> {
    /// The target type of a `Target.from(...)` call: the receiver, when it names a type that nests a `From`
    /// implementation (CONVERT-001). `None` for every other call, which keeps its ordinary member lookup.
    pub(super) fn explicit_conversion_target(
        &self,
        body: &Body,
        base: ExprId,
        name: &Name,
    ) -> Option<DefId> {
        if name.as_str() != "from" {
            return None;
        }
        let target = match &body.exprs[base].kind {
            ExprKind::Def(target) => *target,
            _ => return None,
        };
        self.program
            .conversions
            .contains_key(&target)
            .then_some(target)
    }

    /// Types `Target.from(source)`. The single positional argument is the source value; its type selects the
    /// nested `From<Source>` implementation that accepts it, and the call becomes that implementation's `from`
    /// member with `Self` as the target type (CONVERT-001, CONVERT-002).
    pub(super) fn conversion_call_ty(
        &mut self,
        body: &mut Body,
        call: &mut Call,
        base: ExprId,
        target: DefId,
        named: bool,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let source_expr = match (named, &call.args[..]) {
            (
                false,
                [
                    Arg {
                        value: ArgValue::Expr(id),
                        ..
                    },
                ],
            ) => *id,
            _ => {
                self.conflict(
                    "`From` takes exactly one source value, passed positionally",
                    span,
                );
                return Ok(self.error());
            }
        };
        let target_args = self.fresh_nominal_args(target, &span);
        let target_ty = self.intern(TyKind::Adt(target, target_args.clone()));
        let base_ty = self.infer_expr(body, base, None)?;
        self.unify(base_ty, target_ty, span.clone());
        let source = self.infer_expr(body, source_expr, None)?;
        let source = self.resolved(source);
        match self.kind_of(source) {
            Some(TyKind::Error) => return Ok(self.error()),
            Some(TyKind::Infer(_)) => {
                return Err(NotYetReason::Unsupported(
                    "a conversion from a source whose type is not yet known is not on HIR yet"
                        .into(),
                ));
            }
            _ => {}
        }
        let candidates = self.conversion_candidates(target, &target_args, source);
        match candidates.as_slice() {
            [] => {
                self.conflict(
                    "no `From` implementation of this type accepts this source",
                    span,
                );
                Ok(self.error())
            }
            [(accepted, member)] => {
                let (accepted, member) = (*accepted, *member);
                let Some(member) = member else {
                    self.conflict("the `From` implementation declares no `from` member", span);
                    return Ok(self.error());
                };
                self.unify(accepted, source, span.clone());
                call.callee = self.protocol_callee(member, target_args.to_vec(), span)?;
                Ok(target_ty)
            }
            _ => {
                self.overlap(
                    "two `From` implementations of this type accept this source",
                    span,
                );
                Ok(self.error())
            }
        }
    }

    /// The `From` implementations of `target` whose source type accepts `source`, each with its `from` member: the
    /// implementation's own function, which a conversion invokes by identity (CONVERT-1). The acceptance test is a
    /// trial unification on a copy of the inference state, so nothing is committed here.
    fn conversion_candidates(
        &mut self,
        target: DefId,
        target_args: &[Ty],
        source: Ty,
    ) -> Vec<(Ty, Option<DefId>)> {
        let implementations = self
            .program
            .conversions
            .get(&target)
            .cloned()
            .unwrap_or_default();
        let from = Name::new("from");
        let mut candidates = Vec::new();
        for implementation in implementations {
            let DefKind::Impl(declared) = &self.program.defs[implementation].kind else {
                continue;
            };
            let Some(raw_source) = declared.conversion_source else {
                continue;
            };
            let member = declared
                .members
                .iter()
                .copied()
                .find(|id| self.program.defs[*id].name == from);
            let accepted = self.substitute(raw_source, target_args);
            let mut trial = self.infer.clone();
            if trial.unify(&mut self.program.tys, accepted, source).is_ok() {
                candidates.push((accepted, member));
            }
        }
        candidates
    }
}
