//! Labelled arguments of a member call, bound once its receiver's type settles the callee.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::lower::relational_params;
use crate::{Arg, ArgValue, Callee, DefKind, Name, NamedArg};

use super::BodyCheck;

impl<'a> BodyCheck<'a> {
    /// The parameters a settled member call declares, as `(name, has_default)`, receiver first. A table operation
    /// declares none, so any label on it is unknown.
    fn settled_parameters(&self, callee: &Callee) -> Vec<(Name, bool)> {
        match callee {
            Callee::Source { def, .. } | Callee::Native { def, .. } | Callee::Sys { def, .. } => {
                match &self.program.defs[*def].kind {
                    DefKind::Fn(function) => function
                        .params
                        .iter()
                        .map(|param| (param.name.clone(), param.default.is_some()))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            Callee::Relational { op } => relational_params(*op)
                .unwrap_or_default()
                .into_iter()
                .map(|param| (param.name, param.has_default))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Binds a settled member call's labelled arguments to its declared parameters, in the same slot space as the
    /// positional ones (the receiver, when there is one, is already slot 0). Reports ORNA-EVAL-ARGUMENT for an
    /// unknown label, a parameter supplied twice, or a required parameter left out; returns whether the call binds.
    pub(super) fn bind_named_arguments(
        &mut self,
        call: &mut crate::Call,
        named: &[NamedArg],
        span: SyntaxSpan,
    ) -> bool {
        let params = self.settled_parameters(&call.callee);
        let mut filled: std::collections::BTreeMap<usize, ArgValue> = call
            .args
            .iter()
            .map(|argument| (argument.slot.as_u32() as usize, argument.value))
            .collect();
        let mut bound = true;
        for argument in named {
            let Some(slot) = params.iter().position(|(name, _)| *name == argument.name) else {
                self.argument_error(
                    format!("no parameter is named `{}`", argument.name),
                    argument.span.clone(),
                );
                bound = false;
                continue;
            };
            if filled.contains_key(&slot) {
                self.argument_error(
                    "the same parameter is supplied twice",
                    argument.span.clone(),
                );
                bound = false;
                continue;
            }
            filled.insert(slot, ArgValue::Expr(argument.value));
        }
        for (slot, (_, has_default)) in params.iter().enumerate() {
            if filled.contains_key(&slot) {
                continue;
            }
            if *has_default {
                filled.insert(slot, ArgValue::Default);
            } else {
                self.argument_error("the call omits a required parameter", span.clone());
                bound = false;
            }
        }
        if bound {
            call.args = filled
                .into_iter()
                .map(|(slot, value)| Arg {
                    slot: crate::ParamIdx::new(u32::try_from(slot).expect("slot fits u32")),
                    value,
                })
                .collect();
        }
        bound
    }

    /// Records an argument that does not bind to its callee's parameters (ORNA-EVAL-ARGUMENT).
    fn argument_error(&mut self, message: impl Into<Box<str>>, span: SyntaxSpan) {
        self.conflicts += 1;
        self.diagnostics.push(crate::HirDiagnostic::new(
            crate::diag::ARGUMENT,
            message,
            span,
        ));
    }
}
