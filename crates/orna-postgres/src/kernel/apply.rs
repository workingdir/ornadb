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

#[path = "apply/standard.rs"]
mod standard;

#[path = "apply/reserved_identities.rs"]
mod reserved_identities;

use candidate::*;
use encoding::*;
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

fn validate_expected_base(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    if candidate.expected_base() != active.pair() {
        return Err(PostgresKernelError::ExpectedBaseMismatch {
            expected: candidate.expected_base(),
            active: active.pair(),
        });
    }
    Ok(())
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

fn guard_standard_context_transition(
    active: &CatalogueHashContext,
    candidate: &CatalogueHashContext,
) -> Result<(), PostgresKernelError> {
    match (active, candidate) {
        (CatalogueHashContext::Version1, CatalogueHashContext::Version1) => Ok(()),
        (
            CatalogueHashContext::Version2 { standard: active },
            CatalogueHashContext::Version2 {
                standard: candidate,
            },
        ) => {
            let active = StandardContextIdentity::from_verified_snapshot(active);
            let candidate = StandardContextIdentity::from_verified_snapshot(candidate);
            standard_context_mismatch(active, candidate).map_or(Ok(()), Err)
        }
        _ => Err(PostgresKernelError::StandardContextTransitionRequired {
            active: active.version(),
            candidate: candidate.version(),
        }),
    }
}

fn validate_candidate_preflight(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    validate_expected_base(active, candidate)?;
    guard_standard_context_transition(
        active.catalogue_hash_context(),
        candidate.catalogue_hash_context(),
    )?;
    validate_persistable_catalogue(candidate)
        .map_err(PostgresKernelError::CandidateRevisionInvariant)
}

async fn validate_durable_grant_targets(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    const EXECUTE_RELATION: &str = "_orna_kernel.security_execute_grants";
    const PRIVILEGE_RELATION: &str = "_orna_kernel.security_privilege_grants";
    let execute_rows = transaction
        .query(
            "SELECT function_id
             FROM _orna_kernel.security_execute_grants
             ORDER BY grantee_id, function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &execute_rows {
        let record = DurableRecord::new(EXECUTE_RELATION, "candidate");
        let function = FunctionId::from_bytes(identity_bytes(
            record.column(
                row,
                "function_id",
                "durable EXECUTE grant target identity is not exactly 16 bytes",
            )?,
            &record,
            "durable EXECUTE grant target identity is not exactly 16 bytes",
        )?);
        if !candidate_retains_function_target(candidate, function) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: EXECUTE_RELATION,
                record: "candidate".to_owned(),
                rule: "candidate source must retain every durable EXECUTE grant target",
            });
        }
    }

    let privilege_rows = transaction
        .query(
            "SELECT object_id
             FROM _orna_kernel.security_privilege_grants
             ORDER BY grantee_id, privilege_class, object_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &privilege_rows {
        let record = DurableRecord::new(PRIVILEGE_RELATION, "candidate");
        let object: Vec<u8> = record.column(
            row,
            "object_id",
            "durable privilege grant object identity must be empty or exactly 16 bytes",
        )?;
        if object.is_empty() {
            continue;
        }
        let function = FunctionId::from_bytes(identity_bytes(
            object,
            &record,
            "durable privilege grant object identity must be empty or exactly 16 bytes",
        )?);
        if !candidate_retains_privilege_target(candidate, function) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: PRIVILEGE_RELATION,
                record: "candidate".to_owned(),
                rule: "candidate source must retain every durable privilege grant object target",
            });
        }
    }
    Ok(())
}

fn candidate_retains_function_target(candidate: &DeployableRevision, function: FunctionId) -> bool {
    candidate.candidate().function_by_id(function).is_some()
        || candidate
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
}

fn candidate_retains_privilege_target(
    candidate: &DeployableRevision,
    function: FunctionId,
) -> bool {
    candidate_retains_function_target(candidate, function)
        || system_function_by_id(function).is_some()
}

