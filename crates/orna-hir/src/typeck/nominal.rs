//! Instantiation of generic nominal types. A constructor's result carries
//! fresh type arguments for the declared parameters, and a field's declared
//! type is read at the type arguments of the owner it is accessed through.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{
    Body, DefId, DefKind, ExprId, ExprKind, FieldRef, Name, Program, Ty, TyKind, Visibility,
};

use super::{BodyCheck, NotYetReason};

/// The message for a read of a private nominal field outside its type's implementations.
pub(super) const PRIVATE_READ_MESSAGE: &str =
    "a private nominal field is read outside the implementations of its type";
/// The message for a pattern binding a private nominal field outside its type's implementations.
pub(super) const PRIVATE_PATTERN_MESSAGE: &str =
    "a private nominal field is bound by a pattern outside the implementations of its type";
/// The message for a read of a refined type's base outside the implementations of that type.
pub(super) const REFINED_BASE_READ_MESSAGE: &str =
    "the base of a refined type is read outside the implementations of that type";
/// The member name that reads a refined type's base value.
pub(super) const REFINED_BASE_MEMBER: &str = "base";
/// The message for an assignment to a refined type's base, which is read-only.
pub(super) const REFINED_BASE_WRITE_MESSAGE: &str =
    "the base of a refined type is read-only and cannot be assigned";

impl BodyCheck<'_> {
    /// The declared base of refined type `def`, or `None` when `def` is not a refined type.
    pub(super) fn declared_refined_base(&self, def: DefId) -> Option<Ty> {
        self.program
            .defs
            .get(def)
            .and_then(|definition| match &definition.kind {
                DefKind::Type(definition) => definition.refined_base,
                _ => None,
            })
    }

    /// The type of `.base` read from `resolved`: the declared base of its refined type, instantiated.
    pub(super) fn refined_base_member_ty(&mut self, resolved: Ty) -> Result<Ty, NotYetReason> {
        match self.kind_of(resolved) {
            Some(TyKind::Adt(def, args)) => match self.declared_refined_base(def) {
                Some(declared) => Ok(self.substitute(declared, &args)),
                None => Err(NotYetReason::Unsupported(
                    "the field `base` is not declared by this nominal type on HIR".into(),
                )),
            },
            _ => Err(NotYetReason::Unsupported(
                "a base read does not name a refined type on HIR".into(),
            )),
        }
    }

    /// Types `.base` read through `owner`, a refined instance of `def`, and rewrites the read to
    /// [`FieldRef::Base`]. ORNA-NOMINAL-006 limits the read to nested implementations of `def`. A
    /// refined value is represented by its base value, so the read yields the receiver unchanged.
    pub(super) fn refined_base_read(
        &mut self,
        body: &mut Body,
        id: ExprId,
        base: ExprId,
        owner: Ty,
        def: DefId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let Some(declared) = self.declared_refined_base(def) else {
            return Err(NotYetReason::Unsupported(
                "the field `base` is not declared by this nominal type on HIR".into(),
            ));
        };
        if self.nominal_owners.implementation_owner.get(&self.owner) != Some(&def) {
            self.reject(crate::diag::PRIVATE_FIELD, REFINED_BASE_READ_MESSAGE, span);
            return Ok(self.error());
        }
        body.exprs[id].kind = ExprKind::Field {
            base,
            field: FieldRef::Base,
        };
        let resolved = self.resolved(owner);
        match self.kind_of(resolved) {
            Some(TyKind::Adt(_, args)) => Ok(self.substitute(declared, &args)),
            _ => Ok(declared),
        }
    }

    /// One fresh unsolved variable per declared parameter of a generic type.
    /// Enums, tables and non-generic types take no arguments.
    pub(super) fn fresh_nominal_args(&mut self, def: DefId, span: &SyntaxSpan) -> Box<[Ty]> {
        let count = nominal_param_count(self.program, def);
        (0..count)
            .map(|_| self.infer.new_var(&mut self.program.tys, span.clone()))
            .collect()
    }

    /// The declared type of `field` read through `owner`: when the owner is an
    /// instance of a generic nominal, its type arguments replace the parameters.
    pub(super) fn field_in(&mut self, owner: Ty, field: DefId) -> Result<Ty, NotYetReason> {
        let declared = self.field_definition_ty(field)?;
        let resolved = self.resolved(owner);
        match self.kind_of(resolved) {
            Some(TyKind::Adt(_, args)) => Ok(self.substitute(declared, &args)),
            _ => Ok(declared),
        }
    }

    /// The declared fields of the nominal instance `owner`, with its type
    /// arguments substituted into each field type.
    pub(super) fn nominal_fields_of(&mut self, owner: Ty) -> Option<Vec<(Name, Ty)>> {
        let resolved = self.resolved(owner);
        let Some(TyKind::Adt(def, args)) = self.kind_of(resolved) else {
            return None;
        };
        let fields = self.nominal_fields(def)?;
        Some(
            fields
                .into_iter()
                .map(|(name, ty)| (name, self.substitute(ty, &args)))
                .collect(),
        )
    }

    /// Whether this body may not read or bind `field`: the field is private to its type and
    /// the body is not inside a nested implementation of that type (NOMINAL-006).
    pub(super) fn private_field_hidden(&self, field: DefId) -> bool {
        let Some(owner) = self.nominal_owners.field_owner.get(&field) else {
            return false;
        };
        if self.program.defs[field].vis == Visibility::Public {
            return false;
        }
        self.nominal_owners.implementation_owner.get(&self.owner) != Some(owner)
    }

    /// Rejects a pattern binding private `field` outside the owner's implementations (NOMINAL-006).
    pub(super) fn reject_private_pattern_field(&mut self, field: DefId, span: SyntaxSpan) {
        if self.private_field_hidden(field) {
            self.reject(crate::diag::PRIVATE_FIELD, PRIVATE_PATTERN_MESSAGE, span);
        }
    }
}

/// The number of generic parameters a nominal definition declares. Enums, tables and non-generic types
/// declare none, and so does a definition that is not a nominal type.
pub(super) fn nominal_param_count(program: &Program, def: DefId) -> usize {
    match program.defs.get(def).map(|definition| &definition.kind) {
        Some(DefKind::Type(definition)) => definition.params.len(),
        Some(DefKind::NativeType(definition)) => definition.params.len(),
        _ => 0,
    }
}
