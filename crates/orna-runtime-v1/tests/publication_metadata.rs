//! ORNA-PUB-004 runtime publication metadata and completion accounting.

use std::{path::PathBuf, process::Command};

use orna_evaluator_v1::{Environment, Limits, evaluate_function};
use orna_repository_v1::Repository;
use orna_runtime_v1::{Mutation, NoFault, PublicationCommitId, RuntimeIdentity, RuntimeState};
use orna_syntax_v1::{Declaration, parse_module};
use orna_value_v1::Raw;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

const PUBLICATION_METADATA_SOURCE: &str = include_str!("fixtures/publication_metadata.orna");

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-publication-metadata-")
        .tempdir_in(target)
        .expect("create repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize repository");
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn mutation(id: u8, payload: &[u8]) -> Mutation {
    Mutation {
        id: [id; 16],
        payload: payload.to_vec(),
        digest: Sha256::digest(payload).into(),
    }
}

fn projected_int(
    function_name: &str,
    parameter_name: &str,
    row: orna_foundation_v1::CanonicalValue,
) -> u64 {
    match projected_value(function_name, parameter_name, row).raw() {
        Raw::Int(value) => u64::try_from(value.clone()).expect("non-negative projection"),
        other => panic!("{function_name} returned {other:?}, expected Int"),
    }
}

fn projected_value(
    function_name: &str,
    parameter_name: &str,
    row: orna_foundation_v1::CanonicalValue,
) -> orna_foundation_v1::CanonicalValue {
    let parsed = parse_module(PUBLICATION_METADATA_SOURCE);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let declaration = parsed
        .value
        .items
        .iter()
        .find(|item| {
            matches!(
                &item.declaration,
                Declaration::Function { signature, .. } if signature.name == function_name
            )
        })
        .expect("projection function exists");
    let Declaration::Function { signature, body } = &declaration.declaration else {
        panic!("projection is a function");
    };
    let environment = Environment::from([(parameter_name.into(), row)]);
    evaluate_function(
        &signature.parameters,
        body,
        &Environment::new(),
        &environment,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("{function_name}: {}", error.code()))
}

