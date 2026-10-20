//! Commits one captured media row through an admitted request, the same path
//! the media import tests use. `insert_only` chooses an insert or a replace.
//!
//! `relation_id` is the captured table's admitted object identity. Production
//! attaches it to every durable mutation from the admitted table identity, and
//! publication projects rows into the native store by it, so a harness that
//! omits it stages a row no store root can address.

use orna_conformance_v1::DurableTransactionalEvaluator;
use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_project_v1::{ProjectLoader, admit_source_row_with_owner_fields};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeState, TableMutation, TableObjectId,
    TerminalOutcome, WriterLease,
};
use orna_storage_v1::LoosePath;
use orna_value_v1::Format3Context;

/// Commits one captured media row and publishes it, so the row is durable in
/// the store before the caller reads it.
pub async fn commit_capture(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
    relation_id: [u8; 16],
) {
    commit_capture_for_table(
        repository,
        state,
        writer,
        bindings,
        expression,
        "media",
        key,
        ordinal,
        insert_only,
        relation_id,
    )
    .await;
}

pub async fn commit_capture_for_table(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    table_name: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
    relation_id: [u8; 16],
) {
    commit_capture_inner(
        repository,
        state,
        writer,
        bindings,
        expression,
        key,
        ordinal,
        insert_only,
        relation_id,
        table_name,
        true,
    )
    .await;
}

/// Commits one captured media row and leaves the range pending, so a caller can
/// publish it with the CLI verb exactly as a production run does.
pub async fn commit_capture_pending(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
    relation_id: [u8; 16],
) {
    commit_capture_inner(
        repository,
        state,
        writer,
        bindings,
        expression,
        key,
        ordinal,
        insert_only,
        relation_id,
        "media",
        false,
    )
    .await;
}

