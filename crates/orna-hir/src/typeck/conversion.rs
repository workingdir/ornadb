//! Explicit conversions (CONVERT-001..003): `Target.from(source)` selects the `From<Source>`
//! implementation nested in `Target` whose source type accepts the value. A conversion is never
//! applied implicitly (CONVERT-006); this module is reached only from a call written `Target.from(...)`.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::diag::GENERIC_OVERLAP;
use crate::infer::InferCtxt;
use crate::{
    Arg, ArgValue, Body, Call, DefId, DefKind, ExprId, ExprKind, HirDiagnostic, Name, Program, Ty,
    TyKind,
};

use super::nominal::nominal_param_count;
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
            [(accepted, protocol)] => {
                let (accepted, protocol) = (*accepted, *protocol);
                self.unify(accepted, source, span.clone());
                let from = Name::new("from");
                let Some(member) = self.protocol_member(protocol, 1, &from) else {
                    self.conflict("the `From` protocol declares no `from` member", span);
                    return Ok(self.error());
                };
                call.callee = self.protocol_callee(member, vec![accepted, target_ty], span)?;
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

    /// The `From` implementations of `target` whose source type accepts `source`, each with its protocol. The
    /// acceptance test is a trial unification on a copy of the inference state, so nothing is committed here.
    fn conversion_candidates(
        &mut self,
        target: DefId,
        target_args: &[Ty],
        source: Ty,
    ) -> Vec<(Ty, DefId)> {
        let implementations = self
            .program
            .conversions
            .get(&target)
            .cloned()
            .unwrap_or_default();
        let mut candidates = Vec::new();
        for implementation in implementations {
            let (protocol, raw_source) = match &self.program.defs[implementation].kind {
                DefKind::Impl(declared) => (declared.protocol, declared.conversion_source),
                _ => continue,
            };
            let (Some(protocol), Some(raw_source)) = (protocol, raw_source) else {
                continue;
            };
            let accepted = self.substitute(raw_source, target_args);
            let mut trial = self.infer.clone();
            if trial.unify(&mut self.program.tys, accepted, source).is_ok() {
                candidates.push((accepted, protocol));
            }
        }
        candidates
    }
}

/// Refuses, at the declaration, two `From` implementations of one target whose source types overlap for some
/// instance of the target's type arguments (CONVERT-003). Identical sources are refused when they are lowered;
/// this reports the pairs that only unifying the two sources can see, once per later implementation.
pub(super) fn check_declared_overlaps(program: &mut Program) {
    let targets: Vec<(DefId, Vec<DefId>)> = program
        .conversions
        .iter()
        .map(|(target, implementations)| (*target, implementations.clone()))
        .collect();
    for (target, implementations) in targets {
        let mut sources: Vec<(DefId, Ty)> = Vec::new();
        for implementation in implementations {
            let (protocol, raw_source) = match &program.defs[implementation].kind {
                DefKind::Impl(declared) => (declared.protocol, declared.conversion_source),
                _ => continue,
            };
            let (Some(_), Some(source)) = (protocol, raw_source) else {
                continue;
            };
            sources.push((implementation, source));
        }
        for (position, &(implementation, source)) in sources.iter().enumerate() {
            let span = program.defs[implementation].span.clone();
            let mut overlaps = false;
            for &(_, earlier) in &sources[..position] {
                if earlier != source
                    && declared_sources_overlap(program, target, &span, earlier, source)
                {
                    overlaps = true;
                    break;
                }
            }
            if overlaps {
                let module = program.defs[implementation].module;
                program.modules[module].diagnostics.push(HirDiagnostic::new(
                    GENERIC_OVERLAP,
                    "two `From` implementations of this type accept overlapping sources",
                    span,
                ));
            }
        }
    }
}

/// Whether `left` and `right`, written over the parameters of the nominal `target`, accept a common source for
/// some instance of those parameters. A source that mentions an error or never type is not compared: its error
/// was reported where it was written.
fn declared_sources_overlap(
    program: &mut Program,
    target: DefId,
    span: &SyntaxSpan,
    left: Ty,
    right: Ty,
) -> bool {
    if program.tys.mentions_bottom(left) || program.tys.mentions_bottom(right) {
        return false;
    }
    let mut infer = InferCtxt::new();
    let count = nominal_param_count(program, target);
    let args: Vec<Ty> = (0..count)
        .map(|_| infer.new_var(&mut program.tys, span.clone()))
        .collect();
    let left = program.tys.substitute(left, &args);
    let right = program.tys.substitute(right, &args);
    infer.unify(&mut program.tys, left, right).is_ok()
}
