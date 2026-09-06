//! A bounded Orna 1.0 storage/publication slice.
//!
//! This crate models the deterministic portion of loose-row materialisation
//! and PUB-1.  It deliberately performs neither provider nor Git I/O: an
//! adapter must supply observations and enact returned plans.  Unknown or
//! changed external state is a typed conflict, never permission to overwrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orna_foundation_v1::{CwdCapture, RepositoryGenerationAdapter, RepositoryIdentity};
use orna_repository_v1::{
    GitCommitRef, ManagedFileChange, ManagedPath, PrivateCommit, Repository, RepositoryError,
};
use orna_runtime_v1::{PublicationCommitId, PublicationFreeze, RuntimeState};
use orna_value_v1::{
    path_decode_key_components, path_encode_key_components, path_validate_relative_components,
};
use sha2::{Digest, Sha256};

/// An opaque, deterministic mutation identity supplied by the runtime.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MutationId(String);

impl MutationId {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 || !value.is_ascii() {
            return Err(Error::InvalidMutationId);
        }
        Ok(Self(value))
    }
}

/// A content digest; this is used only for equality/revalidation, not as a
/// substitute for an adapter's durable Git object validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowHash([u8; 32]);

impl RowHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// A validated ordinary loose-row path derived with the v1 path codec.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LoosePath(ManagedPath);

impl LoosePath {
    /// Admits a discovered table-relative encoded row path. The components
    /// exclude the table root and include the final `.orna` extension.
    /// Decoding rejects aliases before construction; this does not perform
    /// filesystem I/O, resolve symlinks, or interpret the key's logical type.
    pub fn from_encoded_key(
        table_root: impl AsRef<str>,
        components: &[String],
    ) -> Result<Self, Error> {
        let key = path_decode_key_components(components).map_err(|_| Error::InvalidKey)?;
        Self::for_key(table_root, &key)
    }

    pub fn for_key(table_root: impl AsRef<str>, key: &[String]) -> Result<Self, Error> {
        let root = table_root.as_ref();
        if root.is_empty() || root.starts_with('.') || root.contains('/') || root.contains('\\') {
            return Err(Error::InvalidTableRoot);
        }
        let components = path_encode_key_components(key).map_err(|_| Error::InvalidKey)?;
        // Logical keys may contain separators, traversal spellings, or empty
        // strings. Only their encoded filesystem components require this check.
        path_validate_relative_components(&components).map_err(|_| Error::InvalidKey)?;
        let path = format!("{root}/{}", components.join("/"));
        ManagedPath::new(path)
            .map(Self)
            .map_err(|_| Error::UnsafePath)
    }

    pub fn as_managed_path(&self) -> &ManagedPath {
        &self.0
    }
}

impl Ord for LoosePath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_path().cmp(other.0.as_path())
    }
}

impl PartialOrd for LoosePath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseRow {
    bytes: Vec<u8>,
    hash: RowHash,
}
impl LooseRow {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::InvalidRow);
        }
        let hash = RowHash::of(&bytes);
        Ok(Self { bytes, hash })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub const fn hash(&self) -> RowHash {
        self.hash
    }
}

/// One final, already-folded runtime mutation.  `expected` is the captured
/// loose projection state and prevents an editor's later change being replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseMutation {
    pub id: MutationId,
    pub path: LoosePath,
    pub expected: Option<RowHash>,
    pub next: Option<LooseRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenBatch {
    pub id: MutationId,
    pub mutations: Vec<LooseMutation>,
    pub watermark: u64,
}