async fn commit_capture_inner(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
    relation_id: [u8; 16],
    table_name: &str,
    publish: bool,
) {
    let request_identity = RequestIdentity {
        session_id: [ordinal; 16],
        request_id: [ordinal + 1; 16],
    };
    let fingerprint = [ordinal + 2; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();

    let value = evaluate_expression_ovb2_with_effects(
        expression,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let (id, key_bytes, row) = ([ordinal + 3; 16], key.as_bytes().to_vec(), Vec::new());
    let mutation = if insert_only {
        TableMutation::insert(id, table_name, key_bytes, row)
    } else {
        TableMutation::new(id, table_name, key_bytes, Some(row))
    }
    .unwrap()
    .with_table_object_id(TableObjectId::new(relation_id))
    .with_orp_blob_binding(binding)
    .unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [ordinal + 4; 32],
            TerminalOutcome::new(vec![ordinal + 5]).unwrap(),
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    if publish {
        crate::format3::publish_media_row(repository, state, key, ordinal + 0x10).await;
    }
}
/// One source row's named Blob captures and whether publication inserts or replaces it.
#[derive(Clone, Copy)]
pub struct SourceCaptureRow<'a> {
    pub key: &'a str,
    pub blob_fields: &'a [(&'a str, &'a str)],
    pub source_row: Option<&'a str>,
    pub ordinal: u8,
    pub insert_only: bool,
}

/// Publishes every source row by its canonical mutation key and committed path.
pub async fn commit_source_captures(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    relation_id: [u8; 16],
    format_context: &Format3Context,
    captures: &[SourceCaptureRow<'_>],
) {
    commit_source_captures_for_table(
        repository,
        state,
        writer,
        bindings,
        relation_id,
        format_context,
        "Object",
        captures,
    )
    .await;
}

/// Captures and commits complete source-owned rows for a source table.
pub async fn commit_source_captures_for_table(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    relation_id: [u8; 16],
    format_context: &Format3Context,
    table_name: &str,
    captures: &[SourceCaptureRow<'_>],
) {
    assert!(!captures.is_empty());
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(repository, &commit)
        .expect("load the committed source project");
    let schema = loader
        .resolve_committed_source_table_schema(repository, &commit, TableObjectId::new(relation_id))
        .expect("resolve the table from its committed identity manifest");
    assert_eq!(schema.declaration_name(), table_name);
    assert_eq!(schema.table_object_id(), TableObjectId::new(relation_id));
    let mut mutations = Vec::with_capacity(captures.len());
    let mut paths_by_key = std::collections::BTreeMap::new();
    let mut retained_sources = Vec::with_capacity(captures.len());
    for capture in captures {
        let logical_path = format!("{table_name}/{}.orna", capture.key);
        let row_source = match capture.source_row {
            Some(row_source) => row_source.to_owned(),
            None => {
                let committed_row = repository
                    .read_committed_file(&commit, &logical_path, 1 << 20)
                    .expect("the committed project contains this source row");
                std::str::from_utf8(&committed_row)
                    .expect("the committed source row is valid UTF-8")
                    .to_owned()
            }
        };
        let mut blob_fields = Vec::with_capacity(capture.blob_fields.len());
        for (field_name, expression) in capture.blob_fields {
            let value = evaluate_expression_ovb2_with_effects(
                expression,
                &Default::default(),
                Limits::default(),
                bindings,
            )
            .expect("capture the source payload");
            let blob_binding = bindings
                .accept_captured_blob_for_row(&value)
                .expect("accept the captured Blob for this row");
            blob_fields.push(((*field_name).to_owned(), blob_binding));
        }
        // Keep source ids non-zero while preserving every 8-bit fixture ordinal.
        let mut source_row_id = [capture.ordinal; 16];
        source_row_id[0] = 1;
        let admitted = admit_source_row_with_owner_fields(
            &project,
            &logical_path,
            &row_source,
            &schema,
            format_context,
            blob_fields,
            vec![],
            source_row_id,
        )
        .unwrap_or_else(|error| {
            panic!(
                "admit source row ordinal {} ({row_source}): {error:?}",
                capture.ordinal
            )
        });
        let (source_mutation, source_bindings, resolved_schema) =
            admitted.into_mutation_and_bindings();
        assert_eq!(
            resolved_schema.table_object_id(),
            TableObjectId::new(relation_id)
        );
        assert_eq!(source_mutation.table(), table_name);
        assert_eq!(
            source_mutation.table_object_id(),
            Some(TableObjectId::new(relation_id))
        );
        let mutation_id = source_mutation.id();
        let mutation_table = source_mutation.table().to_owned();
        let mutation_key = source_mutation.key().to_vec();
        let mutation_value = source_mutation
            .value()
            .expect("admitted source row has a value")
            .to_vec();
        let mut mutation = if capture.insert_only {
            TableMutation::insert(mutation_id, mutation_table, mutation_key, mutation_value)
                .expect("the source row is a new store row")
        } else {
            TableMutation::new(
                mutation_id,
                mutation_table,
                mutation_key,
                Some(mutation_value),
            )
            .expect("the source row replaces an existing store row")
        }
        .with_table_object_id(TableObjectId::new(relation_id));
        for binding in &source_bindings {
            mutation = mutation
                .with_protected_content_pin(binding.pin())
                .expect("retain the admitted Blob pin on the mutation");
        }
        assert!(mutation.value().is_some_and(|value| !value.is_empty()));
        paths_by_key.insert(
            mutation.key().to_vec(),
            LoosePath::for_key(table_name, &[capture.key.to_string()])
                .expect("the committed source row has a managed path"),
        );
        retained_sources.push(source_bindings);
        mutations.push(mutation);
    }
    let ordinal = captures[0].ordinal;
    let mut request_id = [ordinal; 16];
    request_id[0] = 0xA5;
    let request_identity = RequestIdentity {
        session_id: [0x91; 16],
        request_id,
    };
    let mut fingerprint = [ordinal; 32];
    fingerprint[0] = 0x5A;
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &mutations,
            [ordinal.wrapping_add(4); 32],
            TerminalOutcome::new(vec![ordinal.wrapping_add(5)]).unwrap(),
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    crate::format3::publish_source_rows(
        repository,
        state,
        &paths_by_key,
        [ordinal.wrapping_add(6); 16],
    )
    .await;
    drop(retained_sources);
}
