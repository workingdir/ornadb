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

/// The stored references the rows a commit saved must still resolve to: the
/// catalogue of the committed database and each `(table, stored key)` the commit
/// wrote, replaced, deleted or rekeyed (ORNA-REF-001, ORNA-MUT-007, ORNA-MUT-008).
pub struct SavedRowReferences {
    pub catalogue: ReferenceCatalogue,
    pub rows: Vec<(String, Vec<u8>)>,
}

/// The database identity the committed `HEAD` of `repository` records: `None`
/// when there is no commit yet. An unreadable identity fails rather than being
/// guessed.
fn committed_identity(
    repository: &orna_repository::Repository,
) -> Result<Option<[u8; 16]>, StageOutcome<Diagnostic>> {
    let head = match repository.head() {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(None),
        Err(_) => return Err(StageOutcome::Failed(reference_identity_diagnostic())),
    };
    repository
        .open_pinned_format_context(head.as_str())
        .and_then(|format| format.require_database_id())
        .map(|database_id| Some(*database_id.as_bytes()))
        .map_err(|_| StageOutcome::Failed(reference_identity_diagnostic()))
}

/// The database identity a stored reference binds to, read from the committed
/// `HEAD` (ORNA-REF-001). `None` when it cannot be read: a table that stores a
/// reference is then refused by the validator, never checked without a database.
pub fn committed_database_id(repository: &orna_repository::Repository) -> Option<[u8; 16]> {
    committed_identity(repository).ok().flatten()
}

/// The reference catalogue of the committed database `loaded` runs against,
/// bound to the identity its `HEAD` commits. `None` when no table has a
/// committed identity to name, or when there is no commit yet. An unreadable
/// identity fails closed rather than writing references without a database.
pub fn committed_reference_catalogue(
    repository: &orna_repository::Repository,
    loaded: &orna_project::LoadedProject,
    table_object_ids: &BTreeMap<String, orna_runtime::TableObjectId>,
    limits: EvaluatorLimits,
) -> Result<Option<ReferenceCatalogue>, StageOutcome<Diagnostic>> {
    if table_object_ids.is_empty() {
        return Ok(None);
    }
    let Some(database_id) = committed_identity(repository)? else {
        return Ok(None);
    };
    let project = project_unit_of(loaded);
    let catalogue = loaded
        .production_catalogue()
        .map_err(|_| StageOutcome::Failed(standard_profile_diagnostic()))?;
    let admitted =
        admit_transaction_project(&project, limits, "", &catalogue).map_err(|failure| *failure)?;
    ReferenceCatalogue::new(database_id, &admitted.table_schemas, table_object_ids)
        .map_err(|_| StageOutcome::Failed(reference_identity_diagnostic()))
}

/// Whether `loaded` declares a table with a stored reference, so that each of
/// its committed writes is checked against stored references (ORNA-MUT-007,
/// ORNA-MUT-008). A project that cannot be admitted here counts as declaring
/// them: its writes are then checked and refused, never left unchecked.
pub fn project_declares_stored_references(
    loaded: &orna_project::LoadedProject,
    limits: EvaluatorLimits,
) -> bool {
    let Ok(catalogue) = loaded.production_catalogue() else {
        return true;
    };
    let project = project_unit_of(loaded);
    match admit_transaction_project(&project, limits, "", &catalogue) {
        Ok(admitted) => ReferenceCatalogue::declares_references(&admitted.table_schemas),
        Err(_) => true,
    }
}

fn reference_identity_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-REF-IDENTITY").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("the committed database identity could not be read for stored references")
            .expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

/// The candidate rows of a saved-row check: the rows of the candidate state,
/// each row decoded into the map shape a reference column is read from.
struct CandidateRowsState<'a> {
    rows: &'a orna_runtime::RuntimeTableRows,
    keys: &'a TableKeys,
    table_fields: &'a TableFields,
    key_fields: &'a TableFields,
}

impl CandidateRowsState<'_> {
    fn key_names(&self, table: &str) -> Result<&[String], ReferenceError> {
        self.key_fields
            .get(table)
            .map(Vec::as_slice)
            .ok_or(ReferenceError::Unbound)
    }
}

impl CandidateState for CandidateRowsState<'_> {
    fn row(&mut self, table: &str, key: &[OvbRaw]) -> Result<Option<OvbRaw>, ReferenceError> {
        key_from_components(self.keys, table, key)?;
        let key_names = self.key_names(table)?;
        let entries = self.rows.get(table).ok_or(ReferenceError::State)?;
        for (stored_key, encoded) in entries {
            let components = candidate_key_components(stored_key, key_names)
                .map_err(|_| ReferenceError::State)?;
            if components.as_slice() == key {
                return candidate_assertion_row(
                    table,
                    stored_key,
                    encoded,
                    self.table_fields,
                    self.key_fields,
                )
                .map(|row| Some(row.raw().clone()))
                .map_err(|_| ReferenceError::State);
            }
        }
        Ok(None)
    }

    fn rows(&mut self, table: &str) -> Result<Vec<OvbRaw>, ReferenceError> {
        let entries = self.rows.get(table).ok_or(ReferenceError::State)?;
        entries
            .iter()
            .map(|(stored_key, encoded)| {
                candidate_assertion_row(
                    table,
                    stored_key,
                    encoded,
                    self.table_fields,
                    self.key_fields,
                )
                .map(|row| row.raw().clone())
                .map_err(|_| ReferenceError::State)
            })
            .collect()
    }
}

/// Validates that the rows a commit saved still resolve their stored references
/// over the candidate state (ORNA-REF-001, ORNA-MUT-007, ORNA-MUT-008). A row the
/// commit deleted or rekeyed away must not stay referenced. A dangling reference
/// is refused with its own code; any other failure to read the state is refused
/// as an evaluation failure, so nothing commits unchecked.
pub(super) fn validate_saved_row_references(
    saved: &SavedRowReferences,
    keys: &TableKeys,
    table_fields: &TableFields,
    key_fields: &TableFields,
    rows: &orna_runtime::RuntimeTableRows,
) -> Result<(), SafeDiagnostic> {
    let mut touched = Vec::with_capacity(saved.rows.len());
    for (table, key) in &saved.rows {
        let key_names = key_fields
            .get(table)
            .ok_or_else(table_evaluation_diagnostic)?;
        let components =
            candidate_key_components(key, key_names).map_err(|_| table_row_decode_diagnostic())?;
        touched.push((table.clone(), components));
    }
    let mut state = CandidateRowsState {
        rows,
        keys,
        table_fields,
        key_fields,
    };
    saved
        .catalogue
        .validate(&touched, &mut state)
        .map_err(|error| match error {
            ReferenceError::Dangling => dangling_reference_diagnostic(),
            _ => table_evaluation_diagnostic(),
        })
}
