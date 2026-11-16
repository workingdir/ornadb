//! Instantiation of generic nominal types. A constructor's result carries
//! fresh type arguments for the declared parameters, and a field's declared
//! type is read at the type arguments of the owner it is accessed through.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{DefId, DefKind, Name, Program, Ty, TyKind, Visibility};

use super::{BodyCheck, NotYetReason};

/// The message for a read of a private nominal field outside its type's implementations.
pub(super) const PRIVATE_READ_MESSAGE: &str =
    "a private nominal field is read outside the implementations of its type";
/// The message for a pattern binding a private nominal field outside its type's implementations.
pub(super) const PRIVATE_PATTERN_MESSAGE: &str =
    "a private nominal field is bound by a pattern outside the implementations of its type";

impl BodyCheck<'_> {
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