impl FrozenBatch {
    /// Enforces the complete staging gate before any materialisation can begin.
    pub fn new(
        id: MutationId,
        mutations: Vec<LooseMutation>,
        watermark: u64,
    ) -> Result<Self, Error> {
        if mutations.is_empty() || watermark == 0 {
            return Err(Error::IncompleteStaging);
        }
        let mut paths = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for mutation in &mutations {
            if mutation.id == id
                || !paths.insert(mutation.path.clone())
                || !ids.insert(mutation.id.clone())
            {
                return Err(Error::IncompleteStaging);
            }
            if mutation.next.is_none() && mutation.expected.is_none() {
                return Err(Error::IncompleteStaging);
            }
        }
        Ok(Self {
            id,
            mutations,
            watermark,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LooseProjection {
    rows: BTreeMap<LoosePath, LooseRow>,
    applied: BTreeMap<MutationId, LooseMutation>,
}

impl LooseProjection {
    pub fn row(&self, path: &LoosePath) -> Option<&LooseRow> {
        self.rows.get(path)
    }
    pub fn contains_applied(&self, id: &MutationId) -> bool {
        self.applied.contains_key(id)
    }

    /// Applies all of the frozen batch or none of it. A repeated, identical
    /// application is a no-op; a reused id with differing intent fails closed.
    pub fn project(&mut self, batch: &FrozenBatch) -> Result<(), Error> {
        let mut candidate = self.clone();
        for mutation in &batch.mutations {
            candidate.apply(mutation)?;
        }
        validate_portable_paths(candidate.rows.keys())?;
        *self = candidate;
        Ok(())
    }

    fn apply(&mut self, mutation: &LooseMutation) -> Result<(), Error> {
        if let Some(previous) = self.applied.get(&mutation.id) {
            return if previous == mutation {
                Ok(())
            } else {
                Err(Error::MutationReplayConflict {
                    id: mutation.id.clone(),
                })
            };
        }
        let actual = self.rows.get(&mutation.path).map(LooseRow::hash);
        if actual != mutation.expected {
            return Err(Error::ExternalConflict {
                path: mutation.path.clone(),
                expected: mutation.expected,
                actual,
            });
        }
        match &mutation.next {
            Some(row) => {
                self.rows.insert(mutation.path.clone(), row.clone());
            }
            None => {
                self.rows.remove(&mutation.path);
            }
        }
        self.applied.insert(mutation.id.clone(), mutation.clone());
        Ok(())
    }
}

/// The result of materialising one loose row. This operation intentionally
/// stops at the worktree projection boundary; it does not stage an index,
/// advance a ref, or claim PUB-1 completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Materialization {
    Applied,
    AlreadyApplied,
}

/// Materialises one validated loose mutation through the repository boundary.
///
/// A matching next value is idempotent. Any other unexpected current bytes are
/// reported as an external conflict and are never overwritten.
pub fn materialize_loose_mutation(
    repository: &Repository,
    mutation: &LooseMutation,
) -> Result<Materialization, Error> {
    let current = repository
        .managed_file_bytes(mutation.path.as_managed_path())
        .map_err(|_| Error::RepositoryUnavailable)?;
    let actual = current.as_deref().map(RowHash::of);
    let desired = mutation.next.as_ref().map(LooseRow::hash);
    if actual == desired {
        return Ok(Materialization::AlreadyApplied);
    }
    if actual != mutation.expected {
        return Err(Error::ExternalConflict {
            path: mutation.path.clone(),
            expected: mutation.expected,
            actual,
        });
    }
    match repository.materialize_managed_file(
        mutation.path.as_managed_path(),
        current.as_deref(),
        mutation.next.as_ref().map(LooseRow::bytes),
    ) {
        Ok(()) => Ok(Materialization::Applied),
        Err(RepositoryError::ManagedContentConflict) => {
            let actual = repository
                .managed_file_bytes(mutation.path.as_managed_path())
                .map_err(|_| Error::RepositoryUnavailable)?
                .as_deref()
                .map(RowHash::of);
            Err(Error::ExternalConflict {
                path: mutation.path.clone(),
                expected: mutation.expected,
                actual,
            })
        }
        Err(_) => Err(Error::RepositoryUnavailable),
    }
}

/// Builds the real Git candidate for a frozen batch without advancing a ref.
/// The runtime batch remains authoritative until the caller journals, performs
/// index/worktree reconciliation, and completes the publication boundary.
pub fn build_private_publication_candidate(
    repository: &Repository,
    expected_head: &GitCommitRef,
    batch: &FrozenBatch,
    message: &str,
) -> Result<PrivateCommit, Error> {
    validate_portable_paths(
        batch
            .mutations
            .iter()
            .filter_map(|mutation| mutation.next.as_ref().map(|_| &mutation.path)),
    )?;
    let changes = batch
        .mutations
        .iter()
        .map(|mutation| {
            ManagedFileChange::new(
                mutation.path.as_managed_path().clone(),
                mutation.next.as_ref().map(|row| row.bytes().to_vec()),
            )
        })
        .collect::<Vec<_>>();
    repository
        .build_private_commit(expected_head, &changes, message)
        .map_err(|_| Error::RepositoryUnavailable)
}

fn validate_portable_paths<'a>(
    paths: impl IntoIterator<Item = &'a LoosePath>,
) -> Result<(), Error> {
    let mut siblings = BTreeMap::new();
    for path in paths {
        let mut prefix = Vec::new();
        for (index, component) in path.0.as_path().iter().enumerate() {
            let component = component.to_str().ok_or(Error::UnsafePath)?;
            // Table roots are supplied by the schema adapter; collisions here
            // concern encoded key siblings within the same table.
            prefix.push(if index == 0 {
                component.to_owned()
            } else {
                component.to_ascii_lowercase()
            });
            if let Some(previous) = siblings.insert(prefix.clone(), component)
                && previous != component
            {
                return Err(Error::PathCollision);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefName(String);
impl RefName {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() || !value.starts_with("refs/") {
            Err(Error::InvalidRef)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitObjectId(String);
impl GitObjectId {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.len() < 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Err(Error::InvalidObjectId)
        } else {
            Ok(Self(value))
        }
    }
}

/// A byte-exact, simplified ordinary index image. Its map representation makes
/// the carry-forward rule observable without claiming to implement Git's file format.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexImage(BTreeMap<LoosePath, Option<LooseRow>>);
impl IndexImage {
    pub fn entry(&self, path: LoosePath, row: Option<LooseRow>) -> Self {
        let mut next = self.clone();
        next.0.insert(path, row);
        next
    }
    pub fn get(&self, path: &LoosePath) -> Option<&Option<LooseRow>> {
        self.0.get(path)
    }
}

/// Carry the staged difference from H to the private candidate N. Any staged
/// managed-path difference is an overlap and must have been rejected at capture.
pub fn reconcile_index(
    base: &IndexImage,
    ordinary: &IndexImage,
    publication: &FrozenBatch,
) -> Result<IndexImage, Error> {
    let mut result = ordinary.clone();
    for mutation in &publication.mutations {
        if ordinary.get(&mutation.path) != base.get(&mutation.path) {
            return Err(Error::IndexConflict {
                path: mutation.path.clone(),
            });
        }
        result
            .0
            .insert(mutation.path.clone(), mutation.next.clone());
    }
    validate_portable_paths(
        result
            .0
            .iter()
            .filter_map(|(path, row)| row.as_ref().map(|_| path)),
    )?;
    Ok(result)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PubJournal {
    pub batch: FrozenBatch,
    pub target: RefName,
    pub old: GitObjectId,
    pub new: GitObjectId,
    pub base_index: IndexImage,
    pub reconciled_index: IndexImage,
    pub stage: JournalStage,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalStage {
    Journaled,
    RefAdvanced,
    IndexReconciled,
    Cleaned,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    journal: PubJournal,
}
impl Publication {
    pub fn prepare(
        batch: FrozenBatch,
        target: RefName,
        old: GitObjectId,
        new: GitObjectId,
        base: IndexImage,
        ordinary: IndexImage,
    ) -> Result<Self, Error> {
        let reconciled_index = reconcile_index(&base, &ordinary, &batch)?;
        Ok(Self {
            journal: PubJournal {
                batch,
                target,
                old,
                new,
                base_index: ordinary,
                reconciled_index,
                stage: JournalStage::Journaled,
            },
        })
    }
    pub fn journal(&self) -> &PubJournal {
        &self.journal
    }
    pub fn advance_ref(&mut self, observed: &GitObjectId) -> Result<(), Error> {
        if observed != &self.journal.old {
            return Err(Error::RefConflict);
        }
        self.journal.stage = JournalStage::RefAdvanced;
        Ok(())
    }
    pub fn install_index(&mut self, observed: &IndexImage) -> Result<(), Error> {
        if self.journal.stage != JournalStage::RefAdvanced {
            return Err(Error::InvalidTransition);
        }
        if observed != &self.journal.base_index {
            return Err(Error::RecoveryIndexConflict);
        }
        self.journal.stage = JournalStage::IndexReconciled;
        Ok(())
    }
    pub fn cleanup(&mut self) -> Result<(), Error> {
        if self.journal.stage != JournalStage::IndexReconciled {
            return Err(Error::InvalidTransition);
        }
        self.journal.stage = JournalStage::Cleaned;
        Ok(())
    }
    pub fn complete(&mut self) -> Result<(), Error> {
        if self.journal.stage != JournalStage::Cleaned {
            return Err(Error::InvalidTransition);
        }
        self.journal.stage = JournalStage::Complete;
        Ok(())
    }

    /// Computes the only safe recovery action; Git/filesystem effects remain
    /// unsupported adapter responsibilities.
    pub fn recover(
        &mut self,
        observed_ref: &GitObjectId,
        observed_index: &IndexImage,
    ) -> Result<Recovery, Error> {
        if observed_ref == &self.journal.old {
            if observed_index != &self.journal.base_index {
                return Err(Error::RecoveryIndexConflict);
            }
            self.journal.stage = JournalStage::Journaled;
            return Ok(Recovery::KeepPending);
        }
        if observed_ref != &self.journal.new {
            return Err(Error::RefConflict);
        }
        if observed_index == &self.journal.base_index {
            // The durable journal can prove that reconciliation is still
            // required, but it cannot claim the index was installed before
            // the adapter performs that action.
            self.journal.stage = JournalStage::RefAdvanced;
            return Ok(Recovery::InstallIndex(
                self.journal.reconciled_index.clone(),
            ));
        }
        if observed_index == &self.journal.reconciled_index {
            self.journal.stage = JournalStage::Cleaned;
            return Ok(Recovery::FinishCleanup);
        }
        Err(Error::RecoveryIndexConflict)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Recovery {
    KeepPending,
    InstallIndex(IndexImage),
    FinishCleanup,
}

/// Explicitly unsupported I/O boundary. Consumers may provide an adapter only
/// after separately meeting PUB-1's locking, flushing and Git CAS obligations.
pub trait PublicationProvider {
    type Error: std::error::Error + Send + Sync + 'static;
    fn unsupported_git_io(&self) -> Result<(), Self::Error>;
}

/// A compile-time bridge to the existing v1 repository/runtime direction-only
/// contract, without assuming a provider can perform publication I/O.
pub fn capture_runtime<A: RepositoryGenerationAdapter>(
    adapter: &A,
) -> Result<(RepositoryIdentity, CwdCapture), Error> {
    let identity = adapter
        .require_cwd()
        .map_err(|_| Error::RuntimeUnavailable)?;
    let capture = adapter
        .capture_cwd(identity)
        .map_err(|_| Error::RuntimeUnavailable)?;
    Ok((identity, capture))
}

/// Binds a journal entry to the existing runtime's durable freeze identity.
/// The caller retains responsibility for reading and consuming the frozen range.
pub const fn runtime_freeze_id(freeze: &orna_runtime_v1::PublicationFreeze) -> [u8; 16] {
    freeze.intent_id
}

/// Completes the runtime side of publication after the repository adapter has
/// verified the candidate's ref, index, and managed worktree boundaries.
/// Repeating the same candidate is safe; newer runtime mutations remain in the
/// pending tail because the runtime consumes only the supplied freeze prefix.
pub async fn complete_runtime_publication(
    state: &RuntimeState,
    freeze: &PublicationFreeze,
    candidate: &PrivateCommit,
) -> Result<(), Error> {
    let commit = PublicationCommitId::new(candidate.commit().as_str().as_bytes().to_vec())
        .map_err(|_| Error::RuntimeUnavailable)?;
    state
        .complete_publication(freeze, &commit)
        .await
        .map_err(|_| Error::RuntimeUnavailable)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidMutationId,
    InvalidTableRoot,
    InvalidKey,
    UnsafePath,
    InvalidRow,
    PathCollision,
    IncompleteStaging,
    InvalidRef,
    InvalidObjectId,
    MutationReplayConflict {
        id: MutationId,
    },
    ExternalConflict {
        path: LoosePath,
        expected: Option<RowHash>,
        actual: Option<RowHash>,
    },
    IndexConflict {
        path: LoosePath,
    },
    RecoveryIndexConflict,
    RefConflict,
    InvalidTransition,
    RuntimeUnavailable,
    RepositoryUnavailable,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidMutationId => "invalid mutation id",
            Self::InvalidTableRoot => "invalid table root",
            Self::InvalidKey => "invalid loose row key",
            Self::UnsafePath => "unsafe loose row path",
            Self::InvalidRow => "invalid loose row",
            Self::PathCollision => "portable loose-row path collision",
            Self::IncompleteStaging => "incomplete publication staging",
            Self::InvalidRef => "invalid ref",
            Self::InvalidObjectId => "invalid object id",
            Self::MutationReplayConflict { .. } => "mutation replay conflicts",
            Self::ExternalConflict { .. } => "external loose-row conflict",
            Self::IndexConflict { .. } => "managed path conflicts with ordinary index",
            Self::RecoveryIndexConflict => "recovery index conflict",
            Self::RefConflict => "publication ref conflict",
            Self::InvalidTransition => "invalid publication transition",
            Self::RuntimeUnavailable => "runtime contract unavailable",
            Self::RepositoryUnavailable => "repository projection is unavailable",
        })
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_runtime_v1::{RuntimeIdentity, TableMutation};
    use std::{fs, path::Path, process::Command};
    use tempfile::TempDir;

    fn git(directory: &Path, arguments: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(directory)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    fn git_output(directory: &Path, arguments: &[&str]) -> String {
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
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    }

    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().unwrap();
        git(temp.path(), &["init", "-b", "main"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "Storage test"]);
        fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    fn path() -> LoosePath {
        LoosePath::for_key("tables", &["sensor".into(), "one".into()]).unwrap()
    }
    fn row(text: &str) -> LooseRow {
        LooseRow::new(text.as_bytes().to_vec()).unwrap()
    }
    #[test]
    fn logical_keys_are_encoded_before_path_safety_validation() {
        for (key, encoded) in [
            ("", "~ff"),
            (".", "~2e"),
            ("..", "~2e~2e"),
            ("foo/bar", "foo~2fbar"),
            ("foo\\bar", "foo~5cbar"),
            ("/outside", "~2foutside"),
            (".git", "~2egit"),
            ("con", "~63on"),
            ("é", "~c3~a9"),
        ] {
            let path = LoosePath::for_key("Contact", &[key.into()]).unwrap();
            assert_eq!(
                path.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{encoded}.orna"))
            );
            assert_eq!(orna_value_v1::path_decode_component(encoded).unwrap(), key);
        }
        let composite = LoosePath::for_key("Contact", &["..".into(), "a/b".into()]).unwrap();
        assert_eq!(
            composite.as_managed_path().as_path(),
            std::path::Path::new("Contact/~2e~2e/a~2fb.orna")
        );
        assert!(LoosePath::for_key("Contact", &[]).is_err());
        assert!(LoosePath::for_key("Contact", &["a".repeat(201)]).is_err());
        assert!(LoosePath::for_key("Contact", &vec!["a".repeat(200); 6]).is_err());
        for root in ["", ".", "..", "../Contact", "/Contact", "a\\b"] {
            assert!(LoosePath::for_key(root, &["safe".into()]).is_err());
        }
    }
    #[test]
    fn discovered_paths_must_be_canonical_before_admission() {
        for (keys, components) in [
            (vec![""], vec!["~ff.orna"]),
            (vec!["..", "foo/bar"], vec!["~2e~2e", "foo~2fbar.orna"]),
            (
                vec!["first.orna", "last.orna"],
                vec!["first.orna", "last.orna.orna"],
            ),
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            let discovered = LoosePath::from_encoded_key("Contact", &components).unwrap();
            assert_eq!(discovered, LoosePath::for_key("Contact", &keys).unwrap());
            assert_eq!(
                discovered.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{}", components.join("/")))
            );
        }
        for components in [
            vec![],
            vec!["alice"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["..", "alice.orna"],
            vec!["/alice.orna"],
            vec!["a\\alice.orna"],
        ] {
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            assert_eq!(
                LoosePath::from_encoded_key("Contact", &components),
                Err(Error::InvalidKey)
            );
        }
        assert!(LoosePath::from_encoded_key("../Contact", &["alice.orna".into()]).is_err());
    }
    #[test]
    fn portable_sibling_collisions_reject_the_entire_candidate() {
        let mut independent = LooseProjection::default();
        for (name, table, keys) in [
            ("one", "Contact", vec!["Alice", "one"]),
            ("two", "Contact", vec!["Alice", "two"]),
            ("other-table", "Other", vec!["alice", "one"]),
        ] {
            let mutation = LooseMutation {
                id: id(name),
                path: LoosePath::for_key(
                    table,
                    &keys.into_iter().map(String::from).collect::<Vec<_>>(),
                )
                .unwrap(),
                expected: None,
                next: Some(row("body")),
            };
            independent
                .project(&FrozenBatch::new(id("independent"), vec![mutation], 1).unwrap())
                .unwrap();
        }
        assert_eq!(independent.rows.len(), 3);
        for (first, second) in [
            (vec!["Alice"], vec!["alice"]),
            (vec!["Alice", "one"], vec!["alice", "two"]),
        ] {
            let key = |parts: Vec<&str>| {
                LoosePath::for_key(
                    "Contact",
                    &parts.into_iter().map(String::from).collect::<Vec<_>>(),
                )
                .unwrap()
            };
            let first = key(first);
            let second = key(second);
            let insert = |name, path| LooseMutation {
                id: id(name),
                path,
                expected: None,
                next: Some(row("body")),
            };
            let a = insert("first", first.clone());
            let b = insert("second", second.clone());
            for mutations in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
                let mut projection = LooseProjection::default();
                let batch = FrozenBatch::new(id("both"), mutations, 1).unwrap();
                assert_eq!(projection.project(&batch), Err(Error::PathCollision));
                assert_eq!(projection, LooseProjection::default());
            }
            let mut projection = LooseProjection::default();
            projection
                .project(&FrozenBatch::new(id("initial"), vec![a], 1).unwrap())
                .unwrap();
            let before = projection.clone();
            assert_eq!(
                projection.project(&FrozenBatch::new(id("conflict"), vec![b.clone()], 2).unwrap()),
                Err(Error::PathCollision)
            );
            assert_eq!(projection, before);
            let delete = LooseMutation {
                id: id("delete"),
                path: first,
                expected: Some(row("body").hash()),
                next: None,
            };
            for mutations in [vec![b.clone(), delete.clone()], vec![delete, b]] {
                let mut replaced = before.clone();
                replaced
                    .project(&FrozenBatch::new(id("replace"), mutations, 3).unwrap())
                    .unwrap();
                assert!(replaced.row(&second).is_some());
                assert_eq!(replaced.rows.len(), 1);
            }
        }
    }
    fn id(text: &str) -> MutationId {
        MutationId::new(text).unwrap()
    }
    fn batch(expected: Option<RowHash>, next: Option<LooseRow>) -> FrozenBatch {
        FrozenBatch::new(
            id("batch"),
            vec![LooseMutation {
                id: id("mutation"),
                path: path(),
                expected,
                next,
            }],
            1,
        )
        .unwrap()
    }
    fn objects() -> (GitObjectId, GitObjectId) {
        (
            GitObjectId::new("aaaa").unwrap(),
            GitObjectId::new("bbbb").unwrap(),
        )
    }
    fn publication(base: IndexImage, ordinary: IndexImage) -> Publication {
        let (old, new) = objects();
        Publication::prepare(
            batch(None, Some(row("published"))),
            RefName::new("refs/heads/main").unwrap(),
            old,
            new,
            base,
            ordinary,
        )
        .unwrap()
    }

    #[test]
    fn publication_validates_portable_paths_in_its_final_candidate() {
        let path = |key: Vec<&str>| {
            LoosePath::for_key(
                "Contact",
                &key.into_iter().map(String::from).collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let mutation = |name, path, expected, next| LooseMutation {
            id: id(name),
            path,
            expected,
            next,
        };
        let prepare = |batch, base, ordinary| {
            let (old, new) = objects();
            Publication::prepare(
                batch,
                RefName::new("refs/heads/main").unwrap(),
                old,
                new,
                base,
                ordinary,
            )
        };

        let upper = path(vec!["Alice"]);
        let lower = path(vec!["alice"]);
        let same_batch = FrozenBatch::new(
            id("same-batch"),
            vec![
                mutation("same-upper", upper.clone(), None, Some(row("upper"))),
                mutation("same-lower", lower.clone(), None, Some(row("lower"))),
            ],
            1,
        )
        .unwrap();
        assert!(matches!(
            prepare(same_batch, IndexImage::default(), IndexImage::default()),
            Err(Error::PathCollision)
        ));

        let base = IndexImage::default().entry(upper.clone(), Some(row("old")));
        let ordinary = base.clone();
        let pre_existing = FrozenBatch::new(
            id("pre-existing"),
            vec![mutation(
                "pre-existing-lower",
                lower.clone(),
                None,
                Some(row("new")),
            )],
            2,
        )
        .unwrap();
        assert!(matches!(
            prepare(pre_existing, base.clone(), ordinary.clone()),
            Err(Error::PathCollision)
        ));
        assert_eq!(base.get(&upper).unwrap().as_ref().unwrap().bytes(), b"old");
        assert_eq!(ordinary, base);

        let nested_upper = path(vec!["Alice", "one"]);
        let nested_lower = path(vec!["alice", "two"]);
        let base = IndexImage::default().entry(nested_upper, Some(row("old")));
        let composite_collision = FrozenBatch::new(
            id("composite"),
            vec![mutation(
                "composite-lower",
                nested_lower,
                None,
                Some(row("new")),
            )],
            3,
        )
        .unwrap();
        assert!(matches!(
            prepare(composite_collision, base.clone(), base),
            Err(Error::PathCollision)
        ));

        let delete = mutation(
            "rename-delete",
            upper.clone(),
            Some(row("old").hash()),
            None,
        );
        let insert = mutation("rename-insert", lower.clone(), None, Some(row("new")));
        for mutations in [
            vec![delete.clone(), insert.clone()],
            vec![insert.clone(), delete.clone()],
        ] {
            let renamed = prepare(
                FrozenBatch::new(id("case-rename"), mutations, 4).unwrap(),
                IndexImage::default().entry(upper.clone(), Some(row("old"))),
                IndexImage::default().entry(upper.clone(), Some(row("old"))),
            )
            .unwrap();
            assert_eq!(renamed.journal().reconciled_index.get(&upper), Some(&None));
            assert_eq!(
                renamed
                    .journal()
                    .reconciled_index
                    .get(&lower)
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .bytes(),
                b"new"
            );
        }
    }

    #[test]
    fn publication_collision_preserves_unrelated_ordinary_staging() {
        let upper = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        let lower = LoosePath::for_key("Contact", &["alice".into()]).unwrap();
        let unrelated = LoosePath::for_key("Contact", &["unrelated".into()]).unwrap();
        let base = IndexImage::default().entry(upper.clone(), Some(row("head")));
        let ordinary = base
            .clone()
            .entry(unrelated.clone(), Some(row("staged-only")));
        let before = ordinary.clone();
        let batch = FrozenBatch::new(
            id("collision"),
            vec![LooseMutation {
                id: id("collision-lower"),
                path: lower,
                expected: None,
                next: Some(row("published")),
            }],
            1,
        )
        .unwrap();
        let (old, new) = objects();

        assert!(matches!(
            Publication::prepare(
                batch,
                RefName::new("refs/heads/main").unwrap(),
                old,
                new,
                base,
                ordinary.clone(),
            ),
            Err(Error::PathCollision)
        ));
        assert_eq!(ordinary, before);
        assert_eq!(
            ordinary.get(&unrelated).unwrap().as_ref().unwrap().bytes(),
            b"staged-only"
        );
        assert!(ordinary.get(&upper).unwrap().is_some());
    }

    #[test]
    fn projection_is_idempotent_under_fault_retry() {
        let mut projection = LooseProjection::default();
        let frozen = batch(None, Some(row("one")));
        projection.project(&frozen).unwrap();
        projection.project(&frozen).unwrap();
        assert_eq!(projection.row(&path()).unwrap().bytes(), b"one");
    }
    #[test]
    fn external_edit_is_a_typed_conflict() {
        let mut projection = LooseProjection::default();
        projection
            .project(&batch(None, Some(row("editor"))))
            .unwrap();
        let later = FrozenBatch::new(
            id("batch-two"),
            vec![LooseMutation {
                id: id("mutation-two"),
                path: path(),
                expected: None,
                next: Some(row("runtime")),
            }],
            2,
        )
        .unwrap();
        let error = projection.project(&later).unwrap_err();
        assert!(matches!(error, Error::ExternalConflict { .. }));
    }
    #[test]
    fn complete_staging_gate_rejects_ambiguous_delete() {
        assert!(matches!(
            FrozenBatch::new(
                id("batch"),
                vec![LooseMutation {
                    id: id("mutation"),
                    path: path(),
                    expected: None,
                    next: None
                }],
                1
            ),
            Err(Error::IncompleteStaging)
        ));
    }
    #[test]
    fn recovery_before_ref_advance_keeps_pending() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        let (old, _) = objects();
        assert_eq!(
            publication.recover(&old, &base).unwrap(),
            Recovery::KeepPending
        );
        assert_eq!(publication.journal().stage, JournalStage::Journaled);
    }
    #[test]
    fn recovery_after_ref_advance_reinstalls_reconciled_index() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        let (_, new) = objects();
        let Recovery::InstallIndex(index) = publication.recover(&new, &base).unwrap() else {
            panic!("expected index recovery")
        };
        assert_eq!(publication.journal().stage, JournalStage::RefAdvanced);
        assert!(matches!(
            publication.cleanup(),
            Err(Error::InvalidTransition)
        ));
        assert_eq!(
            index.get(&path()).unwrap().as_ref().unwrap().bytes(),
            b"published"
        );
        publication.install_index(&base).unwrap();
        assert_eq!(publication.journal().stage, JournalStage::IndexReconciled);
    }
    #[test]
    fn index_reconciliation_cannot_precede_ref_advance() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        assert!(matches!(
            publication.install_index(&base),
            Err(Error::InvalidTransition)
        ));
        assert_eq!(publication.journal().stage, JournalStage::Journaled);
    }
    #[test]
    fn reconciliation_preserves_unrelated_partially_staged_entry() {
        let managed = path();
        let other = LoosePath::for_key("tables", &["other".into()]).unwrap();
        let base = IndexImage::default()
            .entry(managed.clone(), None)
            .entry(other.clone(), Some(row("head")));
        let ordinary = base.clone().entry(other.clone(), Some(row("staged-only")));
        let result =
            reconcile_index(&base, &ordinary, &batch(None, Some(row("published")))).unwrap();
        assert_eq!(
            result.get(&other).unwrap().as_ref().unwrap().bytes(),
            b"staged-only"
        );
        assert_eq!(
            result.get(&managed).unwrap().as_ref().unwrap().bytes(),
            b"published"
        );
    }

    #[test]
    fn frozen_batch_builds_a_real_private_candidate_without_advancing_head() {
        let (root, repository) = repository();
        let head = repository.head().unwrap().unwrap();
        let candidate = build_private_publication_candidate(
            &repository,
            &head,
            &batch(None, Some(row("published"))),
            "orna: publish runtime data",
        )
        .unwrap();

        assert_ne!(candidate.commit(), &head);
        assert_eq!(repository.head().unwrap().unwrap(), head);
        assert_eq!(
            git_output(
                root.path(),
                &[
                    "show",
                    &format!("{}:tables/sensor/one.orna", candidate.commit())
                ]
            ),
            "published"
        );
    }

    #[tokio::test]
    async fn runtime_completion_consumes_only_the_frozen_publication_prefix() {
        let (_root, repository) = repository();
        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };
        let state = RuntimeState::open(&repository, identity, [3; 32])
            .await
            .unwrap();
        let lease = state.acquire_lease([4; 16]).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &context,
                &[TableMutation::new(
                    [5; 16],
                    "sensor",
                    b"one".to_vec(),
                    Some(b"published".to_vec()),
                )
                .unwrap()],
                [6; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
        let freeze = state.freeze([7; 16], &checkpoint).await.unwrap();
        let head = repository.head().unwrap().unwrap();
        let candidate = build_private_publication_candidate(
            &repository,
            &head,
            &batch(None, Some(row("published"))),
            "orna: publish runtime data",
        )
        .unwrap();

        complete_runtime_publication(&state, &freeze, &candidate)
            .await
            .unwrap();
        assert!(state.pending().await.unwrap().is_empty());
        complete_runtime_publication(&state, &freeze, &candidate)
            .await
            .unwrap();
    }

    #[test]
    fn loose_materialization_is_idempotent_and_does_not_publish_git_state() {
        let (root, repository) = repository();
        let path = path();
        let before_index = repository.index_generation().unwrap();
        let before_head = repository.head().unwrap();
        let first = LooseRow::new(b"first".to_vec()).unwrap();
        let mutation = LooseMutation {
            id: id("materialize-one"),
            path: path.clone(),
            expected: None,
            next: Some(first.clone()),
        };

        assert_eq!(
            materialize_loose_mutation(&repository, &mutation).unwrap(),
            Materialization::Applied
        );
        assert_eq!(
            materialize_loose_mutation(&repository, &mutation).unwrap(),
            Materialization::AlreadyApplied
        );
        assert_eq!(
            fs::read(root.path().join(path.as_managed_path().as_path())).unwrap(),
            b"first"
        );
        assert_eq!(repository.index_generation().unwrap(), before_index);
        assert_eq!(repository.head().unwrap(), before_head);
    }

    #[test]
    fn loose_materialization_preserves_external_edits_and_supports_idempotent_delete() {
        let (root, repository) = repository();
        let path = path();
        let first = LooseRow::new(b"first".to_vec()).unwrap();
        let first_mutation = LooseMutation {
            id: id("materialize-two"),
            path: path.clone(),
            expected: None,
            next: Some(first.clone()),
        };
        materialize_loose_mutation(&repository, &first_mutation).unwrap();

        fs::write(
            root.path().join(path.as_managed_path().as_path()),
            b"editor",
        )
        .unwrap();
        let replacement = LooseRow::new(b"runtime".to_vec()).unwrap();
        let conflict = LooseMutation {
            id: id("materialize-three"),
            path: path.clone(),
            expected: Some(first.hash()),
            next: Some(replacement),
        };
        assert!(matches!(
            materialize_loose_mutation(&repository, &conflict),
            Err(Error::ExternalConflict { .. })
        ));
        assert_eq!(
            fs::read(root.path().join(path.as_managed_path().as_path())).unwrap(),
            b"editor"
        );

        fs::write(
            root.path().join(path.as_managed_path().as_path()),
            first.bytes(),
        )
        .unwrap();
        let deletion = LooseMutation {
            id: id("materialize-four"),
            path: path.clone(),
            expected: Some(first.hash()),
            next: None,
        };
        assert_eq!(
            materialize_loose_mutation(&repository, &deletion).unwrap(),
            Materialization::Applied
        );
        assert_eq!(
            materialize_loose_mutation(&repository, &deletion).unwrap(),
            Materialization::AlreadyApplied
        );
        assert!(!root.path().join(path.as_managed_path().as_path()).exists());
    }
}
