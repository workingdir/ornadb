//! The committed identities the `sys.catalog.*` rows of a CLI runtime carry.
//!
//! `orna run`, `orna repl` and `orna serve` serve the same rows from the same
//! source: the object identity inventory committed at one commit names every
//! table (`ObjectKind::Table`) and every column (`ObjectKind::Field`, qualified
//! `Table.column`) with a stable id, and that commit is the snapshot every
//! reference is pinned to (ORNA-SYS-111, ORNA-SYS-112). The runtime's database
//! id names the database each reference belongs to.

use std::collections::BTreeMap;

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
