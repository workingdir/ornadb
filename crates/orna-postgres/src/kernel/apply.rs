//! One atomic, fail-closed installation of a compiler deployable revision.

// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
// Installation preserves the accepted multi-input transaction seam.
#![allow(clippy::too_many_arguments)]
use std::collections::{BTreeMap, BTreeSet, HashSet};

use orna_core::security::{CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, SecurityAuditDecision};
use orna_core::system::{
    SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
    SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID, system_function_by_id,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, function_declaration_digest,
        source_bundle_digest, source_revision_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction,
        FunctionVolatility, OnDeleteAction, QualifiedSemanticName, TypeBindingKind, TypeLookupName,
        ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    physical::{PhysicalMigrationArtifact, PhysicalMigrationArtifactError, plan_physical_changes},
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
        ExecutableArtifactKind, FunctionRevisionRecord, RecordValueFieldDescriptorClass,
        RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable, StandardLibraryDigestVersion,
        VerifiedStandardLibrarySnapshot, validate_persistable_catalogue,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};
use orna_standard::{
    STANDARD_SOURCE_REVISION_ID, StandardUpgrade, StandardUpgradeIdentity,
    retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use orna_storage::{MigrationLedgerEntry, MigrationLedgerEntryError};
use tokio_postgres::{Client, IsolationLevel, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    decode::{DurableRecord, digest_bytes, identity_bytes, optional_identity_bytes, u32_from_i64},
    is_sealed_inspect_type_id,
    physical::{establish_trusted_search_path, install_physical_plan},
    recovery::recover_active_revision,
    security::{append_security_audit_event, is_admitted_security_identity},
};
#[path = "apply/candidate.rs"]
mod candidate;

#[path = "apply/encoding.rs"]
mod encoding;

#[path = "apply/materialization.rs"]
mod materialization;

#[path = "apply/preflight.rs"]
mod preflight;

#[path = "apply/standard.rs"]
mod standard;

#[path = "apply/reserved_identities.rs"]
mod reserved_identities;

use candidate::*;
use encoding::*;
use materialization::*;
use preflight::*;
use reserved_identities::scan_reserved_standard_identities;
use standard::*;

/// Durable migration-ledger relation supplied by the canonical post-contract
/// migration. This adapter deliberately does not create it: databases must
/// install the typed `_orna_kernel.application_migrations` schema first.
const APPLICATION_MIGRATIONS_RELATION: &str = "_orna_kernel.application_migrations";
const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const CONTRACT_VERSION: i16 = 1;

/// The immutable standard-library facts that pin a version-2 application
/// catalogue context.
///
/// The PostgreSQL kernel constructs this value only from a core-verified
/// standard snapshot while comparing normal apply revisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StandardContextIdentity {
    standard_library_revision: StandardLibraryRevisionId,
    standard_catalogue_revision: CatalogueRevisionId,
    source_bundle: SourceBundleId,
    source_revision: SourceRevisionId,
    source_bundle_hash: Sha256Digest,
    source_revision_hash: Sha256Digest,
    standard_library_digest: Sha256Digest,
}

impl StandardContextIdentity {
    fn from_verified_snapshot(snapshot: &VerifiedStandardLibrarySnapshot) -> Self {
        let source = snapshot.source();
        Self {
            standard_library_revision: snapshot.revision(),
            standard_catalogue_revision: snapshot.catalogue().revision(),
            source_bundle: source.bundle(),
            source_revision: source.id(),
            source_bundle_hash: source.bundle_hash(),
            source_revision_hash: source.revision_hash(),
            standard_library_digest: snapshot.digest(),
        }
    }

    /// Returns the pinned immutable standard-library revision identity.
    pub fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        self.standard_library_revision
    }

    /// Returns the pinned standard catalogue revision identity.
    pub fn standard_catalogue_revision(&self) -> CatalogueRevisionId {
        self.standard_catalogue_revision
    }

    /// Returns the standard source-bundle identity.
    pub fn source_bundle(&self) -> SourceBundleId {
        self.source_bundle
    }

    /// Returns the standard source-revision identity.
    pub fn source_revision(&self) -> SourceRevisionId {
        self.source_revision
    }

    /// Returns the canonical standard source-bundle hash.
    pub fn source_bundle_hash(&self) -> Sha256Digest {
        self.source_bundle_hash
    }

    /// Returns the canonical standard source-revision hash.
    pub fn source_revision_hash(&self) -> Sha256Digest {
        self.source_revision_hash
    }

    /// Returns the verified canonical standard-library digest.
    pub fn standard_library_digest(&self) -> Sha256Digest {
        self.standard_library_digest
    }
}

