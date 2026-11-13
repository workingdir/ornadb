//! Instantiation of generic nominal types. A constructor's result carries
//! fresh type arguments for the declared parameters, and a field's declared
//! type is read at the type arguments of the owner it is accessed through.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{DefId, DefKind, Name, Ty, TyKind};

use super::{BodyCheck, NotYetReason};

impl BodyCheck<'_> {
    /// One fresh unsolved variable per declared parameter of a generic type.
    /// Enums, tables and non-generic types take no arguments.
    pub(super) fn fresh_nominal_args(&mut self, def: DefId, span: &SyntaxSpan) -> Box<[Ty]> {
        let count = match self
            .program
            .defs
            .get(def)
            .map(|definition| &definition.kind)
        {
            Some(DefKind::Type(definition)) => definition.params.len(),
            Some(DefKind::NativeType(definition)) => definition.params.len(),
            _ => 0,
        };
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
}
