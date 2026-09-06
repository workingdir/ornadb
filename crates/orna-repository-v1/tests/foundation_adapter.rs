use std::{fmt, fs, path::Path, process::Command, sync::Mutex};

use num_bigint::BigInt;
use orna_foundation_v1::{
    CanonicalSnapshot, CwdCapture, CwdCas, RepositoryGenerationAdapter, RepositoryIdentity,
    RuntimeIdentityStore,
};
use orna_repository_v1::{OrnaRepositoryAdapter, OrnaRepositoryAdapterError, Repository};
use tempfile::TempDir;

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let temporary = TempDir::new().unwrap();
    git(temporary.path(), &["init", "-b", "main"]);
    git(
        temporary.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(
        temporary.path(),
        &["config", "user.name", "Foundation adapter test"],
    );
    fs::write(temporary.path().join("main.orna"), "module main;\n").unwrap();
    git(temporary.path(), &["add", "."]);
    git(temporary.path(), &["commit", "-m", "initial"]);
    temporary
}

#[derive(Debug)]
struct StoreError;
impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("test runtime store unavailable")
    }
}
impl std::error::Error for StoreError {}

struct Store {
    database: [u8; 16],
    repository: [u8; 16],
    runtime: [u8; 16],
    cwd: Mutex<CwdCapture>,
}
impl Store {
    fn new(cwd: CwdCapture) -> Self {
        Self {
            database: [1; 16],
            repository: [2; 16],
            runtime: [3; 16],
            cwd: Mutex::new(cwd),
        }
    }
}
impl RuntimeIdentityStore for Store {
    type Error = StoreError;
    fn database_id(&self) -> Result<[u8; 16], Self::Error> {
        Ok(self.database)
    }
    fn repository_id(&self) -> Result<[u8; 16], Self::Error> {
        Ok(self.repository)
    }
    fn runtime_id(&self) -> Result<[u8; 16], Self::Error> {
        Ok(self.runtime)
    }
    fn capture_cwd(&self) -> Result<CwdCapture, Self::Error> {
        Ok(self.cwd.lock().map_err(|_| StoreError)?.clone())
    }
    fn compare_and_set_cwd(
        &self,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error> {
        let mut current = self.cwd.lock().map_err(|_| StoreError)?;
        if *current != *expected {
            return Ok(CwdCas::Stale {
                current: current.clone(),
            });
        }
        *current = next.clone();
        Ok(CwdCas::Updated {
            current: next.clone(),
        })
    }
}

fn capture(generation: u64, digest: u8) -> CwdCapture {
    CwdCapture::new(
        CanonicalSnapshot::cwd([1; 16], [3; 16], BigInt::from(generation)).unwrap(),
        [digest; 32],
    )
    .unwrap()
}

#[test]
fn runtime_store_uses_full_capture_for_cas_and_adapter_enforces_identity_and_monotonicity() {
    let root = repository();
    let expected = capture(4, 9);
    let adapter = OrnaRepositoryAdapter::new(
        Repository::discover(root.path()).unwrap(),
        Store::new(expected.clone()),
    );
    let identity = adapter.require_cwd().unwrap();
    assert_eq!(adapter.capture_cwd(identity).unwrap(), expected);

    let same_generation = capture(4, 10);
    assert!(matches!(
        adapter.compare_and_set_cwd(identity, &expected, &same_generation),
        Err(OrnaRepositoryAdapterError::NonMonotonicGeneration)
    ));
    let next = capture(5, 10);
    assert!(matches!(
        adapter.compare_and_set_cwd(identity, &expected, &next),
        Ok(CwdCas::Updated { current }) if current == next
    ));
    // Same generation but an old digest is stale: the digest is part of the
    // persisted capture, rather than an advisory field read separately.
    assert!(matches!(
        adapter.compare_and_set_cwd(identity, &expected, &capture(6, 11)),
        Ok(CwdCas::Stale { current }) if current == next
    ));
    assert!(matches!(
        adapter.capture_cwd(RepositoryIdentity {
            database_id: [9; 16],
            repository_id: [2; 16]
        }),
        Err(OrnaRepositoryAdapterError::Identity)
    ));
}

#[test]
fn adapter_rejects_capture_from_another_runtime_or_database() {
    let root = repository();
    let expected = capture(1, 1);
    let adapter = OrnaRepositoryAdapter::new(
        Repository::discover(root.path()).unwrap(),
        Store::new(expected.clone()),
    );
    let identity = adapter.require_cwd().unwrap();
    let wrong_runtime = CwdCapture::new(
        CanonicalSnapshot::cwd([1; 16], [8; 16], BigInt::from(2_u8)).unwrap(),
        [2; 32],
    )
    .unwrap();
    assert!(matches!(
        adapter.compare_and_set_cwd(identity, &expected, &wrong_runtime),
        Err(OrnaRepositoryAdapterError::Identity)
    ));
}

#[test]
fn adapter_exposes_the_real_head_as_a_committed_snapshot() {
    let root = repository();
    let expected = capture(1, 1);
    let adapter = OrnaRepositoryAdapter::new(
        Repository::discover(root.path()).unwrap(),
        Store::new(expected),
    );

    let snapshot = adapter.committed_snapshot().unwrap().unwrap();
    let CanonicalSnapshot::Commit {
        database,
        algorithm,
        oid,
    } = snapshot
    else {
        panic!("expected a committed snapshot");
    };
    assert_eq!(database, [1; 16]);
    assert_eq!(algorithm, orna_foundation_v1::GitHash::Sha1);
    assert_eq!(oid.len(), 20);
}