fn standard_context_mismatch(
    active: StandardContextIdentity,
    candidate: StandardContextIdentity,
) -> Option<PostgresKernelError> {
    (active != candidate).then(|| PostgresKernelError::StandardContextMismatch {
        active: Box::new(active),
        candidate: Box::new(candidate),
    })
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

struct Materialized {
    current: Vec<FunctionRevisionRecord>,
    catalogue_hash_context: CatalogueHashContext,
}

fn materialize(
    candidate: &DeployableRevision,
    locked: &ActiveDatabaseRevision,
) -> Result<Materialized, PostgresKernelError> {
    let locked_records = locked
        .function_revisions()
        .iter()
        .chain(locked.historical_function_revisions())
        .map(|revision| (revision.id(), revision.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut locked_numbers = BTreeSet::new();
    let mut locked_hashes = HashSet::new();
    for revision in locked_records.values() {
        locked_numbers.insert((revision.function(), revision.revision_number()));
        locked_hashes.insert((
            revision.function(),
            revision.declaration_content_hash(),
            revision.semantic_hash(),
        ));
    }
    for revision in candidate.new_function_revisions() {
        if locked_records.contains_key(&revision.id())
            || locked_numbers.contains(&(revision.function(), revision.revision_number()))
            || locked_hashes.contains(&(
                revision.function(),
                revision.declaration_content_hash(),
                revision.semantic_hash(),
            ))
        {
            return Err(invariant(
                "a new function revision collides with a locked current or historical revision",
            ));
        }
    }
    let new_by_id = candidate
        .new_function_revisions()
        .iter()
        .map(|revision| (revision.id(), revision))
        .collect::<BTreeMap<_, _>>();
    let mut current = Vec::with_capacity(candidate.candidate().functions().len());
    for function in candidate.candidate().functions() {
        let revision = if let Some(revision) = new_by_id.get(&function.current_revision()) {
            (*revision).clone()
        } else {
            locked_records
                .get(&function.current_revision())
                .cloned()
                .ok_or_else(|| {
                    invariant(
                        "candidate current function revision is absent from locked revision history",
                    )
                })?
        };
        if revision.function() != function.id() {
            return Err(invariant(
                "candidate current function revision must belong to its function",
            ));
        }
        current.push(revision);
    }
    let current_ids = current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    let historical: Vec<_> = locked_records
        .into_values()
        .filter(|revision| !current_ids.contains(&revision.id()))
        .collect();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            candidate.candidate_pair(),
            candidate.source().clone(),
            candidate.candidate().clone(),
            candidate.catalogue_hash(),
            ActiveRevisionContent::new(
                candidate.expressions().to_vec(),
                current.clone(),
                candidate.origins().to_vec(),
                candidate.references().to_vec(),
            )
            .with_history(historical.clone()),
        ),
        candidate.catalogue_hash_context().clone(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok(Materialized {
        current,
        catalogue_hash_context: candidate.catalogue_hash_context().clone(),
    })
}

fn verify_candidate_hashes(
    candidate: &DeployableRevision,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    for unit in candidate.source().units() {
        if source_unit_content_digest(unit.content()).map_err(PostgresKernelError::CanonicalHash)?
            != unit.content_hash()
        {
            return Err(invariant(
                "source unit digest must match exact UTF-8 content",
            ));
        }
    }
    if source_bundle_digest(candidate.source().units())
        .map_err(PostgresKernelError::CanonicalHash)?
        != candidate.source().bundle_hash()
    {
        return Err(invariant(
            "source bundle digest must match candidate source units",
        ));
    }
    if source_revision_digest(candidate.source()).map_err(PostgresKernelError::CanonicalHash)?
        != candidate.source().revision_hash()
    {
        return Err(invariant(
            "source revision digest must match candidate source record",
        ));
    }
    for expression in candidate.expressions() {
        if artifact_payload_digest(expression.payload())
            .map_err(PostgresKernelError::CanonicalHash)?
            != expression.content_hash()
        {
            return Err(invariant(
                "expression artifact digest must match exact payload",
            ));
        }
    }
    for revision in candidate.new_function_revisions() {
        let declaration = declaration_bytes(candidate, revision.declaration_origin())?;
        if function_declaration_digest(declaration).map_err(PostgresKernelError::CanonicalHash)?
            != revision.declaration_content_hash()
        {
            return Err(invariant(
                "function declaration digest must match exact candidate source bytes",
            ));
        }
        if artifact_payload_digest(revision.artifact().payload())
            .map_err(PostgresKernelError::CanonicalHash)?
            != revision.artifact().content_hash()
        {
            return Err(invariant(
                "function artifact digest must match exact payload",
            ));
        }
    }
    let digest = catalogue_digest_with_context(
        &materialized.catalogue_hash_context,
        candidate.candidate(),
        &materialized.current,
        candidate.expressions(),
        candidate.origins(),
        candidate.references(),
    )
    .map_err(PostgresKernelError::CanonicalHash)?;
    if digest != candidate.catalogue_hash() {
        return Err(invariant(
            "candidate catalogue digest must match all current semantic records",
        ));
    }
    Ok(())
}

fn declaration_bytes(
    candidate: &DeployableRevision,
    origin: SourceOrigin,
) -> Result<&[u8], PostgresKernelError> {
    let unit = candidate
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == origin.source_unit())
        .ok_or_else(|| {
            invariant("function declaration origin must identify a candidate source unit")
        })?;
    let content = unit.content().as_bytes();
    let start = usize::try_from(origin.byte_start())
        .map_err(|_| invariant("function declaration origin start must fit usize"))?;
    let end = usize::try_from(origin.byte_end())
        .map_err(|_| invariant("function declaration origin end must fit usize"))?;
    content.get(start..end).ok_or_else(|| {
        invariant("function declaration origin must select exact candidate source bytes")
    })
}

fn validate_postgres_encodings(
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    for expression in candidate.expressions() {
        let _ = positive_i32(expression.version(), "expression format version")?;
    }
    for revision in candidate.new_function_revisions() {
        let _ = positive_i64(revision.revision_number(), "function revision number")?;
        let _ = positive_i32(
            revision.artifact().version(),
            "function artifact format version",
        )?;
    }
    for object in candidate.candidate().object_types() {
        schema_for_name(candidate.candidate(), object.name())?;
        for field in object.fields() {
            let _ = encoder.type_columns(field.resolved_type(), false)?;
            let _ = on_delete(field.on_delete());
        }
    }
    for record_type in candidate.candidate().record_value_types() {
        for field in record_type.fields() {
            let _ = encoder.record_value_field_columns(candidate, field.descriptor())?;
        }
    }
    for function in candidate.candidate().functions() {
        schema_for_name(candidate.candidate(), function.name())?;
        let _ = function_domain(function.domain());
        let _ = function_security(function.security());
        let _ = function_transaction(function.transaction())?;
        let _ = function_volatility(function.volatility());
        for parameter in function.parameters() {
            let _ = encoder.function_type_columns(
                function.domain(),
                parameter.resolved_type(),
                false,
            )?;
        }
        match function.return_type() {
            FunctionReturn::Single(resolved) => {
                let _ = encoder.function_type_columns(function.domain(), *resolved, true)?;
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let _ = encoder.type_columns(column.resolved_type(), false)?;
                }
            }
            FunctionReturn::Stream(resolved) => {
                let _ = encoder.function_type_columns(function.domain(), *resolved, false)?;
            }
        }
    }
    for origin in candidate.origins() {
        validate_origin(origin.source())?;
    }
    for reference in candidate.references() {
        validate_origin(reference.source_origin())?;
        let _ = encoder.reference_target(reference.target())?;
        let _ = reference_kind(reference.kind())?;
    }
    Ok(())
}

fn positive_i32(value: u32, rule: &'static str) -> Result<i32, PostgresKernelError> {
    i32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}
fn positive_i64(value: u64, rule: &'static str) -> Result<i64, PostgresKernelError> {
    i64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}

fn semantic_hash_version(
    version: orna_core::revision::FunctionSemanticHashVersion,
) -> Result<i16, PostgresKernelError> {
    i16::try_from(version.to_u32())
        .map_err(|_| invariant("function semantic hash version must fit PostgreSQL smallint"))
}

fn validate_origin(origin: SourceOrigin) -> Result<(), PostgresKernelError> {
    if origin.byte_start() > origin.byte_end() {
        Err(invariant("source origin must be ordered"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "apply/tests.rs"]
mod tests;
