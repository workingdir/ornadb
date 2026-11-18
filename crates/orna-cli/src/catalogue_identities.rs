//! The committed identities the `sys.catalog.*` rows of a CLI runtime carry.
//!
//! `orna run`, `orna repl` and `orna serve` serve the same rows from the same
//! source: the object identity inventory committed at one commit names every
//! table (`ObjectKind::Table`) and every column (`ObjectKind::Field`, qualified
//! `Table.column`) with a stable id, and that commit is the snapshot every
//! reference is pinned to (ORNA-SYS-111, ORNA-SYS-112). The runtime's database
//! id names the database each reference belongs to.

use std::collections::BTreeMap;
use std::path::Path;

use orna_application::AdmittedApplication;
use orna_check::catalogue_identities::{CatalogueIdentities, TableIdentity};
use orna_project::{ObjectIdentityManifest, ObjectKind};
use orna_repository::{GitCommitRef, GitHashAlgorithm, NativeOid, Repository};
use orna_value::{CanonicalSnapshot, GitHash};

use crate::Diagnostic;
use crate::committed::unavailable_table_identity;

/// The identities `manifest` records for the live tables and columns, pinned to
/// `commit` of the database `database_id`. A table or column the manifest
/// records more than once makes the inventory unusable, so the caller fails
/// closed with a reason.
pub(crate) fn catalogue_identities(
    manifest: &ObjectIdentityManifest,
    commit: &GitCommitRef,
    database_id: [u8; 16],
) -> Result<CatalogueIdentities, Diagnostic> {
    let mut objects: BTreeMap<&str, [u8; 16]> = BTreeMap::new();
    for record in manifest.records() {
        if record.kind() == ObjectKind::Table
            && !record.is_retired()
            && objects
                .insert(record.qualified_name(), *record.id())
                .is_some()
        {
            return Err(unavailable_table_identity(format!(
                "table `{}` has more than one live identity at HEAD",
                record.qualified_name()
            )));
        }
    }
    let mut columns: BTreeMap<&str, BTreeMap<String, [u8; 16]>> = BTreeMap::new();
    for record in manifest.records() {
        if record.kind() != ObjectKind::Field || record.is_retired() {
            continue;
        }
        // A field is recorded as `Table.column`, and a module table's own name
        // holds dots, so the column is the last segment.
        let Some((table, column)) = record.qualified_name().rsplit_once('.') else {
            continue;
        };
        if !objects.contains_key(table) {
            continue;
        }
        if columns
            .entry(table)
            .or_default()
            .insert(column.to_owned(), *record.id())
            .is_some()
        {
            return Err(unavailable_table_identity(format!(
                "column `{}` has more than one live identity at HEAD",
                record.qualified_name()
            )));
        }
    }
    let tables = objects
        .into_iter()
        .map(|(table, object)| {
            let columns = columns.remove(table).unwrap_or_default();
            (table.to_owned(), TableIdentity::new(object, columns))
        })
        .collect();
    CatalogueIdentities::new(database_id, commit_snapshot(commit, database_id)?, tables).map_err(
        |_| unavailable_table_identity("the committed identities name another database".to_owned()),
    )
}

/// The identities committed at `commit`, read from its object identity
/// inventory.
pub(crate) fn committed_catalogue_identities(
    repository: &Repository,
    commit: &GitCommitRef,
    database_id: [u8; 16],
) -> Result<CatalogueIdentities, Diagnostic> {
    let manifest = ObjectIdentityManifest::read_committed_if_present(repository, commit)
        .map_err(|_| {
            unavailable_table_identity(
                "the committed object identity manifest could not be read".to_owned(),
            )
        })?
        .ok_or_else(|| {
            unavailable_table_identity("the commit carries no object identity inventory".to_owned())
        })?;
    catalogue_identities(&manifest, commit, database_id)
}

/// The identities committed at `head`, or `None` when the repository has no
/// commit or its inventory cannot be read. A run still executes then (bead
/// ornadb-jm0d), and a catalogue read of a project with tables fails with
/// `sys.snapshot.incomplete` instead of serving null references.
pub(crate) fn head_catalogue_identities(
    repository: &Repository,
    head: Option<&GitCommitRef>,
    database_id: [u8; 16],
) -> Option<CatalogueIdentities> {
    committed_catalogue_identities(repository, head?, database_id).ok()
}

