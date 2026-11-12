//! The fields a table record must supply or may not write. A defaultless field is
//! required on insert; a computed field (`=> expr`) is read-only.

use crate::{DefId, DefKind, FieldDef, FieldInitializer, Name, Ty, TyKind};

use super::BodyCheck;

impl<'a> BodyCheck<'a> {
    /// The fields a table row literal must supply: every field without an
    /// initializer. A default (`= expr`) is allocated on insertion (ORNA-KEYDEF-001),
    /// and a computed field (`=> expr`) is read-only and never written.
    pub(super) fn table_required_fields(&self, def: DefId) -> Option<Vec<Name>> {
        let DefKind::Table(table) = &self.program.defs.get(def)?.kind else {
            return None;
        };
        Some(
            table
                .fields
                .iter()
                .filter_map(|field| {
                    let definition = &self.program.defs[*field];
                    match &definition.kind {
                        DefKind::Field(field_definition)
                            if field_definition.initializer.is_some() =>
                        {
                            None
                        }
                        _ => Some(definition.name.clone()),
                    }
                })
                .collect(),
        )
    }

    /// The fields a table row computes from other fields (`=> expr`): they are
    /// read-only, so a record may not write them. A non-table type has none.
    pub(super) fn table_computed_fields(&self, owner: Ty) -> Vec<Name> {
        let Some(TyKind::Adt(def, _)) = self.kind_of(owner) else {
            return Vec::new();
        };
        let Some(DefKind::Table(table)) = self
            .program
            .defs
            .get(def)
            .map(|definition| &definition.kind)
        else {
            return Vec::new();
        };
        table
            .fields
            .iter()
            .filter_map(|field| {
                let definition = &self.program.defs[*field];
                match &definition.kind {
                    DefKind::Field(FieldDef {
                        initializer: Some(FieldInitializer::Computed(_)),
                        ..
                    }) => Some(definition.name.clone()),
                    _ => None,
                }
            })
            .collect()
    }
}
