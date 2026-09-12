//! Bounded public RuntimeState API audit regressions for Orna 1.0.0.
//!
//! Tracking: https://github.com/workingdir/ornadb/issues/787 (ornadb-gov5.17.5).
//! Ignored regressions are known failures, not conformance passes; execute them
//! explicitly with `--ignored` when collecting review evidence.
//!
//! Normative authority in reference/Orna-1.0.0/source/:
//! - 29-formats.md, "Snapshot encoding": a runtime generation must never
//!   identify two different logical states; ORNA-FORMAT-002 pins a CWD reference
//!   to its captured generation.
//! - 15-system.md, ORNA-SYS-017: a RevisionId identifies one immutable revision.
//! - 34-system-reference.md, sys.Revision: immutable semantic revision, key id.
//!
//! Test repositories and runtime files use isolated workspace-local artifact
//! directories. No source evaluation or CLI conformance is claimed.

use std::{path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CatalogueAdmission, CatalogueAdmissionResult, CatalogueDeclaration, CatalogueError,
    CatalogueFunctionDeclaration, CatalogueObjectKind, CatalogueParameterDeclaration,
    CatalogueTypeDeclaration, CatalogueTypeForm, CatalogueTypeSpec, RuntimeIdentity, RuntimeState,
    WriterLease,
};
use tempfile::{Builder, TempDir};

fn declaration(
    name: &str,
    kind: CatalogueObjectKind,
    revision: u8,
    semantic: u8,
) -> CatalogueDeclaration {
    CatalogueDeclaration {
        qualified_name: name.into(),
        kind,
        revision_id: [revision; 32],
        semantic_hash: [semantic; 32],
        rename_from: None,
    }
}

fn named_type(name: &str, revision: u8, semantic: u8) -> CatalogueTypeDeclaration {
    CatalogueTypeDeclaration {
        declaration: declaration(name, CatalogueObjectKind::Type, revision, semantic),
        form: CatalogueTypeSpec::Named,
    }
}

// Follows src/catalogue.rs's complete-batch declaration helpers. These bytes
// are synthetic API inputs; assertions compare retained facts, not minted IDs.
fn complete_batch() -> CatalogueAdmission {
    CatalogueAdmission {
        predecessor_capture: None,
        types: vec![named_type("pkg.T", 4, 5)],
        functions: vec![CatalogueFunctionDeclaration {
            declaration: declaration("pkg.f", CatalogueObjectKind::Function, 12, 13),
            parameters: vec![CatalogueParameterDeclaration {
                name: "x".into(),
                position: 0,
                type_name: "pkg.T".into(),
            }],
            result_type_name: "pkg.T".into(),
        }],
    }
}

fn repository() -> (TempDir, Repository) {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("create workspace test artifact directory");
    let directory = Builder::new()
        .prefix("orna-v1-catalogue-review-")
        .tempdir_in(&target)
        .expect("create review repository directory");
    // Same Git-init/discover pattern as src/lib.rs's repository fixture; these
    // tests never commit, so they need no author identity or user configuration.
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init for review fixture");
    assert!(initialized.status.success(), "initialize review repository");
    let repository = Repository::discover(directory.path()).expect("discover review repository");
    (directory, repository)
}

