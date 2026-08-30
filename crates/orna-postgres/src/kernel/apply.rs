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
    physical::plan_physical_changes,
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
use tokio_postgres::{Client, IsolationLevel, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    decode::{DurableRecord, identity_bytes},
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
        let apply_result = apply_client(&mut session.client, candidate, false).await;
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
        let apply_result = apply_client(&mut session.client, candidate, true).await;
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
}

async fn apply_client(
    client: &mut Client,
    candidate: &DeployableRevision,
    source_apply_audit: bool,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .read_only(false)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = apply_transaction(&transaction, candidate, source_apply_audit).await;
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
    validate_durable_grant_targets(transaction, candidate).await?;

    let materialized = materialize(candidate, &active)?;
    verify_candidate_hashes(candidate, &materialized)?;
    let encoder = CandidateEncoder::new(candidate.catalogue_hash_context(), candidate.candidate());
    validate_postgres_encodings(candidate, &encoder)?;

    apply_materialized_candidate(
        transaction,
        candidate,
        &active,
        &materialized,
        &encoder,
        None,
        candidate.catalogue_hash_context().standard(),
        source_apply_audit,
    )
    .await
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
        &materialized,
        &encoder,
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
    materialized: &Materialized,
    encoder: &CandidateEncoder<'_>,
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
