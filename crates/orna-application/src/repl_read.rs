//! Committed-state REPL reads for table scans.
//!
//! A table-read-only input evaluates over the activation snapshot the caller
//! built from a committed HEAD, through a handler that serves table reads only.
//! This module takes no writer lease and writes nothing itself.

use super::repl_transaction::{
    ReplTransactionEffectHandler, admitted_project_tables, committed_table_rows,
};
use super::*;

impl ApplicationAuthority {
    /// Evaluates one table-read-only REPL input over `scope`'s committed
    /// snapshot. The session refuses any other effect before evaluation, so
    /// this path cannot write. A failure to build the read scope is reported
    /// as `ORNA-REPL-EFFECT`.
    pub fn evaluate_read_repl_input(
        &self,
        session: &mut AdmittedReplSession,
        source: &str,
        scope: &ReplTransactionScope<'_>,
    ) -> Result<orna_project_repl::ReplOutcome, ReplError> {
        let (tables, table_ids, defaults, functions) =
            admitted_project_tables(self, scope).map_err(read_scope_error)?;
        let explain_catalogue = crate::repl_explain_catalogue(
            &scope.snapshot.context().capture().snapshot(),
            tables.keys().map(String::as_str),
            scope.publication_rows.is_some(),
        )
        .map_err(read_scope_error)?;
        let (captured_rows, _committed) =
            committed_table_rows(scope, &tables).map_err(read_scope_error)?;
        let references = crate::stored_reference::stored_reference_catalogue(
            scope.snapshot.context().capture().database_id(),
            &tables,
            &table_ids,
        )
        .map_err(read_scope_error)?;
        let mutations = SourceMutationEffectHandler::with_table_rows_and_table_object_ids(
            tables,
            captured_rows,
            table_ids,
        )
        .map_err(read_scope_error)?
        .with_insert_defaults(defaults, functions, self.limits)
        .with_optional_publication_rows(scope.publication_rows.clone())
        .with_stored_references(references);
        // A read reaches no sys binding: the registry is empty, so any sys
        // call fails closed.
        let mut bindings = SysHostBindingRegistry::default();
        let mut effects = ReplTransactionEffectHandler {
            bindings: &mut bindings,
            mutations,
            scope,
            next_row_ordinal: 0,
            explain_catalogue,
            external_dispatched: false,
        };
        session.submit_read_outcome(source, &mut effects)
    }
}

fn read_scope_error<E>(_: E) -> ReplError {
    ReplError::read_scope_unavailable()
}