impl PostgresKernel {
    /// Installs a complete candidate revision as one atomic database change.
    pub async fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result = apply_client(&mut session.client, candidate, None, false).await;
        let shutdown_result = session.shutdown().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Applies a compiler-supplied artifact after locking and re-reading the
    /// active revision. This is the backend-neutral
    /// [`orna_storage::RevisionStore`] entry point; unlike [`Self::apply`], it
    /// never regenerates or ignores the caller's artifact.
    pub(crate) async fn apply_with_artifact(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result =
            apply_client(&mut session.client, candidate, Some(artifact), false).await;
        let shutdown_result = session.shutdown().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Installs a source-apply candidate and records its committed candidate
    /// pair in protected audit using the reserved catalogue-health principal.
    ///
    /// The principal is fixed by the installed host contract; callers cannot
    /// provide request-derived audit identity.
    pub async fn apply_source_apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result = apply_client(&mut session.client, candidate, None, true).await;
        let shutdown_result = session.shutdown_for_source_apply().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Atomically installs one compiler-prepared standard library and its
    /// application revision.
    pub async fn apply_standard_upgrade(
        &self,
        upgrade: &StandardUpgrade,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result = apply_standard_upgrade_client(
            &mut session.client,
            upgrade.application_revision(),
            upgrade.verified_standard_snapshot(),
        )
        .await;
        let shutdown_result = session.shutdown().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Installs a verified standard fixture through the production persistence
    /// path while integration tests exercise catalogue shapes that the retained
    /// standard library does not yet contain.
    #[cfg(feature = "test-hooks")]
    pub async fn apply_test_standard_upgrade(
        &self,
        candidate: &DeployableRevision,
        standard: &VerifiedStandardLibrarySnapshot,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result =
            apply_standard_upgrade_client(&mut session.client, candidate, standard).await;
        let shutdown_result = session.shutdown().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Reads the durable application migration ledger oldest-first.
    ///
    /// The canonical post-contract migration supplies
    /// `_orna_kernel.application_migrations`; this adapter intentionally does
    /// not create or synthesize the relation when it is absent.
    pub async fn read_ledger(&self) -> Result<Vec<MigrationLedgerEntry>, PostgresKernelError> {
        let mut session = self.open().await?;
        let ledger_result = read_ledger_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;
        match (ledger_result, shutdown_result) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn apply_client(
    client: &mut Client,
    candidate: &DeployableRevision,
    artifact: Option<&PhysicalMigrationArtifact>,
    source_apply_audit: bool,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .read_only(false)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = apply_transaction(&transaction, candidate, artifact, source_apply_audit).await;
    match result {
        Ok(active) => transaction
            .commit()
            .await
            .map(|()| active)
            .map_err(PostgresKernelError::Database),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(PostgresKernelError::Database(rollback)),
        },
    }
}

async fn read_ledger_client(
    client: &mut Client,
) -> Result<Vec<MigrationLedgerEntry>, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = read_ledger_transaction(&transaction).await;
    match result {
        Ok(entries) => transaction
            .commit()
            .await
            .map(|()| entries)
            .map_err(PostgresKernelError::Database),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(PostgresKernelError::Database(rollback)),
        },
    }
}

async fn read_ledger_transaction(
    transaction: &Transaction<'_>,
) -> Result<Vec<MigrationLedgerEntry>, PostgresKernelError> {
    let active = recover_active_revision(transaction).await?;
    load_and_validate_migration_ledger(transaction, Some(&active)).await
}

async fn load_migration_ledger(
    transaction: &Transaction<'_>,
) -> Result<Vec<MigrationLedgerEntry>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT ordinal, format, version,
                    expected_source_revision_id, expected_catalogue_revision_id,
                    candidate_source_revision_id, candidate_catalogue_revision_id,
                    canonical_bytes, digest
             FROM _orna_kernel.application_migrations
             ORDER BY ordinal ASC",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut entries = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let record = DurableRecord::new(APPLICATION_MIGRATIONS_RELATION, format!("row={index}"));
        let ordinal: i64 = record.column(row, "ordinal", "ledger ordinal must be bigint")?;
        if ordinal < 0 {
            return Err(record.invariant("ledger ordinal must be non-negative"));
        }
        let expected_ordinal = i64::try_from(index)
            .map_err(|_| record.invariant("ledger ordinal index must fit bigint"))?;
        if ordinal != expected_ordinal {
            return Err(record.invariant("ledger ordinals must be contiguous and zero-based"));
        }
        let format: String = record.column(row, "format", "ledger format must be text")?;
        let version = u32_from_i64(
            record.column(row, "version", "ledger version must be bigint")?,
            &record,
            "ledger version must be a positive u32",
        )?;
        let expected_source = SourceRevisionId::from_bytes(identity_bytes(
            record.column(
                row,
                "expected_source_revision_id",
                "ledger expected source identity must be 16 bytes",
            )?,
            &record,
            "ledger expected source identity must be 16 bytes",
        )?);
        let expected_catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
            record.column(
                row,
                "expected_catalogue_revision_id",
                "ledger expected catalogue identity must be 16 bytes",
            )?,
            &record,
            "ledger expected catalogue identity must be 16 bytes",
        )?);
        let candidate_source = SourceRevisionId::from_bytes(identity_bytes(
            record.column(
                row,
                "candidate_source_revision_id",
                "ledger candidate source identity must be 16 bytes",
            )?,
            &record,
            "ledger candidate source identity must be 16 bytes",
        )?);
        let candidate_catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
            record.column(
                row,
                "candidate_catalogue_revision_id",
                "ledger candidate catalogue identity must be 16 bytes",
            )?,
            &record,
            "ledger candidate catalogue identity must be 16 bytes",
        )?);
        let canonical_bytes: Vec<u8> = record.column(
            row,
            "canonical_bytes",
            "ledger canonical bytes must be bytea",
        )?;
        let digest = Sha256Digest::from_bytes(digest_bytes(
            record.column(row, "digest", "ledger digest must be bytea")?,
            &record,
            "ledger digest must be exactly 32 bytes",
        )?);
        let entry = MigrationLedgerEntry::from_parts(
            format,
            version,
            RevisionPair::new(expected_source, expected_catalogue),
            RevisionPair::new(candidate_source, candidate_catalogue),
            canonical_bytes,
            digest,
        )
        .map_err(PostgresKernelError::InvalidLedgerRequest)?;
        entries.push(entry);
    }

    Ok(entries)
}

