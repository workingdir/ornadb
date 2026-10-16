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
        repository, state, writer, bindings, expression, "media", key, ordinal, insert_only,
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
        repository, state, writer, bindings, expression, key, ordinal, insert_only, relation_id,
        table_name, true,
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
        repository, state, writer, bindings, expression, key, ordinal, insert_only, relation_id,
        "media", false,
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
/// Captures and commits complete source-owned Object rows. The schema, row
/// candidates, canonical keys, ordered field tuple, and protected Blob pins all
/// come from the same committed ProjectLoader artifact and repository graph.
pub async fn commit_source_captures(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    relation_id: [u8; 16],
    format_context: &Format3Context,
    captures: &[(&str, &str, u8)],
) {
    assert!(!captures.is_empty());
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(repository, &commit)
        .expect("load the committed source project");
    let schema = loader
        .resolve_committed_source_table_schema(repository, &commit, TableObjectId::new(relation_id))
        .expect("resolve the Object table from its committed identity manifest");
    assert_eq!(schema.declaration_name(), "Object");
    assert_eq!(schema.table_object_id(), TableObjectId::new(relation_id));
    let mut mutations = Vec::with_capacity(captures.len());
    let mut paths_by_key = std::collections::BTreeMap::new();
    let mut retained_sources = Vec::with_capacity(captures.len());
    for (expression, key, ordinal) in captures {
        let logical_path = format!("Object/{key}.orna");
        let row_source = repository
            .read_committed_file(&commit, &logical_path, 1 << 20)
            .expect("the committed project contains this source Object row");
        let row_source = std::str::from_utf8(&row_source)
            .expect("the committed source Object row is valid UTF-8");
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
        let admitted = admit_source_row_with_owner_fields(
            &project,
            &logical_path,
            row_source,
            &schema,
            format_context,
            vec![("payload".to_owned(), blob_binding)],
            vec![],
            [ordinal.wrapping_add(3); 16],
        )
        .expect("admit a source-bound full Object row");
        let (source_mutation, source_bindings, resolved_schema) =
            admitted.into_mutation_and_bindings();
        assert_eq!(
            resolved_schema.table_object_id(),
            TableObjectId::new(relation_id)
        );
        assert_eq!(source_mutation.table(), "Object");
        assert_eq!(
            source_mutation.table_object_id(),
            Some(TableObjectId::new(relation_id))
        );
        let mut mutation = TableMutation::insert(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            source_mutation.key().to_vec(),
            source_mutation
                .value()
                .expect("admitted source row has a value")
                .to_vec(),
        )
        .expect("the source row is a new store row")
        .with_table_object_id(TableObjectId::new(relation_id));
        for binding in &source_bindings {
            mutation = mutation
                .with_protected_content_pin(binding.pin())
                .expect("retain the admitted Blob pin on the insert");
        }
        assert!(mutation.value().is_some_and(|value| !value.is_empty()));
        paths_by_key.insert(
            mutation.key().to_vec(),
            LoosePath::for_key("Object", &[key.to_string()])
                .expect("the committed source row has a managed path"),
        );
        retained_sources.push(source_bindings);
        mutations.push(mutation);
    }
    let request_identity = RequestIdentity {
        session_id: [0x91; 16],
        request_id: [0x92; 16],
    };
    let fingerprint = [0x93; 32];
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
            [0x94; 32],
            TerminalOutcome::new(vec![0x95]).unwrap(),
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    crate::format3::publish_source_rows(repository, state, &paths_by_key).await;
    drop(retained_sources);
}
