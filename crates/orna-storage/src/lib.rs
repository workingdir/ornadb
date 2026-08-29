//! Backend-neutral storage contracts for Orna revision lifecycle operations.
//!
//! This crate deliberately exposes only Orna domain values. Backend adapters
//! own connection pools, transactions, SQL, and driver-specific errors.

pub mod migration;

use std::{error::Error, fmt, future::Future};

use orna_core::{
    CatalogueRevisionId, SourceRevisionId,
    canonical_hash::CanonicalHashError,
    physical::{
        FORMAT_IDENTITY, FORMAT_VERSION, PhysicalMigrationArtifact, PhysicalMigrationArtifactError,
    },
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair, Sha256Digest},
};

/// The seeded active revision pair returned by [`RevisionStore::bootstrap`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootstrapRevision {
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
}

impl BootstrapRevision {
    /// Creates a bootstrap revision pair.
    pub const fn new(source: SourceRevisionId, catalogue: CatalogueRevisionId) -> Self {
        Self { source, catalogue }
    }

    /// Returns the active source revision identity.
    pub const fn source(&self) -> SourceRevisionId {
        self.source
    }

    /// Returns the active catalogue revision identity.
    pub const fn catalogue(&self) -> CatalogueRevisionId {
        self.catalogue
    }
}

/// One backend-neutral durable record for a physical migration artifact.
///
/// The record retains the artifact format metadata, revision binding,
/// canonical bytes, and digest exactly as produced by the physical planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationLedgerEntry {
    format: String,
    version: u32,
    expected_base: RevisionPair,
    candidate_pair: RevisionPair,
    canonical_bytes: Vec<u8>,
    digest: Sha256Digest,
}

impl MigrationLedgerEntry {
    /// Captures the durable fields of one compiler-produced artifact.
    pub fn from_artifact(artifact: &PhysicalMigrationArtifact) -> Self {
        Self {
            format: FORMAT_IDENTITY.to_owned(),
            version: FORMAT_VERSION,
            expected_base: artifact.expected_base(),
            candidate_pair: artifact.candidate_pair(),
            canonical_bytes: artifact.canonical_bytes().to_owned(),
            digest: artifact.digest(),
        }
    }

    /// Reconstructs an entry from recovered durable fields.
    ///
    /// The format, version, canonical bytes structure, revision binding, and
    /// digest are checked before returning. Active-state and candidate binding
    /// are checked by [`Self::validate`].
    pub fn from_parts(
        format: impl Into<String>,
        version: u32,
        expected_base: RevisionPair,
        candidate_pair: RevisionPair,
        canonical_bytes: Vec<u8>,
        digest: Sha256Digest,
    ) -> Result<Self, MigrationLedgerEntryError> {
        let entry = Self {
            format: format.into(),
            version,
            expected_base,
            candidate_pair,
            canonical_bytes,
            digest,
        };
        entry.validate_integrity()?;
        Ok(entry)
    }

    /// Returns the stable artifact format identity.
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Returns the artifact format version.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the source and catalogue revisions required as the active base.
    pub const fn expected_base(&self) -> RevisionPair {
        self.expected_base
    }

    /// Returns the source and catalogue revisions produced by the artifact.
    pub const fn candidate_pair(&self) -> RevisionPair {
        self.candidate_pair
    }

    /// Returns the exact canonical artifact bytes.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Returns the SHA-256 digest retained for the canonical bytes.
    pub const fn digest(&self) -> Sha256Digest {
        self.digest
    }

    /// Validates this entry against the locked active revision and compiler-
    /// produced candidate.
    ///
    /// Validation is fail-closed: supported format metadata, canonical digest,
    /// both revision bindings, and the exact physical plan for the active to
    /// candidate transition must agree before an adapter applies it.
    pub fn validate(
        &self,
        active: &ActiveDatabaseRevision,
        candidate: &DeployableRevision,
    ) -> Result<(), MigrationLedgerEntryError> {
        self.validate_integrity()?;

        if self.expected_base != candidate.expected_base() {
            return Err(MigrationLedgerEntryError::ExpectedBaseMismatch {
                expected: candidate.expected_base(),
                actual: self.expected_base,
            });
        }
        if self.candidate_pair != candidate.candidate_pair() {
            return Err(MigrationLedgerEntryError::CandidatePairMismatch {
                expected: candidate.candidate_pair(),
                actual: self.candidate_pair,
            });
        }

        let expected_artifact = PhysicalMigrationArtifact::from_revisions(active, candidate)
            .map_err(MigrationLedgerEntryError::PhysicalArtifact)?;
        if self.canonical_bytes != expected_artifact.canonical_bytes()
            || self.digest != expected_artifact.digest()
        {
            return Err(MigrationLedgerEntryError::ArtifactMismatch {
                expected: expected_artifact.digest(),
                actual: self.digest,
            });
        }
        Ok(())
    }