async fn load_and_validate_migration_ledger(
    transaction: &Transaction<'_>,
    active: Option<&ActiveDatabaseRevision>,
) -> Result<Vec<MigrationLedgerEntry>, PostgresKernelError> {
    let entries = load_migration_ledger(transaction).await?;
    validate_ledger_chain(&entries, active)?;
    validate_ledger_registries(transaction, &entries).await?;
    Ok(entries)
}

fn validate_ledger_chain(
    entries: &[MigrationLedgerEntry],
    active: Option<&ActiveDatabaseRevision>,
) -> Result<(), PostgresKernelError> {
    if entries.is_empty() {
        if active.is_some_and(|active| active.source().parent().is_some()) {
            return Err(DurableRecord::new(APPLICATION_MIGRATIONS_RELATION, "empty")
                .invariant("an empty migration ledger requires an active root source revision"));
        }
        return Ok(());
    }

    for (index, pair) in entries.windows(2).enumerate() {
        if pair[1].expected_base() != pair[0].candidate_pair() {
            return Err(DurableRecord::new(
                APPLICATION_MIGRATIONS_RELATION,
                format!("row={}", index + 1),
            )
            .invariant("ledger expected base must equal the previous candidate pair"));
        }
    }

    if let Some(active) = active {
        let last = entries
            .last()
            .expect("non-empty migration ledger has a final entry");
        if last.candidate_pair() != active.pair() {
            return Err(DurableRecord::new(APPLICATION_MIGRATIONS_RELATION, "final")
                .invariant("migration ledger must end at the active revision pair"));
        }
    }
    Ok(())
}

