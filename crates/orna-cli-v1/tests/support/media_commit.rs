//! Commits one captured media row through an admitted request, the same path
//! the media import tests use. `insert_only` chooses an insert or a replace.
//!
//! `relation_id` is the captured table's admitted object identity. Production
//! attaches it to every durable mutation from the admitted table identity, and
//! publication projects rows into the native store by it, so a harness that
//! omits it stages a row no store root can address.

use std::{collections::BTreeMap, sync::Arc};

use orna_conformance_v1::{
    DurableTransactionalEvaluator, transactional_loaded_project_candidate_validator,
};
use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_project_v1::{
    ObjectIdentityManifest, ObjectKind, ProjectLoader, admit_source_row_with_owner_fields,
};
use orna_repository_v1::{Repository, RuntimeGeneration};
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeState, RuntimeTableIdentity, TableMutation,
    TableObjectId, TerminalOutcome, WriterLease,
    vfs::{VfsDurableSave, VfsSaveAdmission, VfsSaveCandidate, VfsTableRowReplacement},
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
            orna_runtime_v1::ActivationEffectReport::declare(orna_runtime_v1::DeclaredEffectSet::None),
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    if publish {
        crate::format3::publish_media_row(repository, state, key, ordinal + 0x10).await;
    }
}
/// Source row id for a fixture ordinal. Ordinals below 256 keep the encoding
/// every existing caller was written against: byte 0 is `1`, every other byte
/// is the ordinal, so their ids are unchanged. Ordinal bits 8..14 sit above a
/// marker bit in byte 0; ids from 256 never equal an id below 256.
pub fn source_row_id_for_ordinal(ordinal: u16) -> [u8; 16] {
    assert!(
        ordinal < 0x8000,
        "fixture ordinal {ordinal} exceeds the 15-bit source id encoding"
    );
    let mut id = [ordinal as u8; 16];
    id[0] = 1 | (((ordinal >> 8) as u8) << 1);
    id
}

/// One source row's named Blob captures and whether publication inserts or replaces it.
///
/// `source_row` is the row's complete source document. Format 3 keeps rows
/// only in `.orna/store` (ORNA-REPO-004), so no committed file supplies it.
#[derive(Clone, Copy)]
pub struct SourceCaptureRow<'a> {
    pub key: &'a str,
    pub blob_fields: &'a [(&'a str, &'a str)],
    pub source_row: &'a str,
    pub ordinal: u16,
    pub insert_only: bool,
}

/// Publishes every source row by its canonical mutation key and table path.
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
        let row_source = capture.source_row;
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
        let source_row_id = source_row_id_for_ordinal(capture.ordinal);
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
    let ordinal = captures[0].ordinal as u8;
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
            orna_runtime_v1::ActivationEffectReport::declare(orna_runtime_v1::DeclaredEffectSet::None),
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

/// Replaces one captured source row through the save admission the mount's
/// durable save uses, and leaves it unpublished: the runtime holds the
/// replacement while the repository head still commits the previous row.
pub async fn commit_source_replacement_through_save_admission(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    relation_id: [u8; 16],
    format_context: &Format3Context,
    table_name: &str,
    capture: &SourceCaptureRow<'_>,
) {
    assert!(!capture.insert_only, "a save admission replaces an existing row");
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(repository, &commit)
        .expect("load the committed source project");
    let identities = ObjectIdentityManifest::read_committed(repository, &commit)
        .expect("read the committed identity manifest");
    let table_object_ids: Arc<BTreeMap<String, TableObjectId>> = Arc::new(
        identities
            .records()
            .iter()
            .filter(|identity| identity.kind() == ObjectKind::Table && !identity.is_retired())
            .map(|identity| {
                (
                    identity.qualified_name().to_owned(),
                    TableObjectId::new(*identity.id()),
                )
            })
            .collect(),
    );
    let schema = loader
        .resolve_committed_source_table_schema(repository, &commit, TableObjectId::new(relation_id))
        .expect("resolve the table from its committed identity manifest");
    assert_eq!(schema.declaration_name(), table_name);
    assert_eq!(schema.table_object_id(), TableObjectId::new(relation_id));
    let runtime_capture = state.capture().await.expect("capture the runtime CWD");
    let generation = u64::try_from(runtime_capture.generation())
        .expect("the runtime generation fits a repository generation");
    let cwd_generation = repository
        .cwd_generation(RuntimeGeneration::new(generation))
        .expect("the repository CWD generation is available");
    let table = RuntimeTableIdentity::new(table_name, TableObjectId::new(relation_id))
        .expect("the captured table has a runtime identity");
    let save = VfsDurableSave::new(
        writer.owner_id,
        cwd_generation,
        table,
        capture.key.as_bytes().to_vec(),
    );
    let admission = VfsSaveAdmission::<()>::begin(state, save, &NoFault)
        .await
        .expect("begin the save admission for the captured row");
    let coordinates = admission.coordinates();
    let logical_path = format!("{table_name}/{}.orna", capture.key);
    let row_source = capture.source_row;
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
    let source_row_id = source_row_id_for_ordinal(capture.ordinal);
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
    let (source_mutation, source_bindings, resolved_schema) = admitted.into_mutation_and_bindings();
    assert_eq!(resolved_schema.table_object_id(), TableObjectId::new(relation_id));
    assert_eq!(source_mutation.table(), table_name);
    assert_eq!(source_mutation.table_object_id(), Some(TableObjectId::new(relation_id)));
    assert_eq!(source_mutation.key(), capture.key.as_bytes());
    // Source-row admission creates insert-only mutations; this save writes the
    // replacement over the stored row instead.
    let replacement_value = source_mutation
        .value()
        .expect("admitted source row has a value")
        .to_vec();
    let mut mutation = TableMutation::new(
        source_mutation.id(),
        source_mutation.table().to_owned(),
        coordinates.key().to_vec(),
        Some(replacement_value),
    )
    .expect("the replacement is a valid table mutation")
    .with_table_object_id(coordinates.table().object_id());
    let mut pins = Vec::with_capacity(source_bindings.len());
    for binding in source_bindings {
        let (_, _, pin) = binding.into_parts();
        mutation = mutation
            .with_protected_content_pin(&pin)
            .expect("retain the admitted Blob pin on the replacement");
        pins.push(pin);
    }
    let content_transfers = pins
        .iter()
        .map(|pin| pin.transfer_record())
        .collect::<Vec<_>>();
    let replacement_bytes = mutation
        .value()
        .expect("the replacement row has a value")
        .to_vec();
    let replacement = VfsTableRowReplacement::from_admitted_mutation(
        coordinates.table().clone(),
        mutation,
        pins,
    )
    .expect("the admitted replacement is a valid table row");
    let validator = transactional_loaded_project_candidate_validator(
        &project,
        &identities,
        coordinates.cwd_generation().clone(),
        Arc::clone(&table_object_ids),
        Limits::default(),
    )
    .unwrap_or_else(|_| panic!("the committed project yields a candidate validator"));
    let candidate = VfsSaveCandidate::new(replacement, validator)
        .with_protected_content_transfers(replacement_bytes, content_transfers);
    admission
        .commit(candidate)
        .await
        .expect("the replacement row commits through the save admission");
}