    fn validate_integrity(&self) -> Result<(), MigrationLedgerEntryError> {
        if self.format != FORMAT_IDENTITY {
            return Err(MigrationLedgerEntryError::UnsupportedFormat {
                expected: FORMAT_IDENTITY,
                actual: self.format.clone(),
            });
        }
        if self.version != FORMAT_VERSION {
            return Err(MigrationLedgerEntryError::UnsupportedVersion {
                expected: FORMAT_VERSION,
                actual: self.version,
            });
        }

        PhysicalMigrationArtifact::from_canonical_bytes(
            self.expected_base,
            self.candidate_pair,
            &self.canonical_bytes,
            self.digest,
        )
        .map_err(|error| match error {
            PhysicalMigrationArtifactError::CanonicalHash(error) => {
                MigrationLedgerEntryError::CanonicalHash(error)
            }
            PhysicalMigrationArtifactError::DigestMismatch { expected, actual } => {
                MigrationLedgerEntryError::DigestMismatch { expected, actual }
            }
            error => MigrationLedgerEntryError::PhysicalArtifact(error),
        })?;
        Ok(())
    }
}

impl From<&PhysicalMigrationArtifact> for MigrationLedgerEntry {
    fn from(artifact: &PhysicalMigrationArtifact) -> Self {
        Self::from_artifact(artifact)
    }
}

/// A fail-closed validation error for a migration ledger entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationLedgerEntryError {
    /// The recovered entry uses an unsupported artifact format.
    UnsupportedFormat {
        /// The only format accepted by this contract.
        expected: &'static str,
        /// The recovered format.
        actual: String,
    },
    /// The recovered entry uses an unsupported artifact version.
    UnsupportedVersion {
        /// The only version accepted by this contract.
        expected: u32,
        /// The recovered version.
        actual: u32,
    },
    /// The entry's expected base differs from the compiler candidate.
    ExpectedBaseMismatch {
        /// The pair required by the candidate.
        expected: RevisionPair,
        /// The pair retained by the entry.
        actual: RevisionPair,
    },
    /// The candidate does not target the active revision held by the adapter.
    ActiveBaseMismatch {
        /// The pair required by the candidate.
        expected: RevisionPair,
        /// The pair held by the locked active pointer.
        actual: RevisionPair,
    },
    /// The entry's candidate pair differs from the compiler candidate.
    CandidatePairMismatch {
        /// The pair produced by the candidate.
        expected: RevisionPair,
        /// The pair retained by the entry.
        actual: RevisionPair,
    },
    /// The entry's artifact differs from the exact active-to-candidate plan.
    ArtifactMismatch {
        /// The digest of the recomputed active-to-candidate artifact.
        expected: Sha256Digest,
        /// The digest retained by the entry.
        actual: Sha256Digest,
    },
    /// The canonical bytes failed physical migration artifact validation.
    PhysicalArtifact(PhysicalMigrationArtifactError),
    /// Canonical digest calculation failed for the retained bytes.
    CanonicalHash(CanonicalHashError),
    /// The retained digest does not cover the retained canonical bytes.
    DigestMismatch {
        /// The digest calculated over the retained bytes.
        expected: Sha256Digest,
        /// The digest retained by the entry.
        actual: Sha256Digest,
    },
}