async fn validate_ledger_registries(
    transaction: &Transaction<'_>,
    entries: &[MigrationLedgerEntry],
) -> Result<(), PostgresKernelError> {
    let Some(first) = entries.first() else {
        return Ok(());
    };

    let first_expected_source = first.expected_base().source();
    let first_source_record = DurableRecord::new(
        "_orna_kernel.source_revisions",
        first_expected_source.canonical(),
    );
    let first_source_row = transaction
        .query_opt(
            "SELECT parent_source_revision_id
             FROM _orna_kernel.source_revisions
             WHERE id = $1",
            &[&bytes(first_expected_source)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(first_source_row) = first_source_row else {
        return Err(first_source_record.invariant(
            "the first ledger expected source revision must have a durable registry row",
        ));
    };
    let first_source_parent = optional_identity_bytes(
        first_source_record.column(
            &first_source_row,
            "parent_source_revision_id",
            "source parent identity must be null or 16 bytes",
        )?,
        &first_source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    if first_source_parent.is_some() {
        return Err(first_source_record
            .invariant("the first ledger expected source revision must be a root"));
    }

    let first_expected_catalogue = first.expected_base().catalogue();
    let first_catalogue_record = DurableRecord::new(
        "_orna_kernel.catalogue_revisions",
        first_expected_catalogue.canonical(),
    );
    let first_catalogue_row = transaction
        .query_opt(
            "SELECT source_revision_id, parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = $1",
            &[&bytes(first_expected_catalogue)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(first_catalogue_row) = first_catalogue_row else {
        return Err(first_catalogue_record.invariant(
            "the first ledger expected catalogue revision must have a durable registry row",
        ));
    };
    let first_catalogue_source = SourceRevisionId::from_bytes(identity_bytes(
        first_catalogue_record.column(
            &first_catalogue_row,
            "source_revision_id",
            "catalogue source identity must be 16 bytes",
        )?,
        &first_catalogue_record,
        "catalogue source identity must be 16 bytes",
    )?);
    if first_catalogue_source != first_expected_source {
        return Err(first_catalogue_record.invariant(
            "the first ledger expected catalogue revision must belong to its expected source",
        ));
    }
    let first_catalogue_parent = optional_identity_bytes(
        first_catalogue_record.column(
            &first_catalogue_row,
            "parent_catalogue_revision_id",
            "catalogue parent identity must be null or 16 bytes",
        )?,
        &first_catalogue_record,
        "catalogue parent identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    if first_catalogue_parent.is_some() {
        return Err(first_catalogue_record
            .invariant("the first ledger expected catalogue revision must be a root"));
    }

    for entry in entries {
        let expected_base = entry.expected_base();
        let candidate_source = entry.candidate_pair().source();
        let source_record = DurableRecord::new(
            "_orna_kernel.source_revisions",
            candidate_source.canonical(),
        );
        let source_row = transaction
            .query_opt(
                "SELECT parent_source_revision_id
                 FROM _orna_kernel.source_revisions
                 WHERE id = $1",
                &[&bytes(candidate_source)],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let Some(source_row) = source_row else {
            return Err(source_record.invariant(
                "each ledger candidate source revision must have a durable registry row",
            ));
        };
        let parent = optional_identity_bytes(
            source_record.column(
                &source_row,
                "parent_source_revision_id",
                "source parent identity must be null or 16 bytes",
            )?,
            &source_record,
            "source parent identity must be null or 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);
        if parent != Some(expected_base.source()) {
            return Err(source_record.invariant(
                "each ledger candidate source revision must have its expected source parent",
            ));
        }

        let candidate_catalogue = entry.candidate_pair().catalogue();
        let catalogue_record = DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            candidate_catalogue.canonical(),
        );
        let catalogue_row = transaction
            .query_opt(
                "SELECT source_revision_id, parent_catalogue_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&bytes(candidate_catalogue)],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let Some(catalogue_row) = catalogue_row else {
            return Err(catalogue_record.invariant(
                "each ledger candidate catalogue revision must have a durable registry row",
            ));
        };
        let catalogue_source = SourceRevisionId::from_bytes(identity_bytes(
            catalogue_record.column(
                &catalogue_row,
                "source_revision_id",
                "catalogue source identity must be 16 bytes",
            )?,
            &catalogue_record,
            "catalogue source identity must be 16 bytes",
        )?);
        if catalogue_source != candidate_source {
            return Err(catalogue_record.invariant(
                "each ledger candidate catalogue revision must belong to its candidate source",
            ));
        }
        let catalogue_parent = optional_identity_bytes(
            catalogue_record.column(
                &catalogue_row,
                "parent_catalogue_revision_id",
                "catalogue parent identity must be null or 16 bytes",
            )?,
            &catalogue_record,
            "catalogue parent identity must be null or 16 bytes",
        )?
        .map(CatalogueRevisionId::from_bytes);
        if catalogue_parent != Some(expected_base.catalogue()) {
            return Err(catalogue_record.invariant(
                "each ledger candidate catalogue revision must have its expected catalogue parent",
            ));
        }
    }
    Ok(())
}

async fn apply_standard_upgrade_client(
    client: &mut Client,
    candidate: &DeployableRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        // Security writers serialize on active_revision without changing its tuple;
        // ReadCommitted refreshes grant visibility after this transaction waits for that lock.
        .isolation_level(IsolationLevel::ReadCommitted)
        .read_only(false)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = apply_standard_upgrade_transaction(&transaction, candidate, standard).await;
    match result {
        Ok(active) => transaction
            .commit()
            .await
            .map(|()| active)
            .map_err(PostgresKernelError::Database),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(PostgresKernelError::Database(rollback)),
        },
    }
}

async fn apply_transaction(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    artifact: Option<&PhysicalMigrationArtifact>,
    source_apply_audit: bool,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    // This must remain the first statement. It prevents untrusted schemas from
    // changing the meaning of every later static query and DDL statement.
    establish_trusted_search_path(transaction).await?;
    let locked_pair = lock_active_pair(transaction).await?;
    let active = recover_active_revision(transaction).await?;
    if active.pair() != locked_pair {
        return Err(invariant(
            "locked active pair must recover as the same pair",
        ));
    }
    validate_candidate_preflight(&active, candidate)?;
    let artifact = resolve_artifact(&active, candidate, artifact)?;
    validate_migration_artifact(&active, candidate, &artifact)?;
    let existing_ledger = load_and_validate_migration_ledger(transaction, Some(&active)).await?;
    validate_durable_grant_targets(transaction, candidate).await?;

    let materialized = materialize(candidate, &active)?;
    verify_candidate_hashes(candidate, &materialized)?;
    let encoder = CandidateEncoder::new(candidate.catalogue_hash_context(), candidate.candidate());
    validate_postgres_encodings(candidate, &encoder)?;
    apply_materialized_candidate(
        transaction,
        candidate,
        &active,
        &existing_ledger,
        &materialized,
        &encoder,
        &artifact,
        None,
        candidate.catalogue_hash_context().standard(),
        source_apply_audit,
    )
    .await
}

fn resolve_artifact<'a>(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
    supplied: Option<&'a PhysicalMigrationArtifact>,
) -> Result<Cow<'a, PhysicalMigrationArtifact>, PostgresKernelError> {
    match supplied {
        Some(artifact) => Ok(Cow::Borrowed(artifact)),
        None => PhysicalMigrationArtifact::from_revisions(active, candidate)
            .map(Cow::Owned)
            .map_err(map_artifact_build_error),
    }
}

fn validate_migration_artifact(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
    artifact: &PhysicalMigrationArtifact,
) -> Result<(), PostgresKernelError> {
    MigrationLedgerEntry::from_artifact(artifact)
        .validate(active, candidate)
        .map_err(PostgresKernelError::InvalidLedgerRequest)
}

fn map_artifact_build_error(error: PhysicalMigrationArtifactError) -> PostgresKernelError {
    match error {
        PhysicalMigrationArtifactError::Planning(error) => PostgresKernelError::PhysicalPlan(error),
        error => PostgresKernelError::InvalidLedgerRequest(
            MigrationLedgerEntryError::PhysicalArtifact(error),
        ),
    }
}

async fn apply_standard_upgrade_transaction(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    let locked_pair = lock_active_pair(transaction).await?;
    let active = recover_active_revision(transaction).await?;
    if active.pair() != locked_pair {
        return Err(invariant(
            "locked active pair must recover as the same pair",
        ));
    }
    validate_expected_base(&active, candidate)?;
    let selected = candidate
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| invariant("standard upgrade candidate must select a standard snapshot"))?;
    if StandardContextIdentity::from_verified_snapshot(selected)
        != StandardContextIdentity::from_verified_snapshot(standard)
    {
        return Err(invariant(
            "standard upgrade candidate must select the supplied standard snapshot",
        ));
    }
    let artifact = resolve_artifact(&active, candidate, None)?;
    validate_migration_artifact(&active, candidate, &artifact)?;
    let existing_ledger = load_and_validate_migration_ledger(transaction, Some(&active)).await?;
    validate_durable_grant_targets(transaction, candidate).await?;
    scan_reserved_standard_identities(transaction, &active, standard).await?;
    persist_retained_v1_standard_parent(transaction, standard).await?;

    let materialized = materialize(candidate, &active)?;
    let encoder = CandidateEncoder::new(candidate.catalogue_hash_context(), candidate.candidate());
    validate_postgres_encodings(candidate, &encoder)?;
    apply_materialized_candidate(
        transaction,
        candidate,
        &active,
        &existing_ledger,
        &materialized,
        &encoder,
        &artifact,
        Some(standard),
        Some(standard),
        false,
    )
    .await
}

async fn apply_materialized_candidate(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
    existing_ledger: &[MigrationLedgerEntry],
    materialized: &Materialized,
    encoder: &CandidateEncoder<'_>,
    artifact: &PhysicalMigrationArtifact,
    install_standard: Option<&VerifiedStandardLibrarySnapshot>,
    authority_standard: Option<&VerifiedStandardLibrarySnapshot>,
    source_apply_audit: bool,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let plan =
        plan_physical_changes(active, candidate).map_err(PostgresKernelError::PhysicalPlan)?;
    install_physical_plan(transaction, &plan).await?;
    transaction
        .batch_execute("SET CONSTRAINTS ALL DEFERRED")
        .await
        .map_err(PostgresKernelError::Database)?;
    if let Some(standard) = install_standard {
        persist_standard_library(transaction, standard).await?;
    }
    persist_candidate(transaction, candidate, encoder).await?;
    persist_target_authorities(transaction, candidate, authority_standard).await?;
    transaction
        .batch_execute("SET CONSTRAINTS ALL IMMEDIATE")
        .await
        .map_err(PostgresKernelError::Database)?;
    transition_revision_statuses(transaction, candidate, active, materialized).await?;
    verify_revision_statuses(transaction, materialized).await?;
    update_active_pair(transaction, candidate, active.pair()).await?;

    let recovered = recover_active_revision(transaction).await?;
    if recovered.pair() != candidate.candidate_pair()
        || recovered.source().bundle_hash() != candidate.source().bundle_hash()
        || recovered.source().revision_hash() != candidate.source().revision_hash()
        || recovered.catalogue_hash() != candidate.catalogue_hash()
    {
        return Err(invariant(
            "post-apply recovery must exactly reproduce the candidate hashes",
        ));
    }
    persist_migration_ledger(transaction, artifact, existing_ledger).await?;
    if source_apply_audit {
        append_security_audit_event(
            transaction,
            SecurityAuditDecision::recover_source_apply_allowed(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                candidate.candidate_pair(),
            ),
        )
        .await?;
    }
    Ok(recovered)
}

async fn lock_active_pair(
    transaction: &Transaction<'_>,
) -> Result<RevisionPair, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT singleton, source_revision_id, catalogue_revision_id
         FROM _orna_kernel.active_revision FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(invariant(
            "exactly one active revision singleton must exist",
        ));
    }
    let record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    let row = &rows[0];
    let singleton: bool =
        record.column(row, "singleton", "active singleton flag must be boolean")?;
    if !singleton {
        return Err(record.invariant("active singleton flag must be true"));
    }
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_revision_id",
            "active source identity must be exactly 16 bytes",
        )?,
        &record,
        "active source identity must be exactly 16 bytes",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "active catalogue identity must be exactly 16 bytes",
        )?,
        &record,
        "active catalogue identity must be exactly 16 bytes",
    )?);
    Ok(RevisionPair::new(source, catalogue))
}

#[cfg(test)]
#[path = "apply/tests.rs"]
mod tests;
