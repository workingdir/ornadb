//! Phase 3 signature helpers: generic bounds, parameter types and results.
//!
//! Split out of `items.rs` to keep that file under its size budget. The
//! methods are called by the signature builders in `items.rs`.

use orna_syntax::{Expr, FunctionSignature, GenericParameter, Parameter, TypeExpr};

use crate::{BodyId, DefId, DefKind, GenericParam, ModuleId, ParamDef};

use super::declare::Builder;
use super::items::{generic_names, parameter_name};

impl Builder {
    /// The generic parameters of a declaration, with resolved protocol bounds.
    ///
    /// `names` is every type name in scope for the bounds' arguments: the
    /// enclosing parameters first, then `generics`.
    pub(super) fn build_generics(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        generics: &[GenericParameter],
    ) -> Vec<GenericParam> {
        generics
            .iter()
            .map(|generic| {
                let bounds = generic
                    .bounds
                    .iter()
                    .filter_map(|bound| self.build_bound(module, names, bound))
                    .collect();
                GenericParam {
                    name: crate::Name::new(&generic.name),
                    bounds,
                }
            })
            .collect()
    }

    /// Resolves one protocol bound and lowers its type arguments.
    fn build_bound(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        bound: &TypeExpr,
    ) -> Option<crate::Bound> {
        let TypeExpr::Name {
            path, arguments, ..
        } = bound
        else {
            return None;
        };
        if path.is_empty() {
            return None;
        }
        let segments = path
            .iter()
            .map(|segment| crate::Name::new(segment))
            .collect::<Vec<_>>();
        let super::super::resolve::PathKind::Def(protocol) = self.resolve_path(module, &segments)
        else {
            return None;
        };
        if !self.def_is_protocol(protocol) {
            return None;
        }
        let args = arguments
            .iter()
            .map(|argument| super::types::lower_type(self, module, names, argument))
            .collect::<Vec<_>>();
        Some(crate::Bound {
            protocol,
            args: args.into_boxed_slice(),
        })
    }

    /// The parameters of a function, with default bodies.
    pub(super) fn build_parameters(
        &mut self,
        def: DefId,
        module: ModuleId,
        parameters: &[Parameter],
        receiver: Option<crate::Ty>,
        names: &[crate::Name],
    ) -> (Vec<ParamDef>, Vec<(BodyId, Expr)>) {
        let mut built = Vec::with_capacity(parameters.len());
        let mut defaults = Vec::new();
        for (index, parameter) in parameters.iter().enumerate() {
            let name = parameter_name(&parameter.pattern, index);
            let is_receiver = index == 0 && name == "self";
            let (ty, written) = match (&parameter.annotation, receiver) {
                (Some(annotation), _) => (
                    super::types::lower_type(self, module, names, annotation),
                    true,
                ),
                (None, Some(receiver)) if is_receiver => (receiver, true),
                (None, _) => (self.program.tys.error(), false),
            };
            let default = parameter.default.as_ref().map(|expr| {
                let body = self.allocate_body(def);
                defaults.push((body, expr.clone()));
                body
            });
            built.push(ParamDef {
                name: crate::Name::new(&name),
                ty,
                written,
                default,
                binder: None,
            });
        }
        (built, defaults)
    }

    /// The declared result type.
    pub(super) fn build_result(
        &mut self,
        module: ModuleId,
        names: &[crate::Name],
        result: Option<&TypeExpr>,
    ) -> crate::Ty {
        match result {
            Some(ty) => super::types::lower_type(self, module, names, ty),
            None => self.program.tys.error(),
        }
    }

    /// A signature with no body, for a protocol member. Each member is generic over the protocol's parameters,
    /// then `Self` (bounded by the protocol, ORNA-GENERIC-002/003), then its own generics.
    pub(super) fn signature_only(
        &mut self,
        module: ModuleId,
        signature: &FunctionSignature,
        inherited: &[GenericParam],
    ) -> DefKind {
        let mut names = inherited
            .iter()
            .map(|generic| generic.name.clone())
            .collect::<Vec<_>>();
        names.extend(generic_names(&signature.generics));
        let mut generics = inherited.to_vec();
        generics.extend(self.build_generics(module, &names, &signature.generics));
        let ret = self.build_result(module, &names, signature.result.as_ref());
        // The protocol's `Self` parameter types an unannotated receiver `self`, as in an implementation (D1).
        let self_ty = names
            .iter()
            .position(|name| *name == crate::Name::new("Self"))
            .map(|index| {
                let index = u32::try_from(index).expect("generic parameters fit u32");
                self.program
                    .tys
                    .intern(crate::TyKind::Param(crate::ParamIdx::new(index)))
            });
        let params = signature
            .parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let name = parameter_name(&parameter.pattern, index);
                let is_receiver = index == 0 && name == "self";
                let written = parameter.annotation.is_some() || is_receiver;
                let ty = match (&parameter.annotation, self_ty) {
                    (Some(annotation), _) => {
                        super::types::lower_type(self, module, &names, annotation)
                    }
                    (None, Some(self_ty)) if is_receiver => self_ty,
                    (None, _) => self.program.tys.error(),
                };
                ParamDef {
                    name: crate::Name::new(&name),
                    ty,
                    written,
                    default: None,
                    binder: None,
                }
            })
            .collect();
        DefKind::Fn(crate::FnDef {
            generics,
            params,
            ret,
            effects: crate::EffectSet::empty(),
            body: crate::FnBody::Signature,
        })
    }
}