impl fmt::Display for MigrationLedgerEntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat { expected, actual } => write!(
                formatter,
                "unsupported migration artifact format {actual:?}; expected {expected:?}"
            ),
            Self::UnsupportedVersion { expected, actual } => write!(
                formatter,
                "unsupported migration artifact version {actual}; expected {expected}"
            ),
            Self::ExpectedBaseMismatch { expected, actual } => write!(
                formatter,
                "migration entry expects base {actual:?}; candidate requires {expected:?}"
            ),
            Self::ActiveBaseMismatch { expected, actual } => write!(
                formatter,
                "candidate requires active base {expected:?}; locked active base is {actual:?}"
            ),
            Self::CandidatePairMismatch { expected, actual } => write!(
                formatter,
                "migration entry produces {actual:?}; candidate produces {expected:?}"
            ),
            Self::ArtifactMismatch { expected, actual } => write!(
                formatter,
                "migration entry artifact digest {actual:?} does not match the active-to-candidate plan ({expected:?})"
            ),
            Self::PhysicalArtifact(error) => {
                write!(formatter, "invalid physical migration artifact: {error}")
            }
            Self::CanonicalHash(error) => {
                write!(
                    formatter,
                    "migration artifact digest calculation failed: {error}"
                )
            }
            Self::DigestMismatch { expected, actual } => write!(
                formatter,
                "migration artifact digest {actual:?} does not match canonical bytes ({expected:?})"
            ),
        }
    }
}

impl Error for MigrationLedgerEntryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PhysicalArtifact(error) => Some(error),
            Self::CanonicalHash(error) => Some(error),
            _ => None,
        }
    }
}

/// Backend-neutral revision lifecycle failure.
#[derive(Debug)]
pub enum StorageError<E> {
    /// The backend reported an error while executing the operation.
    Backend(E),
    /// The caller supplied an unsupported, tampered, or mismatched request.
    InvalidRequest(MigrationLedgerEntryError),
}

impl<E: fmt::Display> fmt::Display for StorageError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(formatter, "storage backend error: {error}"),
            Self::InvalidRequest(error) => write!(formatter, "invalid storage request: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for StorageError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error),
            Self::InvalidRequest(error) => Some(error),
        }
    }
}

/// Revision lifecycle contract implemented by each storage backend.
///
/// An adapter must lock and re-read the active pointer, then validate the
/// supplied artifact against that locked active revision and the candidate
/// before beginning durable mutation. `apply` is atomic: a successful result
/// makes exactly one matching [`MigrationLedgerEntry`] visible with the new
/// active pointer; the ledger and active pointer are mutated only after every
/// check succeeds. A failed apply leaves both the visible ledger and active
/// pointer unchanged. `read_ledger` returns entries oldest-first in
/// deterministic apply order, never backend-default row order.
pub trait RevisionStore {
    /// Backend-specific error type.
    type Error: Error + Send + Sync + 'static;

    /// Bootstraps durable state and returns the seeded active pair.
    fn bootstrap(
        &self,
    ) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send;

    /// Recovers the complete active durable revision.
    fn recover(
        &self,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send;

    /// Atomically applies one compiler-produced candidate and its exact
    /// compiler-generated physical artifact. The implementation must lock and
    /// re-read the active pointer before validation, then mutate the ledger and
    /// active pointer only after every check succeeds.
    fn apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send;

    /// Reads the migration ledger oldest-first in deterministic apply order.
    fn read_ledger(
        &self,
    ) -> impl Future<Output = Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::ready, sync::Mutex};

