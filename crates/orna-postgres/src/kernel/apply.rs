//! One atomic, fail-closed installation of a compiler deployable revision.

use std::collections::{BTreeMap, BTreeSet, HashSet};

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
use orna_core::system::{
    SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
    SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID, system_function_by_id,
};
use tokio_postgres::{Client, IsolationLevel, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError, is_sealed_inspect_type_id,
    decode::{DurableRecord, identity_bytes},
    physical::{establish_trusted_search_path, install_physical_plan},
    recovery::recover_active_revision,
    security::is_admitted_security_identity,
};

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
        let apply_result = apply_client(&mut session.client, candidate).await;
        let shutdown_result = session.shutdown().await;
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
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .read_only(false)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = apply_transaction(&transaction, candidate).await;
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
        .isolation_level(IsolationLevel::RepeatableRead)
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
    )
    .await
}

/// Persists the retained version-one standard as historical state when the
/// installed standard's source revision descends from it (ADR 0055).
///
/// The V1-to-V2 upgrade is prepared against the empty expected base, so a
/// fresh database has no V1 rows. The V2 source revision is the append-only
/// child of the exact V1 standard source revision, so the single install
/// transaction must persist V1 as retained historical standard state before
/// V2 can claim its parent. The operation is idempotent: a database that
/// already retains V1 is left untouched, and only the exact V2 parent edge is
/// repaired.
async fn persist_retained_v1_standard_parent(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    if standard.digest_version() != StandardLibraryDigestVersion::Version2 {
        return Ok(());
    }
    if standard.source().parent() != Some(STANDARD_SOURCE_REVISION_ID) {
        return Ok(());
    }
    let parent = standard
        .source()
        .parent()
        .expect("the exact V2 parent edge was checked above");
    let row = transaction
        .query_opt(
            "SELECT 1 FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&bytes(parent)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if row.is_some() {
        return Ok(());
    }
    let retained = retained_standard_library_snapshot()
        .and_then(verify_standard_library_snapshot)
        .map_err(|_| invariant("the retained version-one standard must remain verifiable"))?;
    persist_standard_library(transaction, &retained).await
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

/// One durable application identity that would collide with the executable
/// standard snapshot. The compiler identity vocabulary ends at the catalogue
/// type families; the kernel scan extends the disjointness check to the V2
/// executable function, parameter, and function-revision identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardExecutableIdentity {
    /// A standard catalogue function identity.
    Function(FunctionId),
    /// A pinned standard function-revision identity.
    FunctionRevision(FunctionRevisionId),
}

/// One durable application parameter identity, scoped by its owning function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StandardExecutableParameter {
    /// The owning standard catalogue function identity.
    function: FunctionId,
    /// The parameter identity owned by that function.
    parameter: ParameterId,
}

#[derive(Default)]
struct ReservedIdentityLists {
    standard_library_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    catalogue_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_bundles: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_units: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    schemas: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    types: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    type_bindings: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    functions: Vec<(StandardExecutableIdentity, Vec<u8>)>,
    function_revisions: Vec<(StandardExecutableIdentity, Vec<u8>)>,
    parameters: Vec<StandardExecutableParameter>,
}

impl ReservedIdentityLists {
    const fn classes(&self) -> [&Vec<(StandardUpgradeIdentity, Vec<u8>)>; 8] {
        [
            &self.standard_library_revisions,
            &self.catalogue_revisions,
            &self.source_bundles,
            &self.source_revisions,
            &self.source_units,
            &self.schemas,
            &self.types,
            &self.type_bindings,
        ]
    }
}

fn upgrade_reserved_identities(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> ReservedIdentityLists {
    let catalogue = snapshot.catalogue();
    let source = snapshot.source();
    let mut identities = ReservedIdentityLists::default();
    identities.standard_library_revisions.push((
        StandardUpgradeIdentity::StandardLibraryRevision(snapshot.revision()),
        bytes(snapshot.revision()),
    ));
    identities.catalogue_revisions.push((
        StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
        bytes(catalogue.revision()),
    ));
    identities.source_bundles.push((
        StandardUpgradeIdentity::SourceBundle(source.bundle()),
        bytes(source.bundle()),
    ));
    identities.source_revisions.push((
        StandardUpgradeIdentity::SourceRevision(source.id()),
        bytes(source.id()),
    ));
    for unit in source.units() {
        identities.source_units.push((
            StandardUpgradeIdentity::SourceUnit(unit.id()),
            bytes(unit.id()),
        ));
    }
    for schema in catalogue.schemas() {
        identities.schemas.push((
            StandardUpgradeIdentity::Schema(schema.id()),
            bytes(schema.id()),
        ));
    }
    for value_type in catalogue.value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(value_type.id()),
            bytes(value_type.id()),
        ));
    }
    for enum_type in catalogue.enum_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(enum_type.id()),
            bytes(enum_type.id()),
        ));
    }
    for binding in catalogue.type_bindings() {
        identities.type_bindings.push((
            StandardUpgradeIdentity::TypeBinding(binding.id()),
            bytes(binding.id()),
        ));
    }
    for function in catalogue.functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for executable in snapshot.executables() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(executable.revision().id()),
            bytes(executable.revision().id()),
        ));
    }
    identities
}

fn active_visible_reserved_identities(
    active: &ActiveDatabaseRevision,
    include_standard: bool,
) -> ReservedIdentityLists {
    let mut identities = ReservedIdentityLists::default();
    let source = active.source();
    let catalogue = active.catalogue();
    identities.catalogue_revisions.push((
        StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
        bytes(catalogue.revision()),
    ));
    identities.source_bundles.push((
        StandardUpgradeIdentity::SourceBundle(source.bundle()),
        bytes(source.bundle()),
    ));
    identities.source_revisions.push((
        StandardUpgradeIdentity::SourceRevision(source.id()),
        bytes(source.id()),
    ));
    for unit in source.units() {
        identities.source_units.push((
            StandardUpgradeIdentity::SourceUnit(unit.id()),
            bytes(unit.id()),
        ));
    }
    append_catalogue_reserved_identities(catalogue, &mut identities);
    append_application_executable_reserved_identities(active, &mut identities);
    if include_standard && let Some(standard) = active.catalogue_hash_context().standard() {
        let source = standard.source();
        let catalogue = standard.catalogue();
        identities.standard_library_revisions.push((
            StandardUpgradeIdentity::StandardLibraryRevision(standard.revision()),
            bytes(standard.revision()),
        ));
        identities.catalogue_revisions.push((
            StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
            bytes(catalogue.revision()),
        ));
        identities.source_bundles.push((
            StandardUpgradeIdentity::SourceBundle(source.bundle()),
            bytes(source.bundle()),
        ));
        identities.source_revisions.push((
            StandardUpgradeIdentity::SourceRevision(source.id()),
            bytes(source.id()),
        ));
        for unit in source.units() {
            identities.source_units.push((
                StandardUpgradeIdentity::SourceUnit(unit.id()),
                bytes(unit.id()),
            ));
        }
        append_catalogue_reserved_identities(catalogue, &mut identities);
        append_standard_executable_reserved_identities(standard, &mut identities);
    }
    identities
}

fn append_application_executable_reserved_identities(
    active: &ActiveDatabaseRevision,
    identities: &mut ReservedIdentityLists,
) {
    for function in active.catalogue().functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for revision in active.function_revisions() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(revision.id()),
            bytes(revision.id()),
        ));
    }
    for revision in active.historical_function_revisions() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(revision.id()),
            bytes(revision.id()),
        ));
    }
}

fn append_standard_executable_reserved_identities(
    standard: &VerifiedStandardLibrarySnapshot,
    identities: &mut ReservedIdentityLists,
) {
    for function in standard.catalogue().functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for executable in standard.executables() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(executable.revision().id()),
            bytes(executable.revision().id()),
        ));
    }
}

fn append_catalogue_reserved_identities(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    identities: &mut ReservedIdentityLists,
) {
    for schema in catalogue.schemas() {
        identities.schemas.push((
            StandardUpgradeIdentity::Schema(schema.id()),
            bytes(schema.id()),
        ));
    }
    for object_type in catalogue.object_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(object_type.id()),
            bytes(object_type.id()),
        ));
    }
    for value_type in catalogue.value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(value_type.id()),
            bytes(value_type.id()),
        ));
    }
    for enum_type in catalogue.enum_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(enum_type.id()),
            bytes(enum_type.id()),
        ));
    }
    for record_type in catalogue.record_value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(record_type.id()),
            bytes(record_type.id()),
        ));
    }
    for binding in catalogue.type_bindings() {
        identities.type_bindings.push((
            StandardUpgradeIdentity::TypeBinding(binding.id()),
            bytes(binding.id()),
        ));
    }
}

async fn scan_reserved_standard_identities(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    let upgrade = upgrade_reserved_identities(standard);
    // The in-memory collision check considers only the active revision's own
    // application identities: the pinned standard is the append-only parent
    // edge (work ADR 0059), so its reserved identities legitimately overlap
    // the upgrade's retained parent units. The database scan below still
    // excludes those already-installed parent rows from its collision check.
    let active_own = active_visible_reserved_identities(active, false);
    let active = active_visible_reserved_identities(active, true);
    let queries = [
        "SELECT id AS identity FROM _orna_kernel.standard_library_revisions
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT identity FROM (
             SELECT id AS identity FROM _orna_kernel.catalogue_revisions
             UNION
             SELECT catalogue_revision_id AS identity FROM _orna_kernel.standard_library_revisions
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_bundles
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_revisions
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_units
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT identity FROM (
             SELECT schema_id AS identity FROM _orna_kernel.catalogue_schemas
             UNION
             SELECT schema_id AS identity FROM _orna_kernel.standard_catalogue_schemas
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT identity FROM (
             SELECT type_id AS identity FROM _orna_kernel.catalogue_object_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.catalogue_enum_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.catalogue_record_value_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.standard_catalogue_value_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.standard_catalogue_enum_types
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT type_binding_id AS identity FROM _orna_kernel.standard_catalogue_type_bindings
         WHERE type_binding_id = ANY($1) AND NOT (type_binding_id = ANY($2))
         ORDER BY type_binding_id LIMIT 1",
    ];
    for (((upgrade_class, active_own_class), active_class), query) in upgrade
        .classes()
        .into_iter()
        .zip(active_own.classes())
        .zip(active.classes())
        .zip(queries)
    {
        if let Some(identity) = first_active_reserved_identity(active_own_class, upgrade_class) {
            return Err(PostgresKernelError::ReservedStandardIdentity { identity });
        }
        let requested = upgrade_class
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        if requested.is_empty() {
            continue;
        }
        let excluded = active_class
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(query, &[&requested, &excluded])
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) = first_inactive_reserved_identity(upgrade_class, &[identity])
            else {
                return Err(invariant(
                    "reserved standard identity query must return one requested identity",
                ));
            };
            return Err(PostgresKernelError::ReservedStandardIdentity { identity: reserved });
        }
    }

    if let Some(identity) =
        first_active_standard_executable_identity(&active_own.functions, &upgrade.functions)
    {
        return Err(standard_executable_reserved(identity));
    }
    let requested = upgrade
        .functions
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect::<Vec<_>>();
    if !requested.is_empty() {
        let excluded = active
            .functions
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(
                "SELECT function_id AS identity FROM _orna_kernel.catalogue_functions
                 WHERE function_id = ANY($1) AND NOT (function_id = ANY($2))
                 ORDER BY function_id LIMIT 1",
                &[&requested, &excluded],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) =
                first_inactive_standard_executable_identity(&upgrade.functions, &[identity])
            else {
                return Err(invariant(
                    "reserved standard function identity query must return one requested identity",
                ));
            };
            return Err(standard_executable_reserved(reserved));
        }
    }
    for function in standard.catalogue().functions() {
        let name = function.name().parts().to_vec();
        let rows = transaction
            .query(
                "SELECT function_id AS identity FROM _orna_kernel.catalogue_functions
                 WHERE name_parts = $1 ORDER BY function_id LIMIT 1",
                &[&name],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(standard_executable_name_reserved(function.name()));
        }
    }

    if let Some(identity) = first_active_standard_executable_identity(
        &active_own.function_revisions,
        &upgrade.function_revisions,
    ) {
        return Err(standard_executable_reserved(identity));
    }
    let requested = upgrade
        .function_revisions
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect::<Vec<_>>();
    if !requested.is_empty() {
        let excluded = active
            .function_revisions
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(
                "SELECT id AS identity FROM _orna_kernel.function_revisions
                 WHERE id = ANY($1) AND NOT (id = ANY($2))
                 ORDER BY id LIMIT 1",
                &[&requested, &excluded],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) = first_inactive_standard_executable_identity(
                &upgrade.function_revisions,
                &[identity],
            ) else {
                return Err(invariant(
                    "reserved standard function revision query must return one requested identity",
                ));
            };
            return Err(standard_executable_reserved(reserved));
        }
    }

    if let Some(parameter) =
        first_active_standard_parameter(&active_own.parameters, &upgrade.parameters)
    {
        return Err(standard_executable_parameter_reserved(parameter));
    }
    let parameter_functions = upgrade
        .parameters
        .iter()
        .map(|parameter| bytes(parameter.function))
        .collect::<Vec<_>>();
    let parameter_ids = upgrade
        .parameters
        .iter()
        .map(|parameter| bytes(parameter.parameter))
        .collect::<Vec<_>>();
    if !parameter_functions.is_empty() {
        let rows = transaction
            .query(
                "SELECT 1 FROM _orna_kernel.catalogue_function_parameters AS parameter
                 JOIN unnest($1::bytea[], $2::bytea[])
                   AS wanted(function_id, parameter_id)
                   ON parameter.function_id = wanted.function_id
                  AND parameter.parameter_id = wanted.parameter_id
                 LIMIT 1",
                &[&parameter_functions, &parameter_ids],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(standard_executable_parameter_reserved(
                upgrade.parameters[0],
            ));
        }
    }
    Ok(())
}

