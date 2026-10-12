//! Commits one captured media row through an admitted request, the same path
//! the media import tests use. `insert_only` chooses an insert or a replace.

use orna_application_v1::ApplicationAuthority;
use orna_evaluator_v1::{
    Environment, Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects,
};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeState, TableMutation, TerminalOutcome,
    WriterLease,
};
use orna_semantic_v1::{Catalogue, TableSchema};

use super::format3::publish_media_row;

pub async fn commit_capture(
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
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
        capture_expression(expression),
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let (id, key_bytes, row) = ([ordinal + 3; 16], key.as_bytes().to_vec(), Vec::new());
    let mutation = if insert_only {
        TableMutation::insert(id, "media", key_bytes, row)
    } else {
        TableMutation::new(id, "media", key_bytes, Some(row))
    }
    .unwrap()
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
    publish_media_row(repository, state, key, ordinal + 0x10).await;
}

/// Evaluates the authored importer through `SourceMutationEffectHandler` and
/// commits its complete typed row with the captured Blob pin evidence.
pub async fn commit_source_capture(
    project_root: &std::path::Path,
    repository: &Repository,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    source: &str,
    entry: &str,
    expression: &str,
    key: &str,
    ordinal: u8,
) -> TableSchema {
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
    let table_snapshot = state.begin_table_activation(&["Object"]).await.unwrap();
    let context = table_snapshot.context().clone();

    let captured = evaluate_expression_ovb2_with_effects(
        expression,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .expect("the source capture expression succeeds");
    let blob = captured
        .blob()
        .expect("the capture expression returns a Blob");
    let payload = blob
        .read_to_end()
        .expect("captured source bytes remain locally readable");
    let binding = bindings
        .accept_captured_blob_for_row(&captured)
        .expect("the captured Blob carries its repository candidate");

    // ProjectLoader reads the worktree module; replace only the fixture
    // repository's root source for loading, then restore its committed source.
    let main_path = project_root.join("main.orna");
    let original_source = std::fs::read(&main_path).unwrap();
    std::fs::write(&main_path, source).unwrap();
    let project = ProjectLoader::default()
        .load(repository)
        .expect("ProjectLoader loads the checked-in source producer");
    std::fs::write(&main_path, original_source).unwrap();
    let module = project.modules().first().expect("root source is loaded");
    let identity = project
        .identities()
        .first()
        .expect("root source has identity");
    assert_eq!(identity.logical_path(), "main.orna");
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module(identity.logical_path(), module.source.clone(), entry)
        .expect("the ProjectLoader artifact is admitted");
    let arguments = Environment::from([
        (
            "id".to_owned(),
            CanonicalValue::new(OvbRaw::Text(key.to_owned())).unwrap(),
        ),
        (
            "payload".to_owned(),
            CanonicalValue::new(OvbRaw::Bytes(payload)).unwrap(),
        ),
    ]);
    let staged = authority
        .evaluate_staged_with_table_snapshot(&application, &arguments, &table_snapshot)
        .expect("the source importer stages its typed Object row against the captured snapshot");
    assert_eq!(staged.mutations().len(), 1, "one source row is staged");
    let source_mutation = &staged.mutations()[0];
    assert_eq!(source_mutation.table(), "Object");
    assert!(source_mutation.is_insert());
    let encoded_fields = source_mutation.value().expect("source insert has a row");
    let value = CanonicalValue::decode(encoded_fields)
        .expect("the source row is a canonical ordered value tuple");
    let OvbRaw::Array(fields) = value.raw() else {
        panic!("the source row's value is a canonical field array");
    };
    assert_eq!(fields.len(), 1, "the admitted row stores one Blob field");
    assert!(
        matches!(fields[0], OvbRaw::Bytes(_)),
        "payload is the Blob role"
    );

    let pin = binding.into_pin();
    let mutation = source_mutation
        .clone()
        .with_protected_content_pin(&pin)
        .expect("the captured content pin accompanies the complete source mutation");
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
    publish_media_row(repository, state, key, ordinal + 0x10).await;
    application.table_schemas()["Object"].schema().clone()
}

fn capture_expression(source: &str) -> &str {
    source
        .lines()
        .find_map(|line| line.trim().strip_prefix("// @capture: "))
        .unwrap_or(source)
}
