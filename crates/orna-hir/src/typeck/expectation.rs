//! The expected type, root slot and omitted parameters of one top-level body.

use crate::diag::PUBLIC_ANNOTATION;
use crate::{BodyId, DefKind, FnBody, HirDiagnostic, Ty, Visibility};

use super::{Attempt, NotYetReason, Slot, TypeChecker};

/// The expected type, the slot to solve, and the omitted parameters of one top-level body.
pub(super) type Expectation = (Option<Ty>, Option<Slot>, Vec<usize>);

impl TypeChecker<'_> {
    /// The expected type, root slot and omitted parameters of a top-level body, or the attempt that ends it.
    pub(super) fn expectation(
        &self,
        body_id: BodyId,
        owner_kind: &DefKind,
    ) -> Result<Expectation, Attempt> {
        let owner = self.program.bodies[body_id].owner;
        let public = self.program.defs[owner].vis == Visibility::Public;
        match owner_kind {
            DefKind::Fn(function) => {
                let is_main = matches!(function.body, FnBody::Source(id) if id == body_id);
                if is_main {
                    let omitted = function
                        .params
                        .iter()
                        .enumerate()
                        .filter(|(_, parameter)| !parameter.written)
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    if public && !omitted.is_empty() {
                        let span = self.program.defs[owner].span.clone();
                        let diagnostics = omitted
                            .iter()
                            .map(|&index| {
                                HirDiagnostic::new(
                                    PUBLIC_ANNOTATION,
                                    format!(
                                        "public function parameter `{}` needs a type annotation (decision D1)",
                                        function.params[index].name.as_str()
                                    ),
                                    span.clone(),
                                )
                            })
                            .collect::<Vec<_>>();
                        return Err(Attempt::Failed {
                            bodies: Vec::new(),
                            conflicts: diagnostics.len(),
                            diagnostics,
                            detail: "a public function omits a parameter annotation".into(),
                        });
                    }
                    if omitted
                        .iter()
                        .any(|&index| function.params[index].default.is_some())
                    {
                        return Err(Attempt::NotYet(NotYetReason::UnannotatedParameter(owner)));
                    }
                    let infer = self.is_error(function.ret);
                    let expected = if infer { None } else { Some(function.ret) };
                    Ok((expected, infer.then_some(Slot::Ret), omitted))
                } else if let Some(index) = function
                    .params
                    .iter()
                    .position(|parameter| parameter.default == Some(body_id))
                {
                    let is_omitted = !function.params[index].written;
                    if is_omitted && public {
                        return Err(Attempt::NotYet(NotYetReason::UnannotatedParameter(owner)));
                    }
                    let expected = if is_omitted {
                        None
                    } else {
                        Some(function.params[index].ty)
                    };
                    Ok((
                        expected,
                        is_omitted.then_some(Slot::Param(index)),
                        Vec::new(),
                    ))
                } else {
                    Err(Attempt::NotYet(NotYetReason::Unsupported(
                        "this body is not a function body or default".into(),
                    )))
                }
            }
            DefKind::Let(binding) => {
                let declared = binding.declared.filter(|ty| !self.is_error(*ty));
                Ok((declared, None, Vec::new()))
            }
            DefKind::Assertion(_) => Ok((None, None, Vec::new())),
            _ => Err(Attempt::NotYet(NotYetReason::Unsupported(
                "this definition's body is outside the H2a subset".into(),
            ))),
        }
    }
}