fn standard_executable_reserved(identity: StandardExecutableIdentity) -> PostgresKernelError {
    let (relation, rule) = match identity {
        StandardExecutableIdentity::Function(_) => (
            "_orna_kernel.catalogue_functions",
            "application catalogue functions must not reuse a standard executable function identity",
        ),
        StandardExecutableIdentity::FunctionRevision(_) => (
            "_orna_kernel.function_revisions",
            "application function revisions must not reuse a standard executable revision identity",
        ),
    };
    PostgresKernelError::DurableInvariant {
        relation,
        record: format!("{identity:?}"),
        rule,
    }
}

fn standard_executable_name_reserved(name: &QualifiedSemanticName) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.catalogue_functions",
        record: name.parts().join("."),
        rule: "application catalogue functions must not reuse a standard executable function name",
    }
}

fn standard_executable_parameter_reserved(
    parameter: StandardExecutableParameter,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.catalogue_function_parameters",
        record: format!("{:?}", parameter.parameter),
        rule: "application catalogue parameters must not reuse a standard executable parameter identity within its owning function",
    }
}

fn first_active_standard_executable_identity(
    active: &[(StandardExecutableIdentity, Vec<u8>)],
    upgrade: &[(StandardExecutableIdentity, Vec<u8>)],
) -> Option<StandardExecutableIdentity> {
    active
        .iter()
        .find(|(_, bytes)| upgrade.iter().any(|(_, wanted)| wanted == bytes))
        .map(|(identity, _)| *identity)
}

fn first_inactive_standard_executable_identity(
    upgrade: &[(StandardExecutableIdentity, Vec<u8>)],
    inactive_raw_order: &[Vec<u8>],
) -> Option<StandardExecutableIdentity> {
    inactive_raw_order.iter().find_map(|identity| {
        upgrade
            .iter()
            .find(|(_, wanted)| wanted == identity)
            .map(|(reserved, _)| *reserved)
    })
}

fn first_active_standard_parameter(
    active: &[StandardExecutableParameter],
    upgrade: &[StandardExecutableParameter],
) -> Option<StandardExecutableParameter> {
    active
        .iter()
        .find(|parameter| upgrade.contains(parameter))
        .copied()
}

fn first_active_reserved_identity(
    active: &[(StandardUpgradeIdentity, Vec<u8>)],
    upgrade: &[(StandardUpgradeIdentity, Vec<u8>)],
) -> Option<StandardUpgradeIdentity> {
    active
        .iter()
        .find(|(_, bytes)| upgrade.iter().any(|(_, wanted)| wanted == bytes))
        .map(|(identity, _)| *identity)
}