    use orna_core::{
        SourceBundleId,
        catalogue::CatalogueSnapshot,
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            DeployableRevision, StoredSourceRevision,
        },
    };

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }

    struct Fixture {
        active: ActiveDatabaseRevision,
        candidate: DeployableRevision,
        artifact: PhysicalMigrationArtifact,
    }

    fn fixture(candidate_source: u8, candidate_catalogue: u8) -> Fixture {
        fixture_with_base(1, 2, candidate_source, candidate_catalogue)
    }

    fn fixture_with_base(
        active_source: u8,
        active_catalogue: u8,
        candidate_source: u8,
        candidate_catalogue: u8,
    ) -> Fixture {
        let active_source_id = SourceRevisionId::from_bytes([active_source; 16]);
        let active_catalogue_id = CatalogueRevisionId::from_bytes([active_catalogue; 16]);
        let expected_base = RevisionPair::new(active_source_id, active_catalogue_id);
        let active_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([3; 16]),
            active_source_id,
            None,
            Vec::new(),
            digest(4),
            digest(5),
        )
        .expect("empty source revision is valid");
        let active_catalogue = CatalogueSnapshot::new(active_catalogue_id, Vec::new(), Vec::new())
            .expect("empty catalogue is valid");
        let active = ActiveDatabaseRevision::new(
            expected_base,
            active_source,
            active_catalogue,
            digest(6),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty active revision is valid");

        let candidate_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([7; 16]),
            SourceRevisionId::from_bytes([candidate_source; 16]),
            Some(active_source_id),
            Vec::new(),
            digest(8),
            digest(9),
        )
        .expect("empty candidate source revision is valid");
        let candidate_catalogue_id = CatalogueRevisionId::from_bytes([candidate_catalogue; 16]);
        let candidate_catalogue =
            CatalogueSnapshot::new(candidate_catalogue_id, Vec::new(), Vec::new())
                .expect("empty candidate catalogue is valid");
        let candidate = DeployableRevision::new(
            expected_base,
            candidate_source,
            active_catalogue_id,
            candidate_catalogue,
            digest(10),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty deployable revision is valid");
        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate)
            .expect("empty physical transition is supported");
        Fixture {
            active,
            candidate,
            artifact,
        }
    }

    #[test]
    fn migration_ledger_entry_round_trips_artifact_fields() {
        let fixture = fixture(11, 12);
        let entry = MigrationLedgerEntry::from_artifact(&fixture.artifact);

        assert_eq!(entry.format(), FORMAT_IDENTITY);
        assert_eq!(entry.version(), FORMAT_VERSION);
        assert_eq!(entry.expected_base(), fixture.artifact.expected_base());
        assert_eq!(entry.candidate_pair(), fixture.artifact.candidate_pair());
        assert_eq!(entry.canonical_bytes(), fixture.artifact.canonical_bytes());
        assert_eq!(entry.digest(), fixture.artifact.digest());
        assert_eq!(MigrationLedgerEntry::from(&fixture.artifact), entry);
        assert_eq!(entry.validate(&fixture.active, &fixture.candidate), Ok(()));
    }

    #[test]
    fn migration_ledger_entry_recovers_valid_artifact_parts() {
        let fixture = fixture(31, 32);
        let expected = MigrationLedgerEntry::from_artifact(&fixture.artifact);

        let recovered = MigrationLedgerEntry::from_parts(
            FORMAT_IDENTITY,
            FORMAT_VERSION,
            fixture.artifact.expected_base(),
            fixture.artifact.candidate_pair(),
            fixture.artifact.canonical_bytes().to_vec(),
            fixture.artifact.digest(),
        )
        .expect("valid recovered entry must be accepted");

        assert_eq!(recovered, expected);
        assert_eq!(
            recovered.validate(&fixture.active, &fixture.candidate),
            Ok(())
        );
    }

    #[test]
    fn migration_ledger_entry_rejects_swapped_revision_metadata() {
        let fixture = fixture(33, 34);
        let different_base = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf1; 16]),
            fixture.artifact.expected_base().catalogue(),
        );
        let different_candidate = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf2; 16]),
            fixture.artifact.candidate_pair().catalogue(),
        );

        assert_eq!(
            MigrationLedgerEntry::from_parts(
                FORMAT_IDENTITY,
                FORMAT_VERSION,
                different_base,
                fixture.artifact.candidate_pair(),
                fixture.artifact.canonical_bytes().to_vec(),
                fixture.artifact.digest(),
            ),
            Err(MigrationLedgerEntryError::PhysicalArtifact(
                PhysicalMigrationArtifactError::ExpectedBaseMismatch {
                    expected: different_base,
                    actual: fixture.artifact.expected_base(),
                }
            ))
        );
        assert_eq!(
            MigrationLedgerEntry::from_parts(
                FORMAT_IDENTITY,
                FORMAT_VERSION,
                fixture.artifact.expected_base(),
                different_candidate,
                fixture.artifact.canonical_bytes().to_vec(),
                fixture.artifact.digest(),
            ),
            Err(MigrationLedgerEntryError::PhysicalArtifact(
                PhysicalMigrationArtifactError::CandidatePairMismatch {
                    expected: different_candidate,
                    actual: fixture.artifact.candidate_pair(),
                }
            ))
        );
    }

    #[test]
    fn migration_ledger_entry_rejects_candidate_mismatch() {
        let first = fixture(13, 14);
        let second = fixture(15, 16);
        let entry = MigrationLedgerEntry::from_artifact(&first.artifact);

        assert!(matches!(
            entry.validate(&second.active, &second.candidate),
            Err(MigrationLedgerEntryError::CandidatePairMismatch { .. })
        ));
    }

    #[test]
    fn migration_ledger_entry_rejects_plan_artifact_mismatch() {
        let fixture = fixture(35, 36);
        let mut entry = MigrationLedgerEntry::from_artifact(&fixture.artifact);

        entry.canonical_bytes[76..80].copy_from_slice(&1_u32.to_be_bytes());
        entry.canonical_bytes.extend_from_slice(&[1]);
        entry.canonical_bytes.extend_from_slice(&[0xab; 16]);
        entry
            .canonical_bytes
            .extend_from_slice(&0_u32.to_be_bytes());
        entry.digest = orna_core::canonical_hash::artifact_payload_digest(&entry.canonical_bytes)
            .expect("crafted canonical artifact can be hashed");
        let actual = entry.digest();

        assert_eq!(
            entry.validate(&fixture.active, &fixture.candidate),
            Err(MigrationLedgerEntryError::ArtifactMismatch {
                expected: fixture.artifact.digest(),
                actual,
            })
        );
    }

    #[test]
    fn migration_ledger_entry_rejects_digest_tampering() {
        let fixture = fixture(17, 18);
        let mut entry = MigrationLedgerEntry::from_artifact(&fixture.artifact);
        let mut tampered_digest = entry.digest().to_bytes();
        tampered_digest[0] ^= 0xff;
        entry.digest = Sha256Digest::from_bytes(tampered_digest);

        assert!(matches!(
            entry.validate(&fixture.active, &fixture.candidate),
            Err(MigrationLedgerEntryError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn migration_ledger_entry_rejects_unsupported_recovered_format() {
        let fixture = fixture(19, 20);
        let original = MigrationLedgerEntry::from_artifact(&fixture.artifact);

        assert!(matches!(
            MigrationLedgerEntry::from_parts(
                "orna.unknown",
                FORMAT_VERSION,
                original.expected_base(),
                original.candidate_pair(),
                original.canonical_bytes().to_vec(),
                original.digest(),
            ),
            Err(MigrationLedgerEntryError::UnsupportedFormat { .. })
        ));
    }

    #[derive(Debug)]
    struct FakeError;

    impl fmt::Display for FakeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("fake storage error")
        }
    }

    impl Error for FakeError {}

    struct FakeStore {
        state: Mutex<FakeStoreState>,
    }

    struct FakeStoreState {
        active: ActiveDatabaseRevision,
        entries: Vec<MigrationLedgerEntry>,
    }

    impl FakeStore {
        fn new(active: ActiveDatabaseRevision, entries: Vec<MigrationLedgerEntry>) -> Self {
            Self {
                state: Mutex::new(FakeStoreState { active, entries }),
            }
        }
    }

    impl RevisionStore for FakeStore {
        type Error = FakeError;

        fn bootstrap(
            &self,
        ) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send
        {
            let state = self.state.lock().expect("fake state lock");
            ready(Ok(BootstrapRevision::new(
                state.active.pair().source(),
                state.active.pair().catalogue(),
            )))
        }

        fn recover(
            &self,
        ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
        {
            let state = self.state.lock().expect("fake state lock");
            ready(Ok(state.active.clone()))
        }

        fn apply(
            &self,
            candidate: &DeployableRevision,
            artifact: &PhysicalMigrationArtifact,
        ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
        {
            let mut state = self.state.lock().expect("fake state lock");
            let entry = MigrationLedgerEntry::from_artifact(artifact);
            let active_pair = state.active.pair();
            let result = if candidate.expected_base() != active_pair {
                Err(StorageError::InvalidRequest(
                    MigrationLedgerEntryError::ActiveBaseMismatch {
                        expected: candidate.expected_base(),
                        actual: active_pair,
                    },
                ))
            } else {
                match entry.validate(&state.active, candidate) {
                    Ok(()) => {
                        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
                            ActiveDatabaseRevisionInput::new(
                                candidate.candidate_pair(),
                                candidate.source().clone(),
                                candidate.candidate().clone(),
                                candidate.catalogue_hash(),
                                ActiveRevisionContent::new(
                                    candidate.expressions().to_vec(),
                                    candidate.current_function_revisions().map_or_else(
                                        || candidate.new_function_revisions().to_vec(),
                                        ToOwned::to_owned,
                                    ),
                                    candidate.origins().to_vec(),
                                    candidate.references().to_vec(),
                                ),
                            ),
                            candidate.catalogue_hash_context().clone(),
                        )
                        .expect("valid candidate can become active revision");
                        state.entries.push(entry);
                        state.active = active.clone();
                        Ok(active)
                    }
                    Err(error) => Err(StorageError::InvalidRequest(error)),
                }
            };
            ready(result)
        }

        fn read_ledger(
            &self,
        ) -> impl Future<Output = Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>>> + Send
        {
            let state = self.state.lock().expect("fake state lock");
            ready(Ok(state.entries.clone()))
        }
    }

    fn resolve<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("fake future must be ready"),
        }
    }

    #[test]
    fn revision_store_ledger_is_ordered_and_failed_apply_is_invisible() {
        let first = fixture(21, 22);
        let second = fixture(23, 24);
        let third = fixture(25, 26);
        let mismatch = fixture(27, 28);
        let store = FakeStore::new(
            first.active.clone(),
            vec![
                MigrationLedgerEntry::from_artifact(&first.artifact),
                MigrationLedgerEntry::from_artifact(&second.artifact),
            ],
        );

        let initial = resolve(store.read_ledger()).expect("ledger read succeeds");
        assert_eq!(
            initial
                .iter()
                .map(MigrationLedgerEntry::candidate_pair)
                .collect::<Vec<_>>(),
            vec![
                first.candidate.candidate_pair(),
                second.candidate.candidate_pair()
            ]
        );

        let applied = resolve(store.apply(&third.candidate, &third.artifact))
            .expect("matching apply succeeds");
        assert_eq!(applied.pair(), third.candidate.candidate_pair());

        let after_success = resolve(store.read_ledger()).expect("ledger read succeeds");
        assert_eq!(after_success.len(), 3);
        assert_eq!(
            after_success
                .iter()
                .map(MigrationLedgerEntry::candidate_pair)
                .collect::<Vec<_>>(),
            vec![
                first.candidate.candidate_pair(),
                second.candidate.candidate_pair(),
                third.candidate.candidate_pair()
            ]
        );

        let failed = resolve(store.apply(&mismatch.candidate, &third.artifact));
        assert!(matches!(failed, Err(StorageError::InvalidRequest(_))));
        assert_eq!(
            resolve(store.read_ledger()).expect("ledger read succeeds"),
            after_success
        );
        assert_eq!(
            resolve(store.recover()).expect("recovery succeeds").pair(),
            third.candidate.candidate_pair()
        );
    }

    #[test]
    fn revision_store_rejects_stale_base_without_mutation() {
        let stale = fixture(35, 36);
        let current = fixture_with_base(41, 42, 43, 44);
        let current_entry = MigrationLedgerEntry::from_artifact(&current.artifact);
        let store = FakeStore::new(current.active.clone(), vec![current_entry.clone()]);
        let before = resolve(store.read_ledger()).expect("ledger read succeeds");

        let failed = resolve(store.apply(&stale.candidate, &stale.artifact));
        assert!(matches!(
            failed,
            Err(StorageError::InvalidRequest(
                MigrationLedgerEntryError::ActiveBaseMismatch { expected, actual }
            )) if expected == stale.candidate.expected_base()
                && actual == current.active.pair()
        ));
        assert_eq!(
            resolve(store.read_ledger()).expect("ledger read succeeds"),
            before
        );
        assert_eq!(
            resolve(store.recover()).expect("recovery succeeds").pair(),
            current.active.pair()
        );
    }
}
