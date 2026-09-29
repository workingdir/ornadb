use std::{
    cell::Cell,
    fs,
    path::Path,
    process::Command,
};

use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, SafeText};
use orna_repository_v1::Repository;
use orna_security_v1::{SecretBoundaryError, SecretMetadata, SecretRef, SecretResolver};
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_syntax_v1::parse_module_with_file;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use tempfile::TempDir;

const SECRET_REFERENCE_SOURCE: &str =
    include_str!("fixtures/security-secret-reference.orna");

struct FixtureResolver {
    available: bool,
}

impl SecretResolver for FixtureResolver {
    fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError> {
        Ok(SecretMetadata::new(
            reference.clone(),
            "sops",
            self.available,
        ))
    }

    fn with_secret<T>(
        &self,
        _reference: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, SecretBoundaryError> {
        if !self.available {
            return Err(SecretBoundaryError::Unavailable);
        }
        Ok(operation(b"secret-fixture-payload"))
    }
}

#[test]
fn reference_secret_program_is_real_parseable_and_typechecked_source() {
    assert!(parse_module_with_file(SECRET_REFERENCE_SOURCE, "secret-reference.orna").is_ok());

    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret-reference.orna",
            SECRET_REFERENCE_SOURCE,
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
}

#[test]
fn secret_reference_and_metadata_expose_only_stable_nonsecret_fields() {
    let reference = SecretRef::new("google.personal").unwrap();
    let resolver = FixtureResolver { available: true };
    let metadata = resolver.metadata(&reference).unwrap();
    let rendered = format!("{reference} {metadata:?}");

    assert_eq!(reference.to_string(), "google.personal");
    assert_eq!(metadata.reference(), &reference);
    assert_eq!(metadata.provider(), "sops");
    assert!(metadata.available());
    assert!(!rendered.contains("secret-fixture-payload"));

    let serialized_reference = serde_json::to_string(&reference).unwrap();
    assert_eq!(serialized_reference, "\"google.personal\"");
    assert_eq!(
        serde_json::from_str::<SecretRef>(&serialized_reference).unwrap(),
        reference
    );
    let serialized_metadata = serde_json::to_string(&metadata).unwrap();
    assert!(serialized_metadata.contains("google.personal"));
    assert!(serialized_metadata.contains("sops"));
    assert!(!serialized_metadata.contains("secret-fixture-payload"));
}

#[test]
fn missing_decryption_identity_keeps_metadata_readable_and_resolution_unavailable() {
    let reference = SecretRef::new("google.personal").unwrap();
    let resolver = FixtureResolver { available: false };
    let metadata = resolver.metadata(&reference).unwrap();
    let callback_calls = Cell::new(0);
    let mut callback = |_: &[u8]| callback_calls.set(callback_calls.get() + 1);

    assert_eq!(metadata.reference(), &reference);
    assert_eq!(metadata.provider(), "sops");
    assert!(!metadata.available());
    assert_eq!(
        resolver.with_secret(&reference, &mut callback),
        Err(SecretBoundaryError::Unavailable)
    );
    assert_eq!(callback_calls.get(), 0);
}

#[test]
fn redacted_secret_diagnostics_do_not_disclose_values_at_json_or_ovb_boundaries() {
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-SECRET-TEST").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new("secret-fixture-payload must remain private").unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new("secret-fixture-payload").unwrap())
    .redacted();
    let json = serde_json::to_vec(&diagnostic).unwrap();
    let ovb = diagnostic.encode_ovb().unwrap();

    assert_eq!(diagnostic.message(), "<redacted>");
    assert!(!json
        .windows(b"secret-fixture-payload".len())
        .any(|window| window == b"secret-fixture-payload"));
    assert!(!ovb
        .windows(b"secret-fixture-payload".len())
        .any(|window| window == b"secret-fixture-payload"));
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_repository() -> (TempDir, Repository) {
    let root = tempfile::tempdir_in("/var/tmp").expect("temporary Git repository");
    git(root.path(), &["init", "-b", "main"]);
    git(root.path(), &["config", "user.name", "kierandrewett"]);
    git(
        root.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(root.path().join("main.orna"), SECRET_REFERENCE_SOURCE).unwrap();
    fs::create_dir_all(root.path().join(".orna")).unwrap();
    fs::write(root.path().join(".orna/format.orna"), "format 1\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "fixture repository"]);
    let repository = Repository::discover(root.path()).unwrap();
    (root, repository)
}

#[tokio::test]
async fn embedded_runtime_opens_worktree_state_without_a_database_service() {
    let (_root, repository) = fixture_repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("the local embedded runtime state must open from the Git worktree");

    assert!(repository.runtime_paths().state_db().is_file());
    drop(state);
}

#[test]
fn runtime_owner_lock_excludes_a_second_writer_until_release() {
    let (_root, repository) = fixture_repository();
    let first = repository.acquire_runtime_owner_lock([1; 16]).unwrap();

    assert!(matches!(
        repository.acquire_runtime_owner_lock([2; 16]),
        Err(orna_repository_v1::RepositoryError::RepositoryBusy)
    ));
    drop(first);
    assert!(repository.acquire_runtime_owner_lock([2; 16]).is_ok());
}
