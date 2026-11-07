//! Read-only contextual snapshot reads for served pages.
//!
//! The canonical snapshot path cannot carry format-3 owner contexts: its rows
//! are `CanonicalValue`s, so a committed `Blob` cannot keep the capability
//! context it was minted under. This module decodes snapshot rows against the
//! snapshot's format-3 context and evaluates through the context-preserving
//! evaluator boundary. It never stages mutations and never publishes.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use orna_evaluator::{
    ContextValueEffectHandler, ContextValueRelationPage, EffectHandler, EvaluationError,
    RelationReadScope, StepBudget, invoke_named_with_context_effects,
};
use orna_foundation::{CanonicalValue, compare_primary_keys};
use orna_runtime::RuntimeTableRows;
use orna_syntax::Expr;
use orna_value::{ContextValue, Format3Context, ValueFormat};

use crate::{AdmittedApplication, ApplicationError, SourceMutationEffectHandler};

/// Orders decoded primary-key rows and returns one bounded page.
///
/// The cursor is the big-endian offset of the next row. Each returned row is
/// debited once from `budget`, and a continuation is offered only while rows
/// remain and the page is non-empty.
pub(crate) fn page_table_rows<T: Clone>(
    mut live: Vec<(orna_foundation::Value, T)>,
    after: Option<&[u8]>,
    limit: usize,
    budget: &mut StepBudget,
) -> Result<(Vec<T>, Option<Vec<u8>>), EvaluationError> {
    let mut comparison_failed = false;
    live.sort_by(|(left, _), (right, _)| {
        compare_primary_keys(left, right).unwrap_or_else(|_| {
            comparison_failed = true;
            Ordering::Equal
        })
    });
    if comparison_failed {
        return Err(SourceMutationEffectHandler::effect_error(
            "ORNA-EVAL-TABLE-SNAPSHOT",
        ));
    }
    let start = match after {
        None => 0,
        Some(cursor) => {
            let bytes: [u8; 8] = cursor.try_into().map_err(|_| {
                SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-SNAPSHOT")
            })?;
            usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| {
                SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-SNAPSHOT")
            })?
        }
    }
    .min(live.len());
    let end = start.saturating_add(limit).min(live.len());
    let mut rows = Vec::with_capacity(end - start);
    for (_, row) in &live[start..end] {
        budget.debit(1)?;
        rows.push(row.clone());
    }
    let next = (end < live.len() && !rows.is_empty()).then(|| (end as u64).to_be_bytes().to_vec());
    Ok((rows, next))
}

/// Pages the rows of one context-bearing table, keyed by encoded primary key.
fn scan_context_table(
    table: &BTreeMap<Vec<u8>, ContextValue>,
    after: Option<&[u8]>,
    limit: usize,
    budget: &mut StepBudget,
) -> Result<ContextValueRelationPage, EvaluationError> {
    let mut live = Vec::with_capacity(table.len());
    for (key, row) in table {
        let logical = orna_foundation::Value::decode(key)
            .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-SNAPSHOT"))?;
        live.push((logical, row));
    }
    let (rows, next) = page_table_rows(live, after, limit, budget)?;
    Ok(ContextValueRelationPage {
        rows: rows.into_iter().cloned().collect(),
        next,
    })
}

/// Serves table scans from a decoded, context-bearing snapshot. Effects are
/// declined here: a served read that reaches a non-table effect falls through
/// to the evaluator's normal unhandled-call path.
struct ContextualSnapshotEffectHandler {
    tables: BTreeMap<String, BTreeMap<Vec<u8>, ContextValue>>,
}

impl ContextualSnapshotEffectHandler {
    fn scan_page(
        &self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        let Some(table) = self.tables.get(source) else {
            return Ok(None);
        };
        scan_context_table(table, after, limit, budget).map(Some)
    }
}

impl EffectHandler for ContextualSnapshotEffectHandler {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }
}

