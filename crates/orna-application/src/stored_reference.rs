//! Stored table references at the source-mutation boundary (ORNA-REF-001,
//! ORNA-REF-002, ORNA-MUT-007, ORNA-MUT-008).
//!
//! The reference rules live in `orna_project::ReferenceCatalogue`. This module
//! connects them to the handler's activation state: a written row's target rows
//! become stored references (so the row the activation stages already holds the
//! form the committed descriptor lowers and publication validates), a stored
//! reference resolves through the activation's own rows, and the activation's
//! referential constraints are validated before it is staged for commit.

use std::collections::BTreeMap;

use orna_check::TableSchema;
use orna_evaluator::EvaluationError;
use orna_project::{CandidateState, ReferenceCatalogue, ReferenceError};
use orna_runtime::TableObjectId;
use orna_value::{CanonicalValue, OvbRaw};

use super::{ApplicationError, SourceMutationEffectHandler, encoded_table_key};

/// The reference catalogue of `tables` bound to the committed database
/// `database_id`. `None` when no table holds a reference or no table has a
/// committed identity.
pub(crate) fn stored_reference_catalogue(
    database_id: [u8; 16],
    tables: &BTreeMap<String, TableSchema>,
    ids: &BTreeMap<String, TableObjectId>,
) -> Result<Option<ReferenceCatalogue>, ApplicationError> {
    ReferenceCatalogue::new(database_id, tables, ids)
        .map_err(|_| ApplicationError::InvalidTableMetadata)
}

fn reference_failure(error: ReferenceError) -> EvaluationError {
    SourceMutationEffectHandler::effect_error(error.code())
}

/// The evaluator-form key of a row whose primary key has `components`.
fn key_bytes(components: &[OvbRaw]) -> Result<Vec<u8>, ReferenceError> {
    let values = components
        .iter()
        .map(|component| CanonicalValue::new(component.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ReferenceError::InvalidKey)?;
    encoded_table_key(&values).map_err(|_| ReferenceError::InvalidKey)
}

impl SourceMutationEffectHandler {
    /// Attaches the reference catalogue of the activation's committed database.
    /// Without one, a table-typed field is left as the evaluator produced it.
    #[must_use]
    pub(crate) fn with_stored_references(mut self, references: Option<ReferenceCatalogue>) -> Self {
        self.stored_references = references;
        self
    }

    /// Replaces the target rows that `row`, a row of `table`, binds to its
    /// reference columns with the stored references to them (ORNA-REF-001).
    pub(crate) fn bind_stored_references(
        &self,
        table: &str,
        row: CanonicalValue,
    ) -> Result<CanonicalValue, EvaluationError> {
        let Some(references) = &self.stored_references else {
            return Ok(row);
        };
        match references
            .bind_row(table, row.raw())
            .map_err(reference_failure)?
        {
            Some(bound) => {
                CanonicalValue::new(bound).map_err(|_| reference_failure(ReferenceError::NotARow))
            }
            None => Ok(row),
        }
    }

    /// Validates the activation's referential constraints against its candidate
    /// state, the snapshot's rows with this activation's writes applied
    /// (ORNA-MUT-007, ORNA-MUT-008).
    pub(crate) fn validate_stored_references(&self) -> Result<(), ApplicationError> {
        let Some(references) = &self.stored_references else {
            return Ok(());
        };
        references
            .validate(&self.touched_keys()?, &mut ActivationRows { handler: self })
            .map_err(|error| ApplicationError::EffectRejected(error.code().to_owned()))
    }

    /// The primary key of every row this activation wrote, removed or re-keyed.
    fn touched_keys(&self) -> Result<Vec<(String, Vec<OvbRaw>)>, ApplicationError> {
        let unreadable =
            || ApplicationError::EffectRejected(ReferenceError::State.code().to_owned());
        let mut touched = Vec::new();
        for (table, rows) in &self.overlay {
            let schema = self.tables.get(table).ok_or_else(unreadable)?;
            for key in rows.keys() {
                let key = CanonicalValue::decode(key).map_err(|_| unreadable())?;
                let components = self
                    .key_components(schema, &key)
                    .map_err(|_| unreadable())?;
                touched.push((
                    table.clone(),
                    components
                        .iter()
                        .map(|component| component.raw().clone())
                        .collect(),
                ));
            }
        }
        Ok(touched)
    }

    /// The row the stored reference `reference` names in the activation's own
    /// snapshot (ORNA-REF-002), or `None` when no live row answers to it.
    pub(crate) fn resolve_stored_reference(
        &self,
        reference: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let references = self
            .stored_references
            .as_ref()
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-UNSUPPORTED"))?;
        let identity = references
            .identity(reference.raw())
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-VALUE"))?;
        let key = key_bytes(&identity.key).map_err(reference_failure)?;
        self.current_row(identity.table, &key)
    }
}

/// The activation's candidate rows: the snapshot's with this activation's
/// writes, and its removals, applied.
struct ActivationRows<'a> {
    handler: &'a SourceMutationEffectHandler,
}