async fn admitted_state() -> (
    TempDir,
    RuntimeState,
    WriterLease,
    CatalogueAdmission,
    CatalogueAdmissionResult,
) {
    let (directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open fresh runtime");
    let lease = state
        .acquire_lease([15; 16])
        .await
        .expect("acquire writer lease");
    let capture = state.capture().await.expect("capture initial generation");
    let batch = complete_batch();
    let first = state
        .admit_catalogue_at(lease, &capture, batch.clone())
        .await
        .expect("admit initial complete catalogue");
    assert_eq!(first.capture, capture);
    assert_eq!(first.types.len(), batch.types.len());
    assert_eq!(first.functions.len(), batch.functions.len());
    assert_eq!(
        first.functions[0].parameters.len(),
        batch.functions[0].parameters.len()
    );
    assert_unchanged(&state, &first).await;
    (directory, state, lease, batch, first)
}

async fn assert_unchanged(state: &RuntimeState, first: &CatalogueAdmissionResult) {
    assert_eq!(
        state.capture().await.expect("capture after replay"),
        first.capture,
        "replay must preserve the exact snapshot and generation digest"
    );
    assert_eq!(
        state
            .catalogue_type("pkg.T", CatalogueTypeForm::Named)
            .await
            .expect("read original type"),
        Some(first.types[0].clone()),
        "replay must preserve type identity, form and pinned reference"
    );
    assert_eq!(
        state
            .catalogue_function("pkg.f")
            .await
            .expect("read original function"),
        Some(first.functions[0].clone()),
        "replay must preserve object/revision identity, hash, references and full signature"
    );
    assert_eq!(
        state
            .catalogue_type("pkg.U", CatalogueTypeForm::Named)
            .await
            .expect("check absent declaration"),
        None,
        "replay must not append a declaration to an already admitted snapshot"
    );
}

#[tokio::test]
async fn catalogue_identical_same_snapshot_replay_is_idempotent() {
    let (_directory, state, lease, batch, first) = admitted_state().await;
    let replay = state
        .admit_catalogue_at(lease, &first.capture, batch)
        .await
        .expect("identical complete catalogue replay is accepted");
    assert_eq!(
        replay, first,
        "identical replay must return identical catalogue facts"
    );
    assert_unchanged(&state, &first).await;
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #787; run explicitly for review"]
async fn catalogue_same_snapshot_additive_replay_is_rejected_without_changes() {
    let (_directory, state, lease, mut batch, first) = admitted_state().await;
    batch.types.push(named_type("pkg.U", 6, 7));
    let replay = state.admit_catalogue_at(lease, &first.capture, batch).await;
    // ORNA-FORMAT-002 and chapter 29's generation invariant: once this complete
    // catalogue is observable, a strict superset cannot inhabit the same pin.
    assert!(
        replay.is_err(),
        "an admitted snapshot must reject added declarations"
    );
    assert_unchanged(&state, &first).await;
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #787; run explicitly for review"]
async fn catalogue_same_revision_appended_parameter_is_rejected_without_changes() {
    let (_directory, state, lease, mut batch, first) = admitted_state().await;
    batch.functions[0]
        .parameters
        .push(CatalogueParameterDeclaration {
            name: "y".into(),
            position: 1,
            type_name: "pkg.T".into(),
        });
    // Only the signature changes: the declaration's revision and hash stay
    // untouched. ORNA-SYS-017 prohibits giving that revision another meaning.
    let replay = state.admit_catalogue_at(lease, &first.capture, batch).await;
    assert!(
        replay.is_err(),
        "an immutable revision must reject an appended parameter"
    );
    assert_unchanged(&state, &first).await;
}

#[tokio::test]
async fn catalogue_same_revision_removed_parameter_is_rejected_without_changes() {
    let (_directory, state, lease, mut batch, first) = admitted_state().await;
    batch.functions[0].parameters.clear();
    let replay = state.admit_catalogue_at(lease, &first.capture, batch).await;
    assert_eq!(replay, Err(CatalogueError::CatalogueRevisionConflict));
    assert_unchanged(&state, &first).await;
}

#[tokio::test]
async fn catalogue_same_revision_changed_hash_is_rejected_without_changes() {
    let (_directory, state, lease, mut batch, first) = admitted_state().await;
    batch.functions[0].declaration.semantic_hash = [14; 32];
    let replay = state.admit_catalogue_at(lease, &first.capture, batch).await;
    assert_eq!(replay, Err(CatalogueError::CatalogueRevisionConflict));
    assert_unchanged(&state, &first).await;
}