/// `admitted` bound to the identities committed at `commit`. Identities that
/// cannot be read leave it unbound: a catalogue read of a project with tables
/// then fails with `sys.snapshot.incomplete`, and a page that reads no
/// catalogue is unaffected.
pub(crate) fn bind_committed_catalogue(
    admitted: AdmittedApplication,
    repository: &Repository,
    commit: &GitCommitRef,
    database_id: [u8; 16],
) -> AdmittedApplication {
    match committed_catalogue_identities(repository, commit, database_id) {
        Ok(identities) => admitted.with_catalogue_identities(identities),
        Err(_) => admitted,
    }
}

/// The commit one `orna serve` request is served at, and what is committed in it.
pub(crate) struct RequestCommit {
    /// The raw Git object id of the commit `HEAD` names, the pin of the
    /// `sys.git.*` rows. `None` when the clone has no readable commit.
    pub(crate) head: Option<Vec<u8>>,
    /// The identities committed at `head`. `None` when there is no commit or
    /// its inventory cannot be read.
    pub(crate) identities: Option<CatalogueIdentities>,
}

/// The commit the clone at `root` has now. A server outlives every commit it
/// serves, so each request reads `HEAD` afresh and binds what it reads to that
/// one request (ORNA-SERVE-003, ORNA-SYS-011). A clone with no readable commit
/// yields neither, and the rows that need them fail closed with
/// `sys.snapshot.incomplete`.
pub(crate) fn request_commit(root: &Path, database_id: [u8; 16]) -> RequestCommit {
    let repository = Repository::discover(root).ok();
    let head = repository
        .as_ref()
        .and_then(|repository| repository.head().ok().flatten());
    let identities = repository
        .as_ref()
        .and_then(|repository| head_catalogue_identities(repository, head.as_ref(), database_id));
    RequestCommit {
        head: head.as_ref().and_then(crate::sys_host::commit_object_id),
        identities,
    }
}

/// The commit pin every reference of the catalogue carries.
fn commit_snapshot(
    commit: &GitCommitRef,
    database_id: [u8; 16],
) -> Result<CanonicalSnapshot, Diagnostic> {
    let unreadable =
        || unavailable_table_identity("the commit id is not a Git object id".to_owned());
    let (algorithm, hash) = match commit.as_str().len() {
        40 => (GitHashAlgorithm::Sha1, GitHash::Sha1),
        64 => (GitHashAlgorithm::Sha256, GitHash::Sha256),
        _ => return Err(unreadable()),
    };
    let oid = NativeOid::from_hex(algorithm, commit.as_str()).map_err(|_| unreadable())?;
    Ok(CanonicalSnapshot::Commit {
        database: database_id,
        algorithm: hash,
        oid: oid.as_bytes().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::request_commit;

    const DATABASE: [u8; 16] = [7; 16];

    /// Runs `git` in `root` under a fixed identity and returns its trimmed
    /// standard output.
    fn git(root: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=orna-test",
                "-c",
                "user.email=orna-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(arguments)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git prints UTF-8")
            .trim()
            .to_owned()
    }

    /// The raw bytes of a hex object id.
    fn object_id(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|start| u8::from_str_radix(&hex[start..start + 2], 16).expect("hex object id"))
            .collect()
    }

    fn repository_with_a_commit() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("temporary repository");
        git(directory.path(), &["init", "-q"]);
        git(
            directory.path(),
            &["commit", "-q", "--allow-empty", "-m", "first"],
        );
        directory
    }

    #[test]
    fn a_request_is_pinned_to_the_commit_head_names() {
        let directory = repository_with_a_commit();
        let commit = request_commit(directory.path(), DATABASE);
        assert_eq!(
            commit.head,
            Some(object_id(&git(directory.path(), &["rev-parse", "HEAD"])))
        );
        // The pin is the commit `HEAD` names, not the identity inventory: a commit
        // that records none still pins the Git state, and only the catalogue fails.
        assert!(commit.identities.is_none());
    }

    #[test]
    fn each_request_reads_head_when_it_runs() {
        let directory = repository_with_a_commit();
        let first = request_commit(directory.path(), DATABASE).head;
        git(
            directory.path(),
            &["commit", "-q", "--allow-empty", "-m", "second"],
        );
        let second = request_commit(directory.path(), DATABASE).head;
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "a request does not reuse an earlier pin");
        assert_eq!(
            second,
            Some(object_id(&git(directory.path(), &["rev-parse", "HEAD"])))
        );
    }

    #[test]
    fn a_clone_whose_head_names_no_commit_has_no_pin() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git(directory.path(), &["init", "-q"]);
        let commit = request_commit(directory.path(), DATABASE);
        assert!(commit.head.is_none());
        assert!(commit.identities.is_none());
    }
}