impl CandidateState for ActivationRows<'_> {
    fn row(&mut self, table: &str, key: &[OvbRaw]) -> Result<Option<OvbRaw>, ReferenceError> {
        let key = key_bytes(key)?;
        if let Some(row) = self
            .handler
            .overlay
            .get(table)
            .and_then(|rows| rows.get(&key))
        {
            return Ok(row.as_ref().map(|row| row.raw().clone()));
        }
        // Without the snapshot an untouched row cannot be seen, so its
        // existence is unproven and the activation is refused.
        let captured = self
            .handler
            .captured_rows
            .as_ref()
            .ok_or(ReferenceError::State)?;
        Ok(captured
            .get(table)
            .and_then(|rows| rows.get(&key))
            .map(|row| row.raw().clone()))
    }

    fn rows(&mut self, table: &str) -> Result<Vec<OvbRaw>, ReferenceError> {
        let captured = self
            .handler
            .captured_rows
            .as_ref()
            .ok_or(ReferenceError::State)?;
        let mut merged: BTreeMap<&Vec<u8>, Option<&CanonicalValue>> = captured
            .get(table)
            .into_iter()
            .flatten()
            .map(|(key, row)| (key, Some(row)))
            .collect();
        for (key, row) in self.handler.overlay.get(table).into_iter().flatten() {
            merged.insert(key, row.as_ref());
        }
        Ok(merged
            .into_values()
            .flatten()
            .map(|row| row.raw().clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_check::{ModuleInput, analyze};
    use orna_evaluator::EffectHandler;
    use orna_project::admitted_table_schemas;
    use orna_runtime::RuntimeTableRows;
    use orna_syntax::parse_expression;
    use orna_value::Value;

    const DATABASE: [u8; 16] = [7; 16];
    const OWNER: [u8; 16] = [1; 16];
    const VEHICLE: [u8; 16] = [2; 16];
    const SOURCE: &str =
        "pub table Owner(id: Str) { name: Str, }\npub table Vehicle(id: Str) { owner: Owner, }\n";

    fn text(value: &str) -> OvbRaw {
        OvbRaw::Text(value.to_owned())
    }

    fn value(raw: OvbRaw) -> CanonicalValue {
        CanonicalValue::new(raw).expect("a canonical test value")
    }

    fn record(fields: Vec<(&str, OvbRaw)>) -> CanonicalValue {
        let mut fields = fields;
        fields.sort_by(|(left, _), (right, _)| {
            left.len().cmp(&right.len()).then_with(|| left.cmp(right))
        });
        value(OvbRaw::Map(
            fields
                .into_iter()
                .map(|(name, field)| (text(name), field))
                .collect(),
        ))
    }

    fn field(row: &CanonicalValue, name: &str) -> OvbRaw {
        let OvbRaw::Map(entries) = row.raw() else {
            panic!("a row is a record");
        };
        entries
            .iter()
            .find(|(candidate, _)| candidate == &text(name))
            .map(|(_, field)| field.clone())
            .unwrap_or_else(|| panic!("the row has a `{name}` field"))
    }

    fn schemas() -> BTreeMap<String, TableSchema> {
        let analysis = analyze(&[ModuleInput::new("main.orna", SOURCE)]);
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        analysis
            .modules
            .values()
            .flat_map(admitted_table_schemas)
            .collect()
    }

    fn ids() -> BTreeMap<String, TableObjectId> {
        BTreeMap::from([
            ("Owner".to_owned(), TableObjectId::new(OWNER)),
            ("Vehicle".to_owned(), TableObjectId::new(VEHICLE)),
        ])
    }

    fn reference(key: &str) -> CanonicalValue {
        Value::table_reference(DATABASE, OWNER, text(key)).expect("a text key is permitted")
    }

    fn key(id: &str) -> CanonicalValue {
        value(text(id))
    }

    fn owner_row(id: &str) -> CanonicalValue {
        record(vec![("id", text(id)), ("name", text("Name"))])
    }

    fn catalogue() -> Option<ReferenceCatalogue> {
        stored_reference_catalogue(DATABASE, &schemas(), &ids()).expect("the catalogue builds")
    }

    /// A handler over a snapshot holding the Owner rows `owners` and the
    /// Vehicle rows `vehicles`, each `(id, owner key)`.
    fn handler(owners: &[&str], vehicles: &[(&str, &str)]) -> SourceMutationEffectHandler {
        let encode = |row: &CanonicalValue| row.encode().expect("a canonical row");
        let rows = RuntimeTableRows::from([
            (
                "Owner".to_owned(),
                owners
                    .iter()
                    .map(|id| (encode(&key(id)), encode(&owner_row(id))))
                    .collect(),
            ),
            (
                "Vehicle".to_owned(),
                vehicles
                    .iter()
                    .map(|(id, owner)| {
                        let row = record(vec![
                            ("id", text(id)),
                            ("owner", reference(owner).raw().clone()),
                        ]);
                        (encode(&key(id)), encode(&row))
                    })
                    .collect(),
            ),
        ]);
        SourceMutationEffectHandler::with_table_rows_and_table_object_ids(schemas(), rows, ids())
            .expect("a handler over the snapshot")
            .with_stored_references(catalogue())
    }

    fn call(
        handler: &mut SourceMutationEffectHandler,
        callee: &str,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::handle(handler, &parse_expression(callee).value, arguments)
    }

    fn dangling() -> ApplicationError {
        ApplicationError::EffectRejected("ORNA-REF-DANGLING".to_owned())
    }

    #[test]
    fn a_row_bound_to_a_table_field_is_staged_as_its_stored_reference() {
        let mut handler = handler(&["ada"], &[]);
        let bound = record(vec![
            ("id", text("v1")),
            ("owner", owner_row("ada").raw().clone()),
        ]);
        let staged = call(&mut handler, "Vehicle.insert", &[bound])
            .expect("the insert is admitted")
            .expect("an insert returns its row");

        assert_eq!(
            field(&staged, "owner"),
            reference("ada").raw().clone(),
            "the row the activation holds names the owner, it does not copy it"
        );
        let mutations = handler.into_mutations().expect("the activation validates");
        let stored = CanonicalValue::decode(mutations[0].value().expect("an insert has a row"))
            .expect("a canonical row");
        assert_eq!(field(&stored, "owner"), reference("ada").raw().clone());
    }

    #[test]
    fn a_patch_that_binds_a_row_is_staged_as_its_stored_reference() {
        let mut handler = handler(&["ada", "bob"], &[("v1", "ada")]);
        let patch = record(vec![("owner", owner_row("bob").raw().clone())]);
        let updated = call(&mut handler, "Vehicle.update", &[key("v1"), patch])
            .expect("the update is admitted")
            .expect("an update returns its row");
        assert_eq!(field(&updated, "owner"), reference("bob").raw().clone());
        assert!(handler.into_mutations().is_ok());
    }

    #[test]
    fn an_activation_that_references_a_missing_row_is_refused() {
        let mut handler = handler(&[], &[]);
        let ghost = record(vec![
            ("id", text("v1")),
            ("owner", owner_row("ghost").raw().clone()),
        ]);
        call(&mut handler, "Vehicle.insert", &[ghost]).expect("the insert is admitted");
        assert_eq!(
            handler
                .into_mutations()
                .expect_err("the reference names no row"),
            dangling()
        );
    }

    #[test]
    fn deleting_a_referenced_row_is_restricted_until_its_dependents_move() {
        let mut restricted = handler(&["ada", "bob"], &[("v1", "ada")]);
        call(&mut restricted, "Owner.delete", &[key("ada")]).expect("the delete is admitted");
        assert_eq!(
            restricted.into_mutations().expect_err("v1 still names ada"),
            dangling()
        );

        // ORNA-MUT-008: the caller updates the dependent in the same activation.
        let mut moved = handler(&["ada", "bob"], &[("v1", "ada")]);
        let patch = record(vec![("owner", owner_row("bob").raw().clone())]);
        call(&mut moved, "Vehicle.update", &[key("v1"), patch]).expect("the update is admitted");
        call(&mut moved, "Owner.delete", &[key("ada")]).expect("the delete is admitted");
        assert!(moved.into_mutations().is_ok());

        // An unreferenced row deletes freely.
        let mut free = handler(&["ada", "bob"], &[("v1", "ada")]);
        call(&mut free, "Owner.delete", &[key("bob")]).expect("the delete is admitted");
        assert!(free.into_mutations().is_ok());
    }

    #[test]
    fn rekeying_a_referenced_row_needs_its_dependents_updated_in_the_same_activation() {
        let mut alone = handler(&["ada"], &[("v1", "ada")]);
        call(&mut alone, "Owner.rekey", &[key("ada"), key("ada2")])
            .expect("the re-key is admitted");
        assert_eq!(
            alone
                .into_mutations()
                .expect_err("v1 still names the old key"),
            dangling()
        );

        // ORNA-MUT-007: no invalid intermediate state is observable, so the
        // re-key and the dependent's update may share the activation.
        let mut together = handler(&["ada"], &[("v1", "ada")]);
        let moved = call(&mut together, "Owner.rekey", &[key("ada"), key("ada2")])
            .expect("the re-key is admitted")
            .expect("a re-key returns the moved row");
        let patch = record(vec![("owner", moved.raw().clone())]);
        call(&mut together, "Vehicle.update", &[key("v1"), patch]).expect("the update is admitted");
        assert!(together.into_mutations().is_ok());
    }

    #[test]
    fn a_reference_resolves_through_the_activations_own_rows() {
        let mut handler = handler(&["ada"], &[]);
        let committed = handler
            .resolve_stored_reference(&reference("ada"))
            .expect("the reference resolves")
            .expect("the snapshot holds the row");
        assert_eq!(field(&committed, "name"), text("Name"));

        // A write earlier in the activation is what the reference reads, and a
        // delete makes the reference name no row.
        let patch = record(vec![("name", text("Renamed"))]);
        call(&mut handler, "Owner.update", &[key("ada"), patch]).expect("the update is admitted");
        let renamed = handler
            .resolve_stored_reference(&reference("ada"))
            .expect("the reference resolves")
            .expect("the row is live");
        assert_eq!(field(&renamed, "name"), text("Renamed"));
        call(&mut handler, "Owner.delete", &[key("ada")]).expect("the delete is admitted");
        assert_eq!(
            handler.resolve_stored_reference(&reference("ada")),
            Ok(None)
        );

        // Another database's reference is refused, never read as an absent row.
        let foreign = Value::table_reference([8; 16], OWNER, text("ada")).expect("a reference");
        assert!(handler.resolve_stored_reference(&foreign).is_err());
    }

    #[test]
    fn existence_cannot_be_proven_without_the_activations_snapshot() {
        let mut handler = SourceMutationEffectHandler::new_with_table_object_ids(schemas(), ids())
            .expect("a handler without a snapshot")
            .with_stored_references(catalogue());
        let bound = record(vec![
            ("id", text("v1")),
            ("owner", owner_row("ada").raw().clone()),
        ]);
        call(&mut handler, "Vehicle.insert", &[bound]).expect("the insert is admitted");
        assert_eq!(
            handler.into_mutations().expect_err("ada is unproven"),
            ApplicationError::EffectRejected("ORNA-EVAL-TABLE-SNAPSHOT".to_owned())
        );
    }

    #[test]
    fn without_a_catalogue_a_table_typed_field_is_left_as_the_evaluator_made_it() {
        let mut handler = SourceMutationEffectHandler::new(schemas());
        let bound = record(vec![
            ("id", text("v1")),
            ("owner", owner_row("ada").raw().clone()),
        ]);
        let staged = call(&mut handler, "Vehicle.insert", std::slice::from_ref(&bound))
            .expect("a name-only admission stages the row")
            .expect("an insert returns its row");
        assert_eq!(
            staged, bound,
            "nothing could name the row, so nothing is bound"
        );
    }
}