#[tokio::test]
async fn publication_metadata_tracks_frozen_prefix_and_matches_both_projections() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    assert!(
        state
            .set_publication_compressed_target_bytes(8 * 1024 * 1024 - 1)
            .await
            .is_err()
    );
    assert!(
        state
            .set_publication_compressed_target_bytes(32 * 1024 * 1024 + 1)
            .await
            .is_err()
    );
    state
        .set_publication_compressed_target_bytes(24 * 1024 * 1024)
        .await
        .expect("persist an in-range publication target");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    // Exercise the checked-in Orna source as the durable mutation itself, so
    // this proof covers a real source payload rather than a Rust-only marker.
    let published_mutation = mutation(5, PUBLICATION_METADATA_SOURCE.as_bytes());
    let first_capture = state.capture().await.expect("capture before mutation");
    state
        .commit(
            writer,
            &first_capture,
            &published_mutation,
            [6; 32],
            &NoFault,
        )
        .await
        .expect("append frozen mutation");
    let freeze = state
        .freeze(
            [7; 16],
            &state
                .latest_checkpoint()
                .await
                .expect("read frozen checkpoint")
                .expect("frozen checkpoint exists"),
        )
        .await
        .expect("freeze pending prefix");

    let trailing_mutation = mutation(8, b"trailing-payload");
    let capture = state.capture().await.expect("capture before tail");
    state
        .commit(writer, &capture, &trailing_mutation, [9; 32], &NoFault)
        .await
        .expect("append mutation after freeze");

    let pending_metadata = state
        .publication_metadata()
        .await
        .expect("read publication metadata");
    assert_eq!(
        pending_metadata.publication_policy.compressed_target_bytes,
        24 * 1024 * 1024
    );
    assert_eq!(
        pending_metadata.publication_policy.max_file_bytes,
        64 * 1024 * 1024
    );
    assert_eq!(
        pending_metadata.publication_policy.max_pending_age_seconds,
        60
    );
    assert_eq!(pending_metadata.pending_rows, 2);
    assert_eq!(
        pending_metadata.pending_bytes,
        (published_mutation.payload.len() + trailing_mutation.payload.len()) as u64
    );
    assert_eq!(pending_metadata.published_rows, 0);
    assert_eq!(pending_metadata.published_bytes, 0);
    assert_eq!(pending_metadata.last_publication_ms, None);
    let pending_rows = state
        .publication_metadata_rows()
        .await
        .expect("project publication metadata rows");
    let storage_values = pending_rows.sys_storage.clone();
    let maintenance_values = pending_rows.maintenance_job.clone();
    assert_eq!(
        projected_int("storage_policy_target", "storage", storage_values.clone()),
        24 * 1024 * 1024
    );
    assert_eq!(
        projected_int(
            "storage_policy_file_bound",
            "storage",
            storage_values.clone()
        ),
        64 * 1024 * 1024
    );
    assert_eq!(
        projected_int(
            "storage_policy_pending_age",
            "storage",
            storage_values.clone()
        ),
        60
    );
    assert_eq!(
        projected_int("storage_pending_rows", "storage", storage_values.clone()),
        2
    );
    assert_eq!(
        projected_int("storage_pending_bytes", "storage", storage_values.clone()),
        pending_metadata.pending_bytes
    );
    assert_eq!(
        projected_int("storage_published_rows", "storage", storage_values.clone()),
        0
    );
    assert_eq!(
        projected_int("storage_published_bytes", "storage", storage_values),
        0
    );
    assert_eq!(
        projected_int(
            "maintenance_policy_target",
            "job",
            maintenance_values.clone()
        ),
        24 * 1024 * 1024
    );
    assert_eq!(
        projected_int(
            "maintenance_policy_file_bound",
            "job",
            maintenance_values.clone()
        ),
        64 * 1024 * 1024
    );
    assert_eq!(
        projected_int(
            "maintenance_policy_pending_age",
            "job",
            maintenance_values.clone()
        ),
        60
    );
    assert_eq!(
        projected_int(
            "maintenance_pending_rows",
            "job",
            maintenance_values.clone()
        ),
        2
    );
    assert_eq!(
        projected_int(
            "maintenance_pending_bytes",
            "job",
            maintenance_values.clone()
        ),
        pending_metadata.pending_bytes
    );
    assert_eq!(
        projected_int(
            "maintenance_published_rows",
            "job",
            maintenance_values.clone()
        ),
        0
    );
    assert_eq!(
        projected_int("maintenance_published_bytes", "job", maintenance_values),
        0
    );
    assert!(matches!(
        projected_value(
            "storage_last_publication",
            "storage",
            pending_rows.sys_storage.clone(),
        )
        .raw(),
        Raw::Null
    ));
    assert!(matches!(
        projected_value(
            "maintenance_last_publication",
            "job",
            pending_rows.maintenance_job.clone(),
        )
        .raw(),
        Raw::Null
    ));

    let commit = PublicationCommitId::new(vec![b'a'; 40]).expect("valid commit id");
    state
        .complete_publication(&freeze, &commit)
        .await
        .expect("complete publication");
    // A replayed successful completion must not double-count the prefix.
    state
        .complete_publication(&freeze, &commit)
        .await
        .expect("retry publication completion");

    let published_metadata = state
        .publication_metadata()
        .await
        .expect("read published metadata");
    let published_rows = state
        .publication_metadata_rows()
        .await
        .expect("project published metadata rows");
    assert_eq!(published_metadata.pending_rows, 1);
    assert_eq!(
        published_metadata.pending_bytes,
        trailing_mutation.payload.len() as u64
    );
    assert_eq!(published_metadata.published_rows, 1);
    assert_eq!(
        published_metadata.published_bytes,
        published_mutation.payload.len() as u64
    );
    assert!(published_metadata.last_publication_ms.is_some());
    assert_eq!(
        projected_int(
            "storage_published_rows",
            "storage",
            published_rows.sys_storage.clone()
        ),
        1
    );
    assert_eq!(
        projected_int(
            "maintenance_published_bytes",
            "job",
            published_rows.maintenance_job.clone(),
        ),
        published_mutation.payload.len() as u64
    );
    assert!(matches!(
        projected_value(
            "storage_last_publication",
            "storage",
            published_rows.sys_storage.clone(),
        )
        .raw(),
        Raw::Tag(60002, _)
    ));

    drop(state);
    let reopened = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("reopen runtime");
    let reopened_metadata = reopened
        .publication_metadata()
        .await
        .expect("read persisted publication metadata");
    let reopened_rows = reopened
        .publication_metadata_rows()
        .await
        .expect("project reopened publication rows");
    assert_eq!(reopened_metadata, published_metadata);
    assert_eq!(
        projected_int(
            "storage_published_rows",
            "storage",
            reopened_rows.sys_storage,
        ),
        1
    );
    assert_eq!(
        projected_int(
            "maintenance_published_rows",
            "job",
            reopened_rows.maintenance_job,
        ),
        1
    );
    assert_eq!(
        reopened.pending().await.expect("read pending tail"),
        vec![trailing_mutation]
    );
}
