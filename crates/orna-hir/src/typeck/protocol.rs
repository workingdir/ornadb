//! Protocol member calls: the member a receiver's type selects.
//!
//! A call `receiver.name(...)` names the protocol member whose implementation
//! the receiver's type selects. The member's signature types the call, and the
//! receiver is its first argument. Protocol dispatch is static (ORNA-GENERIC-002),
//! so the selection is decided here rather than at run time.

use orna_syntax::SyntaxSpan;

use crate::diag::GENERIC_OVERLAP;
use crate::{Bound, Callee, DefId, DefKind, HirDiagnostic, Name, ParamIdx, Ty, TyKind};

use super::{BodyCheck, NotYetReason};

/// One implementation that can answer a member call.
struct Candidate {
    protocol: DefId,
    /// The protocol's type arguments, in parameter order.
    args: Box<[Ty]>,
    /// The protocol member the call names.
    member: DefId,
}

impl BodyCheck<'_> {
    /// Selects the protocol member that a receiver's type decides.
    ///
    /// `Ok(None)` means the selection was ambiguous; the conflict is recorded.
    pub(super) fn settle_protocol_call(
        &mut self,
        receiver: Ty,
        name: &Name,
        span: SyntaxSpan,
    ) -> Result<Option<Callee>, NotYetReason> {
        match self.kind_of(receiver) {
            Some(TyKind::Adt(owner, _)) => self.settle_instance_call(receiver, owner, name, span),
            Some(TyKind::Param(index)) => self.settle_bound_call(receiver, index, name, span),
            _ => Err(not_yet(name)),
        }
    }

    /// Selects among the protocol implementations a nominal type declares.
    fn settle_instance_call(
        &mut self,
        receiver: Ty,
        owner: DefId,
        name: &Name,
        span: SyntaxSpan,
    ) -> Result<Option<Callee>, NotYetReason> {
        let implementations = self
            .program
            .instances
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        let receiver_args = self.nominal_args(receiver);
        let mut candidates = Vec::new();
        for implementation in implementations {
            let DefKind::Impl(declared) = &self.program.defs[implementation].kind else {
                continue;
            };
            let Some(protocol) = declared.protocol else {
                continue;
            };
            let raw_args = declared.protocol_args.clone();
            let args: Box<[Ty]> = raw_args
                .iter()
                .map(|argument| self.substitute(*argument, &receiver_args))
                .collect();
            if let Some(member) = self.protocol_member(protocol, args.len(), name) {
                candidates.push(Candidate {
                    protocol,
                    args,
                    member,
                });
            }
        }
        if candidates.len() > 1 {
            if overlapping(&candidates) {
                self.overlap(name, span);
                return Ok(None);
            }
            return Err(NotYetReason::Unsupported(
                format!("choosing between protocol implementations of `.{name}` is not on HIR yet")
                    .into(),
            ));
        }
        let Some(candidate) = candidates.into_iter().next() else {
            return Err(not_yet(name));
        };
        let mut targs = candidate.args.into_vec();
        targs.push(receiver);
        self.protocol_callee(candidate.member, targs, span)
            .map(Some)
    }

    /// Selects the one protocol bound on a generic parameter that supplies the member.
    fn settle_bound_call(
        &mut self,
        receiver: Ty,
        index: ParamIdx,
        name: &Name,
        span: SyntaxSpan,
    ) -> Result<Option<Callee>, NotYetReason> {
        let mut found = Vec::new();
        for bound in self.bounds_of(index) {
            if let Some(member) = self.protocol_member(bound.protocol, bound.args.len(), name) {
                found.push((member, bound.args));
            }
        }
        let [(member, args)] = found.as_slice() else {
            return Err(not_yet(name));
        };
        let mut targs = args.to_vec();
        targs.push(receiver);
        self.protocol_callee(*member, targs, span).map(Some)
    }

    /// The type arguments of a nominal receiver; any other type has none.
    pub(super) fn nominal_args(&self, ty: Ty) -> Box<[Ty]> {
        match self.kind_of(ty) {
            Some(TyKind::Adt(_, arguments)) => arguments,
            _ => Box::new([]),
        }
    }

    /// The bounds a generic parameter of the body's definition declares.
    fn bounds_of(&self, index: ParamIdx) -> Vec<Bound> {
        match &self.program.defs[self.owner].kind {
            DefKind::Fn(function) => function
                .generics
                .get(index.as_u32() as usize)
                .map(|generic| generic.bounds.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The member of `protocol` called `name`, when the protocol takes `arguments` type arguments.
    fn protocol_member(&self, protocol: DefId, arguments: usize, name: &Name) -> Option<DefId> {
        let DefKind::Protocol(declared) = &self.program.defs[protocol].kind else {
            return None;
        };
        if declared.params.len() != arguments {
            return None;
        }
        declared
            .members
            .iter()
            .copied()
            .find(|member| self.program.defs[*member].name == *name)
    }

    /// Names a protocol member as the callee, with its inherited generic arguments filled in.
    ///
    /// `targs` holds the protocol's arguments then the `Self` type; the member's own generics
    /// are solved by unification at the call.
    fn protocol_callee(
        &mut self,
        member: DefId,
        mut targs: Vec<Ty>,
        span: SyntaxSpan,
    ) -> Result<Callee, NotYetReason> {
        let generics = match &self.program.defs[member].kind {
            DefKind::Fn(function) => function.generics.len(),
            _ => {
                return Err(NotYetReason::Unsupported(
                    "the protocol member is not a function".into(),
                ));
            }
        };
        if generics < targs.len() {
            return Err(NotYetReason::Unsupported(
                "the protocol member's generics do not match its protocol".into(),
            ));
        }
        while targs.len() < generics {
            targs.push(self.infer.new_var(&mut self.program.tys, span.clone()));
        }
        Ok(Callee::Source {
            def: member,
            targs: targs.into_boxed_slice(),
        })
    }

    /// Records an overlap between two protocol implementations (ORNA-GENERIC-014).
    fn overlap(&mut self, name: &Name, span: SyntaxSpan) {
        self.conflicts += 1;
        self.diagnostics.push(HirDiagnostic::new(
            GENERIC_OVERLAP,
            format!("two protocol implementations supply `.{name}` on this type"),
            span,
        ));
    }
}

/// Whether two candidates are the same protocol at the same arguments.
fn overlapping(candidates: &[Candidate]) -> bool {
    candidates.iter().enumerate().any(|(index, left)| {
        candidates[index + 1..]
            .iter()
            .any(|right| right.protocol == left.protocol && right.args == left.args)
    })
}

fn not_yet(name: &Name) -> NotYetReason {
    NotYetReason::Unsupported(
        format!("no protocol member `.{name}` applies to this receiver type yet").into(),
    )
}