impl ContextValueEffectHandler for ContextualSnapshotEffectHandler {
    fn handle_context(
        &mut self,
        _callee: &Expr,
        _arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        Ok(None)
    }

    fn scan_context_relation_page_scoped(
        &mut self,
        source: &str,
        _scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        self.scan_page(source, after, limit, budget)
    }
}

/// Evaluates the admitted entry as a read over a committed snapshot whose
/// format-3 rows are decoded against `context`.
///
/// Every admitted table must be present in `rows`, as on the canonical
/// snapshot path. Each row is `(encoded primary key, encoded format-3 value)`.
pub fn evaluate_contextual_snapshot_read(
    application: &AdmittedApplication,
    arguments: &BTreeMap<String, ContextValue>,
    rows: &RuntimeTableRows,
    context: &Format3Context,
) -> Result<ContextValue, ApplicationError> {
    let mut tables = BTreeMap::new();
    for table in crate::application_table_schemas(application).into_keys() {
        let table_rows = rows
            .get(&table)
            .ok_or_else(|| ApplicationError::UnadmittedTable(table.clone()))?;
        let mut decoded = BTreeMap::new();
        for (key, value) in table_rows {
            orna_foundation::Value::decode(key).map_err(|_| {
                ApplicationError::Runtime("contextual snapshot row had an invalid key".into())
            })?;
            let row = ContextValue::decode_in_context(value, ValueFormat::Rov3, context).map_err(
                |_| ApplicationError::Runtime("contextual snapshot row was not canonical".into()),
            )?;
            if decoded.insert(key.clone(), row).is_some() {
                return Err(ApplicationError::Runtime(
                    "contextual snapshot had duplicate keys".into(),
                ));
            }
        }
        tables.insert(table, decoded);
    }
    let mut handler = ContextualSnapshotEffectHandler { tables };
    let mut budget = StepBudget::new(application.limits.max_steps);
    invoke_named_with_context_effects(
        &application.entry,
        &application.functions,
        arguments,
        application.limits,
        &mut handler,
        &mut budget,
    )
    .map_err(|error| ApplicationError::Evaluation(error.code().to_owned()))
}

impl ContextValueEffectHandler for SourceMutationEffectHandler {
    fn handle_context(
        &mut self,
        _callee: &Expr,
        _arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        Ok(None)
    }

    fn scan_context_relation_page_scoped(
        &mut self,
        source: &str,
        _scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        let Some(table) = self.context_table_rows.get(source) else {
            return Ok(None);
        };
        scan_context_table(table, after, limit, budget).map(Some)
    }
}