fn first_inactive_reserved_identity(
    upgrade: &[(StandardUpgradeIdentity, Vec<u8>)],
    inactive_raw_order: &[Vec<u8>],
) -> Option<StandardUpgradeIdentity> {
    inactive_raw_order.iter().find_map(|identity| {
        upgrade
            .iter()
            .find(|(_, wanted)| wanted == identity)
            .map(|(reserved, _)| *reserved)
    })
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
            let _ = encoder.function_type_columns(function.domain(), parameter.resolved_type(), false)?;
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

async fn persist_candidate(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let source = candidate.source();
    persist_source(transaction, source, false).await?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash,
                 hash_algorithm, hash_contract_version, canonical_hash_version,
                 standard_library_revision_id)
             VALUES ($1, $2, $3, $4, 'sha256', $5, $6, $7)",
            &[
                &bytes(candidate.candidate().revision()),
                &bytes(source.id()),
                &bytes(candidate.parent_catalogue()),
                &digest(candidate.catalogue_hash()),
                &CONTRACT_VERSION,
                &encoder.catalogue_hash_version()?,
                &encoder.standard_library_revision().map(bytes),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    persist_semantics(transaction, candidate, encoder).await?;
    persist_revisions_and_references(transaction, candidate, encoder).await
}

async fn persist_source(
    transaction: &Transaction<'_>,
    source: &orna_core::revision::StoredSourceRevision,
    reuse_existing_units: bool,
) -> Result<(), PostgresKernelError> {
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', $3)",
            &[
                &bytes(source.bundle()),
                &digest(source.bundle_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for unit in source.units() {
        if reuse_existing_units {
            // The append-only standard parent edge (work ADR 0059): an
            // already-installed unit with the same reserved identity is the
            // retained parent unit. It must be byte-identical and is
            // re-parented into the child bundle so the child snapshot owns
            // its complete unit set; any other pre-existing row fails closed.
            let existing = transaction
                .query_opt(
                    "SELECT ordinal, logical_path, content, content_hash
                     FROM _orna_kernel.source_units WHERE id = $1",
                    &[&bytes(unit.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            if let Some(row) = existing {
                let ordinal: i64 = row.try_get(0).map_err(PostgresKernelError::Database)?;
                let logical_path: String = row.try_get(1).map_err(PostgresKernelError::Database)?;
                let content: String = row.try_get(2).map_err(PostgresKernelError::Database)?;
                let content_hash: Vec<u8> =
                    row.try_get(3).map_err(PostgresKernelError::Database)?;
                if ordinal != i64::from(unit.ordinal())
                    || logical_path != unit.logical_path()
                    || content != unit.content()
                    || content_hash != digest(unit.content_hash())
                {
                    return Err(invariant(
                        "reused standard source unit must be byte-identical to the retained parent",
                    ));
                }
                transaction
                    .execute(
                        "UPDATE _orna_kernel.source_units SET bundle_id = $1 WHERE id = $2",
                        &[&bytes(source.bundle()), &bytes(unit.id())],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                continue;
            }
        }
        transaction
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash,
                     hash_algorithm, encoding, hash_contract_version)
                 VALUES ($1, $2, $3, $4, $5, $6, 'sha256', 'utf-8', $7)",
                &[
                    &bytes(unit.id()),
                    &bytes(source.bundle()),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &digest(unit.content_hash()),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, 'sha256', $5)",
            &[
                &bytes(source.id()),
                &source.parent().map(bytes),
                &bytes(source.bundle()),
                &digest(source.revision_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn persist_standard_library(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    persist_source(transaction, standard.source(), true).await?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_library_revisions
                (id, source_revision_id, catalogue_revision_id, digest_version,
                 language_version, content_hash, hash_algorithm)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256')",
            &[
                &bytes(standard.revision()),
                &bytes(standard.source().id()),
                &bytes(standard.catalogue().revision()),
                &standard_digest_version(standard.digest_version())?,
                &standard.language_version(),
                &digest(standard.digest()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for schema in standard.catalogue().schemas() {
        let source = origin(standard.origins(), DefinitionIdentity::Schema(schema.id()))?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_schemas
                    (standard_library_revision_id, schema_id, name_parts, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &bytes(standard.revision()),
                    &bytes(schema.id()),
                    &schema.name().parts(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for value_type in standard.catalogue().value_types() {
        let schema = schema_for_name(standard.catalogue(), value_type.name())?;
        let source = origin(
            standard.origins(),
            DefinitionIdentity::ValueType(value_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_value_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, value_kind,
                     mutability, persistence, representation_contract, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &bytes(standard.revision()),
                    &bytes(value_type.id()),
                    &bytes(schema),
                    &value_type.name().parts(),
                    &standard_value_kind(value_type.kind())?,
                    &standard_value_mutability(value_type.mutability())?,
                    &standard_value_persistence(value_type.persistence())?,
                    &value_type.representation_contract(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for enum_type in standard.catalogue().enum_types() {
        let schema = schema_for_name(standard.catalogue(), enum_type.name())?;
        let source = origin(
            standard.origins(),
            DefinitionIdentity::ValueType(enum_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_enum_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &bytes(standard.revision()),
                    &bytes(enum_type.id()),
                    &bytes(schema),
                    &enum_type.name().parts(),
                    &enum_type.labels(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for binding in standard.catalogue().type_bindings() {
        let source = origin(
            standard.origins(),
            DefinitionIdentity::TypeBinding(binding.id()),
        )?;
        let (kind, name_parts) = standard_type_binding_name(binding.kind(), binding.name())?;
        let (target_kind, value_target, enum_target) = if standard
            .catalogue()
            .value_type_by_id(binding.target())
            .is_some()
        {
            ("value", Some(bytes(binding.target())), None)
        } else if standard
            .catalogue()
            .enum_type_by_id(binding.target())
            .is_some()
        {
            ("enum", None, Some(bytes(binding.target())))
        } else {
            return Err(invariant(
                "standard type binding target must identify one standard value or enum type",
            ));
        };
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_type_bindings
                    (standard_library_revision_id, type_binding_id, kind, name_parts,
                     target_type_kind, target_type_id, target_enum_type_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &bytes(standard.revision()),
                    &bytes(binding.id()),
                    &kind,
                    &name_parts,
                    &target_kind,
                    &value_target,
                    &enum_target,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    validate_standard_executable_facts(
        standard.digest_version(),
        standard.catalogue(),
        standard.executables(),
    )?;
    if standard.digest_version() == StandardLibraryDigestVersion::Version2 {
        persist_standard_executable_facts(transaction, standard).await?;
    }
    Ok(())
}

/// Fail-closed checks for the executable facts of one verified standard
/// snapshot. A version-1 snapshot must carry no executable; a version-2
/// snapshot must carry one executable per catalogue function. Each
/// executable's catalogue function, current revision, artifact domain, and
/// reference evidence must agree.
fn validate_standard_executable_facts(
    digest_version: StandardLibraryDigestVersion,
    catalogue: &CatalogueSnapshot,
    executables: &[StandardExecutable],
) -> Result<(), PostgresKernelError> {
    match digest_version {
        StandardLibraryDigestVersion::Version1 => {
            if !executables.is_empty() {
                return Err(invariant(
                    "version-one standard library snapshot must not carry executable records",
                ));
            }
        }
        StandardLibraryDigestVersion::Version2 => {
            if executables.is_empty() || executables.len() != catalogue.functions().len() {
                return Err(invariant(
                    "version-two standard library snapshot must carry one executable per catalogue function",
                ));
            }
            let mut function_ids = HashSet::with_capacity(executables.len());
            for executable in executables {
                if !function_ids.insert(executable.function()) {
                    return Err(invariant(
                        "version-two standard library snapshot must carry each catalogue function exactly once",
                    ));
                }
                let function = catalogue
                    .function_by_id(executable.function())
                    .ok_or_else(|| {
                        invariant("standard executable function must exist in the standard catalogue")
                    })?;
                if function.current_revision() != executable.revision().id() {
                    return Err(invariant(
                        "standard catalogue function and executable current revision must agree",
                    ));
                }
                if executable.revision().function() != executable.function() {
                    return Err(invariant(
                        "standard executable revision function must agree with its executable",
                    ));
                }
                let domain_matches = match function.domain() {
                    FunctionDomain::Server => matches!(
                        executable.revision().artifact().kind(),
                        ExecutableArtifactKind::Server
                    ),
                    FunctionDomain::Client => matches!(
                        executable.revision().artifact().kind(),
                        ExecutableArtifactKind::Client
                    ),
                };
                if !domain_matches {
                    return Err(invariant(
                        "standard executable artifact kind must match its function domain",
                    ));
                }
                for (index, reference) in executable.references().iter().enumerate() {
                    let ordinal = u32::try_from(index).map_err(|_| {
                        invariant("standard executable reference ordinal must fit the u32 range")
                    })?;
                    if reference.ordinal() != ordinal
                        || reference.source_function() != executable.function()
                        || reference.source_revision() != executable.revision().id()
                    {
                        return Err(invariant(
                            "standard executable references must name the exact function revision with contiguous zero-based ordinals",
                        ));
                    }
                }
            }
        }
        _ => {
            return Err(invariant(
                "standard library digest version is not supported by PostgreSQL persistence",
            ));
        }
    }
    Ok(())
}

/// Persists the complete V2 standard executable facts: the immutable function
/// revisions, their server or client artifacts, the catalogue functions with
/// their resolved signatures, the ordered parameter records, and the ordered
/// reference sequences. Every row is written under the selected standard
/// revision.
async fn persist_standard_executable_facts(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    validate_standard_executable_facts(
        standard.digest_version(),
        standard.catalogue(),
        standard.executables(),
    )?;
    for executable in standard.executables() {
        persist_standard_executable_fact(transaction, standard, executable).await?;
    }
    Ok(())
}

/// Persists one standard executable and all facts owned by its function
/// revision.
async fn persist_standard_executable_fact(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
    executable: &StandardExecutable,
) -> Result<(), PostgresKernelError> {
    let catalogue = standard.catalogue();
    let function = catalogue
        .function_by_id(executable.function())
        .ok_or_else(|| {
            invariant("standard executable function must exist in the standard catalogue")
        })?;
    let revision = executable.revision();
    let artifact = revision.artifact();
    let standard_revision = standard.revision();
    let function_origin = origin(
        standard.origins(),
        DefinitionIdentity::Function(function.id()),
    )?;
    let schema = schema_for_name(catalogue, function.name())?;
    let revision_number = positive_i64(
        revision.revision_number(),
        "standard function revision number",
    )?;
    let artifact_version = positive_i32(
        artifact.version(),
        "standard function artifact format version",
    )?;
    let semantic_hash_version = semantic_hash_version(revision.semantic_hash_version())?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_function_revisions
                (standard_library_revision_id, function_revision_id, function_id,
                 revision_number, declaration_source_unit_id, declaration_source_start,
                 declaration_source_end, declaration_content_hash, semantic_hash,
                 semantic_hash_version, language_version, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &bytes(standard_revision),
                &bytes(revision.id()),
                &bytes(revision.function()),
                &revision_number,
                &bytes(revision.declaration_origin().source_unit()),
                &i64::from(revision.declaration_origin().byte_start()),
                &i64::from(revision.declaration_origin().byte_end()),
                &digest(revision.declaration_content_hash()),
                &digest(revision.semantic_hash()),
                &semantic_hash_version,
                &revision.language_version(),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_function_artifacts
                (standard_library_revision_id, function_revision_id, artifact_kind,
                 format, format_version, payload, content_hash, hash_algorithm,
                 hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'sha256', $8)",
            &[
                &bytes(standard_revision),
                &bytes(revision.id()),
                &artifact_kind(artifact.kind()),
                &artifact.format(),
                &artifact_version,
                &artifact.payload(),
                &digest(artifact.content_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let (return_shape, return_kind, return_scalar, return_value_type) = match function.return_type()
    {
        FunctionReturn::Single(value) => {
            let columns = standard_resolved_type_columns(*value, true)?;
            (
                "single",
                Some(columns.kind),
                columns.scalar,
                columns.value_type.map(bytes),
            )
        }
        FunctionReturn::Rows(_) => {
            return Err(invariant(
                "standard catalogue functions with ROWS results are not supported by standard persistence",
            ));
        }
        FunctionReturn::Stream(_) => {
            return Err(invariant(
                "standard catalogue functions with STREAM results are not supported by standard persistence",
            ));
        }
    };
    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_functions
                (standard_library_revision_id, function_id, schema_id, name_parts,
                 domain, security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_value_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
            &[
                &bytes(standard_revision),
                &bytes(function.id()),
                &bytes(schema),
                &function.name().parts(),
                &function_domain(function.domain()),
                &function_security(function.security()),
                &function_transaction(function.transaction())?,
                &function_volatility(function.volatility()),
                &return_shape,
                &return_kind,
                &return_scalar,
                &return_value_type,
                &bytes(function.current_revision()),
                &bytes(function_origin.source_unit()),
                &i64::from(function_origin.byte_start()),
                &i64::from(function_origin.byte_end()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for parameter in function.parameters() {
        let columns = standard_resolved_type_columns(parameter.resolved_type(), false)?;
        let parameter_origin = origin(
            standard.origins(),
            DefinitionIdentity::Parameter {
                owner: function.id(),
                parameter: parameter.id(),
            },
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_function_parameters
                    (standard_library_revision_id, function_id, parameter_id, name,
                     ordinal, type_kind, scalar_type, value_type_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &bytes(standard_revision),
                    &bytes(function.id()),
                    &bytes(parameter.id()),
                    &parameter.name(),
                    &i64::from(parameter.ordinal()),
                    &columns.kind,
                    &columns.scalar,
                    &columns.value_type.map(bytes),
                    &bytes(parameter_origin.source_unit()),
                    &i64::from(parameter_origin.byte_start()),
                    &i64::from(parameter_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for reference in executable.references() {
        let (target_kind, target_definition_id, owner_type, owner_function, standard_pin) =
            standard_reference_target_columns(reference.target(), standard_revision)?;
        let reference_kind = reference_kind(reference.kind())?;
        let source = reference.source_origin();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_definition_references
                    (standard_library_revision_id, function_revision_id, ordinal,
                     target_definition_id, target_kind, target_owner_type_id,
                     target_owner_function_id, target_standard_library_revision_id,
                     reference_kind, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                &[
                    &bytes(standard_revision),
                    &bytes(reference.source_revision()),
                    &i64::from(reference.ordinal()),
                    &target_definition_id,
                    &target_kind,
                    &owner_type,
                    &owner_function,
                    &standard_pin,
                    &reference_kind,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

/// The closed resolved-type projection persisted for standard catalogue
/// functions and parameters. Standard signatures resolve to legacy scalars or
/// pinned standard value types; every other shape is rejected.
struct StandardResolvedTypeColumns {
    kind: &'static str,
    scalar: Option<&'static str>,
    value_type: Option<TypeId>,
}

fn standard_resolved_type_columns(
    value: ResolvedType,
    allow_void: bool,
) -> Result<StandardResolvedTypeColumns, PostgresKernelError> {
    if let Some(value) = value.legacy_scalar() {
        return Ok(StandardResolvedTypeColumns {
            kind: "scalar",
            scalar: Some(scalar(value, allow_void)?),
            value_type: None,
        });
    }
    if let Some(value_type) = value.value_type() {
        return Ok(StandardResolvedTypeColumns {
            kind: "value",
            scalar: None,
            value_type: Some(value_type),
        });
    }
    Err(invariant(
        "standard catalogue resolved types must be scalar or standard value types",
    ))
}

/// The closed reference target projection persisted for standard definition
/// references. The standard pin is the row's own standard revision.
type StandardReferenceTargetColumns = (
    &'static str,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

fn standard_reference_target_columns(
    value: DefinitionReferenceTarget,
    standard_revision: StandardLibraryRevisionId,
) -> Result<StandardReferenceTargetColumns, PostgresKernelError> {
    Ok(match value {
        DefinitionReferenceTarget::ObjectType(id) => ("object_type", bytes(id), None, None, None),
        DefinitionReferenceTarget::Field { owner, field } => {
            ("field", bytes(field), Some(bytes(owner)), None, None)
        }
        DefinitionReferenceTarget::Function(id) => ("function", bytes(id), None, None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => (
            "parameter",
            bytes(parameter),
            None,
            Some(bytes(owner)),
            None,
        ),
        DefinitionReferenceTarget::ValueType(id) => (
            "value_type",
            bytes(id),
            None,
            None,
            Some(bytes(standard_revision)),
        ),
        DefinitionReferenceTarget::Expression(id) => ("expression", bytes(id), None, None, None),
        _ => {
            return Err(invariant(
                "definition reference target is not supported by standard persistence",
            ));
        }
    })
}

/// Persists one target-authority row for every applied application catalogue
/// function and, for a version-two standard install, one standard authority
/// row under the new application catalogue revision. Apply is the only writer
/// of this relation; the migration backfilled the historical application
/// rows. A missing or duplicate authority row fails the apply closed.
async fn persist_target_authorities(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    standard: Option<&VerifiedStandardLibrarySnapshot>,
) -> Result<(), PostgresKernelError> {
    let catalogue_revision = candidate.candidate().revision();
    let system_identity_functions = [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ];
    let mut expected_authority_count =
        candidate.candidate().functions().len() as i64 + system_identity_functions.len() as i64;
    for function in candidate.candidate().functions() {
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'application', $3, NULL)",
                &[
                    &bytes(catalogue_revision),
                    &bytes(function.id()),
                    &bytes(function.current_revision()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for function in system_identity_functions {
        if !system_function_by_id(function).is_some_and(is_admitted_security_identity) {
            return Err(invariant(
                "persisted system invocation target must be an admitted sealed security identity",
            ));
        }
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'system', $2, NULL)",
                &[&bytes(catalogue_revision), &bytes(function)],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    if let Some(standard) = standard
        && standard.digest_version() == StandardLibraryDigestVersion::Version2
    {
        validate_standard_executable_facts(
            standard.digest_version(),
            standard.catalogue(),
            standard.executables(),
        )?;
        for executable in standard.executables() {
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.invocation_target_authorities
                        (catalogue_revision_id, function_id, target_class,
                         function_revision_id, standard_library_revision_id)
                     VALUES ($1, $2, 'standard', $3, $4)",
                    &[
                        &bytes(catalogue_revision),
                        &bytes(executable.function()),
                        &bytes(executable.revision().id()),
                        &bytes(standard.revision()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            expected_authority_count += 1;
        }
    }
    let rows = transaction
        .query(
            "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
             WHERE catalogue_revision_id = $1",
            &[&bytes(catalogue_revision)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let written: i64 = rows[0].try_get(0).map_err(PostgresKernelError::Database)?;
    if written != expected_authority_count {
        return Err(invariant(
            "invocation target authority rows must match the applied catalogue functions and sealed system audit anchors exactly",
        ));
    }
    Ok(())
}

fn standard_digest_version(
    version: orna_core::revision::StandardLibraryDigestVersion,
) -> Result<i16, PostgresKernelError> {
    i16::try_from(version.to_u32())
        .map_err(|_| invariant("standard library digest version must fit PostgreSQL smallint"))
}

fn standard_value_kind(value: ValueTypeKind) -> Result<&'static str, PostgresKernelError> {
    match value {
        ValueTypeKind::Primitive => Ok("primitive"),
        ValueTypeKind::Opaque => Ok("opaque"),
        _ => Err(invariant(
            "standard value type kind is not supported by PostgreSQL persistence",
        )),
    }
}

fn standard_value_mutability(
    value: ValueTypeMutability,
) -> Result<&'static str, PostgresKernelError> {
    if matches!(value, ValueTypeMutability::Immutable) {
        Ok("immutable")
    } else {
        Err(invariant(
            "standard value type mutability is not supported by PostgreSQL persistence",
        ))
    }
}

fn standard_value_persistence(
    value: ValueTypePersistence,
) -> Result<&'static str, PostgresKernelError> {
    if matches!(value, ValueTypePersistence::Persistable) {
        Ok("persistable")
    } else if matches!(value, ValueTypePersistence::Transient) {
        Ok("transient")
    } else {
        Err(invariant(
            "standard value type persistence is not supported by PostgreSQL persistence",
        ))
    }
}

fn standard_type_binding_name(
    kind: TypeBindingKind,
    name: &TypeLookupName,
) -> Result<(&'static str, Vec<String>), PostgresKernelError> {
    if matches!(kind, TypeBindingKind::Qualified) {
        if let TypeLookupName::Qualified(name) = name {
            return Ok(("qualified", name.parts().to_vec()));
        }
        return Err(invariant(
            "qualified standard type binding must retain a qualified name",
        ));
    }
    if matches!(kind, TypeBindingKind::Prelude) {
        if let TypeLookupName::Prelude(name) = name {
            return Ok(("prelude", name.words().to_vec()));
        }
        return Err(invariant(
            "prelude standard type binding must retain a prelude name",
        ));
    }
    Err(invariant(
        "standard type binding kind is not supported by PostgreSQL persistence",
    ))
}

async fn persist_semantics(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for schema in candidate.candidate().schemas() {
        let origin = origin(candidate.origins(), DefinitionIdentity::Schema(schema.id()))?;
        transaction.execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &bytes(catalogue), &bytes(schema.id()), &schema.name().parts(),
                &bytes(origin.source_unit()), &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    }
    for expression in candidate.expressions() {
        let expression_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Expression(expression.id()),
        )?;
        let version = positive_i32(expression.version(), "expression format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                (catalogue_revision_id, expression_id, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version, source_unit_id,
                 source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, $8, $9, $10)",
                &[
                    &bytes(catalogue),
                    &bytes(expression.id()),
                    &expression.format(),
                    &version,
                    &expression.payload(),
                    &digest(expression.content_hash()),
                    &CONTRACT_VERSION,
                    &bytes(expression_origin.source_unit()),
                    &i64::from(expression_origin.byte_start()),
                    &i64::from(expression_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for enum_type in candidate.candidate().enum_types() {
        let schema = schema_for_name(candidate.candidate(), enum_type.name())?;
        let origin = origin(
            candidate.origins(),
            DefinitionIdentity::ValueType(enum_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_enum_types
                    (catalogue_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &bytes(catalogue),
                    &bytes(enum_type.id()),
                    &bytes(schema),
                    &enum_type.name().parts(),
                    &enum_type.labels(),
                    &bytes(origin.source_unit()),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for record_type in candidate.candidate().record_value_types() {
        let schema = schema_for_name(candidate.candidate(), record_type.name())?;
        let type_origin = origin(
            candidate.origins(),
            DefinitionIdentity::ValueType(record_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_record_value_types
                    (catalogue_revision_id, type_id, schema_id, name_parts,
                     value_kind, mutability, persistence,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, 'record', 'immutable', 'persistable',
                         $5, $6, $7)",
                &[
                    &bytes(catalogue),
                    &bytes(record_type.id()),
                    &bytes(schema),
                    &record_type.name().parts(),
                    &bytes(type_origin.source_unit()),
                    &i64::from(type_origin.byte_start()),
                    &i64::from(type_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;

        for field in record_type.fields() {
            let RecordValueFieldColumns {
                kind,
                value_type,
                value_standard_library_revision,
                application_enum_type,
                enum_standard_library_revision,
                standard_enum_type,
                application_record_type,
            } = encoder.record_value_field_columns(candidate, field.descriptor())?;
            let field_origin = origin(
                candidate.origins(),
                DefinitionIdentity::Field {
                    owner: record_type.id(),
                    field: field.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_record_value_fields
                        (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                         type_kind, value_type_id, value_standard_library_revision_id,
                         enum_type_id, enum_standard_library_revision_id,
                         standard_enum_type_id, record_type_id,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
                    &[
                        &bytes(catalogue),
                        &bytes(record_type.id()),
                        &bytes(field.id()),
                        &field.name(),
                        &i64::from(field.ordinal()),
                        &kind,
                        &value_type.map(bytes),
                        &value_standard_library_revision.map(bytes),
                        &application_enum_type.map(bytes),
                        &enum_standard_library_revision.map(bytes),
                        &standard_enum_type.map(bytes),
                        &application_record_type.map(bytes),
                        &bytes(field_origin.source_unit()),
                        &i64::from(field_origin.byte_start()),
                        &i64::from(field_origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
    }
    for object in candidate.candidate().object_types() {
        let schema = schema_for_name(candidate.candidate(), object.name())?;
        let origin = origin(
            candidate.origins(),
            DefinitionIdentity::ObjectType(object.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_object_types
                (catalogue_revision_id, type_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &bytes(catalogue),
                    &bytes(object.id()),
                    &bytes(schema),
                    &object.name().parts(),
                    &bytes(origin.source_unit()),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for object in candidate.candidate().object_types() {
        for field in object.fields() {
            let TypeColumns {
                kind,
                scalar,
                target,
                value_type,
                standard_library_revision,
                enum_type,
                record_type,
            } = encoder.type_columns(field.resolved_type(), false)?;
            let delete = on_delete(field.on_delete());
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Field {
                    owner: object.id(),
                    field: field.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_fields
                    (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, value_type_id,
                     value_standard_library_revision_id, enum_type_id, record_type_id,
                     nullable, is_unique,
                     default_expression_id, on_delete, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
                    &[
                        &bytes(catalogue),
                        &bytes(object.id()),
                        &bytes(field.id()),
                        &field.name(),
                        &i64::from(field.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &value_type.map(bytes),
                        &standard_library_revision.map(bytes),
                        &enum_type.map(bytes),
                        &record_type.map(bytes),
                        &field.nullable(),
                        &field.unique(),
                        &field.default_expression().map(bytes),
                        &delete,
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
    }
    persist_functions(transaction, candidate, encoder).await
}

async fn persist_functions(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for function in candidate.candidate().functions() {
        let schema = schema_for_name(candidate.candidate(), function.name())?;
        let function_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Function(function.id()),
        )?;
        let (
            shape,
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
            enum_type,
            record_type,
        ) = match function.return_type() {
            FunctionReturn::Single(value) => {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.function_type_columns(function.domain(), *value, true)?;
                (
                    "single",
                    Some(kind),
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                )
            }
            FunctionReturn::Rows(_) => ("rows", None, None, None, None, None, None, None),
            FunctionReturn::Stream(value) => {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.function_type_columns(function.domain(), *value, false)?;
                (
                    "stream",
                    Some(kind),
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                )
            }
        };
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                 security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_target_type_id,
                 return_value_type_id, return_standard_library_revision_id,
                 return_enum_type_id, return_record_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
                &[
                    &bytes(catalogue),
                    &bytes(function.id()),
                    &bytes(schema),
                    &function.name().parts(),
                    &function_domain(function.domain()),
                    &function_security(function.security()),
                    &function_transaction(function.transaction())?,
                    &function_volatility(function.volatility()),
                    &shape,
                    &kind,
                    &scalar,
                    &target.map(bytes),
                    &value_type.map(bytes),
                    &standard_library_revision.map(bytes),
                    &enum_type.map(bytes),
                    &record_type.map(bytes),
                    &bytes(function.current_revision()),
                    &bytes(function_origin.source_unit()),
                    &i64::from(function_origin.byte_start()),
                    &i64::from(function_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        for parameter in function.parameters() {
            let TypeColumns {
                kind,
                scalar,
                target,
                value_type,
                standard_library_revision,
                enum_type,
                record_type,
            } = encoder.function_type_columns(function.domain(), parameter.resolved_type(), false)?;
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Parameter {
                    owner: function.id(),
                    parameter: parameter.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_function_parameters
                    (catalogue_revision_id, function_id, parameter_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, value_type_id,
                     value_standard_library_revision_id, enum_type_id, record_type_id,
                     default_expression_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
                    &[
                        &bytes(catalogue),
                        &bytes(function.id()),
                        &bytes(parameter.id()),
                        &parameter.name(),
                        &i64::from(parameter.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &value_type.map(bytes),
                        &standard_library_revision.map(bytes),
                        &enum_type.map(bytes),
                        &record_type.map(bytes),
                        &parameter.default_expression().map(bytes),
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
        if let FunctionReturn::Rows(columns) = function.return_type() {
            for column in columns {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.type_columns(column.resolved_type(), false)?;
                let origin = origin(
                    candidate.origins(),
                    DefinitionIdentity::FunctionReturnColumn {
                        owner: function.id(),
                        ordinal: column.ordinal(),
                    },
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.catalogue_function_return_columns
                        (catalogue_revision_id, function_id, name, ordinal, type_kind,
                         scalar_type, target_type_id, value_type_id,
                         value_standard_library_revision_id, enum_type_id, record_type_id,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
                        &[
                            &bytes(catalogue),
                            &bytes(function.id()),
                            &column.name(),
                            &i64::from(column.ordinal()),
                            &kind,
                            &scalar,
                            &target.map(bytes),
                            &value_type.map(bytes),
                            &standard_library_revision.map(bytes),
                            &enum_type.map(bytes),
                            &record_type.map(bytes),
                            &bytes(origin.source_unit()),
                            &i64::from(origin.byte_start()),
                            &i64::from(origin.byte_end()),
                        ],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
            }
        }
    }
    Ok(())
}

async fn persist_revisions_and_references(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for revision in candidate.new_function_revisions() {
        let version = positive_i64(revision.revision_number(), "function revision number")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, hash_algorithm, language_version,
                 status, hash_contract_version, semantic_hash_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, 'candidate', $8, $9)",
                &[
                    &bytes(revision.id()),
                    &bytes(catalogue),
                    &bytes(revision.function()),
                    &version,
                    &digest(revision.declaration_content_hash()),
                    &digest(revision.semantic_hash()),
                    &revision.language_version(),
                    &CONTRACT_VERSION,
                    &semantic_hash_version(revision.semantic_hash_version())?,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let artifact = revision.artifact();
        let version = positive_i32(artifact.version(), "function artifact format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7)",
                &[
                    &bytes(revision.id()),
                    &artifact_kind(artifact.kind()),
                    &artifact.format(),
                    &version,
                    &artifact.payload(),
                    &digest(artifact.content_hash()),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for reference in candidate.references() {
        let (
            target,
            kind,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ) = encoder.reference_columns(reference)?;
        let reference_kind = reference_kind(reference.kind())?;
        let source = reference.source_origin();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id, source_function_revision_id,
                 ordinal, target_definition_id, target_kind, target_owner_type_id,
                 target_owner_function_id, target_standard_library_revision_id,
                 target_enum_catalogue_revision_id, target_record_catalogue_revision_id,
                 target_record_field_catalogue_revision_id,
                 target_record_field_owner_type_id,
                 reference_kind, source_subobject_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NULL, $15, $16, $17)",
                &[
                    &bytes(catalogue),
                    &bytes(reference.source_function()),
                    &bytes(reference.source_revision()),
                    &i64::from(reference.ordinal()),
                    &target,
                    &kind,
                    &owner_type,
                    &owner_function,
                    &standard_library_revision,
                    &enum_catalogue_revision,
                    &record_catalogue_revision,
                    &record_field_catalogue_revision,
                    &record_field_owner_type,
                    &reference_kind,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

async fn transition_revision_statuses(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    locked: &ActiveDatabaseRevision,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let new_current = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in locked.function_revisions() {
        if !new_current.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'retired'
                     WHERE id = $1 AND status = 'active'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "active function revision retirement")?;
        }
    }
    let new_ids = candidate
        .new_function_revisions()
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in &materialized.current {
        if new_ids.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'candidate'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "candidate function revision activation")?;
        } else if locked
            .historical_function_revisions()
            .iter()
            .any(|old| old.id() == revision.id())
        {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'retired'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "historical function revision activation")?;
        }
    }
    Ok(())
}

async fn verify_revision_statuses(
    transaction: &Transaction<'_>,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let expected = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    let record = DurableRecord::new("_orna_kernel.function_revisions", "status sweep");
    let mut actual = BTreeSet::new();
    for row in rows {
        let id = FunctionRevisionId::from_bytes(identity_bytes(
            record.column(
                &row,
                "id",
                "function revision identity must be exactly 16 bytes",
            )?,
            &record,
            "function revision identity must be exactly 16 bytes",
        )?);
        let status: String = record.column(
            &row,
            "status",
            "function revision status must be active or retired after apply",
        )?;
        match status.as_str() {
            "active" => {
                actual.insert(id);
            }
            "retired" => {}
            _ => {
                return Err(record
                    .invariant("function revision status must be active or retired after apply"));
            }
        }
    }
    if actual != expected {
        return Err(record.invariant(
            "active function revision identities must exactly equal candidate current identities",
        ));
    }
    Ok(())
}

async fn update_active_pair(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.active_revision
             SET source_revision_id = $1,
                 catalogue_revision_id = $2,
                 updated_at = transaction_timestamp()
             WHERE singleton = true
               AND source_revision_id = $3
               AND catalogue_revision_id = $4",
            &[
                &bytes(candidate.source().id()),
                &bytes(candidate.candidate().revision()),
                &bytes(expected.source()),
                &bytes(expected.catalogue()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one(updated, "active revision pointer update")
}

fn require_one(value: u64, rule: &'static str) -> Result<(), PostgresKernelError> {
    if value == 1 {
        Ok(())
    } else {
        Err(invariant(rule))
    }
}
fn invariant(rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.apply",
        record: "candidate".into(),
        rule,
    }
}
fn bytes<I>(id: I) -> Vec<u8>
where
    I: IntoBytes,
{
    id.into_bytes().to_vec()
}
trait IntoBytes {
    fn into_bytes(self) -> [u8; 16];
}
macro_rules! id_bytes {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoBytes for $ty {
                fn into_bytes(self) -> [u8; 16] {
                    self.to_bytes()
                }
            }
        )*
    };
}
id_bytes!(
    CatalogueRevisionId,
    ExpressionId,
    FieldId,
    FunctionId,
    FunctionRevisionId,
    ParameterId,
    SchemaId,
    SourceRevisionId,
    StandardLibraryRevisionId,
    TypeBindingId,
    TypeId,
    orna_core::SourceBundleId,
    orna_core::SourceUnitId
);
fn digest(value: Sha256Digest) -> Vec<u8> {
    value.to_bytes().to_vec()
}
fn origin(
    origins: &[DefinitionOrigin],
    identity: DefinitionIdentity,
) -> Result<SourceOrigin, PostgresKernelError> {
    origins
        .iter()
        .find(|origin| origin.identity() == identity)
        .map(DefinitionOrigin::source)
        .ok_or_else(|| {
            invariant("every persisted semantic definition must have one candidate source origin")
        })
}
fn schema_for_name(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    name: &QualifiedSemanticName,
) -> Result<SchemaId, PostgresKernelError> {
    let namespace = name
        .parts()
        .get(..name.parts().len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| invariant("qualified definition name must contain its schema namespace"))?;
    catalogue
        .schemas()
        .iter()
        .find(|schema| schema.name().parts() == namespace)
        .map(|schema| schema.id())
        .ok_or_else(|| invariant("definition schema namespace must resolve exactly"))
}
fn scalar(scalar: StandardScalar, allow_void: bool) -> Result<&'static str, PostgresKernelError> {
    match scalar {
        StandardScalar::Boolean => Ok("boolean"),
        StandardScalar::Integer => Ok("integer"),
        StandardScalar::BigInt => Ok("bigint"),
        StandardScalar::Float => Ok("float"),
        StandardScalar::Decimal => Ok("decimal"),
        StandardScalar::CharacterLargeObject => Ok("character_large_object"),
        StandardScalar::BinaryLargeObject => Ok("binary_large_object"),
        StandardScalar::Uuid => Ok("uuid"),
        StandardScalar::Date => Ok("date"),
        StandardScalar::Time => Ok("time"),
        StandardScalar::Timestamp => Ok("timestamp"),
        StandardScalar::Duration => Ok("duration"),
        StandardScalar::Void if allow_void => Ok("void"),
        StandardScalar::Void => Err(invariant("VOID is valid only as a SINGLE function return")),
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypeColumns {
    kind: &'static str,
    scalar: Option<&'static str>,
    target: Option<TypeId>,
    value_type: Option<TypeId>,
    standard_library_revision: Option<StandardLibraryRevisionId>,
    enum_type: Option<TypeId>,
    record_type: Option<TypeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecordValueFieldColumns {
    kind: &'static str,
    value_type: Option<TypeId>,
    value_standard_library_revision: Option<StandardLibraryRevisionId>,
    application_enum_type: Option<TypeId>,
    enum_standard_library_revision: Option<StandardLibraryRevisionId>,
    standard_enum_type: Option<TypeId>,
    application_record_type: Option<TypeId>,
}

/// The one context-aware PostgreSQL projection for candidate type and reference
/// storage. It preserves the version-one tuple exactly and uses the selected
/// version-two standard pin only for durable value identities.
struct CandidateEncoder<'a> {
    context: &'a CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
}

impl<'a> CandidateEncoder<'a> {
    const fn new(context: &'a CatalogueHashContext, catalogue: &'a CatalogueSnapshot) -> Self {
        Self { context, catalogue }
    }

    fn catalogue_hash_version(&self) -> Result<i16, PostgresKernelError> {
        i16::try_from(self.context.version().to_u32())
            .map_err(|_| invariant("catalogue hash version must fit PostgreSQL smallint"))
    }

    fn standard_library_revision(&self) -> Option<StandardLibraryRevisionId> {
        self.context
            .standard()
            .map(VerifiedStandardLibrarySnapshot::revision)
    }

    fn record_value_field_columns(
        &self,
        candidate: &DeployableRevision,
        descriptor: &TypeDescriptor,
    ) -> Result<RecordValueFieldColumns, PostgresKernelError> {
        let class = candidate
            .record_value_field_descriptor_class(descriptor)
            .map_err(|_| {
                invariant(
                    "record value fields must use one supported standard value, enum, or record type",
                )
            })?;
        match class {
            RecordValueFieldDescriptorClass::ApplicationEnum(type_id) => {
                Ok(RecordValueFieldColumns {
                    kind: "enum",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: Some(type_id),
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: None,
                })
            }
            RecordValueFieldDescriptorClass::ApplicationRecord(type_id) => {
                Ok(RecordValueFieldColumns {
                    kind: "record",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: Some(type_id),
                })
            }
            RecordValueFieldDescriptorClass::StandardEnum(type_id) => {
                let standard_library_revision =
                    self.standard_library_revision().ok_or_else(|| {
                        invariant("record value field standard enum must retain its standard pin")
                    })?;
                Ok(RecordValueFieldColumns {
                    kind: "enum",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: Some(standard_library_revision),
                    standard_enum_type: Some(type_id),
                    application_record_type: None,
                })
            }
            RecordValueFieldDescriptorClass::StandardPrimitive(type_id) => {
                let standard_library_revision =
                    self.standard_library_revision().ok_or_else(|| {
                        invariant(
                            "record value field standard primitive must retain its standard pin",
                        )
                    })?;
                Ok(RecordValueFieldColumns {
                    kind: "value",
                    value_type: Some(type_id),
                    value_standard_library_revision: Some(standard_library_revision),
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: None,
                })
            }
            _ => Err(invariant(
                "record value field descriptor class must be supported by this kernel",
            )),
        }
    }

    fn type_columns(
        &self,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        if let Some(value) = value.legacy_scalar() {
            return Ok(TypeColumns {
                kind: "scalar",
                scalar: Some(scalar(value, allow_void)?),
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(value) = value.named_type() {
            if self.catalogue.enum_type_by_id(value).is_some() {
                return Ok(TypeColumns {
                    kind: "enum",
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: Some(value),
                    record_type: None,
                });
            }
            if self.catalogue.record_value_type_by_id(value).is_some() {
                return Ok(TypeColumns {
                    kind: "record",
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: Some(value),
                });
            }
            return Ok(TypeColumns {
                kind: "named",
                scalar: None,
                target: Some(value),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(target) = value.reference_target() {
            return Ok(TypeColumns {
                kind: "reference",
                scalar: None,
                target: Some(target),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(value_type) = value.value_type() {
            let standard_library_revision = if is_sealed_inspect_type_id(value_type) {
                None
            } else {
                Some(self.standard_library_revision().ok_or_else(|| {
                    invariant("resolved value types require version-two PostgreSQL encoding")
                })?)
            };
            return Ok(TypeColumns {
                kind: "value",
                scalar: None,
                target: None,
                value_type: Some(value_type),
                standard_library_revision,
                enum_type: None,
                record_type: None,
            });
        }
        Err(invariant(
            "resolved type must expose one supported PostgreSQL type shape",
        ))
    }

    fn client_type_columns(
        &self,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        let mut columns = self.type_columns(value, allow_void)?;
        if columns
            .value_type
            .is_some_and(is_sealed_inspect_type_id)
        {
            columns.standard_library_revision = None;
        }
        Ok(columns)
    }

    fn function_type_columns(
        &self,
        domain: FunctionDomain,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        if domain == FunctionDomain::Client {
            self.client_type_columns(value, allow_void)
        } else {
            self.type_columns(value, allow_void)
        }
    }

    fn reference_target(
        &self,
        value: DefinitionReferenceTarget,
    ) -> Result<ReferenceTargetColumns, PostgresKernelError> {
        if let DefinitionReferenceTarget::ObjectType(id) = value {
            return Ok((
                "object_type",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::ValueType(id) = value {
            if self.catalogue.enum_type_by_id(id).is_some() {
                return Ok((
                    "enum_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    None,
                    None,
                    None,
                ));
            }
            if self.catalogue.record_value_type_by_id(id).is_some() {
                return Ok((
                    "record_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    None,
                    None,
                ));
            }
            if is_sealed_inspect_type_id(id) {
                return Ok((
                    "value_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ));
            }
            let standard_library_revision = self.standard_library_revision().ok_or_else(|| {
                invariant("value type references require version-two PostgreSQL encoding")
            })?;
            return Ok((
                "value_type",
                bytes(id),
                None,
                None,
                Some(bytes(standard_library_revision)),
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Field { owner, field } = value {
            let record_field = self
                .catalogue
                .record_value_type_by_id(owner)
                .is_some_and(|record| record.field_by_id(field).is_some());
            if record_field {
                return Ok((
                    "record_field",
                    bytes(field),
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    Some(bytes(owner)),
                ));
            }
            if self
                .catalogue
                .object_type_by_id(owner)
                .is_none_or(|object| object.field_by_id(field).is_none())
            {
                return Err(invariant(
                    "definition reference field target is absent from the candidate catalogue",
                ));
            }
            return Ok((
                "field",
                bytes(field),
                Some(bytes(owner)),
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Function(id) = value {
            return Ok((
                "function",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Parameter { owner, parameter } = value {
            return Ok((
                "parameter",
                bytes(parameter),
                None,
                Some(bytes(owner)),
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Expression(id) = value {
            return Ok((
                "expression",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        Err(invariant(
            "definition reference target is not supported by PostgreSQL persistence",
        ))
    }

    fn reference_columns(
        &self,
        reference: &DefinitionReference,
    ) -> Result<ReferenceInsertColumns, PostgresKernelError> {
        let (
            kind,
            target,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ) = self.reference_target(reference.target())?;
        Ok((
            target,
            kind,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyTypeColumns {
    Scalar(&'static str),
    Named(TypeId),
    Reference(TypeId),
}

#[cfg(test)]
impl LegacyTypeColumns {
    const fn tuple(self) -> (&'static str, Option<&'static str>, Option<TypeId>) {
        match self {
            Self::Scalar(value) => ("scalar", Some(value), None),
            Self::Named(value) => ("named", None, Some(value)),
            Self::Reference(target) => ("reference", None, Some(target)),
        }
    }
}

#[cfg(test)]
fn legacy_type_projection(
    value: ResolvedType,
    allow_void: bool,
) -> Result<LegacyTypeColumns, PostgresKernelError> {
    if let Some(value) = value.legacy_scalar() {
        return Ok(LegacyTypeColumns::Scalar(scalar(value, allow_void)?));
    }
    if let Some(value) = value.named_type() {
        return Ok(LegacyTypeColumns::Named(value));
    }
    if let Some(target) = value.reference_target() {
        return Ok(LegacyTypeColumns::Reference(target));
    }
    if value.value_type().is_some() {
        return Err(invariant(
            "resolved value types are not supported by legacy PostgreSQL type encoding",
        ));
    }
    Err(invariant(
        "resolved type must expose one supported PostgreSQL type shape",
    ))
}

#[cfg(test)]
fn type_columns(
    value: ResolvedType,
    allow_void: bool,
) -> Result<(&'static str, Option<&'static str>, Option<TypeId>), PostgresKernelError> {
    Ok(legacy_type_projection(value, allow_void)?.tuple())
}
fn on_delete(value: Option<OnDeleteAction>) -> Option<&'static str> {
    value.map(|value| match value {
        OnDeleteAction::Restrict => "restrict",
        OnDeleteAction::SetNull => "set_null",
        OnDeleteAction::Cascade => "cascade",
    })
}
fn function_domain(value: FunctionDomain) -> &'static str {
    match value {
        FunctionDomain::Server => "server",
        FunctionDomain::Client => "client",
    }
}
fn function_security(value: FunctionSecurity) -> &'static str {
    match value {
        FunctionSecurity::Invoker => "invoker",
        FunctionSecurity::Definer => "definer",
    }
}
fn function_transaction(
    value: Option<FunctionTransaction>,
) -> Result<Option<&'static str>, PostgresKernelError> {
    match value {
        None => Ok(None),
        Some(FunctionTransaction::Atomic) => Ok(Some("atomic")),
        Some(FunctionTransaction::ReadOnly) => Ok(Some("read_only")),
        Some(FunctionTransaction::Manual) => Err(invariant(
            "manual function transactions are not supported by PostgreSQL",
        )),
    }
}
fn function_volatility(value: FunctionVolatility) -> &'static str {
    match value {
        FunctionVolatility::Immutable => "immutable",
        FunctionVolatility::Stable => "stable",
        FunctionVolatility::Volatile => "volatile",
    }
}
fn artifact_kind(value: orna_core::revision::ExecutableArtifactKind) -> &'static str {
    match value {
        orna_core::revision::ExecutableArtifactKind::Server => "server_plan",
        orna_core::revision::ExecutableArtifactKind::Client => "client_bytecode",
    }
}
fn reference_kind(value: DefinitionReferenceKind) -> Result<&'static str, PostgresKernelError> {
    POSTGRES_REFERENCE_KINDS
        .iter()
        .find(|(kind, _)| *kind == value)
        .map(|(_, name)| *name)
        .ok_or_else(|| {
            invariant("definition reference kind is not supported by PostgreSQL persistence")
        })
}
const POSTGRES_REFERENCE_KINDS: &[(DefinitionReferenceKind, &str)] = &[
    (DefinitionReferenceKind::FunctionCall, "function_call"),
    (DefinitionReferenceKind::NamedType, "named_type"),
    (DefinitionReferenceKind::ObjectReference, "object_reference"),
    (DefinitionReferenceKind::ParameterRead, "parameter_read"),
    (DefinitionReferenceKind::QueryObject, "query_object"),
    (DefinitionReferenceKind::QueryField, "query_field"),
    (DefinitionReferenceKind::Expression, "expression"),
    (DefinitionReferenceKind::WriteObject, "write_object"),
    (DefinitionReferenceKind::WriteField, "write_field"),
];
type ReferenceTargetColumns = (
    &'static str,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

#[cfg(test)]
type LegacyReferenceTargetColumns = (&'static str, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

#[cfg(test)]
fn reference_target(
    value: DefinitionReferenceTarget,
) -> Result<LegacyReferenceTargetColumns, PostgresKernelError> {
    Ok(match value {
        DefinitionReferenceTarget::ObjectType(id) => ("object_type", bytes(id), None, None),
        DefinitionReferenceTarget::Field { owner, field } => {
            ("field", bytes(field), Some(bytes(owner)), None)
        }
        DefinitionReferenceTarget::Function(id) => ("function", bytes(id), None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => {
            ("parameter", bytes(parameter), None, Some(bytes(owner)))
        }
        other => {
            let DefinitionReferenceTarget::Expression(id) = other else {
                return Err(invariant(
                    "definition reference target is not supported by PostgreSQL persistence",
                ));
            };
            ("expression", bytes(id), None, None)
        }
    })
}
type ReferenceInsertColumns = (
    Vec<u8>,
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
        SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
        TypeId,
        canonical_hash::{
            catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest, verify_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition,
            FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction,
            FunctionVolatility, ObjectTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition, ValueTypeKind,
        },
        physical::{PhysicalPlanError, plan_physical_changes},
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReferenceKind,
            DefinitionReferenceTarget, DeployableRevision, DeployableRevisionContent,
            DeployableRevisionInput, ExecutableArtifact, ExecutableArtifactKind,
            FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
            StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
            StoredSourceUnit,
        },
        types::{ResolvedType, StandardScalar, TypeDescriptor},
    };

    use super::{
        CandidateEncoder, LegacyTypeColumns, POSTGRES_REFERENCE_KINDS, StandardContextIdentity,
        StandardExecutableIdentity, StandardExecutableParameter, TypeColumns, artifact_kind,
        first_active_reserved_identity, first_active_standard_executable_identity,
        first_active_standard_parameter, first_inactive_reserved_identity,
        first_inactive_standard_executable_identity, function_transaction,
        guard_standard_context_transition, legacy_type_projection, materialize, positive_i32,
        positive_i64, reference_kind, reference_target, scalar, standard_reference_target_columns,
        standard_resolved_type_columns, standard_value_kind, type_columns,
        validate_candidate_preflight, validate_expected_base, validate_standard_executable_facts,
    };
    use crate::PostgresKernelError;

    #[derive(Clone, Copy)]
    struct StandardContextFixture {
        source_unit: [u8; 16],
        source_bundle: [u8; 16],
        source_revision: [u8; 16],
        standard_revision: [u8; 16],
        catalogue_revision: [u8; 16],
        logical_path: &'static str,
        content: &'static str,
        source_bundle_hash: [u8; 32],
        source_revision_hash: [u8; 32],
        standard_digest: [u8; 32],
    }

    const BASE_STANDARD_CONTEXT: StandardContextFixture = StandardContextFixture {
        source_unit: [4; 16],
        source_bundle: [5; 16],
        source_revision: [6; 16],
        standard_revision: [7; 16],
        catalogue_revision: [8; 16],
        logical_path: "std/malformed.orna",
        content: "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;",
        source_bundle_hash: [
            0x7e, 0x67, 0xc9, 0x9b, 0x30, 0x05, 0xb6, 0x4f, 0x0e, 0x4f, 0x6a, 0xb9, 0xe4, 0xde,
            0x40, 0x3b, 0xe3, 0xb9, 0xdb, 0xb9, 0x57, 0x59, 0xe6, 0x57, 0x6d, 0x8e, 0x3e, 0x7f,
            0xfb, 0xa4, 0x80, 0xd8,
        ],
        source_revision_hash: [
            0x80, 0x16, 0x8f, 0xbd, 0xf3, 0xba, 0xa8, 0x30, 0x37, 0xd7, 0x17, 0xfc, 0xa8, 0xfd,
            0xc3, 0x02, 0x34, 0x11, 0x18, 0x79, 0xe1, 0x33, 0x0a, 0x27, 0x98, 0x0f, 0x4a, 0xa7,
            0x65, 0x6c, 0x61, 0xea,
        ],
        standard_digest: [
            0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d,
            0x6c, 0xaf, 0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d,
            0x14, 0xcc, 0x68, 0xae,
        ],
    };

    const ALTERNATE_STANDARD_CONTEXT: StandardContextFixture = StandardContextFixture {
        source_unit: [14; 16],
        source_bundle: [15; 16],
        source_revision: [16; 16],
        standard_revision: [17; 16],
        catalogue_revision: [18; 16],
        logical_path: "std/alternate.orna",
        content: "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;\n",
        source_bundle_hash: [
            0x9c, 0xb1, 0x72, 0x54, 0x07, 0x7f, 0xdb, 0xae, 0x68, 0x2d, 0x7b, 0xd8, 0x52, 0x91,
            0x3f, 0x91, 0xe6, 0x07, 0x44, 0x16, 0x1f, 0xc9, 0xee, 0x32, 0x20, 0xc9, 0xef, 0xc9,
            0x9b, 0x5d, 0x19, 0x2d,
        ],
        source_revision_hash: [
            0x67, 0x6a, 0xdc, 0x25, 0xfc, 0xc1, 0xd6, 0x7a, 0x53, 0xfe, 0x5d, 0x84, 0x2e, 0xdc,
            0x0f, 0xe3, 0x04, 0x61, 0x33, 0x7d, 0x95, 0x5a, 0x4f, 0x04, 0x78, 0x84, 0xd7, 0xed,
            0xd1, 0x71, 0x19, 0xab,
        ],
        standard_digest: [
            0xa3, 0xce, 0x6d, 0x48, 0x15, 0x61, 0x63, 0x33, 0x7b, 0xad, 0xe0, 0xae, 0xb9, 0x18,
            0x6e, 0x05, 0x00, 0x66, 0x20, 0x31, 0xe9, 0x0c, 0xae, 0x60, 0x14, 0x87, 0x1a, 0x7c,
            0x3f, 0xd3, 0xe1, 0x5a,
        ],
    };

    fn verified_standard_context(
        fixture: StandardContextFixture,
    ) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes(fixture.source_unit),
            0,
            fixture.logical_path,
            fixture.content,
            source_unit_content_digest(fixture.content).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes(fixture.source_bundle);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes(fixture.source_revision),
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(fixture.standard_revision),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            CatalogueSnapshot::new_with_types(
                CatalogueRevisionId::from_bytes(fixture.catalogue_revision),
                vec![],
                vec![],
                vec![],
                vec![],
            )
            .unwrap(),
            vec![],
            Sha256Digest::from_bytes(fixture.standard_digest),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn assert_standard_context_identity(
        identity: StandardContextIdentity,
        fixture: StandardContextFixture,
    ) {
        assert_eq!(
            identity.standard_library_revision(),
            StandardLibraryRevisionId::from_bytes(fixture.standard_revision)
        );
        assert_eq!(
            identity.standard_catalogue_revision(),
            CatalogueRevisionId::from_bytes(fixture.catalogue_revision)
        );
        assert_eq!(
            identity.source_bundle(),
            SourceBundleId::from_bytes(fixture.source_bundle)
        );
        assert_eq!(
            identity.source_revision(),
            SourceRevisionId::from_bytes(fixture.source_revision)
        );
        assert_eq!(
            identity.source_bundle_hash(),
            Sha256Digest::from_bytes(fixture.source_bundle_hash)
        );
        assert_eq!(
            identity.source_revision_hash(),
            Sha256Digest::from_bytes(fixture.source_revision_hash)
        );
        assert_eq!(
            identity.standard_library_digest(),
            Sha256Digest::from_bytes(fixture.standard_digest)
        );
    }

    fn preflight_object_type() -> TypeId {
        TypeId::from_bytes([0x44; 16])
    }

    fn preflight_field() -> FieldId {
        FieldId::from_bytes([0x45; 16])
    }

    fn preflight_value_type() -> TypeId {
        orna_standard::BOOLEAN_TYPE_ID
    }

    fn preflight_active(
        standard: orna_core::revision::VerifiedStandardLibrarySnapshot,
    ) -> ActiveDatabaseRevision {
        let bundle = SourceBundleId::from_bytes([0x40; 16]);
        let bundle_hash = source_bundle_digest(&[]).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0x41; 16]),
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x42; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn preflight_active_version_one() -> ActiveDatabaseRevision {
        let bundle = SourceBundleId::from_bytes([0x50; 16]);
        let bundle_hash = source_bundle_digest(&[]).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0x51; 16]),
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x52; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = CatalogueHashContext::version_one();
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn preflight_candidate(
        expected_base: RevisionPair,
        context: CatalogueHashContext,
        resolved_type: ResolvedType,
    ) -> DeployableRevision {
        let source_unit = SourceUnitId::from_bytes([0x46; 16]);
        let unit = StoredSourceUnit::new(
            source_unit,
            0,
            "preflight.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([0x47; 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0x48; 16]),
            Some(expected_base.source()),
            vec![unit],
            bundle_hash,
            source_revision_record_digest(bundle, Some(expected_base.source()), bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes([0x49; 16]),
            QualifiedSemanticName::new(["preflight"]).unwrap(),
        );
        let object_type = ObjectTypeDefinition::new(
            preflight_object_type(),
            QualifiedSemanticName::new(["preflight", "flags"]).unwrap(),
            vec![FieldDefinition::new(
                preflight_field(),
                "enabled",
                0,
                resolved_type,
                false,
                true,
                None,
                None,
            )],
        );
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x4a; 16]),
            vec![schema.clone()],
            vec![object_type.clone()],
        )
        .unwrap();
        let source_origin = SourceOrigin::new(source_unit, 0, 0).unwrap();
        let origins = vec![
            DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), source_origin),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object_type.id()),
                source_origin,
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: object_type.id(),
                    field: preflight_field(),
                },
                source_origin,
            ),
        ];
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source,
                expected_base.catalogue(),
                catalogue,
                catalogue_hash,
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    #[test]
    fn candidate_preflight_accepts_a_version_two_value_before_physical_planning() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let active = preflight_active(standard.clone());
        let candidate = preflight_candidate(
            active.pair(),
            CatalogueHashContext::version_two(standard),
            ResolvedType::value(preflight_value_type()),
        );

        assert!(validate_candidate_preflight(&active, &candidate).is_ok());
        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: preflight_object_type(),
                field: preflight_field(),
            })
        );
    }

    #[test]
    fn candidate_encoder_projects_version_two_value_type_identity_and_pin() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard.clone());
        let catalogue =
            CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();

        assert_eq!(
            CandidateEncoder::new(&context, &catalogue)
                .type_columns(ResolvedType::value(preflight_value_type()), false)
                .unwrap(),
            TypeColumns {
                kind: "value",
                scalar: None,
                target: None,
                value_type: Some(preflight_value_type()),
                standard_library_revision: Some(standard.revision()),
                enum_type: None,
                record_type: None,
            }
        );
    }

    #[test]
    fn candidate_encoder_keeps_version_one_tuples_and_value_references_explicit() {
        let version_one = CatalogueHashContext::version_one();
        let catalogue =
            CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
        let version_one_encoder = CandidateEncoder::new(&version_one, &catalogue);
        assert_eq!(
            version_one_encoder
                .type_columns(ResolvedType::scalar(StandardScalar::Boolean), false)
                .unwrap(),
            TypeColumns {
                kind: "scalar",
                scalar: Some("boolean"),
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            }
        );
        assert_eq!(
            version_one_encoder
                .reference_target(DefinitionReferenceTarget::ObjectType(
                    preflight_object_type(),
                ))
                .unwrap(),
            (
                "object_type",
                preflight_object_type().to_bytes().to_vec(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
        );

        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let version_two = CatalogueHashContext::version_two(standard.clone());
        let version_two_encoder = CandidateEncoder::new(&version_two, &catalogue);
        assert_eq!(
            version_two_encoder
                .reference_target(DefinitionReferenceTarget::ValueType(preflight_value_type()))
                .unwrap(),
            (
                "value_type",
                preflight_value_type().to_bytes().to_vec(),
                None,
                None,
                Some(standard.revision().to_bytes().to_vec()),
                None,
                None,
                None,
                None,
            )
        );
    }

    #[test]
    fn candidate_encoder_separates_application_named_types() {
        let enum_type = TypeId::from_bytes([0x61; 16]);
        let record_type = TypeId::from_bytes([0x64; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x62; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x63; 16]),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["app", "stage"]).unwrap(),
                ["lead", "customer"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["app", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes([0x65; 16]),
                        "stage",
                        0,
                        TypeDescriptor::named(enum_type),
                    )
                    .unwrap(),
                ],
            )],
            Vec::new(),
        )
        .unwrap();
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let encoder = CandidateEncoder::new(&context, &catalogue);

        assert_eq!(
            encoder
                .type_columns(ResolvedType::named(enum_type), false)
                .unwrap(),
            TypeColumns {
                kind: "enum",
                scalar: None,
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: Some(enum_type),
                record_type: None,
            }
        );
        assert_eq!(
            encoder
                .reference_target(DefinitionReferenceTarget::ValueType(enum_type))
                .unwrap(),
            (
                "enum_type",
                enum_type.to_bytes().to_vec(),
                None,
                None,
                None,
                Some(catalogue.revision().to_bytes().to_vec()),
                None,
                None,
                None,
            )
        );
        assert_eq!(
            encoder
                .type_columns(ResolvedType::named(record_type), false)
                .unwrap(),
            TypeColumns {
                kind: "record",
                scalar: None,
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: Some(record_type),
            }
        );
        assert_eq!(
            encoder
                .reference_target(DefinitionReferenceTarget::ValueType(record_type))
                .unwrap(),
            (
                "record_type",
                record_type.to_bytes().to_vec(),
                None,
                None,
                None,
                None,
                Some(catalogue.revision().to_bytes().to_vec()),
                None,
                None,
            )
        );
        let record_field = FieldId::from_bytes([0x65; 16]);
        assert_eq!(
            encoder
                .reference_target(DefinitionReferenceTarget::Field {
                    owner: record_type,
                    field: record_field,
                })
                .unwrap(),
            (
                "record_field",
                record_field.to_bytes().to_vec(),
                None,
                None,
                None,
                None,
                None,
                Some(catalogue.revision().to_bytes().to_vec()),
                Some(record_type.to_bytes().to_vec()),
            )
        );
    }

    #[test]
    fn materialization_retains_candidate_context_for_context_aware_hashing() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let active = preflight_active(standard.clone());
        let candidate = preflight_candidate(
            active.pair(),
            CatalogueHashContext::version_two(standard),
            ResolvedType::value(preflight_value_type()),
        );

        let materialized = materialize(&candidate, &active).unwrap();
        assert_eq!(
            materialized.catalogue_hash_context.version(),
            candidate.catalogue_hash_context().version()
        );
        assert!(super::verify_candidate_hashes(&candidate, &materialized).is_ok());
    }

    #[test]
    fn standard_upgrade_base_gate_has_no_normal_context_transition_check() {
        let active = preflight_active_version_one();
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let candidate = preflight_candidate(
            active.pair(),
            CatalogueHashContext::version_two(standard),
            ResolvedType::value(preflight_value_type()),
        );

        assert!(validate_expected_base(&active, &candidate).is_ok());
        let stale = preflight_candidate(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x71; 16]),
                CatalogueRevisionId::from_bytes([0x72; 16]),
            ),
            candidate.catalogue_hash_context().clone(),
            ResolvedType::value(preflight_value_type()),
        );
        assert!(matches!(
            validate_expected_base(&active, &stale),
            Err(PostgresKernelError::ExpectedBaseMismatch { .. })
        ));
    }

    #[test]
    fn reserved_identity_selector_keeps_active_before_inactive_raw_order() {
        let standard = orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(
            StandardLibraryRevisionId::from_bytes([0x80; 16]),
        );
        let source = orna_standard::StandardUpgradeIdentity::SourceUnit(SourceUnitId::from_bytes(
            [0x81; 16],
        ));
        let inactive_earlier = orna_standard::StandardUpgradeIdentity::SourceUnit(
            SourceUnitId::from_bytes([0x82; 16]),
        );
        let upgrade = vec![
            (
                source,
                SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
            ),
            (
                inactive_earlier,
                SourceUnitId::from_bytes([0x82; 16]).to_bytes().to_vec(),
            ),
        ];
        let active = vec![(
            standard,
            StandardLibraryRevisionId::from_bytes([0x80; 16])
                .to_bytes()
                .to_vec(),
        )];

        assert_eq!(first_active_reserved_identity(&active, &upgrade), None);
        let standard_upgrade = vec![(
            standard,
            StandardLibraryRevisionId::from_bytes([0x80; 16])
                .to_bytes()
                .to_vec(),
        )];
        assert_eq!(
            first_inactive_reserved_identity(
                &standard_upgrade,
                &[StandardLibraryRevisionId::from_bytes([0x80; 16])
                    .to_bytes()
                    .to_vec()],
            ),
            Some(standard)
        );
        assert_eq!(
            first_inactive_reserved_identity(
                &upgrade,
                &[
                    SourceUnitId::from_bytes([0x82; 16]).to_bytes().to_vec(),
                    SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
                ],
            ),
            Some(inactive_earlier)
        );
        let active_source = vec![(
            source,
            SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
        )];
        assert_eq!(
            first_active_reserved_identity(&active_source, &upgrade),
            Some(source)
        );
    }

    #[test]
    fn candidate_preflight_preserves_expected_base_and_standard_context_precedence() {
        let active_standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let active = preflight_active(active_standard.clone());
        let matching_context = CatalogueHashContext::version_two(active_standard.clone());

        let stale_expected = RevisionPair::new(
            SourceRevisionId::from_bytes([0x50; 16]),
            CatalogueRevisionId::from_bytes([0x51; 16]),
        );
        let stale = preflight_candidate(
            stale_expected,
            matching_context.clone(),
            ResolvedType::value(preflight_value_type()),
        );
        assert!(matches!(
            validate_candidate_preflight(&active, &stale),
            Err(PostgresKernelError::ExpectedBaseMismatch {
                expected,
                active: actual_active,
            }) if expected == stale_expected && actual_active == active.pair()
        ));

        let version_one = preflight_candidate(
            active.pair(),
            CatalogueHashContext::version_one(),
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        assert!(matches!(
            validate_candidate_preflight(&active, &version_one),
            Err(PostgresKernelError::StandardContextTransitionRequired {
                active: orna_core::revision::CatalogueHashVersion::Version2,
                candidate: orna_core::revision::CatalogueHashVersion::Version1,
            })
        ));

        let alternate_standard = verified_standard_context(ALTERNATE_STANDARD_CONTEXT);
        let different_context = preflight_candidate(
            active.pair(),
            CatalogueHashContext::version_two(alternate_standard.clone()),
            ResolvedType::named(preflight_object_type()),
        );
        let mismatch = validate_candidate_preflight(&active, &different_context).unwrap_err();
        let (actual_active, actual_candidate) = match mismatch {
            PostgresKernelError::StandardContextMismatch { active, candidate } => {
                (active, candidate)
            }
            other => {
                assert!(
                    matches!(other, PostgresKernelError::StandardContextMismatch { .. }),
                    "different verified version-two contexts must mismatch before persistence"
                );
                return;
            }
        };
        assert_eq!(
            *actual_active,
            StandardContextIdentity::from_verified_snapshot(&active_standard)
        );
        assert_eq!(
            *actual_candidate,
            StandardContextIdentity::from_verified_snapshot(&alternate_standard)
        );
    }

    #[test]
    fn standard_context_guard_uses_core_verified_version_two_facts() {
        let active_standard = verified_standard_context(BASE_STANDARD_CONTEXT);
        let candidate_standard = verified_standard_context(ALTERNATE_STANDARD_CONTEXT);
        let active = StandardContextIdentity::from_verified_snapshot(&active_standard);
        let candidate = StandardContextIdentity::from_verified_snapshot(&candidate_standard);

        assert_standard_context_identity(active, BASE_STANDARD_CONTEXT);
        assert_standard_context_identity(candidate, ALTERNATE_STANDARD_CONTEXT);
        assert!(
            guard_standard_context_transition(
                &CatalogueHashContext::version_one(),
                &CatalogueHashContext::version_one(),
            )
            .is_ok()
        );

        let active_context = CatalogueHashContext::version_two(active_standard.clone());
        assert!(guard_standard_context_transition(&active_context, &active_context).is_ok());

        let transition = guard_standard_context_transition(
            &active_context,
            &CatalogueHashContext::version_one(),
        )
        .unwrap_err();
        assert!(matches!(
            transition,
            PostgresKernelError::StandardContextTransitionRequired {
                active: orna_core::revision::CatalogueHashVersion::Version2,
                candidate: orna_core::revision::CatalogueHashVersion::Version1,
            }
        ));

        let mismatch = guard_standard_context_transition(
            &active_context,
            &CatalogueHashContext::version_two(candidate_standard),
        )
        .unwrap_err();
        let (actual_active, actual_candidate) = match mismatch {
            PostgresKernelError::StandardContextMismatch { active, candidate } => {
                (active, candidate)
            }
            other => {
                assert!(
                    matches!(other, PostgresKernelError::StandardContextMismatch { .. }),
                    "version-two contexts must report a standard context mismatch"
                );
                return;
            }
        };
        assert_eq!(*actual_active, active);
        assert_eq!(*actual_candidate, candidate);
        let error = PostgresKernelError::StandardContextMismatch {
            active: actual_active,
            candidate: actual_candidate,
        };
        assert_eq!(
            error.to_string(),
            "the active and candidate standard contexts do not match"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn scalar_encoder_uses_the_complete_stable_postgres_vocabulary() {
        let expected = [
            (StandardScalar::Boolean, "boolean"),
            (StandardScalar::Integer, "integer"),
            (StandardScalar::BigInt, "bigint"),
            (StandardScalar::Float, "float"),
            (StandardScalar::Decimal, "decimal"),
            (
                StandardScalar::CharacterLargeObject,
                "character_large_object",
            ),
            (StandardScalar::BinaryLargeObject, "binary_large_object"),
            (StandardScalar::Uuid, "uuid"),
            (StandardScalar::Date, "date"),
            (StandardScalar::Time, "time"),
            (StandardScalar::Timestamp, "timestamp"),
            (StandardScalar::Duration, "duration"),
        ];
        for (value, spelling) in expected {
            assert_eq!(scalar(value, false).expect("storable scalar"), spelling);
        }
        assert!(scalar(StandardScalar::Void, false).is_err());
        assert_eq!(
            scalar(StandardScalar::Void, true).expect("single VOID"),
            "void"
        );
    }

    #[test]
    fn type_encoder_preserves_closed_type_tuple_shapes() {
        let target = TypeId::from_bytes([3; 16]);
        assert_eq!(
            legacy_type_projection(ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
            LegacyTypeColumns::Scalar("integer")
        );
        assert_eq!(
            legacy_type_projection(ResolvedType::named(target), false).unwrap(),
            LegacyTypeColumns::Named(target)
        );
        assert_eq!(
            legacy_type_projection(ResolvedType::reference(target), false).unwrap(),
            LegacyTypeColumns::Reference(target)
        );
        assert_eq!(
            type_columns(ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
            ("scalar", Some("integer"), None)
        );
        assert_eq!(
            type_columns(ResolvedType::named(target), false).unwrap(),
            ("named", None, Some(target))
        );
        assert_eq!(
            type_columns(ResolvedType::reference(target), false).unwrap(),
            ("reference", None, Some(target))
        );
        assert!(type_columns(ResolvedType::scalar(StandardScalar::Void), false).is_err());
    }

    #[test]
    fn standard_value_kind_encoder_preserves_opaque_definitions() {
        assert_eq!(
            standard_value_kind(ValueTypeKind::Primitive).unwrap(),
            "primitive"
        );
        assert_eq!(
            standard_value_kind(ValueTypeKind::Opaque).unwrap(),
            "opaque"
        );
    }

    #[test]
    fn transaction_and_artifact_encoders_are_closed() {
        assert_eq!(function_transaction(None).unwrap(), None);
        assert_eq!(
            function_transaction(Some(FunctionTransaction::Atomic)).unwrap(),
            Some("atomic")
        );
        assert_eq!(
            function_transaction(Some(FunctionTransaction::ReadOnly)).unwrap(),
            Some("read_only")
        );
        assert!(function_transaction(Some(FunctionTransaction::Manual)).is_err());
        assert_eq!(artifact_kind(ExecutableArtifactKind::Server), "server_plan");
        assert_eq!(
            artifact_kind(ExecutableArtifactKind::Client),
            "client_bytecode"
        );
    }

    #[test]
    fn reference_encoder_keeps_owner_qualified_targets() {
        let object = TypeId::from_bytes([1; 16]);
        let field = FieldId::from_bytes([2; 16]);
        let function = FunctionId::from_bytes([3; 16]);
        let parameter = ParameterId::from_bytes([4; 16]);
        let expression = ExpressionId::from_bytes([5; 16]);
        assert_eq!(
            reference_target(DefinitionReferenceTarget::ObjectType(object))
                .unwrap()
                .0,
            "object_type"
        );
        let field_target = reference_target(DefinitionReferenceTarget::Field {
            owner: object,
            field,
        })
        .unwrap();
        assert_eq!(field_target.0, "field");
        assert_eq!(field_target.2, Some(object.to_bytes().to_vec()));
        assert_eq!(
            reference_target(DefinitionReferenceTarget::Function(function))
                .unwrap()
                .0,
            "function"
        );
        let parameter_target = reference_target(DefinitionReferenceTarget::Parameter {
            owner: function,
            parameter,
        })
        .unwrap();
        assert_eq!(parameter_target.0, "parameter");
        assert_eq!(parameter_target.3, Some(function.to_bytes().to_vec()));
        assert_eq!(
            reference_target(DefinitionReferenceTarget::Expression(expression))
                .unwrap()
                .0,
            "expression"
        );
        let expected_kinds = [
            (DefinitionReferenceKind::FunctionCall, "function_call"),
            (DefinitionReferenceKind::NamedType, "named_type"),
            (DefinitionReferenceKind::ObjectReference, "object_reference"),
            (DefinitionReferenceKind::ParameterRead, "parameter_read"),
            (DefinitionReferenceKind::QueryObject, "query_object"),
            (DefinitionReferenceKind::QueryField, "query_field"),
            (DefinitionReferenceKind::Expression, "expression"),
            (DefinitionReferenceKind::WriteObject, "write_object"),
            (DefinitionReferenceKind::WriteField, "write_field"),
        ];
        assert_eq!(POSTGRES_REFERENCE_KINDS, expected_kinds.as_slice());
        assert_eq!(
            reference_kind(DefinitionReferenceKind::FunctionCall).unwrap(),
            "function_call"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::NamedType).unwrap(),
            "named_type"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::ObjectReference).unwrap(),
            "object_reference"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::ParameterRead).unwrap(),
            "parameter_read"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::QueryObject).unwrap(),
            "query_object"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::QueryField).unwrap(),
            "query_field"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::Expression).unwrap(),
            "expression"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::WriteObject).unwrap(),
            "write_object"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::WriteField).unwrap(),
            "write_field"
        );
    }

    #[test]
    fn postgres_positive_integer_bounds_fail_closed() {
        assert_eq!(positive_i32(1, "test").unwrap(), 1);
        assert_eq!(positive_i32(i32::MAX as u32, "test").unwrap(), i32::MAX);
        assert!(positive_i32(0, "test").is_err());
        assert!(positive_i32(i32::MAX as u32 + 1, "test").is_err());
        assert_eq!(positive_i64(1, "test").unwrap(), 1);
        assert_eq!(positive_i64(i64::MAX as u64, "test").unwrap(), i64::MAX);
        assert!(positive_i64(0, "test").is_err());
        assert!(positive_i64(i64::MAX as u64 + 1, "test").is_err());
    }

    #[test]
    fn standard_executable_contract_fails_closed_on_sequences_and_agreement() {
        let empty = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x30; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version1,
                &empty,
                &[],
            )
            .is_ok()
        );

        let executable = standard_executable_fixture();
        let function = executable_function_fixture(executable.revision().id());
        let second_function_id = FunctionId::from_bytes([0x20; 16]);
        let second_executable = standard_executable_fixture_with_function(
            second_function_id,
            FunctionRevisionId::from_bytes([0x20; 16]),
        );
        let second_function = executable_function_fixture_with_id(
            second_function_id,
            second_executable.revision().id(),
            QualifiedSemanticName::new(["std", "invoke", "other"]).unwrap(),
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x30; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
            )],
            Vec::new(),
            vec![function.clone(), second_function],
        )
        .unwrap();

        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version1,
                &catalogue,
                std::slice::from_ref(&executable),
            )
            .is_err()
        );
        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version2,
                &catalogue,
                &[],
            )
            .is_err()
        );
        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version2,
                &catalogue,
                &[executable.clone(), executable.clone()],
            )
            .is_err()
        );
        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version2,
                &catalogue,
                std::slice::from_ref(&executable),
            )
            .is_err()
        );
        assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version2,
                &catalogue,
                &[executable.clone(), second_executable.clone()],
            )
            .is_ok()
        );

        let wrong_revision =
            standard_executable_fixture_with_revision(FunctionRevisionId::from_bytes([0x55; 16]));
        let error = validate_standard_executable_facts(
            StandardLibraryDigestVersion::Version2,
            &catalogue,
            &[wrong_revision, second_executable],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant { rule, .. }
                if rule == "standard catalogue function and executable current revision must agree"
        ));
    }

    #[test]
    fn standard_resolved_type_columns_close_scalar_and_value_shapes() {
        let scalar =
            standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Integer), true)
                .unwrap();
        assert_eq!(scalar.kind, "scalar");
        assert_eq!(scalar.scalar, Some("integer"));
        assert!(scalar.value_type.is_none());

        let value_type = TypeId::from_bytes([0x77; 16]);
        let resolved = ResolvedType::value(value_type);
        let value = standard_resolved_type_columns(resolved, false).unwrap();
        assert_eq!(value.kind, "value");
        assert!(value.scalar.is_none());
        assert_eq!(value.value_type, Some(value_type));

        let void =
            standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Void), false);
        assert!(void.is_err());
        assert!(
            standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Void), true)
                .is_ok()
        );
    }

    #[test]
    fn standard_reference_target_columns_preserve_owner_and_pin_shapes() {
        let standard_revision = StandardLibraryRevisionId::from_bytes([0x44; 16]);
        let object = TypeId::from_bytes([1; 16]);
        let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
            DefinitionReferenceTarget::ObjectType(object),
            standard_revision,
        )
        .unwrap();
        assert_eq!(kind, "object_type");
        assert_eq!(target, object.to_bytes().to_vec());
        assert!(owner_type.is_none());
        assert!(owner_function.is_none());
        assert!(pin.is_none());

        let field = FieldId::from_bytes([2; 16]);
        let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
            DefinitionReferenceTarget::Field {
                owner: object,
                field,
            },
            standard_revision,
        )
        .unwrap();
        assert_eq!(kind, "field");
        assert_eq!(target, field.to_bytes().to_vec());
        assert_eq!(owner_type, Some(object.to_bytes().to_vec()));
        assert!(owner_function.is_none());
        assert!(pin.is_none());

        let function = FunctionId::from_bytes([0x10; 16]);
        let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
            DefinitionReferenceTarget::Function(function),
            standard_revision,
        )
        .unwrap();
        assert_eq!(kind, "function");
        assert_eq!(target, function.to_bytes().to_vec());
        assert!(owner_type.is_none());
        assert!(owner_function.is_none());
        assert!(pin.is_none());

        let parameter = ParameterId::from_bytes([0x10; 16]);
        let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
            DefinitionReferenceTarget::Parameter {
                owner: function,
                parameter,
            },
            standard_revision,
        )
        .unwrap();
        assert_eq!(kind, "parameter");
        assert_eq!(target, parameter.to_bytes().to_vec());
        assert!(owner_type.is_none());
        assert_eq!(owner_function, Some(function.to_bytes().to_vec()));
        assert!(pin.is_none());

        let value_type = TypeId::from_bytes([2; 16]);
        let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
            DefinitionReferenceTarget::ValueType(value_type),
            standard_revision,
        )
        .unwrap();
        assert_eq!(kind, "value_type");
        assert_eq!(target, value_type.to_bytes().to_vec());
        assert!(owner_type.is_none());
        assert!(owner_function.is_none());
        assert_eq!(pin, Some(standard_revision.to_bytes().to_vec()));
    }

    #[test]
    fn standard_executable_identity_selectors_keep_active_before_inactive() {
        let active_function = (
            StandardExecutableIdentity::Function(FunctionId::from_bytes([1; 16])),
            vec![1; 16],
        );
        let inactive_function = (
            StandardExecutableIdentity::Function(FunctionId::from_bytes([2; 16])),
            vec![2; 16],
        );
        assert_eq!(
            first_active_standard_executable_identity(
                std::slice::from_ref(&active_function),
                &[inactive_function],
            ),
            None
        );
        assert_eq!(
            first_active_standard_executable_identity(
                std::slice::from_ref(&active_function),
                std::slice::from_ref(&active_function),
            ),
            Some(StandardExecutableIdentity::Function(
                FunctionId::from_bytes([1; 16])
            ))
        );
        let revision = (
            StandardExecutableIdentity::FunctionRevision(FunctionRevisionId::from_bytes([9; 16])),
            vec![9; 16],
        );
        assert_eq!(
            first_inactive_standard_executable_identity(
                std::slice::from_ref(&revision),
                &[vec![9; 16]]
            ),
            Some(StandardExecutableIdentity::FunctionRevision(
                FunctionRevisionId::from_bytes([9; 16])
            ))
        );
        assert_eq!(
            first_inactive_standard_executable_identity(&[revision], &[vec![8; 16]]),
            None
        );
    }

    #[test]
    fn standard_parameter_selector_matches_scoped_pairs() {
        let function = FunctionId::from_bytes([0x10; 16]);
        let parameter = ParameterId::from_bytes([0x10; 16]);
        let wanted = StandardExecutableParameter {
            function,
            parameter,
        };
        assert_eq!(first_active_standard_parameter(&[], &[wanted]), None);
        assert_eq!(
            first_active_standard_parameter(&[wanted], &[wanted]),
            Some(wanted)
        );
        let other_owner = StandardExecutableParameter {
            function: FunctionId::from_bytes([0x11; 16]),
            parameter,
        };
        assert_eq!(
            first_active_standard_parameter(&[other_owner], &[wanted]),
            None
        );
    }

    fn standard_executable_fixture() -> StandardExecutable {
        standard_executable_fixture_with_function(
            FunctionId::from_bytes([0x10; 16]),
            FunctionRevisionId::from_bytes([0x10; 16]),
        )
    }

    fn standard_executable_fixture_with_revision(
        revision_id: FunctionRevisionId,
    ) -> StandardExecutable {
        standard_executable_fixture_with_function(FunctionId::from_bytes([0x10; 16]), revision_id)
    }

    fn standard_executable_fixture_with_function(
        function: FunctionId,
        revision_id: FunctionRevisionId,
    ) -> StandardExecutable {
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.server-parameter-echo",
            1,
            vec![0x4f, 0x52, 0x4e, 0x41, 0x50, 0x45, 0, 0, 0, 0, 0, 1],
            Sha256Digest::from_bytes([7; 32]),
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function,
            revision_id,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([3; 16]), 0, 1).unwrap(),
            Sha256Digest::from_bytes([5; 32]),
            Sha256Digest::from_bytes([6; 32]),
            "orna.language/1",
            artifact,
        )
        .unwrap();
        StandardExecutable::new(function, revision, Vec::new()).unwrap()
    }

    fn executable_function_fixture(current_revision: FunctionRevisionId) -> FunctionDefinition {
        executable_function_fixture_with_id(
            FunctionId::from_bytes([0x10; 16]),
            current_revision,
            QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
        )
    }

    fn executable_function_fixture_with_id(
        function: FunctionId,
        current_revision: FunctionRevisionId,
        name: QualifiedSemanticName,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            function,
            name,
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            current_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }
}
