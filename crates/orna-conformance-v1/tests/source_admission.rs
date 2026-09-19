use std::{fs, path::Path, process::Command};

use orna_compiler::{check, materialize_resolved_source_catalogue};
use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
    revision::{ActiveDatabaseRevision, RevisionPair, StoredSourceRevision, StoredSourceUnit},
    source::{SourceBundle, SourceUnit},
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
}

fn repository() -> (TempDir, Repository) {
    let temp = TempDir::new_in("/var/tmp").unwrap();
    git(temp.path(), &["init", "--quiet"]);
    git(
        temp.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(
        temp.path(),
        &["config", "user.name", "source admission test"],
    );
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "--quiet", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).unwrap();
    (temp, repository)
}

fn empty_active() -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        orna_core::canonical_hash::source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn real_source_candidate_projects_and_admits_at_runtime_capture() {
    let (_temp, repository) = repository();
    let runtime = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .unwrap();
    let active = empty_active();
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (value INTEGER);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check(&bundle, active.catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let resolved = materialize_resolved_source_catalogue(&report, active.pair(), &active).unwrap();
    assert_eq!(resolved.type_witnesses().len(), 1);
    assert!(resolved.type_witness_errors().is_empty());

    let capture = runtime.capture().await.unwrap();
    let admission = orna_conformance_v1::project_source_catalogue(&resolved, None).unwrap();
    assert_eq!(admission.predecessor_capture, None);
    assert_eq!(admission.types.len(), 1);
    assert!(admission.functions.is_empty());

    let writer = runtime.acquire_lease([4; 16]).await.unwrap();
    let result = runtime
        .admit_catalogue_at(writer, &capture, admission)
        .await
        .unwrap();
    assert_eq!(result.capture, capture);
    assert_eq!(runtime.capture().await.unwrap(), capture);
    assert_eq!(runtime.capture().await.unwrap(), result.capture);
}