/// Evaluates the admitted entry over a committed snapshot in which every table
/// whose schema holds a Blob is read from `context_rows` (format-3 rows decoded
/// in the owner context), and every other table is read from `rows` through
/// the canonical scan, as `evaluate_staged_over_table_rows` reads it.
///
/// `context_rows` must name exactly the Blob-holding tables, and each of those
/// tables must carry no canonical rows in `rows`. Only table scans are served:
/// any other effect is declined, and a read that stages a mutation is refused.
/// No system host is attached to this read.
pub fn evaluate_contextual_page_read(
    application: &AdmittedApplication,
    arguments: &BTreeMap<String, ContextValue>,
    mut rows: RuntimeTableRows,
    context_rows: BTreeMap<String, Vec<(Vec<u8>, ContextValue)>>,
) -> Result<ContextValue, ApplicationError> {
    let tables = crate::application_table_schemas(application);
    let blob_tables = tables
        .iter()
        .filter(|(_, schema)| schema.fields.values().any(orna_project::holds_blob))
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    if context_rows
        .keys()
        .any(|table| !blob_tables.contains(table))
    {
        return Err(ApplicationError::Runtime(
            "contextual rows named a table without a Blob field".into(),
        ));
    }
    for table in &blob_tables {
        if !context_rows.contains_key(table) {
            return Err(ApplicationError::UnadmittedTable(table.clone()));
        }
        if rows
            .get(table)
            .is_some_and(|table_rows| !table_rows.is_empty())
        {
            return Err(ApplicationError::Runtime(
                "a Blob table carried canonical rows".into(),
            ));
        }
        rows.insert(table.clone(), Vec::new());
    }
    for table in tables.keys().filter(|table| !blob_tables.contains(*table)) {
        if !rows.contains_key(table) {
            return Err(ApplicationError::UnadmittedTable(table.clone()));
        }
    }
    if let Some(table) = rows.keys().find(|table| !tables.contains_key(*table)) {
        return Err(ApplicationError::UnadmittedTable(table.clone()));
    }
    let mut context_table_rows = BTreeMap::new();
    for (table, table_rows) in context_rows {
        let mut decoded = BTreeMap::new();
        for (key, row) in table_rows {
            orna_foundation::Value::decode(&key).map_err(|_| {
                ApplicationError::Runtime("contextual table row had an invalid key".into())
            })?;
            if decoded.insert(key, row).is_some() {
                return Err(ApplicationError::Runtime(
                    "contextual table had duplicate keys".into(),
                ));
            }
        }
        context_table_rows.insert(table, decoded);
    }
    let table_object_ids = crate::application_table_object_ids(application);
    let mut handler = SourceMutationEffectHandler::with_table_rows_and_table_object_ids(
        tables,
        rows,
        table_object_ids,
    )?
    .with_insert_defaults(
        application.table_insert_defaults.clone(),
        application.functions.clone(),
        application.limits,
    );
    handler.context_table_rows = context_table_rows;
    let mut budget = StepBudget::new(application.limits.max_steps);
    let value = invoke_named_with_context_effects(
        &application.entry,
        &application.functions,
        arguments,
        application.limits,
        &mut handler,
        &mut budget,
    )
    .map_err(|error| ApplicationError::Evaluation(error.code().to_owned()))?;
    if !handler.mutations.is_empty() {
        return Err(ApplicationError::Runtime(
            "a contextual read staged a mutation".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ApplicationAuthority;
    use orna_evaluator::{Environment, Limits};
    use orna_semantic::Catalogue;

    struct UnavailableResolver;

    impl orna_value::BlobResolver for UnavailableResolver {
        fn read_range(
            &self,
            _reference: &orna_value::ContentReference,
            _offset: u64,
            _length: u64,
        ) -> orna_value::Result<Vec<u8>> {
            Err(orna_value::Error::ContentUnavailable)
        }
    }

    fn context() -> Format3Context {
        let database = [0x21; 16];
        Format3Context::from_persisted(
            database,
            orna_value::Snapshot::cwd(database, [0x22; 16], 0.into()).expect("snapshot"),
            orna_value::GitHash::Sha1,
            orna_value::NativeOid::from_bytes(&[0x23; 20]).expect("schema root"),
            orna_value::NativeOid::from_bytes(&[0x24; 20]).expect("store root"),
            orna_value::AvailabilityState::Available,
            orna_value::OwnerLifetime::new([0x25; 16]),
            orna_value::Format3Quota::new(1024, 1 << 20).expect("quota"),
            Arc::new(UnavailableResolver),
        )
        .expect("format-3 context")
    }

    #[test]
    fn contextual_blob_row_keeps_its_owner_context_through_the_scan() {
        let context = context();
        let reference = context
            .reference(
                orna_value::ContentIdentity::from_bytes(b"served payload"),
                orna_value::NativeOid::from_bytes(&[0x26; 20]).expect("descriptor"),
            )
            .expect("owner-scoped reference");
        let blob = orna_value::Blob::from_reference(reference).expect("owner-scoped blob");
        let row = ContextValue::from_blob(&blob, ValueFormat::Rov3).expect("contextual row");
        let key = orna_foundation::Value::int(1.into())
            .encode()
            .expect("key bytes");
        let handler = ContextualSnapshotEffectHandler {
            tables: BTreeMap::from([("Media".to_owned(), BTreeMap::from([(key, row)]))]),
        };
        let mut budget = StepBudget::new(100);
        let page = handler
            .scan_page("Media", None, 10, &mut budget)
            .expect("scan")
            .expect("admitted table");
        assert_eq!(page.rows.len(), 1);
        let scanned = &page.rows[0];
        assert_eq!(scanned.format(), ValueFormat::Rov3);
        assert!(scanned.context().is_some(), "owner context must survive");
        assert!(page.next.is_none());
    }

    #[test]
    fn unknown_table_declines_the_scan() {
        let handler = ContextualSnapshotEffectHandler {
            tables: BTreeMap::new(),
        };
        let mut budget = StepBudget::new(10);
        assert!(
            handler
                .scan_page("Missing", None, 1, &mut budget)
                .expect("scan")
                .is_none()
        );
    }

    const BLOB_SOURCE: &str = "pub table Media(id: Int) { payload: Blob, }\n\npub fn main(): Int {\n  Media | count()\n}\n";
    const NOTE_SOURCE: &str =
        "pub table Note(id: Int) { text: Str, }\n\npub fn main(): Int {\n  Note | count()\n}\n";

    fn admit(source: &str) -> AdmittedApplication {
        ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
            .admit_module("main.orna", source, "main")
            .expect("module admitted")
    }

    #[test]
    fn blob_table_page_is_read_from_its_owner_context_rows() {
        let context = context();
        let reference = context
            .reference(
                orna_value::ContentIdentity::from_bytes(b"served payload"),
                orna_value::NativeOid::from_bytes(&[0x26; 20]).expect("descriptor"),
            )
            .expect("owner-scoped reference");
        let blob = orna_value::Blob::from_reference(reference).expect("owner-scoped blob");
        let row = ContextValue::from_blob(&blob, ValueFormat::Rov3).expect("contextual row");
        let key = orna_foundation::Value::int(1.into())
            .encode()
            .expect("key bytes");
        let result = evaluate_contextual_page_read(
            &admit(BLOB_SOURCE),
            &BTreeMap::new(),
            RuntimeTableRows::from([("Media".to_owned(), Vec::new())]),
            BTreeMap::from([("Media".to_owned(), vec![(key, row)])]),
        )
        .expect("contextual page read");
        assert_eq!(result.raw(), orna_foundation::Value::int(1.into()).raw());
    }

    #[test]
    fn blob_table_without_context_rows_is_refused() {
        let result = evaluate_contextual_page_read(
            &admit(BLOB_SOURCE),
            &BTreeMap::new(),
            RuntimeTableRows::from([("Media".to_owned(), Vec::new())]),
            BTreeMap::new(),
        );
        assert!(
            matches!(&result, Err(ApplicationError::UnadmittedTable(table)) if table == "Media")
        );
    }

    #[test]
    fn context_rows_for_a_non_blob_table_are_refused() {
        let result = evaluate_contextual_page_read(
            &admit(NOTE_SOURCE),
            &BTreeMap::new(),
            RuntimeTableRows::from([("Note".to_owned(), Vec::new())]),
            BTreeMap::from([("Note".to_owned(), Vec::new())]),
        );
        assert!(matches!(result, Err(ApplicationError::Runtime(_))));
    }

    #[test]
    fn non_blob_page_matches_the_canonical_read() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module("main.orna", NOTE_SOURCE, "main")
            .expect("module admitted");
        let rows = RuntimeTableRows::from([("Note".to_owned(), Vec::new())]);
        let staged = authority
            .evaluate_staged_over_table_rows(
                &application,
                &Environment::new(),
                rows.clone(),
                None,
                None,
            )
            .expect("canonical read");
        let contextual =
            evaluate_contextual_page_read(&application, &BTreeMap::new(), rows, BTreeMap::new())
                .expect("contextual read");
        assert_eq!(contextual.raw(), staged.value().raw());
    }
}
