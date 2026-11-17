//! Stored table references on the durable `orna run` path (ORNA-REF-001,
//! ORNA-REF-002, ORNA-MUT-007, ORNA-MUT-008).
//!
//! The reference rules live in `orna_project::ReferenceCatalogue`. This module
//! connects them to the transaction activation: a written row's target rows
//! become stored references before the row is staged, a stored reference
//! resolves through the activation's own rows, and the activation's
//! referential constraints are validated over its candidate state before the
//! root activation can commit.

use super::*;
use orna_project::{CandidateState, ReferenceCatalogue, ReferenceError};

fn reference_failure(error: ReferenceError) -> EvaluationError {
    transaction_error(error.code())
}

/// The activation key of the row of `table` whose primary key has `components`.
fn key_from_components(
    key_fields: &TableKeys,
    table: &str,
    components: &[OvbRaw],
) -> Result<TransactionKey, ReferenceError> {
    let schema = key_fields.get(table).ok_or(ReferenceError::Unbound)?;
    let values = components
        .iter()
        .map(|component| Value::new(component.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ReferenceError::InvalidKey)?;
    schema
        .key_from_values(&values)
        .map_err(|_| ReferenceError::InvalidKey)
}

impl TransactionalEvaluator {
    /// Attaches the stored references of the committed database this
    /// activation runs against. Without them a table-typed field is left as the
    /// evaluator produced it.
    #[must_use]
    pub(super) fn with_stored_references(
        mut self,
        references: Option<std::sync::Arc<ReferenceCatalogue>>,
    ) -> Self {
        self.stored_references = references;
        self
    }
}

impl DurableTransactionalEvaluator {
    /// Attaches the stored references of the committed database the project
    /// runs against: each table-typed field a run writes holds the identity of
    /// the row it names (ORNA-REF-001), and a run whose activation leaves a
    /// reference without its row is refused at commit (ORNA-MUT-007,
    /// ORNA-MUT-008).
    #[must_use]
    pub fn with_stored_references(mut self, references: Option<ReferenceCatalogue>) -> Self {
        self.stored_references = references.map(std::sync::Arc::new);
        self
    }
}

impl TableEffectHandler<'_, '_> {
    /// The row of `table` with its reference columns bound to stored references
    /// (ORNA-REF-001), or `None` when `row` already holds them.
    pub(super) fn bind_stored_references(
        &self,
        table: &str,
        row: &Value,
    ) -> Result<Option<Value>, EvaluationError> {
        let Some(references) = self.references else {
            return Ok(None);
        };
        references
            .bind_row(table, row.raw())
            .map_err(reference_failure)?
            .map(|bound| Value::new(bound).map_err(|_| reference_failure(ReferenceError::NotARow)))
            .transpose()
    }

    /// The row the stored reference `reference` names in this activation's own
    /// candidate rows (ORNA-REF-002), or `None` when no live row answers to it.
    pub(super) fn resolve_stored_reference(
        &self,
        reference: &Value,
    ) -> Result<Option<Value>, EvaluationError> {
        let references = self
            .references
            .ok_or_else(|| transaction_error("ORNA-EVAL-UNSUPPORTED"))?;
        let identity = references
            .identity(reference.raw())
            .ok_or_else(|| transaction_error("ORNA-EVAL-VALUE"))?;
        let key = key_from_components(self.key_fields, identity.table, &identity.key)
            .map_err(reference_failure)?;
        self.activation
            .read(&identity.table.to_owned(), &key)
            .map(Option::<&Value>::cloned)
            .map_err(|error| transaction_error(table_error_code(error)))
    }
}

/// Validates the referential constraints of the activation `mutations` wrote
/// over its candidate rows (ORNA-MUT-007, ORNA-MUT-008). The activation is
/// refused, and none of its writes commit, when a written row names a row that
/// is not live or a live reference names a row the activation removed.
pub(super) fn validate_stored_references(
    activation: &TransactionActivation<'_>,
    references: Option<&ReferenceCatalogue>,
    key_fields: &TableKeys,
    mutations: &[TableMutation],
) -> Result<(), EvaluationError> {
    let Some(references) = references else {
        return Ok(());
    };
    let mut touched = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let Some(schema) = key_fields.get(mutation.table()) else {
            continue;
        };
        // A re-key writes its destination row as well as removing the old key,
        // so both keys are judged over the candidate state (ORNA-MUT-007).
        for key in std::iter::once(mutation.key()).chain(mutation.rekey_to()) {
            let components = schema
                .values_from_encoded(key)
                .map_err(|_| reference_failure(ReferenceError::State))?;
            touched.push((
                mutation.table().to_owned(),
                components
                    .iter()
                    .map(|component| component.raw().clone())
                    .collect::<Vec<_>>(),
            ));
        }
    }
    references
        .validate(
            &touched,
            &mut ActivationRows {
                activation,
                key_fields,
            },
        )
        .map_err(reference_failure)
}

/// The activation's candidate rows: the committed rows with every write of the
/// activation applied.
struct ActivationRows<'a, 'runtime> {
    activation: &'a TransactionActivation<'runtime>,
    key_fields: &'a TableKeys,
}

impl CandidateState for ActivationRows<'_, '_> {
    fn row(&mut self, table: &str, key: &[OvbRaw]) -> Result<Option<OvbRaw>, ReferenceError> {
        let key = key_from_components(self.key_fields, table, key)?;
        self.activation
            .read(&table.to_owned(), &key)
            .map(|row| row.map(|row| row.raw().clone()))
            .map_err(|_| ReferenceError::State)
    }

    fn rows(&mut self, table: &str) -> Result<Vec<OvbRaw>, ReferenceError> {
        self.activation
            .candidate_relation(&table.to_owned())
            .map(|relation| relation.map(|(_, row)| row.raw().clone()).collect())
            .map_err(|_| ReferenceError::State)
    }
}
