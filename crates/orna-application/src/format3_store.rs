//! Format-3 store relations for a staged source.
//!
//! `orna commit` admits every table the staged source declares into the staged
//! `.orna/store`. Collecting those relations needs the loaded project's table
//! schemas together with the analysed declarations, so it is application
//! orchestration; the repository admits whatever relations it is handed.

use std::collections::BTreeMap;

use orna_project::{LoadedProject, ObjectIdentityManifest, ProjectLoadError};
use orna_repository::relation_admission::StagedRelation;
use orna_semantic::{Analysis, SymbolKind};

/// The relations one staged source admits, in deterministic module and table
/// name order, with the source table name behind each relation.
#[derive(Debug, Eq, PartialEq)]
pub struct StagedRelations {
    relations: Vec<StagedRelation>,
    table_names: BTreeMap<[u8; 16], String>,
}

impl StagedRelations {
    /// The relations to admit into the staged store.
    #[must_use]
    pub fn relations(&self) -> &[StagedRelation] {
        &self.relations
    }

    /// The table's key path behind `relation_id`, for relations that came from
    /// a declared table: its namespace segments and name joined by `/`, the
    /// form a legacy editable table's directory takes (`contacts/Contact`).
    #[must_use]
    pub fn table_name(&self, relation_id: &[u8; 16]) -> Option<&str> {
        self.table_names.get(relation_id).map(String::as_str)
    }
}

/// A declared table whose format-3 schema could not be built from the staged
/// source.
#[derive(Debug)]
pub struct Format3TableSchemaError {
    table: String,
    error: ProjectLoadError,
}

impl Format3TableSchemaError {
    /// The declared table name whose schema failed.
    #[must_use]
    pub fn table(&self) -> &str {
        &self.table
    }

    /// The loader failure for [`Self::table`].
    #[must_use]
    pub const fn error(&self) -> &ProjectLoadError {
        &self.error
    }
}

/// Collects the store relations of every admitted table in `analysis`.
///
/// A symbol is admitted when it is a table whose schema carries an admission
/// policy. Each admitted table's relation id and storage descriptor are built
/// in `database_id` from the committed object `identities`. A table with an
/// automatic key is marked as such, and each carries the introduced values of
/// its constant defaults (ORNA-FIELD-005).
pub fn build_format3_store_relations(
    project: &LoadedProject,
    analysis: &Analysis,
    identities: &ObjectIdentityManifest,
    database_id: [u8; 16],
) -> Result<StagedRelations, Format3TableSchemaError> {
    let mut relations = Vec::new();
    let mut table_names = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        let module_name = namespace.display();
        for (name, symbol) in &module.symbols {
            let Some(admission) = symbol
                .table_schema
                .as_ref()
                .and_then(|schema| schema.admission.as_ref())
                .filter(|_| symbol.kind == SymbolKind::Table)
            else {
                continue;
            };
            let table = project
                .source_table_schema_in_database(identities, database_id, &module_name, name)
                .map_err(|error| Format3TableSchemaError {
                    table: name.clone(),
                    error,
                })?;
            let key_path = if namespace.0.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", namespace.0.join("/"))
            };
            table_names.insert(*table.table_object_id(), key_path);
            let relation =
                StagedRelation::new(*table.table_object_id(), table.descriptor().clone())
                    .with_introductions(project.constant_default_stored_values(&table));
            relations.push(if admission.automatic_key {
                relation.with_automatic_key()
            } else {
                relation
            });
        }
    }
    Ok(StagedRelations {
        relations,
        table_names,
    })
}

#[cfg(test)]
impl StagedRelations {
    /// Declared relations with their source table names, for tests of the row
    /// mapping.
    pub(crate) fn for_tests(
        relations: Vec<StagedRelation>,
        table_names: Vec<([u8; 16], &str)>,
    ) -> Self {
        Self {
            relations,
            table_names: table_names
                .into_iter()
                .map(|(id, name)| (id, name.to_owned()))
                .collect(),
        }
    }
}
