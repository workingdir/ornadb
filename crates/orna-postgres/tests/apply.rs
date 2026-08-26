mod support;

use std::{
    collections::BTreeMap,
    str::FromStr,
    time::{Duration, Instant},
};

#[cfg(feature = "test-hooks")]
use orna_artifact::server_parameter_echo::ServerParameterEcho;
use orna_compiler::{
    PrepareStandardUpgradeError, StandardApplicationCheckContext, check,
    check_standard_application, prepare, prepare_standard_application,
};
#[cfg(feature = "test-hooks")]
use orna_compiler::{
    STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
    STD_INVOKE_SOURCE_UNIT_ID, STD_TYPES_SOURCE_UNIT_ID,
};
use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, InspectEpochId, InvocationId, ObjectId, PrincipalId,
    SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, StateSlotId, TypeId,
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, FunctionReturn, TypeLookupName, ValueTypeKind, ValueTypeMutability,
        ValueTypePersistence,
    },
    inspect::{
        InspectOutcomeKind, InspectPrivilege, InspectSecurityDecisionKind,
        InspectSecurityDecisionOutcome, InspectTraceEventKind, InspectTracePayload,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
        DeployableRevisionContent, DeployableRevisionInput, FunctionRevisionRecord, RevisionPair,
        Sha256Digest, SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
        StoredSourceRevision, StoredSourceUnit,
    },
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar, TypeDescriptorKind},
    value::{ConstructedValueKind, RuntimeValue},
};
#[cfg(feature = "test-hooks")]
use orna_core::{
    SchemaId,
    canonical_hash::{
        artifact_payload_digest, function_declaration_digest,
        function_semantic_digest_with_version, verify_standard_library_v2_snapshot,
    },
    revision::{
        DefinitionReference, ExecutableArtifact, ExecutableArtifactKind, StandardExecutable,
    },
};
#[cfg(feature = "test-hooks")]
use orna_core::{
    catalogue::{
        EnumTypeDefinition, FunctionDefinition, FunctionDomain, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ParameterDefinition, PreludeTypeName,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition, TypeBinding, ValueTypeDefinition,
    },
    revision::{DefinitionOrigin, FunctionSemanticHashVersion},
    types::TypeDescriptor,
};
use orna_core::{
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationParameterSelector,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeEvent,
        InvokeRequest, InvokeRequestInput, InvokeValue,
    },
    security::{
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision, ExecuteDenial, ExecuteGrant,
        InvocationTarget, Principal, PrincipalKind, PrincipalStatus, PrivilegeClass,
        PrivilegeDecision, PrivilegeDenial, PrivilegeGrant, RoleMembership,
        SecurityAdminAuditOperation, SecurityAuditKind, SecurityAuditOutcome,
        SecurityFunctionTarget, SecuritySnapshot, UserStateAuditOperation,
    },
    state::{UserStateChange, UserStateWriteOutcome},
    system::{
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_PRINCIPAL_TYPE_ID,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID, SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
    },
};
use orna_postgres::{PostgresKernel, PostgresKernelError, SealedInvocationResult};
use orna_protocol::{
    decode_invocation_event_batch, encode_constructed_value, encode_invocation_event_batch,
    encode_invoke_request,
};
use orna_standard::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_SOURCE_V2_REVISION_ID, STANDARD_SOURCE_V3_BUNDLE_ID, STANDARD_SOURCE_V3_REVISION_ID,
    STD_IO_BYTE_STREAM_CONTRACT, STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID,
    STD_OUTPUT_SOURCE_UNIT_ID, STD_TERMINAL_DOCUMENT_CONTRACT, STD_TERMINAL_DOCUMENT_TYPE_ID,
    STD_TERMINAL_SCHEMA_ID, registered_opaque_codecs,
};
use support::{TestDatabase, TestResult, failure, with_test_database};

const BASIC_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

#[cfg(feature = "test-hooks")]
const FROZEN_STANDARD_ENUM_DIGEST: [u8; 32] = [
    0xac, 0x5e, 0x03, 0x56, 0xb6, 0xb2, 0x5d, 0xae, 0x93, 0x07, 0x25, 0x3a, 0xba, 0x41, 0x57, 0x26,
    0xd2, 0xa3, 0xc2, 0xb4, 0xa8, 0xe9, 0xe2, 0x9a, 0x71, 0xad, 0xdf, 0xd4, 0xc8, 0xa8, 0x0f, 0xd5,
];

const BASIC_SOURCE_ONLY_EDIT: &str = "-- source-only formatting edit\n\
    CREATE SCHEMA app;\n\n\
    CREATE TYPE app.widget AS OBJECT ( name TEXT NOT NULL, active BOOL NOT NULL );\n\
    CREATE SERVER FUNCTION app.list_widgets() RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

const BASIC_SOURCE_WITHOUT_FUNCTION: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n";

const BASIC_CHANGED_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = TRUE;\n";

const STANDARD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n";

const ENUM_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.stage AS ENUM ('lead', 'owner''s', 'customer');\n\
    CREATE TYPE app.case AS OBJECT (stage app.stage NOT NULL);\n";

const RECORD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.stage AS ENUM ('lead', 'customer');\n\
    CREATE TYPE app.status AS VALUE (enabled BOOLEAN, stage app.stage)\n\
    IMMUTABLE PERSISTABLE;\n";

const STANDARD_UPGRADE_V1_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read()\n\
    RETURNS ROWS (visible BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT item.done FROM app.item item;\n";

const STANDARD_APPLICATION_SOURCE_EDIT: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read()\n\
    RETURNS ROWS (visible BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT item.done FROM app.item item;\n\
    CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_done BOOLEAN)\n\
    RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC\n\
    AS INSERT INTO app.item AS made (done) VALUES (p_done) RETURNING REF(made);\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;\n";

const FIELD_RENAME_ORIGINAL_SOURCE: &str = "CREATE SCHEMA people;\n\
    CREATE TYPE people.person AS OBJECT (email TEXT NOT NULL);\n\
    CREATE SERVER FUNCTION people.list_emails()\n\
    RETURNS ROWS (email TEXT)\n\
    AS SELECT p.email FROM people.person p;\n";

const FIELD_RENAME_FINAL_SOURCE: &str = "CREATE SCHEMA people;\n\
    CREATE TYPE people.person AS OBJECT (primary_email TEXT NOT NULL);\n\
    ALTER TYPE people.person RENAME FIELD email TO primary_email;\n\
    CREATE SERVER FUNCTION people.list_emails()\n\
    RETURNS ROWS (email TEXT)\n\
    AS SELECT p.primary_email FROM people.person p;\n";

const UNIQUE_REFERENCE_ORIGINAL_SOURCE: &str = "CREATE SCHEMA assignments;\n\
    CREATE TYPE assignments.person AS OBJECT ();\n\
    CREATE TYPE assignments.assignment AS OBJECT (\n\
        owner REF assignments.person NOT NULL UNIQUE\n\
    );\n";

const UNIQUE_REFERENCE_RENAMED_SOURCE: &str = "CREATE SCHEMA assignments;\n\
    CREATE TYPE assignments.person AS OBJECT ();\n\
    CREATE TYPE assignments.assignment AS OBJECT (\n\
        assignee REF assignments.person NOT NULL UNIQUE\n\
    );\n\
    ALTER TYPE assignments.assignment RENAME FIELD owner TO assignee;\n";

const UNIQUE_TEXT_ORIGINAL_SOURCE: &str = "CREATE SCHEMA accounts;\n\
    CREATE TYPE accounts.account AS OBJECT (\n\
        email TEXT UNIQUE,\n\
        username TEXT NOT NULL UNIQUE\n\
    );\n";

const UNIQUE_TEXT_RENAMED_SOURCE: &str = "CREATE SCHEMA accounts;\n\
    CREATE TYPE accounts.account AS OBJECT (\n\
        contact_email TEXT UNIQUE,\n\
        handle TEXT NOT NULL UNIQUE\n\
    );\n\
    ALTER TYPE accounts.account RENAME FIELD email TO contact_email;\n\
    ALTER TYPE accounts.account RENAME FIELD username TO handle;\n";

const MUTUAL_REFERENCE_SOURCE: &str = "CREATE SCHEMA graph;\n\
    CREATE TYPE graph.left AS OBJECT (right REF graph.right);\n\
    CREATE TYPE graph.right AS OBJECT (left REF graph.left);\n";

const RACE_LEFT_SOURCE: &str = "CREATE SCHEMA race_left;\n\
    CREATE TYPE race_left.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_left.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_left.item item;\n";

const RACE_RIGHT_SOURCE: &str = "CREATE SCHEMA race_right;\n\
    CREATE TYPE race_right.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_right.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_right.item item;\n";

const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
const RACE_LOCK_KEY: i64 = 0x4f52_4e41_4150_504c;
const USER_STATE_INSERT_RACE_LOCK_KEY: i64 = 0x4f52_4e41_5354_4154;
const EMPTY_STANDARD_SOURCE: &str = "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;";
const EMPTY_STANDARD_DIGEST: [u8; 32] = [
    0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d, 0x6c, 0xaf,
    0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d, 0x14, 0xcc, 0x68, 0xae,
];

#[derive(Clone, Copy)]
enum FailurePoint {
    SourceBundle,
    CatalogueSchema,
    FunctionArtifact,
    DefinitionReference,
    DeferredReference,
    StatusSweep,
    ActivePointer,
    PostPointerRecovery,
    AuditAppend,
}

impl FailurePoint {
    const ALL: [Self; 9] = [
        Self::SourceBundle,
        Self::CatalogueSchema,
        Self::FunctionArtifact,
        Self::DefinitionReference,
        Self::DeferredReference,
        Self::StatusSweep,
        Self::ActivePointer,
        Self::PostPointerRecovery,
        Self::AuditAppend,
    ];
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_a_compiler_candidate_and_recovers_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &active)?;

        let applied = kernel.apply(&candidate).await?;
        let recovered = kernel.recover().await?;

        require_recovered_new_candidate(&candidate, &applied)?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        require(
            recovered.catalogue().schemas().len() == 1
                && recovered.catalogue().object_types().len() == 1
                && recovered.catalogue().functions().len() == 1
                && recovered.function_revisions().len() == 1,
            "basic apply did not recover one schema, object, function, and immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_source_apply_and_records_one_protected_audit_event() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;

        let applied = kernel.apply_source_apply(&candidate).await?;
        require_recovered_new_candidate(&candidate, &applied)?;
        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;

        let events = reopened.recover_security_audit_events().await?;
        let source_apply_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SourceApply)
            .collect::<Vec<_>>();
        require(
            events.len() == 1 && source_apply_events.len() == 1,
            "source apply did not record exactly one protected SourceApply event",
        )?;
        let decision = source_apply_events[0].decision();
        require(
            decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.session_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.source_apply_candidate() == Some(candidate.candidate_pair())
                && decision.target().is_none()
                && decision.denial().is_none(),
            "SourceApply audit detail did not match the committed candidate",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_rejects_removing_durable_execute_grant_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x71; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let initial_candidate = candidate(BASIC_SOURCE, &empty)?;
        let active = kernel.apply(&initial_candidate).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("execute-grant fixture omitted app.list_widgets"))?
            .id();
        let grant = ExecuteGrant::new(GRANTEE, function);
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![grant],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let omission = candidate(BASIC_SOURCE_WITHOUT_FUNCTION, &active)?;
        let error = kernel
            .apply_source_apply(&omission)
            .await
            .expect_err("source apply must reject removal of a durable EXECUTE target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "source apply returned the wrong durable EXECUTE target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected source apply changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().collect::<Vec<_>>() == [grant]
                && recovered_security.privilege_grants().next().is_none(),
            "rejected source apply changed the durable EXECUTE grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_rejects_removing_durable_privilege_grant_object_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let initial_candidate = candidate(BASIC_SOURCE, &empty)?;
        let active = kernel.apply(&initial_candidate).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("privilege-grant fixture omitted app.list_widgets"))?
            .id();
        let grant = PrivilegeGrant::new(GRANTEE, PrivilegeClass::Execute, Some(function))?;
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let omission = candidate(BASIC_SOURCE_WITHOUT_FUNCTION, &active)?;
        let error = kernel
            .apply_source_apply(&omission)
            .await
            .expect_err("source apply must reject removal of a durable privilege object target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    rule: "candidate source must retain every durable privilege grant object target",
                    ..
                }
            ),
            "source apply returned the wrong durable privilege target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected source apply changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().next().is_none()
                && recovered_security.privilege_grants().collect::<Vec<_>>() == [grant],
            "rejected source apply changed the durable privilege grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_upgrade_rejects_removing_durable_execute_grant_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("standard-upgrade fixture omitted app.list_widgets"))?
            .id();
        let grant = ExecuteGrant::new(GRANTEE, function);
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![grant],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let standard = verified_empty_non_golden_standard()?;
        let omission = standard_context_candidate(active.pair())?;
        let error = kernel
            .apply_test_standard_upgrade(&omission, &standard)
            .await
            .expect_err("standard upgrade must reject removal of a durable EXECUTE target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "standard upgrade returned the wrong durable EXECUTE target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected standard upgrade changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().collect::<Vec<_>>() == [grant],
            "rejected standard upgrade changed the durable EXECUTE grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_upgrade_refreshes_grants_after_waiting_for_active_revision_lock() -> TestResult<()>
{
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x74; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("standard-upgrade race fixture omitted app.list_widgets"))?
            .id();
        let standard = verified_empty_non_golden_standard()?;
        let omission = standard_context_candidate(active.pair())?;

        // Hold the same singleton row lock that standard upgrades acquire, then
        // commit a durable grant while the upgrade is waiting. ReadCommitted
        // must take the grant-validation snapshot after that wait.
        let writer = database.open().await?;
        writer
            .client()
            .batch_execute("BEGIN")
            .await
            .map_err(|error| failure(format!("beginning grant writer failed: {error}")))?;
        writer
            .client()
            .query_one(
                "SELECT singleton
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true
                 FOR UPDATE",
                &[],
            )
            .await?;
        let grantee = GRANTEE.to_bytes().to_vec();
        let function_bytes = function.to_bytes().to_vec();
        writer
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES ($1, 'user', 'active')",
                &[&grantee],
            )
            .await?;
        writer
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[&grantee, &function_bytes],
            )
            .await?;

        let upgrade_task = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel
                    .apply_test_standard_upgrade(&omission, &standard)
                    .await
            }
        });

        let observer = database.open().await?;
        let mut waiting = false;
        for _ in 0..500 {
            let row = observer
                .client()
                .query_one(
                    "SELECT EXISTS (
                         SELECT 1
                         FROM pg_catalog.pg_stat_activity
                         WHERE datname = pg_catalog.current_database()
                           AND pid <> pg_catalog.pg_backend_pid()
                           AND wait_event_type = 'Lock'
                           AND query LIKE '%_orna_kernel.active_revision%'
                     )",
                    &[],
                )
                .await?;
            if row.get::<_, bool>(0) {
                waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        observer.shutdown().await?;
        if !waiting {
            upgrade_task.abort();
            writer.client().batch_execute("ROLLBACK").await?;
            writer.shutdown().await?;
            return Err(failure(
                "standard upgrade did not reach the active-revision lock wait",
            ));
        }

        writer.client().batch_execute("COMMIT").await?;
        writer.shutdown().await?;
        let error = upgrade_task
            .await
            .map_err(|error| failure(format!("standard-upgrade task failed: {error}")))?
            .expect_err("standard upgrade must reject a grant committed after its lock wait");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "standard upgrade returned the wrong post-wait durable grant rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected post-wait standard upgrade changed the active revision",
        )?;
        let grants = kernel.recover_security_snapshot().await?;
        require(
            grants
                .execute_grants()
                .any(|grant| grant.function() == function),
            "committed durable EXECUTE grant disappeared after rejected upgrade",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn lists_revision_pairs_with_parent_links_and_active_candidate() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let base_pair = base.pair();
        let candidate = candidate(BASIC_SOURCE, &base)?;
        let candidate_pair = candidate.candidate_pair();
        kernel.apply(&candidate).await?;
        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;
        let entries = reopened.list_revision_pairs().await?;
        let reopened_again = PostgresKernel::from_str(&database.connection_string())?;
        reopened_again.recover().await?;
        let repeated_entries = reopened_again.list_revision_pairs().await?;
        require(
            entries == repeated_entries,
            "revision pair history changed across repeated reopen and listing",
        )?;
        require(
            entries.len() == 2,
            "revision pair history did not contain exactly the bootstrap and candidate pairs",
        )?;
        require(
            entries.windows(2).all(|window| {
                (
                    window[0].source_revision_id(),
                    window[0].catalogue_revision_id(),
                ) < (
                    window[1].source_revision_id(),
                    window[1].catalogue_revision_id(),
                )
            }),
            "revision pair history was not returned in deterministic source/catalogue order",
        )?;
        let base_entry = entries
            .iter()
            .find(|entry| {
                RevisionPair::new(entry.source_revision_id(), entry.catalogue_revision_id())
                    == base_pair
            })
            .ok_or_else(|| failure("revision pair history did not contain the bootstrap pair"))?;
        require(
            base_entry.source_parent_revision_id().is_none()
                && base_entry.catalogue_parent_revision_id().is_none(),
            "bootstrap revision pair unexpectedly carried parent links",
        )?;
        let candidate_entry = entries
            .iter()
            .find(|entry| {
                RevisionPair::new(entry.source_revision_id(), entry.catalogue_revision_id())
                    == candidate_pair
            })
            .ok_or_else(|| failure("revision pair history did not contain the candidate pair"))?;
        require(
            candidate_entry.source_parent_revision_id() == Some(base_pair.source())
                && candidate_entry.catalogue_parent_revision_id() == Some(base_pair.catalogue()),
            "candidate revision pair did not retain the bootstrap pair as both parents",
        )?;
        let active_entries = entries
            .iter()
            .filter(|entry| entry.is_active())
            .collect::<Vec<_>>();
        require(
            active_entries.len() == 1
                && RevisionPair::new(
                    active_entries[0].source_revision_id(),
                    active_entries[0].catalogue_revision_id(),
                ) == candidate_pair,
            "revision pair history did not mark exactly the candidate pair active",
        )
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_failure_rolls_back_candidate_and_audit_event() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        install_failure_point(&database, FailurePoint::PostPointerRecovery, &candidate).await?;

        let error = kernel
            .apply_source_apply(&candidate)
            .await
            .expect_err("source apply must fail when post-pointer recovery is tampered");
        assert_failure_shape(FailurePoint::PostPointerRecovery, &error)?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&base, &recovered),
            "failed source apply changed the active revision",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events
                .iter()
                .all(|event| event.decision().kind() != SecurityAuditKind::SourceApply),
            "failed source apply left a protected SourceApply audit event",
        )?;
        require_no_candidate_residue(&database, &candidate, &base).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_append_failure_rolls_back_candidate_and_audit_event() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        let baseline = baseline(&database, &base).await?;
        let audit_count = kernel.recover_security_audit_events().await?.len();
        install_failure_point(&database, FailurePoint::AuditAppend, &candidate).await?;

        let error = kernel
            .apply_source_apply(&candidate)
            .await
            .expect_err("source apply must fail while appending its protected audit event");
        assert_failure_shape(FailurePoint::AuditAppend, &error)?;

        require_baseline(&database, &baseline, &kernel).await?;
        require_no_candidate_residue(&database, &candidate, &base).await?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == audit_count
                && events
                    .iter()
                    .all(|event| event.decision().kind() != SecurityAuditKind::SourceApply),
            "failed source-apply audit append left partial protected audit history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_rejects_a_mismatched_revision_pair() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        kernel.apply_source_apply(&candidate).await?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = $1
                 WHERE event_kind = 'source_apply'",
                &[&base.pair().source().to_bytes().to_vec()],
            )
            .await?;
        session.shutdown().await?;

        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        let error = reopened
            .recover()
            .await
            .expect_err("mismatched source apply audit pair must fail recovery");
        if !matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                rule: "source apply audit target pair must exist in protected revisions",
                ..
            }
        ) {
            return Err(failure(format!(
                "unexpected mismatched source apply audit error: {error:?}"
            )));
        }
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_rejects_a_wrong_principal() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        kernel.apply_source_apply(&candidate).await?;

        let session = database.open().await?;
        let wrong_principal = vec![0_u8; 16];
        let error = session
            .client()
            .execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET session_principal_id = $1
                 WHERE event_kind = 'source_apply'",
                &[&wrong_principal],
            )
            .await
            .expect_err("source apply audit row with a wrong principal must be rejected");
        session.shutdown().await?;

        let database_error = error.as_db_error().ok_or_else(|| {
            failure(format!(
                "wrong-principal update was not a database error: {error}"
            ))
        })?;
        require(
            database_error.code().code() == "23514"
                && database_error.constraint()
                    == Some("security_audit_events_source_apply_principal_check"),
            "wrong-principal source apply update did not fail its principal CHECK constraint",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_version_two_candidate_before_any_apply_write() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = standard_context_candidate(active.pair())?;
        require(
            candidate.catalogue_hash_context().version() == CatalogueHashVersion::Version2
                && candidate.catalogue_hash() != active.catalogue_hash(),
            "version-two transition fixture did not carry a distinct later catalogue hash",
        )?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "version-two candidate unexpectedly reached a successful normal apply",
        )?;

        require(
            error.to_string()
                == "the active and candidate catalogue hash versions require a standard context transition"
                && std::error::Error::source(&error).is_none(),
            "standard context transition error did not preserve its exact source-free contract",
        )?;
        match error {
            PostgresKernelError::StandardContextTransitionRequired {
                active: CatalogueHashVersion::Version1,
                candidate: CatalogueHashVersion::Version2,
            } => {}
            error => {
                return Err(failure(format!(
                    "expected standard context transition error, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn checks_the_expected_base_before_standard_context_transition() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let stale = RevisionPair::new(
            SourceRevisionId::from_bytes([0x91; 16]),
            active.pair().catalogue(),
        );
        let candidate = standard_context_candidate(stale)?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "stale version-two candidate unexpectedly reached a successful apply",
        )?;

        require(
            error.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&error).is_none(),
            "expected-base mismatch did not preserve its existing source-free contract",
        )?;
        match error {
            PostgresKernelError::ExpectedBaseMismatch {
                expected,
                active: actual_active,
            } => require(
                expected == stale && actual_active == active.pair(),
                "expected-base mismatch did not win before the standard context guard",
            )?,
            error => {
                return Err(failure(format!(
                    "expected stale-base mismatch before standard transition, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_the_standard_upgrade_then_reuses_normal_version_two_apply() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            version_two.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&version_two, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(upgrade.application_revision(), &version_two)?;
        let replay_baseline = baseline(&database, &version_two).await?;
        let replay = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "replaying a standard upgrade unexpectedly succeeded",
        )?;
        require(
            replay.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&replay).is_none(),
            "standard-upgrade replay changed the exact expected-base error contract",
        )?;
        match replay {
            PostgresKernelError::ExpectedBaseMismatch { expected, active } => require(
                expected == upgrade.application_revision().expected_base()
                    && active == version_two.pair(),
                "standard-upgrade replay did not fail before collision scanning",
            )?,
            error => {
                return Err(failure(format!(
                    "expected standard-upgrade replay to fail with ExpectedBaseMismatch, got {error}"
                )));
            }
        }
        require_baseline(&database, &replay_baseline, &kernel).await?;

        let repeated = orna_standard::prepare_standard_upgrade(&version_two)
            .expect_err("re-preparing an installed standard upgrade unexpectedly succeeded");
        require(
            repeated.to_string()
                == format!(
                    "standard library {} is already installed",
                    upgrade.verified_standard_snapshot().revision()
                ),
            "re-preparing an installed standard did not preserve the exact compiler error",
        )?;
        match repeated {
            orna_standard::StandardUpgradeError::Prepare {
                source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision,
                },
            } => require(
                revision == upgrade.verified_standard_snapshot().revision(),
                "re-preparation reported the wrong installed standard revision",
            )?,
            error => {
                return Err(failure(format!(
                    "expected StandardLibraryAlreadyInstalled, got {error}"
                )));
            }
        }

        let second_candidate = standard_application_candidate(
            STANDARD_APPLICATION_SOURCE_EDIT,
            &version_two,
            &upgrade,
        )?;
        let second = kernel.apply(&second_candidate).await?;
        require(
            second.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "normal same-context apply did not retain the installed standard context",
        )?;
        require_standard_context(&second, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(&second_candidate, &second)?;
        require_standard_upgrade_storage(&database, &second, &upgrade, &second_candidate).await?;

        let restarted = named_kernel(&database, "orna-standard-restart")?
            .recover()
            .await?;
        require_standard_context(&restarted, upgrade.verified_standard_snapshot())?;
        require(
            same_recovered(&restarted, &second),
            "reconnect changed current or historical function revision facts",
        )?;
        require_recovered_snapshot(&second_candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_recovers_ordered_catalogue_enum_labels() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(ENUM_APPLICATION_SOURCE, &version_two, &upgrade)?;
        let expected = candidate
            .candidate()
            .enum_types()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its declaration"))?;
        let expected_schema = candidate
            .candidate()
            .schemas()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its schema"))?;
        let expected_object = candidate
            .candidate()
            .object_types()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its object"))?;
        let expected_field = expected_object
            .fields()
            .first()
            .ok_or_else(|| failure("enum candidate object did not contain its field"))?;
        let expected_origin = candidate
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected.id()))
            .ok_or_else(|| failure("enum candidate did not contain its source origin"))?;

        let applied = kernel.apply(&candidate).await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT type_id, schema_id, name_parts, labels,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_enum_types
                 WHERE catalogue_revision_id = $1",
                &[&candidate.candidate().revision().to_bytes().to_vec()],
            )
            .await?;
        let postgres_enum_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype = 'e'
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')",
                &[],
            )
            .await?
            .try_get(0)?;
        let field_row = session
            .client()
            .query_one(
                "SELECT type_kind, scalar_type, target_type_id, value_type_id,
                        value_standard_library_revision_id, enum_type_id
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                   AND owner_type_id = $2 AND field_id = $3",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected_object.id().to_bytes().to_vec(),
                    &expected_field.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let physical_row = session
            .client()
            .query_one(
                "SELECT pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                        attribute.attnotnull
                 FROM pg_catalog.pg_attribute AS attribute
                 WHERE attribute.attrelid = pg_catalog.to_regclass($1)
                   AND attribute.attname = $2 AND NOT attribute.attisdropped",
                &[&relation(expected_object.id()), &field(expected_field.id())],
            )
            .await?;
        require(
            row.try_get::<_, Vec<u8>>(0)? == expected.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<u8>>(1)? == expected_schema.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<String>>(2)? == expected.name().parts()
                && row.try_get::<_, Vec<String>>(3)? == expected.labels()
                && row.try_get::<_, Vec<u8>>(4)?
                    == expected_origin.source().source_unit().to_bytes().to_vec()
                && row.try_get::<_, i64>(5)? == i64::from(expected_origin.source().byte_start())
                && row.try_get::<_, i64>(6)? == i64::from(expected_origin.source().byte_end())
                && expected_field.resolved_type() == ResolvedType::named(expected.id())
                && field_row.try_get::<_, String>(0)? == "enum"
                && field_row.try_get::<_, Option<String>>(1)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(2)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(3)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_row.try_get::<_, Vec<u8>>(5)? == expected.id().to_bytes().to_vec()
                && physical_row.try_get::<_, String>(0)? == "text"
                && physical_row.try_get::<_, bool>(1)?
                && postgres_enum_count == 0,
            "enum apply did not preserve its exact catalogue and text storage rows",
        )?;
        session.shutdown().await?;

        let restarted = named_kernel(&database, "orna-enum-restart")?
            .recover()
            .await?;
        require_recovered_snapshot(&candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_recovers_named_record_definitions() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(RECORD_APPLICATION_SOURCE, &version_two, &upgrade)?;
        let expected = candidate
            .candidate()
            .record_value_types()
            .first()
            .ok_or_else(|| failure("record candidate did not contain its declaration"))?;
        let expected_type_origin = candidate
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected.id()))
            .ok_or_else(|| failure("record candidate did not contain its type origin"))?;
        let expected_field_origins = expected
            .fields()
            .iter()
            .map(|field| {
                candidate
                    .origins()
                    .iter()
                    .find(|origin| {
                        origin.identity()
                            == DefinitionIdentity::Field {
                                owner: expected.id(),
                                field: field.id(),
                            }
                    })
                    .ok_or_else(|| failure("record candidate did not contain a field origin"))
            })
            .collect::<TestResult<Vec<_>>>()?;

        let applied = kernel.apply(&candidate).await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let type_row = session
            .client()
            .query_one(
                "SELECT type_id, name_parts, value_kind, mutability, persistence,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_record_value_types
                 WHERE catalogue_revision_id = $1 AND type_id = $2",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let field_rows = session
            .client()
            .query(
                "SELECT field_id, name, ordinal, type_kind, value_type_id,
                        value_standard_library_revision_id, enum_type_id,
                        source_unit_id, source_start, source_end, record_type_id
                 FROM _orna_kernel.catalogue_record_value_fields
                 WHERE catalogue_revision_id = $1 AND owner_type_id = $2
                 ORDER BY ordinal",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let postgres_composite_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype = 'c'
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND type.typname = $1",
                &[&expected
                    .name()
                    .parts()
                    .last()
                    .ok_or_else(|| failure("record name has no local part"))?],
            )
            .await?
            .try_get(0)?;
        let expected_value_type = match expected.fields()[0].descriptor().kind() {
            TypeDescriptorKind::Named(type_id) => type_id.to_bytes().to_vec(),
            _ => return Err(failure("record value field has no named descriptor")),
        };
        let expected_enum_type = match expected.fields()[1].descriptor().kind() {
            TypeDescriptorKind::Named(type_id) => type_id.to_bytes().to_vec(),
            _ => return Err(failure("record enum field has no named descriptor")),
        };
        require(
            type_row.try_get::<_, Vec<u8>>(0)? == expected.id().to_bytes().to_vec()
                && type_row.try_get::<_, Vec<String>>(1)? == expected.name().parts()
                && type_row.try_get::<_, String>(2)? == "record"
                && type_row.try_get::<_, String>(3)? == "immutable"
                && type_row.try_get::<_, String>(4)? == "persistable"
                && type_row.try_get::<_, Vec<u8>>(5)?
                    == expected_type_origin
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && type_row.try_get::<_, i64>(6)?
                    == i64::from(expected_type_origin.source().byte_start())
                && type_row.try_get::<_, i64>(7)?
                    == i64::from(expected_type_origin.source().byte_end())
                && field_rows.len() == 2
                && field_rows[0].try_get::<_, Vec<u8>>(0)?
                    == expected.fields()[0].id().to_bytes().to_vec()
                && field_rows[0].try_get::<_, String>(1)? == "enabled"
                && field_rows[0].try_get::<_, i64>(2)? == 0
                && field_rows[0].try_get::<_, String>(3)? == "value"
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(4)? == Some(expected_value_type)
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(5)?
                    == Some(
                        upgrade
                            .verified_standard_snapshot()
                            .revision()
                            .to_bytes()
                            .to_vec(),
                    )
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                && field_rows[0].try_get::<_, Vec<u8>>(7)?
                    == expected_field_origins[0]
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && field_rows[0].try_get::<_, i64>(8)?
                    == i64::from(expected_field_origins[0].source().byte_start())
                && field_rows[0].try_get::<_, i64>(9)?
                    == i64::from(expected_field_origins[0].source().byte_end())
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(10)?.is_none()
                && field_rows[1].try_get::<_, Vec<u8>>(0)?
                    == expected.fields()[1].id().to_bytes().to_vec()
                && field_rows[1].try_get::<_, String>(1)? == "stage"
                && field_rows[1].try_get::<_, i64>(2)? == 1
                && field_rows[1].try_get::<_, String>(3)? == "enum"
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(6)? == Some(expected_enum_type)
                && field_rows[1].try_get::<_, Vec<u8>>(7)?
                    == expected_field_origins[1]
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && field_rows[1].try_get::<_, i64>(8)?
                    == i64::from(expected_field_origins[1].source().byte_start())
                && field_rows[1].try_get::<_, i64>(9)?
                    == i64::from(expected_field_origins[1].source().byte_end())
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(10)?.is_none()
                && postgres_composite_count == 0,
            "record apply did not preserve its exact protected definition rows",
        )?;
        session.shutdown().await?;

        let restarted = named_kernel(&database, "orna-record-restart")?
            .recover()
            .await?;
        require_recovered_snapshot(&candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_reconnects_a_standard_enum_record_field() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let standard = verified_standard_enum_fixture()?;
        let active = kernel_instance.recover().await?;
        let candidate = standard_enum_record_candidate(&active, &standard)?;
        let expected_record = candidate
            .candidate()
            .record_value_types()
            .first()
            .ok_or_else(|| failure("standard enum candidate has no record"))?;
        let expected_field = expected_record
            .fields()
            .first()
            .ok_or_else(|| failure("standard enum candidate has no record field"))?;
        let expected_enum = standard
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no enum"))?;
        let expected_enum_origin = standard
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected_enum.id()))
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("standard enum fixture has no enum origin"))?;
        let expected_origin = candidate
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Field {
                        owner: expected_record.id(),
                        field: expected_field.id(),
                    }
            })
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("standard enum candidate has no field origin"))?;

        let expected_binding = standard
            .catalogue()
            .type_bindings()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no binding"))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT field_id, name, ordinal, type_kind,
                        value_type_id, value_standard_library_revision_id, enum_type_id,
                        enum_standard_library_revision_id, standard_enum_type_id,
                        source_unit_id, source_start, source_end, record_type_id
                 FROM _orna_kernel.catalogue_record_value_fields
                 WHERE catalogue_revision_id = $1 AND owner_type_id = $2",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected_record.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let postgres_type_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype IN ('c', 'e')
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND type.typname = ANY($1)",
                &[&vec!["status", "mode"]],
            )
            .await?
            .try_get(0)?;
        let standard_enum = session
            .client()
            .query_one(
                "SELECT name_parts, labels, source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_enum_types
                 WHERE standard_library_revision_id = $1 AND type_id = $2",
                &[
                    &standard.revision().to_bytes().to_vec(),
                    &expected_enum.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let standard_binding = session
            .client()
            .query_one(
                "SELECT target_type_kind, target_type_id, target_enum_type_id
                 FROM _orna_kernel.standard_catalogue_type_bindings
                 WHERE standard_library_revision_id = $1 AND type_binding_id = $2",
                &[
                    &standard.revision().to_bytes().to_vec(),
                    &expected_binding.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            row.try_get::<_, Vec<u8>>(0)? == expected_field.id().to_bytes().to_vec()
                && row.try_get::<_, String>(1)? == "mode"
                && row.try_get::<_, i64>(2)? == 0
                && row.try_get::<_, String>(3)? == "enum"
                && row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(7)?
                    == Some(standard.revision().to_bytes().to_vec())
                && row.try_get::<_, Option<Vec<u8>>>(8)?
                    == Some(expected_enum.id().to_bytes().to_vec())
                && row.try_get::<_, Vec<u8>>(9)?
                    == expected_origin.source_unit().to_bytes().to_vec()
                && row.try_get::<_, i64>(10)? == i64::from(expected_origin.byte_start())
                && row.try_get::<_, i64>(11)? == i64::from(expected_origin.byte_end())
                && row.try_get::<_, Option<Vec<u8>>>(12)?.is_none()
                && standard_enum.try_get::<_, Vec<String>>(0)? == expected_enum.name().parts()
                && standard_enum.try_get::<_, Vec<String>>(1)? == expected_enum.labels()
                && standard_enum.try_get::<_, Vec<u8>>(2)?
                    == expected_enum_origin.source_unit().to_bytes().to_vec()
                && standard_enum.try_get::<_, i64>(3)?
                    == i64::from(expected_enum_origin.byte_start())
                && standard_enum.try_get::<_, i64>(4)?
                    == i64::from(expected_enum_origin.byte_end())
                && standard_binding.try_get::<_, String>(0)? == "enum"
                && standard_binding.try_get::<_, Option<Vec<u8>>>(1)?.is_none()
                && standard_binding.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(expected_enum.id().to_bytes().to_vec())
                && postgres_type_count == 0,
            "standard enum upgrade did not persist its definition, binding, and record tuple",
        )?;
        session.shutdown().await?;

        let reconnected = kernel(&database)?.recover().await?;
        require_recovered_snapshot(&candidate, &reconnected)?;
        let recovered_standard = reconnected
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("reconnected record recovery returned no standard"))?;
        let recovered_field = reconnected
            .catalogue()
            .record_value_type_by_id(expected_record.id())
            .and_then(|record| record.fields().first())
            .ok_or_else(|| failure("reconnected record recovery returned no field"))?;
        require(
            recovered_standard.revision() == standard.revision()
                && recovered_standard.digest() == standard.digest()
                && recovered_standard.catalogue().enum_types() == standard.catalogue().enum_types()
                && recovered_standard.catalogue().type_bindings()
                    == standard.catalogue().type_bindings()
                && recovered_field.descriptor() == &TypeDescriptor::named(expected_enum.id()),
            "reconnected standard enum record recovery changed its pinned descriptor facts",
        )
    })
    .await
}
#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_nested_record_field_targets_through_the_two_trigger_oracle() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let empty = kernel_instance.recover().await?;
        let version_one = kernel_instance
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
        let candidate = standard_application_candidate(
            NESTED_RECORD_APPLICATION_SOURCE,
            &version_two,
            &upgrade,
        )?;
        let records = candidate.candidate().record_value_types();
        if !(records.len() == 2
            && records[0].name().to_string() == "app.outer"
            && records[1].name().to_string() == "app.inner")
        {
            return Err(failure(format!(
                "nested candidate did not preserve source declaration order: {:?}",
                records
                    .iter()
                    .map(|record| record.name().to_string())
                    .collect::<Vec<_>>()
            )));
        }
        let outer = &records[0];
        let inner = &records[1];
        let child = outer
            .fields()
            .iter()
            .find(|field| field.name() == "child")
            .ok_or_else(|| failure("outer record has no child field"))?;
        let TypeDescriptorKind::Named(target) = child.descriptor().kind() else {
            return Err(failure(
                "child field descriptor is not a resolved Named identity",
            ));
        };
        require(
            target == inner.id(),
            "child field does not target the exact inner application record identity",
        )?;
        let child_origin = candidate
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Field {
                        owner: outer.id(),
                        field: child.id(),
                    }
            })
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("child field has no declaration origin"))?;
        require(
            child_origin.source_unit().to_bytes().to_vec()
                == candidate
                    .source()
                    .units()
                    .first()
                    .ok_or_else(|| failure("nested candidate has no source unit"))?
                    .id()
                    .to_bytes()
                    .to_vec(),
            "child field origin does not slice the candidate source unit",
        )?;
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let outer_type = outer.id().to_bytes().to_vec();
        let inner_type = inner.id().to_bytes().to_vec();
        let child_field = child.id().to_bytes().to_vec();

        let session = database.open().await?;
        install_nested_record_field_oracle_triggers(
            session.client(),
            &catalogue_revision,
            &outer_type,
            &child_field,
            &inner_type,
            &child_origin,
        )
        .await?;
        session.shutdown().await?;

        let before = baseline(&database, &version_two).await?;
        let error = failed_apply_error(
            kernel_instance.apply(&candidate).await,
            "nested record candidate unexpectedly survived the sentinel oracle",
        )?;
        match &error {
            PostgresKernelError::Database(database_error) => {
                let database_error = database_error.as_db_error().ok_or_else(|| {
                    failure(format!(
                        "nested record apply failed without database fields: {error}"
                    ))
                })?;
                if !(database_error.code().code() == "P0001"
                    && database_error.message() == "ORNA_APPLY_NESTED_RECORD_FIELD_OK")
                {
                    return Err(failure(format!(
                        "nested record apply failed with the wrong sentinel: {error}"
                    )));
                }
            }
            _ => {
                return Err(failure(format!(
                    "nested record apply failed before the P0001 sentinel: {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel_instance).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
const NESTED_RECORD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE app.inner AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n";

#[cfg(feature = "test-hooks")]
fn bytea_hex_literal(bytes: &[u8]) -> String {
    format!(
        "'\\x{}'::bytea",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(feature = "test-hooks")]
async fn install_nested_record_field_oracle_triggers(
    client: &tokio_postgres::Client,
    catalogue_revision: &[u8],
    outer_type: &[u8],
    child_field: &[u8],
    inner_type: &[u8],
    child_origin: &SourceOrigin,
) -> TestResult<()> {
    let revision = bytea_hex_literal(catalogue_revision);
    let outer = bytea_hex_literal(outer_type);
    let child = bytea_hex_literal(child_field);
    let inner = bytea_hex_literal(inner_type);
    let source_unit = bytea_hex_literal(&child_origin.source_unit().to_bytes());
    let source_start = i64::from(child_origin.byte_start());
    let source_end = i64::from(child_origin.byte_end());
    client
        .batch_execute(&format!(
            "CREATE FUNCTION _orna_kernel.orna_nested_target_ordering_assert()
             RETURNS trigger LANGUAGE plpgsql AS $function$
             BEGIN
                 IF NEW.catalogue_revision_id = {revision} AND NEW.type_id = {inner} THEN
                     IF NOT EXISTS (
                         SELECT 1 FROM _orna_kernel.catalogue_record_value_fields
                         WHERE catalogue_revision_id = {revision}
                           AND owner_type_id = {outer}
                           AND field_id = {child}
                     ) THEN
                         RAISE EXCEPTION 'ORNA_APPLY_FIELD_BEFORE_TARGET_VIOLATED';
                     END IF;
                 END IF;
                 RETURN NEW;
             END
             $function$;
             CREATE TRIGGER orna_nested_target_ordering
             BEFORE INSERT ON _orna_kernel.catalogue_record_value_types
             FOR EACH ROW EXECUTE FUNCTION _orna_kernel.orna_nested_target_ordering_assert();
             CREATE FUNCTION _orna_kernel.orna_nested_field_tuple_assert()
             RETURNS trigger LANGUAGE plpgsql AS $function$
             BEGIN
                 IF NOT (NEW.owner_type_id = {outer} AND NEW.field_id = {child}) THEN
                     RETURN NULL;
                 END IF;
                 IF NEW.type_kind <> 'record'
                     OR NEW.record_type_id <> {inner}
                     OR NEW.value_type_id IS NOT NULL
                     OR NEW.value_standard_library_revision_id IS NOT NULL
                     OR NEW.enum_type_id IS NOT NULL
                     OR NEW.enum_standard_library_revision_id IS NOT NULL
                     OR NEW.standard_enum_type_id IS NOT NULL
                     OR NEW.name <> 'child'
                     OR NEW.ordinal <> 0
                     OR NEW.source_unit_id <> {source_unit}
                     OR NEW.source_start <> {source_start}
                     OR NEW.source_end <> {source_end}
                 THEN
                     RAISE EXCEPTION 'ORNA_APPLY_TUPLE_MISMATCH %', NEW;
                 END IF;
                 IF NEW.catalogue_revision_id <> {revision} THEN
                     RAISE EXCEPTION 'ORNA_APPLY_REVISION_MISMATCH %', NEW.catalogue_revision_id;
                 END IF;
                 IF NOT EXISTS (
                     SELECT 1 FROM _orna_kernel.catalogue_record_value_types
                     WHERE catalogue_revision_id = NEW.catalogue_revision_id
                       AND type_id = NEW.record_type_id
                 ) THEN
                     RAISE EXCEPTION 'ORNA_APPLY_TARGET_MISSING';
                 END IF;
                 RAISE EXCEPTION 'ORNA_APPLY_NESTED_RECORD_FIELD_OK';
             END
             $function$;
             CREATE CONSTRAINT TRIGGER orna_nested_field_tuple
             AFTER INSERT ON _orna_kernel.catalogue_record_value_fields
             DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW EXECUTE FUNCTION _orna_kernel.orna_nested_field_tuple_assert();"
        ))
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn prepares_standard_upgrade_from_postgres_recovered_version_one_members() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_UPGRADE_V1_SOURCE, &empty)?;
        kernel.apply(&version_one_candidate).await?;

        let recovered = named_kernel(&database, "orna-standard-preparation-recovery")?
            .recover()
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&recovered)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        require(
            upgrade.application_revision().expected_base() == recovered.pair(),
            "standard upgrade expected base did not use the PostgreSQL-recovered pair",
        )?;

        let upgraded = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            upgraded.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&upgraded, upgrade.verified_standard_snapshot())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_an_inactive_standard_revision_collision_before_standard_writes() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let reserved_revision = upgrade.verified_standard_snapshot().revision();
        let hostile_catalogue = CatalogueRevisionId::from_bytes([0xe1; 16]);
        require(
            hostile_catalogue != upgrade.verified_standard_snapshot().catalogue().revision(),
            "hostile standard revision collision accidentally reused the standard catalogue ID",
        )?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, 'orna.test/hostile', $4, 'sha256')",
                &[
                    &reserved_revision.to_bytes().to_vec(),
                    &version_one.source().id().to_bytes().to_vec(),
                    &hostile_catalogue.to_bytes().to_vec(),
                    &vec![0_u8; 32],
                ],
            )
            .await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive standard identity collision unexpectedly allowed the upgrade",
        )?;
        require(
            error.to_string()
                == "the database contains an identity reserved for the standard library"
                && std::error::Error::source(&error).is_none(),
            "inactive standard identity collision changed its exact source-free error contract",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(revision),
            } => require(
                revision == reserved_revision,
                "inactive standard identity collision returned the wrong durable identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive standard revision collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let hostile = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash, hash_algorithm
                 FROM _orna_kernel.standard_library_revisions WHERE id = $1",
                &[&reserved_revision.to_bytes().to_vec()],
            )
            .await?;
        require(
            hostile.try_get::<_, Vec<u8>>(0)? == version_one.source().id().to_bytes()
                && hostile.try_get::<_, Vec<u8>>(1)? == hostile_catalogue.to_bytes()
                && hostile.try_get::<_, i16>(2)? == 1
                && hostile.try_get::<_, String>(3)? == "orna.test/hostile"
                && hostile.try_get::<_, Vec<u8>>(4)? == vec![0_u8; 32]
                && hostile.try_get::<_, String>(5)? == "sha256",
            "inactive standard revision collision row changed after the rejected upgrade",
        )?;
        let standard_rows = session
            .client()
            .query_one(
                "SELECT
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_schemas),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_value_types),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_type_bindings)",
                &[],
            )
            .await?;
        require(
            standard_rows.try_get::<_, i64>(0)? == 0
                && standard_rows.try_get::<_, i64>(1)? == 0
                && standard_rows.try_get::<_, i64>(2)? == 0,
            "rejected collision unexpectedly wrote standard catalogue rows",
        )?;
        session.shutdown().await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_reserved_type_stored_as_an_inactive_standard_enum() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let requested_type = orna_standard::BOOLEAN_TYPE_ID;
        let hostile_revision = StandardLibraryRevisionId::from_bytes([0xe2; 16]);
        let hostile_catalogue = CatalogueRevisionId::from_bytes([0xe3; 16]);
        let hostile_schema = orna_core::SchemaId::from_bytes([0xe4; 16]);
        let source_unit = version_one
            .source()
            .units()
            .first()
            .ok_or_else(|| failure("hostile enum collision fixture has no source unit"))?;

        let session = database.open().await?;
        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, 'orna.test/hostile-enum', $4, 'sha256')",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &version_one.source().id().to_bytes().to_vec(),
                    &hostile_catalogue.to_bytes().to_vec(),
                    &vec![0_u8; 32],
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_schemas
                    (standard_library_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, ARRAY['hostile'], $3, 0, 1)",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &hostile_schema.to_bytes().to_vec(),
                    &source_unit.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_enum_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, ARRAY['hostile', 'collision'], ARRAY['x'], $4, 0, 1)",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &requested_type.to_bytes().to_vec(),
                    &hostile_schema.to_bytes().to_vec(),
                    &source_unit.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive standard enum identity collision unexpectedly allowed the upgrade",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::Type(type_id),
            } => require(
                type_id == requested_type,
                "inactive standard enum collision returned the wrong type identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive standard enum type collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let hostile_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.standard_catalogue_enum_types
                 WHERE standard_library_revision_id = $1 AND type_id = $2",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &requested_type.to_bytes().to_vec(),
                ],
            )
            .await?
            .try_get(0)?;
        require(
            hostile_count == 1,
            "rejected standard enum collision changed its hostile durable row",
        )?;
        session.shutdown().await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_reserved_types_stored_as_inactive_application_values() -> TestResult<()> {
    reject_inactive_application_type_collision(
        InactiveApplicationTypeKind::Enum,
        orna_standard::BOOLEAN_TYPE_ID,
    )
    .await?;
    reject_inactive_application_type_collision(
        InactiveApplicationTypeKind::Record,
        orna_standard::INTEGER_TYPE_ID,
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_mutual_references_with_real_postgres_foreign_keys() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(MUTUAL_REFERENCE_SOURCE, &active)?;
        let applied = kernel.apply(&candidate).await?;

        let left = applied.catalogue().object_types()[0].id();
        let right = applied.catalogue().object_types()[1].id();
        let session = database.open().await?;
        let foreign_keys = session
            .client()
            .query(
                "SELECT conrelid::regclass::text, confrelid::regclass::text, confdeltype::text\n                 FROM pg_constraint\n                 WHERE contype = 'f'\n                   AND conrelid::regclass::text = ANY($1::text[])\n                 ORDER BY conrelid::regclass::text",
                &[&vec![relation(left), relation(right)]],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<(String, String, String)>, tokio_postgres::Error>>()?;
        session.shutdown().await?;
        require(
            same_members(
                &foreign_keys,
                &[
                    (relation(left), relation(right), "a".into()),
                    (relation(right), relation(left), "a".into()),
                ],
            ),
            "mutual REF apply did not install exact left/right NO ACTION foreign keys",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_only_edit_reuses_the_immutable_function_revision_and_artifact() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let first_revision = only_revision(&first)?.clone();
        let before = immutable_rows(&database, &first_revision).await?;
        let candidate = candidate(BASIC_SOURCE_ONLY_EDIT, &first)?;
        require(
            candidate.new_function_revisions().is_empty(),
            "source-only compiler preparation allocated an immutable function revision",
        )?;

        let applied = kernel.apply(&candidate).await?;
        let reused = only_revision(&applied)?;
        require_recovered_snapshot(&candidate, &applied)?;
        require(
            reused == &first_revision,
            "source-only apply changed the complete immutable function revision record",
        )?;
        let after = immutable_rows(&database, reused).await?;
        require(
            before == after,
            "source-only apply rewrote or added immutable function revision or artifact rows",
        )?;
        require(
            applied.historical_function_revisions().is_empty(),
            "source-only apply invented function revision history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn replay_safe_field_rename_preserves_live_storage_and_execution() -> TestResult<()> {
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            FIELD_RENAME_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        let object = original.catalogue().object_types()[0].id();
        let original_field = original.catalogue().object_types()[0].fields()[0].id();
        let function = original.catalogue().functions()[0].id();
        let original_revision = only_revision(&original)?.clone();
        let original_immutable = immutable_rows(&database, &original_revision).await?;
        let original_physical = physical_catalogue(&database, object).await?;
        let stored_object = ObjectId::from_bytes([91; 16]);
        insert_private_text(
            &database,
            object,
            original_field,
            stored_object,
            "kept@example.test",
        )
        .await?;
        let proof = RenameProof {
            object,
            field: original_field,
            function,
            revision: original_revision,
            immutable: original_immutable,
            physical: original_physical,
            stored_object,
        };

        let renamed_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &original)?;
        require(
            renamed_candidate.new_function_revisions().is_empty(),
            "field rename allocated a new immutable function revision",
        )?;
        require(
            renamed_candidate.source().bundle_hash() != original.source().bundle_hash()
                && renamed_candidate.source().revision_hash() != original.source().revision_hash()
                && renamed_candidate.catalogue_hash() != original.catalogue_hash(),
            "field rename did not change all source and catalogue hashes",
        )?;
        require_rename_semantics(
            &renamed_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let renamed = initial_kernel.apply(&renamed_candidate).await?;
        require_recovered_snapshot(&renamed_candidate, &renamed)?;
        require_rename_state(&database, &renamed, &proof).await?;

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_rename_state(&database, &recovered, &proof).await?;
        let replay_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &recovered)?;
        require(
            replay_candidate.new_function_revisions().is_empty(),
            "exact field-rename replay allocated a new immutable function revision",
        )?;
        require_rename_semantics(
            &replay_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let replayed = replay_kernel.apply(&replay_candidate).await?;
        let final_kernel = kernel(&database)?;
        let final_recovered = final_kernel.recover().await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_recovered_snapshot(&replay_candidate, &final_recovered)?;
        require_rename_state(&database, &final_recovered, &proof).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn required_unique_reference_replay_and_rename_preserve_physical_identity() -> TestResult<()>
{
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_REFERENCE_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        require_recovered_snapshot(&original_candidate, &original)?;

        let person = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "person"])
            .ok_or_else(|| failure("initial apply did not create assignments.person"))?;
        let assignment = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "assignment"])
            .ok_or_else(|| failure("initial apply did not create assignments.assignment"))?;
        let owner = assignment
            .field_by_name("owner")
            .ok_or_else(|| failure("initial apply did not create assignment.owner"))?;
        require(
            assignment.fields().len() == 1
                && owner.is_required_unique_reference()
                && owner.resolved_type() == ResolvedType::reference(person.id()),
            "initial apply changed the required unique reference semantics",
        )?;

        let assignment_physical = physical_catalogue(&database, assignment.id()).await?;
        let person_physical = physical_catalogue(&database, person.id()).await?;
        let field_hex = format!("{:032x}", u128::from_be_bytes(owner.id().to_bytes()));
        let unique_name = format!("uq_{field_hex}");
        let foreign_key_name = format!("fk_{field_hex}");
        require(
            assignment_physical
                .constraints
                .iter()
                .any(|(_, name, _)| name == &unique_name)
                && assignment_physical
                    .constraints
                    .iter()
                    .any(|(_, name, _)| name == &foreign_key_name)
                && assignment_physical
                    .indexes
                    .iter()
                    .any(|(_, name, _)| name == &unique_name),
            "initial apply did not install the stable unique and foreign-key identities",
        )?;
        let proof = UniqueReferenceProof {
            person: person.id(),
            assignment: assignment.id(),
            field: owner.id(),
            person_physical,
            assignment_physical,
        };

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_unique_reference_state(&database, &recovered, &proof, "owner").await?;
        let replay_candidate = candidate(UNIQUE_REFERENCE_ORIGINAL_SOURCE, &recovered)?;
        let replayed = replay_kernel.apply(&replay_candidate).await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_unique_reference_state(&database, &replayed, &proof, "owner").await?;

        let rename_candidate = candidate(UNIQUE_REFERENCE_RENAMED_SOURCE, &replayed)?;
        let renamed = replay_kernel.apply(&rename_candidate).await?;
        require_recovered_snapshot(&rename_candidate, &renamed)?;
        require_unique_reference_state(&database, &renamed, &proof, "assignee").await?;

        let final_recovered = kernel(&database)?.recover().await?;
        require_unique_reference_state(&database, &final_recovered, &proof, "assignee").await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unique_text_replay_and_rename_preserve_c_collation_and_physical_identity() -> TestResult<()>
{
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_TEXT_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        require_recovered_snapshot(&original_candidate, &original)?;

        let account = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["accounts", "account"])
            .ok_or_else(|| failure("initial apply did not create accounts.account"))?;
        let email = account
            .field_by_name("email")
            .ok_or_else(|| failure("initial apply did not create account.email"))?;
        let username = account
            .field_by_name("username")
            .ok_or_else(|| failure("initial apply did not create account.username"))?;
        require(
            account.fields().len() == 2
                && email.nullable()
                && email.unique()
                && email.resolved_type()
                    == ResolvedType::scalar(StandardScalar::CharacterLargeObject)
                && !username.nullable()
                && username.unique()
                && username.resolved_type()
                    == ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            "initial apply changed the v1 unique Text field semantics",
        )?;
        require_unique_text_physical_shape(&database, account.id(), email.id()).await?;
        require_unique_text_physical_shape(&database, account.id(), username.id()).await?;
        let proof = UniqueTextProof {
            object: account.id(),
            nullable_field: email.id(),
            required_field: username.id(),
            physical: physical_catalogue(&database, account.id()).await?,
        };

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_unique_text_state(&database, &recovered, &proof, "email", "username").await?;
        let replay_candidate = candidate(UNIQUE_TEXT_ORIGINAL_SOURCE, &recovered)?;
        let replayed = replay_kernel.apply(&replay_candidate).await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_unique_text_state(&database, &replayed, &proof, "email", "username").await?;

        let renamed_candidate = candidate(UNIQUE_TEXT_RENAMED_SOURCE, &replayed)?;
        let renamed = replay_kernel.apply(&renamed_candidate).await?;
        require_recovered_snapshot(&renamed_candidate, &renamed)?;
        require_unique_text_state(&database, &renamed, &proof, "contact_email", "handle").await?;

        let restarted = named_kernel(&database, "orna-unique-text-restart")?
            .recover()
            .await?;
        require_unique_text_state(&database, &restarted, &proof, "contact_email", "handle").await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unique_text_non_c_collation_tamper_fails_recovery_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_TEXT_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        let account = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["accounts", "account"])
            .ok_or_else(|| failure("initial apply did not create accounts.account"))?;
        let email = account
            .field_by_name("email")
            .ok_or_else(|| failure("initial apply did not create account.email"))?;
        tamper_unique_text_column_collation(&database, account.id(), email.id()).await?;

        let error = kernel(&database)?
            .recover()
            .await
            .expect_err("non-C unique Text column and index unexpectedly passed recovery");
        let table_name = format!(
            "t_{:032x}",
            u128::from_be_bytes(account.id().to_bytes())
        );
        require(
            error.to_string()
                == format!(
                    "durable invariant failed for _orna_data record {table_name}.2: column must have the exact private name, PostgreSQL type, shape, and PUBLIC access"
                )
                && std::error::Error::source(&error).is_none(),
            "non-C unique Text tamper changed the exact source-free recovery failure",
        )?;
        match error {
            PostgresKernelError::DurableInvariant {
                relation: "_orna_data",
                record,
                rule: "column must have the exact private name, PostgreSQL type, shape, and PUBLIC access",
            } if record == format!("{table_name}.2") => Ok(()),
            error => Err(failure(format!(
                "expected unique Text column collation invariant, got {error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn changed_function_history_is_retained_and_revert_reactivates_it() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let original = only_revision(&first)?.clone();
        let changed_candidate = candidate(BASIC_CHANGED_SOURCE, &first)?;
        let changed = kernel.apply(&changed_candidate).await?;
        let changed_revision = only_revision(&changed)?.clone();
        require(
            changed_revision.id() != original.id()
                && changed.historical_function_revisions() == [original.clone()],
            "changed function apply did not retain the previous immutable revision",
        )?;
        require_recovered_snapshot(&changed_candidate, &changed)?;
        require(
            changed.function_revisions() == changed_candidate.new_function_revisions(),
            "changed function apply did not activate its newly prepared immutable revision",
        )?;

        let revert_candidate = candidate(BASIC_SOURCE, &changed)?;
        require(
            revert_candidate.new_function_revisions().is_empty(),
            "revert preparation allocated a new immutable function revision",
        )?;
        let reverted = kernel.apply(&revert_candidate).await?;
        require_recovered_snapshot(&revert_candidate, &reverted)?;
        require(
            only_revision(&reverted)?.id() == original.id(),
            "revert did not reactivate the retained matching immutable revision",
        )?;
        require(
            reverted.historical_function_revisions() == [changed_revision],
            "revert did not retire the changed immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_apply_has_one_winner_and_no_loser_residue() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator.client().query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY]).await?;
        let left_kernel = named_kernel(&database, "orna-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move { left_kernel.apply(&left_for_task).await });
        wait_for_advisory_wait(&database, "orna-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move { right_kernel.apply(&right_for_task).await });
        wait_for_active_lock_block(&database, "orna-apply-race-a", "orna-apply-race-b").await?;
        coordinator.client().query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY]).await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left").await?;
        let right_result = wait_for_apply_task(right_task, "right").await?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate, &empty).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_source_apply_has_one_winner_one_audit_and_no_loser_residue()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator
            .client()
            .query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY])
            .await?;
        let left_kernel = named_kernel(&database, "orna-source-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move {
            left_kernel.apply_source_apply(&left_for_task).await
        });
        wait_for_advisory_wait(&database, "orna-source-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-source-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move {
            right_kernel.apply_source_apply(&right_for_task).await
        });
        wait_for_active_lock_block(
            &database,
            "orna-source-apply-race-a",
            "orna-source-apply-race-b",
        )
        .await?;
        coordinator
            .client()
            .query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY])
            .await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left source apply").await?;
        let right_result = wait_for_apply_task(right_task, "right source apply").await?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base source-apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base source-apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate, &empty).await?;

        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;
        let events = reopened.recover_security_audit_events().await?;
        let source_apply_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SourceApply)
            .collect::<Vec<_>>();
        require(
            source_apply_events.len() == 1,
            "same-base source-apply race did not record exactly one protected SourceApply event",
        )?;
        let decision = source_apply_events[0].decision();
        require(
            decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.session_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.source_apply_candidate() == Some(winner_candidate.candidate_pair())
                && decision.target().is_none()
                && decision.denial().is_none(),
            "same-base source-apply race audit detail did not match the winning candidate",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn every_apply_failure_point_rolls_back_to_the_exact_base() -> TestResult<()> {
    for point in FailurePoint::ALL {
        with_test_database(|database| async move {
            let kernel = kernel(&database)?;
            kernel.bootstrap().await?;
            let initial = kernel.recover().await?;
            let (base, candidate) = if matches!(point, FailurePoint::StatusSweep) {
                let committed = kernel.apply(&candidate(BASIC_SOURCE, &initial)?).await?;
                let changed = candidate(BASIC_CHANGED_SOURCE, &committed)?;
                (committed, changed)
            } else {
                let candidate = candidate(BASIC_SOURCE, &initial)?;
                (initial, candidate)
            };
            if matches!(
                point,
                FailurePoint::DefinitionReference | FailurePoint::DeferredReference
            ) {
                require(
                    !candidate.references().is_empty(),
                    "reference trigger fixture must contain references",
                )?;
            }
            let baseline = baseline(&database, &base).await?;
            install_failure_point(&database, point, &candidate).await?;

            let result = if matches!(point, FailurePoint::AuditAppend) {
                kernel.apply_source_apply(&candidate).await
            } else {
                kernel.apply(&candidate).await
            };
            let error = result.expect_err("triggered apply must fail");
            assert_failure_shape(point, &error)?;
            require_baseline(&database, &baseline, &kernel).await?;
            require_no_candidate_residue(&database, &candidate, &base).await
        })
        .await?;
    }
    Ok(())
}

async fn install_race_pause_trigger(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    session
        .client()
        .execute("SELECT pg_advisory_unlock_all()", &[])
        .await?;
    session.client().batch_execute(
        "CREATE FUNCTION _orna_kernel.test_apply_pause_pointer() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(5715716919262203980); RETURN NEW; END $$;
         CREATE TRIGGER pause_active_pointer BEFORE UPDATE ON _orna_kernel.active_revision
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_pause_pointer();",
    ).await?;
    session.shutdown().await
}

async fn install_user_state_insert_pause_trigger(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = session
        .client()
        .batch_execute(&format!(
            "CREATE FUNCTION _orna_kernel.test_user_state_insert_pause() \
             RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM pg_advisory_xact_lock({USER_STATE_INSERT_RACE_LOCK_KEY});
                 RETURN NEW;
             END $$;
             CREATE TRIGGER pause_user_state_insert
             AFTER INSERT ON _orna_kernel.user_state_cells
             FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_user_state_insert_pause();"
        ))
        .await
        .map_err(Into::into);
    finish_test_session(
        operation,
        session.shutdown().await,
        "USER state INSERT pause trigger installation",
    )
}

async fn remove_user_state_insert_pause_trigger(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = session
        .client()
        .batch_execute(
            "DROP TRIGGER IF EXISTS pause_user_state_insert ON _orna_kernel.user_state_cells;
             DROP FUNCTION IF EXISTS _orna_kernel.test_user_state_insert_pause();",
        )
        .await
        .map_err(Into::into);
    finish_test_session(
        operation,
        session.shutdown().await,
        "USER state INSERT pause trigger cleanup",
    )
}

async fn wait_for_advisory_wait(database: &TestDatabase, application: &str) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let waiting: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                WHERE application_name = $1
                  AND wait_event_type = 'Lock'
                  AND wait_event = 'advisory'
             )",
                &[&application],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if waiting {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "timed out waiting for {application} to block on the advisory lock"
            )));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_active_lock_block(
    database: &TestDatabase,
    holder: &str,
    waiter: &str,
) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let blocked: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1
                FROM pg_stat_activity AS holder
                JOIN pg_stat_activity AS waiter ON holder.pid = ANY(pg_blocking_pids(waiter.pid))
                WHERE holder.application_name = $1
                  AND waiter.application_name = $2
                  AND waiter.wait_event_type = 'Lock'
             )",
                &[&holder, &waiter],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if blocked {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "timed out waiting for B to block on A's active revision lock",
            ));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_apply_task(
    task: tokio::task::JoinHandle<Result<ActiveDatabaseRevision, PostgresKernelError>>,
    name: &'static str,
) -> TestResult<Result<ActiveDatabaseRevision, PostgresKernelError>> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    while !task.is_finished() {
        if Instant::now() >= deadline {
            task.abort();
            return Err(failure(format!("timed out waiting for {name} apply task")));
        }
        tokio::task::yield_now().await;
    }
    task.await
        .map_err(|error| failure(format!("{name} apply task failed: {error}")))
}

async fn wait_for_kernel_task<T>(
    task: tokio::task::JoinHandle<Result<T, PostgresKernelError>>,
    name: &'static str,
) -> TestResult<Result<T, PostgresKernelError>> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    while !task.is_finished() {
        if Instant::now() >= deadline {
            task.abort();
            let _ = task.await;
            return Err(failure(format!("timed out waiting for {name} task")));
        }
        tokio::task::yield_now().await;
    }
    task.await
        .map_err(|error| failure(format!("{name} task failed: {error}")))
}

async fn abort_kernel_task<T>(task: Option<tokio::task::JoinHandle<T>>) {
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}

#[derive(Clone, Debug)]
struct Baseline {
    active_source: Vec<u8>,
    active_catalogue: Vec<u8>,
    active_updated_at: String,
    counts: Vec<i64>,
    statuses: Vec<(Vec<u8>, String)>,
    recovered: ActiveDatabaseRevision,
}

async fn baseline(
    database: &TestDatabase,
    recovered: &ActiveDatabaseRevision,
) -> TestResult<Baseline> {
    let session = database.open().await?;
    let active = session
        .client()
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id, updated_at::text
         FROM _orna_kernel.active_revision WHERE singleton = true",
            &[],
        )
        .await?;
    let counts = session
        .client()
        .query_one(
            "SELECT
          (SELECT count(*) FROM _orna_kernel.schema_migrations),
          (SELECT count(*) FROM _orna_kernel.source_bundles),
          (SELECT count(*) FROM _orna_kernel.source_bundle_units),
          (SELECT count(*) FROM _orna_kernel.source_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_schemas),
          (SELECT count(*) FROM _orna_kernel.catalogue_object_types),
          (SELECT count(*) FROM _orna_kernel.catalogue_expressions),
          (SELECT count(*) FROM _orna_kernel.catalogue_fields),
          (SELECT count(*) FROM _orna_kernel.catalogue_functions),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns),
          (SELECT count(*) FROM _orna_kernel.function_revisions),
          (SELECT count(*) FROM _orna_kernel.function_artifacts),
          (SELECT count(*) FROM _orna_kernel.active_revision),
          (SELECT count(*) FROM _orna_kernel.definition_references),
          (SELECT count(*) FROM _orna_kernel.catalogue_enum_types),
          (SELECT count(*) FROM _orna_kernel.catalogue_record_value_types),
          (SELECT count(*) FROM _orna_kernel.catalogue_record_value_fields)",
            &[],
        )
        .await?;
    let statuses = session
        .client()
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
        .collect::<Result<Vec<(Vec<u8>, String)>, tokio_postgres::Error>>()?;
    let counts = (0..19)
        .map(|index| counts.try_get(index))
        .collect::<Result<Vec<i64>, _>>()?;
    let result = Baseline {
        active_source: active.try_get(0)?,
        active_catalogue: active.try_get(1)?,
        active_updated_at: active.try_get(2)?,
        counts,
        statuses,
        recovered: recovered.clone(),
    };
    session.shutdown().await?;
    Ok(result)
}

async fn require_baseline(
    database: &TestDatabase,
    expected: &Baseline,
    kernel: &PostgresKernel,
) -> TestResult<()> {
    let actual = baseline(database, &kernel.recover().await?).await?;
    require(
        actual.active_source == expected.active_source
            && actual.active_catalogue == expected.active_catalogue
            && actual.active_updated_at == expected.active_updated_at
            && actual.counts == expected.counts
            && actual.statuses == expected.statuses
            && same_recovered(&actual.recovered, &expected.recovered),
        "failed apply changed the exact durable base baseline",
    )
}

async fn require_standard_upgrade_storage(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let catalogue_id = active.catalogue().revision().to_bytes().to_vec();
        let standard = upgrade.verified_standard_snapshot();
        let standard_revision = standard.revision().to_bytes().to_vec();
        let object = active
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("standard application object row is missing"))?;
        let field = object
            .fields()
            .first()
            .ok_or_else(|| failure("standard application field definition is missing"))?;
        require(
            standard
                .catalogue()
                .value_types()
                .iter()
                .all(|value_type| value_type.id() != object.id()),
            "application object TypeId unexpectedly overlaps a standard Value TypeId",
        )?;
        let boolean_type_id = field
            .resolved_type()
            .value_type()
            .ok_or_else(|| failure("standard application field did not retain a Value identity"))?;
        let boolean_definition = standard
            .catalogue()
            .value_type_by_id(boolean_type_id)
            .ok_or_else(|| failure("application Boolean Value identity is not pinned"))?;
        require(
            boolean_definition.representation_contract() == "orna.kernel.value.boolean@1",
            "application Boolean Value identity does not select the verified Boolean contract",
        )?;
        let boolean_type = boolean_type_id.to_bytes().to_vec();

        let revision = session
            .client()
            .query_one(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash
                 FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&standard_revision],
            )
            .await?;
        require(
            revision.try_get::<_, Vec<u8>>(0)? == standard.revision().to_bytes()
                && revision.try_get::<_, Vec<u8>>(1)? == standard.source().id().to_bytes()
                && revision.try_get::<_, Vec<u8>>(2)? == standard.catalogue().revision().to_bytes()
                && revision.try_get::<_, i16>(3)? == standard.digest_version().to_u32() as i16
                && revision.try_get::<_, String>(4)? == standard.language_version()
                && revision.try_get::<_, Vec<u8>>(5)? == standard.digest().to_bytes(),
            "standard library revision rows changed during atomic installation",
        )?;

        let source_bundle_row = session
            .client()
            .query_one(
                "SELECT id, content_hash, hash_algorithm, hash_contract_version
                 FROM _orna_kernel.source_bundles WHERE id = $1",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_bundle_row.try_get::<_, Vec<u8>>(0)? == standard.source().bundle().to_bytes()
                && source_bundle_row.try_get::<_, Vec<u8>>(1)?
                    == standard.source().bundle_hash().to_bytes()
                && source_bundle_row.try_get::<_, String>(2)? == "sha256"
                && source_bundle_row.try_get::<_, i16>(3)? == 1,
            "standard source bundle row differs from the verified snapshot",
        )?;
        let source_revision_row = session
            .client()
            .query_one(
                "SELECT id, parent_source_revision_id, bundle_id, content_hash,
                        hash_algorithm, hash_contract_version
                 FROM _orna_kernel.source_revisions WHERE id = $1",
                &[&standard.source().id().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_revision_row.try_get::<_, Vec<u8>>(0)? == standard.source().id().to_bytes()
                && source_revision_row.try_get::<_, Option<Vec<u8>>>(1)?
                    == standard.source().parent().map(|id| id.to_bytes().to_vec())
                && source_revision_row.try_get::<_, Vec<u8>>(2)?
                    == standard.source().bundle().to_bytes()
                && source_revision_row.try_get::<_, Vec<u8>>(3)?
                    == standard.source().revision_hash().to_bytes()
                && source_revision_row.try_get::<_, String>(4)? == "sha256"
                && source_revision_row.try_get::<_, i16>(5)? == 1,
            "standard source revision row differs from the verified snapshot",
        )?;

        let catalogue_revision_row = session
            .client()
            .query_one(
                "SELECT id, source_revision_id, parent_catalogue_revision_id, content_hash,
                        canonical_hash_version, standard_library_revision_id, hash_contract_version
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&catalogue_id],
            )
            .await?;
        require(
            catalogue_revision_row.try_get::<_, Vec<u8>>(0)?
                == active.catalogue().revision().to_bytes()
                && catalogue_revision_row.try_get::<_, Vec<u8>>(1)?
                    == active.source().id().to_bytes()
                && catalogue_revision_row.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(candidate.parent_catalogue().to_bytes().to_vec())
                && catalogue_revision_row.try_get::<_, Vec<u8>>(3)?
                    == candidate.catalogue_hash().to_bytes()
                && catalogue_revision_row.try_get::<_, i16>(4)? == 2
                && catalogue_revision_row.try_get::<_, Option<Vec<u8>>>(5)?
                    == Some(standard.revision().to_bytes().to_vec())
                && catalogue_revision_row.try_get::<_, i16>(6)? == 1,
            "application catalogue revision did not retain its exact version-two context and hash",
        )?;

        let semantic_revisions = session
            .client()
            .query(
                "SELECT id, function_id, semantic_hash_version
                 FROM _orna_kernel.function_revisions
                 WHERE status = 'active' ORDER BY function_id",
                &[],
            )
            .await?;
        require(
            semantic_revisions.len() == active.function_revisions().len()
                && semantic_revisions.iter().all(|row| {
                    active.function_revisions().iter().any(|revision| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(revision.id().to_bytes().to_vec())
                            && row.try_get::<_, Vec<u8>>(1).ok()
                                == Some(revision.function().to_bytes().to_vec())
                            && row.try_get::<_, i16>(2).ok()
                                == Some(revision.semantic_hash_version().to_u32() as i16)
                    })
                }),
            "standard application function revisions did not retain semantic hash version two",
        )?;
        let expected_current = candidate
            .current_function_revisions()
            .ok_or_else(|| failure("standard application candidate omitted current revisions"))?;
        require(
            same_members(active.function_revisions(), expected_current),
            "recovered current function revisions differ from the prepared candidate",
        )?;

        let source_units = session
            .client()
            .query(
                "SELECT membership.source_unit_id, membership.bundle_id,
                        membership.ordinal, source_unit.logical_path,
                        source_unit.content, source_unit.content_hash
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_units.len() == standard.source().units().len(),
            "standard source-unit row count differs from the verified snapshot",
        )?;
        for unit in standard.source().units() {
            let row = source_units
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(unit.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard source unit is missing"))?;
            require(
                row.try_get::<_, Vec<u8>>(1)? == standard.source().bundle().to_bytes()
                    && row.try_get::<_, i64>(2)? == i64::from(unit.ordinal())
                    && row.try_get::<_, String>(3)? == unit.logical_path()
                    && row.try_get::<_, String>(4)? == unit.content()
                    && row.try_get::<_, Vec<u8>>(5)? == unit.content_hash().to_bytes(),
                "standard source-unit row differs from the verified snapshot",
            )?;
        }

        let schemas = session
            .client()
            .query(
                "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_schemas
                 WHERE standard_library_revision_id = $1 ORDER BY schema_id",
                &[&standard_revision],
            )
            .await?;
        require(
            schemas.len() == standard.catalogue().schemas().len(),
            "standard schema row count differs from the verified snapshot",
        )?;
        for schema in standard.catalogue().schemas() {
            let row = schemas
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(schema.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard schema row is missing"))?;
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::Schema(schema.id()))
                .ok_or_else(|| failure("verified standard schema origin is missing"))?
                .source();
            require(
                row.try_get::<_, Vec<String>>(1)? == schema.name().parts()
                    && row.try_get::<_, Vec<u8>>(2)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(3)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(4)? == i64::from(origin.byte_end()),
                "standard schema row differs from the verified snapshot",
            )?;
        }

        let value_types = session
            .client()
            .query(
                "SELECT type_id, schema_id, name_parts, value_kind, mutability,
                        persistence, representation_contract, source_unit_id,
                        source_start, source_end
                 FROM _orna_kernel.standard_catalogue_value_types
                 WHERE standard_library_revision_id = $1 ORDER BY type_id",
                &[&standard_revision],
            )
            .await?;
        require(
            value_types.len() == standard.catalogue().value_types().len(),
            "standard value-type row count differs from the verified snapshot",
        )?;
        for value_type in standard.catalogue().value_types() {
            let row = value_types
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(value_type.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard value-type row is missing"))?;
            let schema_name = value_type
                .name()
                .parts()
                .get(..value_type.name().parts().len().saturating_sub(1))
                .filter(|parts| !parts.is_empty())
                .ok_or_else(|| failure("standard value type name has no schema part"))?;
            let schema = standard
                .catalogue()
                .schemas()
                .iter()
                .find(|schema| schema.name().parts() == schema_name)
                .ok_or_else(|| failure("standard value type schema is missing"))?;
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::ValueType(value_type.id()))
                .ok_or_else(|| failure("verified standard value-type origin is missing"))?
                .source();
            require(
                row.try_get::<_, Vec<u8>>(1)? == schema.id().to_bytes(),
                "standard value-type schema differs from the verified snapshot",
            )?;
            require(
                row.try_get::<_, Vec<String>>(2)? == value_type.name().parts(),
                "standard value-type name differs from the verified snapshot",
            )?;
            require(
                row.try_get::<_, String>(3)?
                    == match value_type.kind() {
                        ValueTypeKind::Primitive => "primitive",
                        ValueTypeKind::Opaque => "opaque",
                        _ => "unsupported",
                    }
                    && row.try_get::<_, String>(4)?
                        == if matches!(value_type.mutability(), ValueTypeMutability::Immutable) {
                            "immutable"
                        } else {
                            "unsupported"
                        }
                    && row.try_get::<_, String>(5)?
                        == if matches!(value_type.persistence(), ValueTypePersistence::Persistable)
                        {
                            "persistable"
                        } else {
                            "transient"
                        }
                    && row.try_get::<_, String>(6)? == value_type.representation_contract(),
                "standard value-type contract facts differ from the verified snapshot",
            )?;
            require(
                row.try_get::<_, Vec<u8>>(7)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(8)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(9)? == i64::from(origin.byte_end()),
                "standard value-type origin differs from the verified snapshot",
            )?;
        }

        let bindings = session
            .client()
            .query(
                "SELECT type_binding_id, kind, name_parts, target_type_kind,
                        target_type_id, target_enum_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_type_bindings
                 WHERE standard_library_revision_id = $1 ORDER BY type_binding_id",
                &[&standard_revision],
            )
            .await?;
        require(
            bindings.len() == standard.catalogue().type_bindings().len(),
            "standard type-binding row count differs from the verified snapshot",
        )?;
        for binding in standard.catalogue().type_bindings() {
            let row = bindings
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(binding.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard type-binding row is missing"))?;
            let (kind, parts) = match binding.name() {
                TypeLookupName::Qualified(name) => ("qualified", name.parts().to_vec()),
                TypeLookupName::Prelude(name) => ("prelude", name.words().to_vec()),
                _ => return Err(failure("standard type binding has an unknown name shape")),
            };
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::TypeBinding(binding.id()))
                .ok_or_else(|| failure("verified standard type-binding origin is missing"))?
                .source();
            require(
                row.try_get::<_, String>(1)? == kind
                    && row.try_get::<_, Vec<String>>(2)? == parts
                    && row.try_get::<_, String>(3)? == "value"
                    && row.try_get::<_, Option<Vec<u8>>>(4)?
                        == Some(binding.target().to_bytes().to_vec())
                    && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                    && row.try_get::<_, Vec<u8>>(6)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(7)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(8)? == i64::from(origin.byte_end()),
                "standard type-binding row differs from the verified snapshot",
            )?;
        }
        let standard_enum_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.standard_catalogue_enum_types
                 WHERE standard_library_revision_id = $1",
                &[&standard_revision],
            )
            .await?
            .try_get(0)?;
        require(
            standard_enum_count == i64::try_from(standard.catalogue().enum_types().len())?,
            "standard enum rows differ from the verified snapshot",
        )?;

        let field_rows = session
            .client()
            .query(
                "SELECT owner_type_id, field_id, type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1 ORDER BY owner_type_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        require(
            field_rows.len() == 1,
            "standard application fixture must retain one object field row",
        )?;
        require(
            field_rows[0].try_get::<_, Vec<u8>>(0)? == object.id().to_bytes()
                && field_rows[0].try_get::<_, Vec<u8>>(1)? == field.id().to_bytes()
                && field_rows[0].try_get::<_, String>(2)? == "value"
                && field_rows[0].try_get::<_, Option<String>>(3)?.is_none()
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(5)? == Some(boolean_type.clone())
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(6)?
                    == Some(standard_revision.clone()),
            "application field did not persist the exact Boolean value tuple and pin",
        )?;

        let parameter_rows = session
            .client()
            .query(
                "SELECT function_id, parameter_id, ordinal, type_kind, scalar_type,
                        target_type_id, value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1 ORDER BY function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let value_parameters = parameter_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(3).ok().as_deref() == Some("value"))
            .collect::<Vec<_>>();
        let reference_parameters = parameter_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(3).ok().as_deref() == Some("reference"))
            .collect::<Vec<_>>();
        let expected_value_parameters = active
            .catalogue()
            .functions()
            .iter()
            .flat_map(|function| function.parameters())
            .filter(|parameter| parameter.resolved_type().value_type().is_some())
            .count();
        let expected_reference_parameters = active
            .catalogue()
            .functions()
            .iter()
            .flat_map(|function| function.parameters())
            .filter(|parameter| parameter.resolved_type().reference_target().is_some())
            .count();
        require(
            value_parameters.len() == expected_value_parameters
                && reference_parameters.len() == expected_reference_parameters,
            "application parameter rows differ from the active Value and REF families",
        )?;
        for row in value_parameters {
            require(
                row.try_get::<_, Option<String>>(4)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(6)? == Some(boolean_type.clone())
                    && row.try_get::<_, Option<Vec<u8>>>(7)? == Some(standard_revision.clone()),
                "application parameter did not persist the exact Boolean value tuple and pin",
            )?;
        }
        let object_id = object.id().to_bytes().to_vec();
        for row in reference_parameters {
            require(
                row.try_get::<_, Option<String>>(4)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(5)? == Some(object_id.clone())
                    && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(7)?.is_none(),
                "REF parameter did not retain its exact object target and null value pin",
            )?;
        }
        for function in active.catalogue().functions() {
            for parameter in function.parameters() {
                let row = parameter_rows
                    .iter()
                    .find(|row| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(function.id().to_bytes().to_vec())
                            && row.try_get::<_, Vec<u8>>(1).ok()
                                == Some(parameter.id().to_bytes().to_vec())
                            && row.try_get::<_, i64>(2).ok() == Some(i64::from(parameter.ordinal()))
                    })
                    .ok_or_else(|| failure("active function parameter row is missing"))?;
                if let Some(value_type) = parameter.resolved_type().value_type() {
                    require(
                        row.try_get::<_, String>(3)? == "value"
                            && row.try_get::<_, Option<Vec<u8>>>(6)?
                                == Some(value_type.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(7)?
                                == Some(standard_revision.clone()),
                        "active Value parameter does not match its exact durable row",
                    )?;
                } else if let Some(target) = parameter.resolved_type().reference_target() {
                    require(
                        row.try_get::<_, String>(3)? == "reference"
                            && row.try_get::<_, Option<Vec<u8>>>(5)?
                                == Some(target.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                            && row.try_get::<_, Option<Vec<u8>>>(7)?.is_none(),
                        "active REF parameter does not match its exact durable row",
                    )?;
                } else {
                    return Err(failure(
                        "active parameter has an unsupported resolved shape",
                    ));
                }
            }
        }

        let return_rows = session
            .client()
            .query(
                "SELECT function_id, ordinal, type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1 ORDER BY function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let value_return_rows = return_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(2).ok().as_deref() == Some("value"))
            .collect::<Vec<_>>();
        let reference_return_rows = return_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(2).ok().as_deref() == Some("reference"))
            .collect::<Vec<_>>();
        let expected_value_returns = active
            .catalogue()
            .functions()
            .iter()
            .filter_map(|function| match function.return_type() {
                FunctionReturn::Rows(columns) => Some(columns),
                FunctionReturn::Single(_) | FunctionReturn::Stream(_) => None,
            })
            .flatten()
            .filter(|column| column.resolved_type().value_type().is_some())
            .count();
        let expected_reference_returns = active
            .catalogue()
            .functions()
            .iter()
            .filter_map(|function| match function.return_type() {
                FunctionReturn::Rows(columns) => Some(columns),
                FunctionReturn::Single(_) | FunctionReturn::Stream(_) => None,
            })
            .flatten()
            .filter(|column| column.resolved_type().reference_target().is_some())
            .count();
        require(
            value_return_rows.len() == expected_value_returns
                && reference_return_rows.len() == expected_reference_returns,
            "application ROWS return rows differ from the active Value and REF families",
        )?;
        for function in active.catalogue().functions() {
            if let FunctionReturn::Rows(columns) = function.return_type() {
                for column in columns {
                    let row = return_rows
                        .iter()
                        .find(|row| {
                            row.try_get::<_, Vec<u8>>(0).ok()
                                == Some(function.id().to_bytes().to_vec())
                                && row.try_get::<_, i64>(1).ok()
                                    == Some(i64::from(column.ordinal()))
                        })
                        .ok_or_else(|| failure("active ROWS return row is missing"))?;
                    if let Some(value_type) = column.resolved_type().value_type() {
                        require(
                            row.try_get::<_, String>(2)? == "value"
                                && row.try_get::<_, Option<Vec<u8>>>(5)?
                                    == Some(value_type.to_bytes().to_vec())
                                && row.try_get::<_, Option<Vec<u8>>>(6)?
                                    == Some(standard_revision.clone()),
                            "active Value ROWS return does not match its exact durable row",
                        )?;
                    } else if let Some(target) = column.resolved_type().reference_target() {
                        require(
                            row.try_get::<_, String>(2)? == "reference"
                                && row.try_get::<_, Option<Vec<u8>>>(4)?
                                    == Some(target.to_bytes().to_vec())
                                && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                                && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none(),
                            "active REF ROWS return does not match its exact durable row",
                        )?;
                    } else {
                        return Err(failure(
                            "active ROWS return has an unsupported resolved shape",
                        ));
                    }
                }
            }
        }

        let single_rows = session
            .client()
            .query(
                "SELECT function_id, return_type_kind, return_scalar_type,
                        return_target_type_id, return_value_type_id,
                        return_standard_library_revision_id
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1 AND return_shape = 'single'",
                &[&catalogue_id],
            )
            .await?;
        let expected_single_count = active
            .catalogue()
            .functions()
            .iter()
            .filter(|function| matches!(function.return_type(), FunctionReturn::Single(_)))
            .count();
        require(
            single_rows.len() == expected_single_count,
            "application SINGLE return rows differ from the active catalogue",
        )?;
        for function in active.catalogue().functions() {
            if let FunctionReturn::Single(resolved) = function.return_type() {
                let row = single_rows
                    .iter()
                    .find(|row| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(function.id().to_bytes().to_vec())
                    })
                    .ok_or_else(|| failure("active SINGLE return row is missing"))?;
                if let Some(value_type) = resolved.value_type() {
                    require(
                        row.try_get::<_, String>(1)? == "value"
                            && row.try_get::<_, Option<Vec<u8>>>(4)?
                                == Some(value_type.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(5)?
                                == Some(standard_revision.clone()),
                        "active Value SINGLE return does not match its exact durable row",
                    )?;
                } else {
                    return Err(failure(
                        "active SINGLE return has an unsupported resolved shape",
                    ));
                }
            }
        }

        let references = session
            .client()
            .query(
                "SELECT source_function_id, source_function_revision_id, ordinal,
                        target_definition_id, target_standard_library_revision_id
                 FROM _orna_kernel.definition_references
                 WHERE catalogue_revision_id = $1 AND target_kind = 'value_type'
                 ORDER BY source_function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let expected_references = active
            .references()
            .iter()
            .filter_map(|reference| match reference.target() {
                DefinitionReferenceTarget::ValueType(target) => Some((
                    reference.source_function().to_bytes().to_vec(),
                    reference.source_revision().to_bytes().to_vec(),
                    reference.ordinal() as i64,
                    target.to_bytes().to_vec(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        require(
            !expected_references.is_empty(),
            "standard application fixture did not retain a ValueType reference",
        )?;
        require(
            references.len() == expected_references.len(),
            "ValueType reference rows differ from the recovered application evidence",
        )?;
        for expected in expected_references {
            require(
                references.iter().any(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(expected.0.clone())
                        && row.try_get::<_, Vec<u8>>(1).ok() == Some(expected.1.clone())
                        && row.try_get::<_, i64>(2).ok() == Some(expected.2)
                        && row.try_get::<_, Vec<u8>>(3).ok() == Some(expected.3.clone())
                        && row.try_get::<_, Vec<u8>>(4).ok() == Some(standard_revision.clone())
                }),
                "ValueType reference row did not retain its exact target pin",
            )?;
        }
        Ok(())
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "standard upgrade storage inspection",
    )
}

fn require_standard_context(
    active: &ActiveDatabaseRevision,
    expected: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let selected = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("active catalogue did not retain a standard context"))?;
    require(
        selected.revision() == expected.revision()
            && selected.catalogue().revision() == expected.catalogue().revision()
            && selected.source().bundle() == expected.source().bundle()
            && selected.source().id() == expected.source().id()
            && selected.source().bundle_hash() == expected.source().bundle_hash()
            && selected.source().revision_hash() == expected.source().revision_hash()
            && selected.digest() == expected.digest(),
        "active catalogue standard context does not match the verified upgrade snapshot",
    )
}

fn same_recovered(left: &ActiveDatabaseRevision, right: &ActiveDatabaseRevision) -> bool {
    left.pair() == right.pair()
        && left.source() == right.source()
        && left.catalogue().revision() == right.catalogue().revision()
        && left.catalogue().schemas() == right.catalogue().schemas()
        && left.catalogue().object_types() == right.catalogue().object_types()
        && left.catalogue().enum_types() == right.catalogue().enum_types()
        && left.catalogue().record_value_types() == right.catalogue().record_value_types()
        && left.catalogue().functions() == right.catalogue().functions()
        && left.catalogue_hash() == right.catalogue_hash()
        && left.expressions() == right.expressions()
        && same_members(left.origins(), right.origins())
        && left.references() == right.references()
        && left.function_revisions() == right.function_revisions()
        && left.historical_function_revisions() == right.historical_function_revisions()
}

async fn install_failure_point(
    database: &TestDatabase,
    point: FailurePoint,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let statement = match point {
        FailurePoint::SourceBundle => source_bundle_failure_trigger(candidate)?,
        FailurePoint::CatalogueSchema => catalogue_schema_failure_trigger(candidate),
        FailurePoint::FunctionArtifact => function_artifact_failure_trigger(candidate),
        FailurePoint::DefinitionReference => definition_reference_failure_trigger(candidate),
        FailurePoint::DeferredReference => "
            CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred definition reference' USING ERRCODE = 'P0001'; END $$;
            CREATE CONSTRAINT TRIGGER deferred_definition_reference
            AFTER INSERT ON _orna_kernel.definition_references
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();
            CREATE FUNCTION _orna_kernel.test_status_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached status transition' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_status_sentinel BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            EXECUTE FUNCTION _orna_kernel.test_status_sentinel();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::StatusSweep => "
            CREATE FUNCTION _orna_kernel.test_apply_status_invalid() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN NEW.status := 'invalid'; RETURN NEW; END $$;
            CREATE TRIGGER rewrite_active_status BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            WHEN (NEW.status = 'active') EXECUTE FUNCTION _orna_kernel.test_apply_status_invalid();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'status sweep reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER status_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::ActivePointer => fail_trigger("active_revision", "before_active_pointer", "BEFORE", "UPDATE", "before active pointer"),
        FailurePoint::PostPointerRecovery => "
            CREATE FUNCTION _orna_kernel.test_apply_tamper_source() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              UPDATE _orna_kernel.source_revisions
              SET content_hash = decode(repeat('00', 32), 'hex')
              WHERE id = NEW.source_revision_id;
              RETURN NEW;
            END $$;
            CREATE TRIGGER tamper_after_active_pointer AFTER UPDATE
            ON _orna_kernel.active_revision FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_tamper_source();".into(),
        // The production audit schema accepts a valid SourceApply row, so its
        // append cannot be isolated through an existing constraint. This
        // trigger exists only in the Compose test database and fails the
        // append itself, after physical/catalogue changes and before commit.
        FailurePoint::AuditAppend => "
            CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              RAISE EXCEPTION 'source apply audit append' USING ERRCODE = 'P0001';
            END $$;
            CREATE TRIGGER before_source_apply_audit
            BEFORE INSERT ON _orna_kernel.security_audit_events
            FOR EACH ROW WHEN (NEW.event_kind = 'source_apply')
            EXECUTE FUNCTION _orna_kernel.test_apply_fail();".into(),
    };
    session.client().batch_execute(&statement).await?;
    session.shutdown().await
}

fn source_bundle_failure_trigger(candidate: &DeployableRevision) -> TestResult<String> {
    let object = candidate
        .candidate()
        .object_types()
        .first()
        .ok_or_else(|| failure("source-bundle rollback fixture requires an object type"))?;
    let expected_relation = relation(object.id());
    Ok(prerequisite_trigger(
        "source_bundles",
        "before_source_bundle",
        format!("pg_catalog.to_regclass('{expected_relation}') IS NOT NULL"),
        "physical plan missing before source bundle",
        "before source bundle",
    ))
}

fn catalogue_schema_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let source_complete = state.source_complete_condition();
    let catalogue_present = state.catalogue_revision_present_condition();
    prerequisite_trigger(
        "catalogue_schemas",
        "before_catalogue_schema",
        format!("{source_complete} AND {catalogue_present}"),
        "source or catalogue state missing before catalogue schema",
        "before catalogue schema",
    )
}

fn function_artifact_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let semantics_complete = state.semantics_complete_condition();
    let revisions_complete = state.function_revisions_complete_condition();
    prerequisite_trigger(
        "function_artifacts",
        "before_function_artifact",
        format!("{semantics_complete} AND {revisions_complete}"),
        "candidate semantics or revision missing before function artifact",
        "before function artifact",
    )
}

fn definition_reference_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let artifacts_complete = state.artifacts_complete_condition();
    prerequisite_trigger(
        "definition_references",
        "before_definition_reference",
        artifacts_complete,
        "candidate artifact missing before definition reference",
        "before definition reference",
    )
}

fn prerequisite_trigger(
    table: &str,
    trigger: &str,
    prerequisite: String,
    missing_marker: &str,
    expected_marker: &str,
) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT ({prerequisite}) THEN
             RAISE EXCEPTION '{missing_marker}' USING ERRCODE = 'P0001';
           END IF;
           RAISE EXCEPTION '{expected_marker}' USING ERRCODE = 'P0001';
         END $$;
         CREATE TRIGGER {trigger} BEFORE INSERT ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

#[derive(Debug)]
struct CandidateSqlState {
    source_bundle_id: String,
    source_revision_id: String,
    catalogue_revision_id: String,
    source_unit_count: usize,
    schema_count: usize,
    object_type_count: usize,
    expression_count: usize,
    field_count: usize,
    function_count: usize,
    parameter_count: usize,
    return_column_count: usize,
    new_function_revision_ids: Vec<String>,
}

impl CandidateSqlState {
    fn from_candidate(candidate: &DeployableRevision) -> Self {
        let catalogue = candidate.candidate();
        let functions = catalogue.functions();
        Self {
            source_bundle_id: hex_bytes(candidate.source().bundle().to_bytes()),
            source_revision_id: hex_bytes(candidate.source().id().to_bytes()),
            catalogue_revision_id: hex_bytes(catalogue.revision().to_bytes()),
            source_unit_count: candidate.source().units().len(),
            schema_count: catalogue.schemas().len(),
            object_type_count: catalogue.object_types().len(),
            expression_count: candidate.expressions().len(),
            field_count: catalogue
                .object_types()
                .iter()
                .map(|object| object.fields().len())
                .sum(),
            function_count: functions.len(),
            parameter_count: functions
                .iter()
                .map(|function| function.parameters().len())
                .sum(),
            return_column_count: functions
                .iter()
                .map(|function| match function.return_type() {
                    FunctionReturn::Single(_) | FunctionReturn::Stream(_) => 0,
                    FunctionReturn::Rows(columns) => columns.len(),
                })
                .sum(),
            new_function_revision_ids: candidate
                .new_function_revisions()
                .iter()
                .map(|revision| hex_bytes(revision.id().to_bytes()))
                .collect(),
        }
    }

    fn source_complete_condition(&self) -> String {
        let source_bundle = self.source_bundle();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.source_bundles
                      WHERE id = {source_bundle})
             AND (SELECT count(*) FROM _orna_kernel.source_bundle_units
                  WHERE bundle_id = {source_bundle}) = {source_unit_count}
             AND EXISTS (SELECT 1 FROM _orna_kernel.source_revisions
                         WHERE id = {source_revision} AND bundle_id = {source_bundle})",
            source_unit_count = self.source_unit_count,
        )
    }

    fn catalogue_revision_present_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.catalogue_revisions
                      WHERE id = {catalogue_revision}
                        AND source_revision_id = {source_revision})"
        )
    }

    fn semantics_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        format!(
            "{catalogue_present}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_schemas
                  WHERE catalogue_revision_id = {catalogue_revision}) = {schema_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_object_types
                  WHERE catalogue_revision_id = {catalogue_revision}) = {object_type_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_expressions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {expression_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_fields
                  WHERE catalogue_revision_id = {catalogue_revision}) = {field_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_functions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {function_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters
                  WHERE catalogue_revision_id = {catalogue_revision}) = {parameter_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns
                  WHERE catalogue_revision_id = {catalogue_revision}) = {return_column_count}",
            catalogue_present = self.catalogue_revision_present_condition(),
            schema_count = self.schema_count,
            object_type_count = self.object_type_count,
            expression_count = self.expression_count,
            field_count = self.field_count,
            function_count = self.function_count,
            parameter_count = self.parameter_count,
            return_column_count = self.return_column_count,
        )
    }

    fn function_revisions_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1 FROM _orna_kernel.function_revisions
                          WHERE id = {} AND introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn artifacts_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1
                          FROM _orna_kernel.function_artifacts AS artifact
                          JOIN _orna_kernel.function_revisions AS revision
                            ON revision.id = artifact.function_revision_id
                          WHERE revision.id = {}
                            AND revision.introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn source_bundle(&self) -> String {
        bytea(&self.source_bundle_id)
    }

    fn source_revision(&self) -> String {
        bytea(&self.source_revision_id)
    }

    fn catalogue_revision(&self) -> String {
        bytea(&self.catalogue_revision_id)
    }
}

fn conjunction(conditions: impl IntoIterator<Item = String>) -> String {
    let conditions = conditions.into_iter().collect::<Vec<_>>();
    if conditions.is_empty() {
        "TRUE".into()
    } else {
        conditions.join(" AND ")
    }
}

fn bytea(hex: &str) -> String {
    format!("decode('{hex}', 'hex')")
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fail_trigger(table: &str, trigger: &str, timing: &str, event: &str, marker: &str) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION '{marker}' USING ERRCODE = 'P0001'; END $$;
         CREATE TRIGGER {trigger} {timing} {event} ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

fn assert_failure_shape(point: FailurePoint, error: &PostgresKernelError) -> TestResult<()> {
    match point {
        FailurePoint::StatusSweep => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.function_revisions",
                    ..
                }
            ),
            "status rewrite must fail the global status sweep with a function revision invariant",
        ),
        FailurePoint::PostPointerRecovery => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.source_revisions",
                    ..
                }
            ),
            "post-pointer source tampering must fail recovery with a source invariant",
        ),
        _ => match error {
            PostgresKernelError::Database(source) => {
                let marker = source
                    .as_db_error()
                    .map(|detail| detail.message())
                    .unwrap_or("");
                require(
                    source.code().is_some_and(|code| code.code() == "P0001")
                        && marker == trigger_marker(point).expect("trigger point has a marker"),
                    "trigger error did not preserve SQLSTATE P0001 and its exact marker",
                )
            }
            _ => Err(failure(format!(
                "expected PostgreSQL P0001 trigger failure, got {error}"
            ))),
        },
    }
}

fn trigger_marker(point: FailurePoint) -> Option<&'static str> {
    match point {
        FailurePoint::SourceBundle => Some("before source bundle"),
        FailurePoint::CatalogueSchema => Some("before catalogue schema"),
        FailurePoint::FunctionArtifact => Some("before function artifact"),
        FailurePoint::DefinitionReference => Some("before definition reference"),
        FailurePoint::DeferredReference => Some("deferred definition reference"),
        FailurePoint::ActivePointer => Some("before active pointer"),
        FailurePoint::StatusSweep | FailurePoint::PostPointerRecovery => None,
        FailurePoint::AuditAppend => Some("source apply audit append"),
    }
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn named_kernel(database: &TestDatabase, application_name: &str) -> TestResult<PostgresKernel> {
    let mut config = database.config()?;
    config.application_name(application_name);
    Ok(PostgresKernel::new(config))
}

fn standard_context_candidate(expected_base: RevisionPair) -> TestResult<DeployableRevision> {
    let context = CatalogueHashContext::version_two(verified_empty_non_golden_standard()?);
    let bundle = SourceBundleId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x93; 16]),
        Some(expected_base.source()),
        vec![],
        bundle_hash,
        source_revision_record_digest(bundle, Some(expected_base.source()), bundle_hash)?,
    )?;
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([0x94; 16]), vec![], vec![])?;
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;

    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source,
            expected_base.catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        context,
    )?)
}

fn verified_empty_non_golden_standard()
-> TestResult<orna_core::revision::VerifiedStandardLibrarySnapshot> {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([4; 16]),
        0,
        "std/malformed.orna",
        EMPTY_STANDARD_SOURCE,
        source_unit_content_digest(EMPTY_STANDARD_SOURCE)?,
    )?;
    let bundle = SourceBundleId::from_bytes([5; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash)?,
    )?;
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )?;
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        vec![],
        Sha256Digest::from_bytes(EMPTY_STANDARD_DIGEST),
    )?;

    Ok(verify_standard_library_snapshot(snapshot)?)
}

#[derive(Clone, Copy)]
enum InactiveApplicationTypeKind {
    Enum,
    Record,
}

async fn reject_inactive_application_type_collision(
    kind: InactiveApplicationTypeKind,
    requested_type: TypeId,
) -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let discriminator: u8 = match kind {
            InactiveApplicationTypeKind::Enum => 0xe5,
            InactiveApplicationTypeKind::Record => 0xea,
        };
        let bundle = vec![discriminator; 16];
        let unit = vec![discriminator + 1; 16];
        let source = vec![discriminator + 2; 16];
        let catalogue = vec![discriminator + 3; 16];
        let schema = vec![discriminator + 4; 16];
        let content_hash = vec![discriminator; 32];

        let session = database.open().await?;
        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                 VALUES ($1, $2)",
                &[&bundle, &content_hash],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, 0, 'hostile/type.orna', 'hostile', $3)",
                &[&unit, &bundle, &content_hash],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, 0)",
                &[&bundle, &unit],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &source,
                    &version_one.source().id().to_bytes().to_vec(),
                    &bundle,
                    &content_hash,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_revisions
                    (id, source_revision_id, parent_catalogue_revision_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &catalogue,
                    &source,
                    &version_one.catalogue().revision().to_bytes().to_vec(),
                    &content_hash,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_schemas
                    (catalogue_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, ARRAY['hostile'], $3, 0, 1)",
                &[&catalogue, &schema, &unit],
            )
            .await?;
        let table = match kind {
            InactiveApplicationTypeKind::Enum => "catalogue_enum_types",
            InactiveApplicationTypeKind::Record => "catalogue_record_value_types",
        };
        match kind {
            InactiveApplicationTypeKind::Enum => {
                session
                    .client()
                    .execute(
                        "INSERT INTO _orna_kernel.catalogue_enum_types
                            (catalogue_revision_id, type_id, schema_id, name_parts, labels,
                             source_unit_id, source_start, source_end)
                         VALUES ($1, $2, $3, ARRAY['hostile', 'enum'], ARRAY['x'], $4, 0, 1)",
                        &[
                            &catalogue,
                            &requested_type.to_bytes().to_vec(),
                            &schema,
                            &unit,
                        ],
                    )
                    .await?;
            }
            InactiveApplicationTypeKind::Record => {
                session
                    .client()
                    .execute(
                        "INSERT INTO _orna_kernel.catalogue_record_value_types
                            (catalogue_revision_id, type_id, schema_id, name_parts,
                             value_kind, mutability, persistence,
                             source_unit_id, source_start, source_end)
                         VALUES ($1, $2, $3, ARRAY['hostile', 'record'],
                                 'record', 'immutable', 'persistable', $4, 0, 1)",
                        &[
                            &catalogue,
                            &requested_type.to_bytes().to_vec(),
                            &schema,
                            &unit,
                        ],
                    )
                    .await?;
            }
        }
        session.client().batch_execute("COMMIT").await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive application type collision unexpectedly allowed the standard upgrade",
        )?;
        require(
            error.to_string()
                == "the database contains an identity reserved for the standard library"
                && std::error::Error::source(&error).is_none(),
            "inactive application type collision changed its exact source-free error contract",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::Type(type_id),
            } => require(
                type_id == requested_type,
                "inactive application collision returned the wrong type identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive application type collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let preserved: i64 = session
            .client()
            .query_one(
                &format!(
                    "SELECT count(*) FROM _orna_kernel.{table}
                     WHERE catalogue_revision_id = $1 AND type_id = $2"
                ),
                &[&catalogue, &requested_type.to_bytes().to_vec()],
            )
            .await?
            .try_get(0)?;
        require(
            preserved == 1,
            "rejected application type collision changed its hostile durable row",
        )?;
        session.shutdown().await
    })
    .await
}

#[cfg(feature = "test-hooks")]
fn verified_standard_enum_fixture()
-> TestResult<orna_core::revision::VerifiedStandardLibrarySnapshot> {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xc1; 16]),
        0,
        "std/enum.orna",
        "standard enum fixture",
        source_unit_content_digest("standard enum fixture")?,
    )?;
    let bundle = SourceBundleId::from_bytes([0xc2; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0xc3; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash)?,
    )?;
    let schema = SchemaDefinition::new(
        orna_core::SchemaId::from_bytes([0xc4; 16]),
        QualifiedSemanticName::new(["std"])?,
    );
    let enum_type = EnumTypeDefinition::new(
        TypeId::from_bytes([0xc5; 16]),
        QualifiedSemanticName::new(["std", "mode"])?,
        vec!["one".to_owned(), "two".to_owned()],
    );
    let binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "mode_alias"])?,
        enum_type.id(),
    )?;
    let source_unit = SourceUnitId::from_bytes([0xc1; 16]);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            SourceOrigin::new(source_unit, 0, 1)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(enum_type.id()),
            SourceOrigin::new(source_unit, 1, 2)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding.id()),
            SourceOrigin::new(source_unit, 2, 3)?,
        ),
    ];
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0xc6; 16]),
        vec![schema],
        vec![],
        vec![],
        vec![enum_type],
        vec![binding],
    )?;
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0xc7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes(FROZEN_STANDARD_ENUM_DIGEST),
    )?;
    Ok(verify_standard_library_snapshot(snapshot)?)
}

#[cfg(feature = "test-hooks")]
fn standard_enum_record_candidate(
    active: &ActiveDatabaseRevision,
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let content = "standard enum record fixture";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xd1; 16]),
        0,
        "app/status.orna",
        content,
        source_unit_content_digest(content)?,
    )?;
    let bundle = SourceBundleId::from_bytes([0xd2; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0xd3; 16]),
        Some(active.pair().source()),
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)?,
    )?;
    let schema = SchemaDefinition::new(
        orna_core::SchemaId::from_bytes([0xd4; 16]),
        QualifiedSemanticName::new(["app"])?,
    );
    let enum_type = standard
        .catalogue()
        .enum_types()
        .first()
        .ok_or_else(|| failure("standard enum candidate has no pinned enum"))?;
    let record_type = RecordValueTypeDefinition::new(
        TypeId::from_bytes([0xd5; 16]),
        QualifiedSemanticName::new(["app", "status"])?,
        vec![RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0xd6; 16]),
            "mode",
            0,
            TypeDescriptor::named(enum_type.id()),
        )?],
    );
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0xd7; 16]),
        vec![schema.clone()],
        vec![],
        vec![],
        vec![],
        vec![record_type.clone()],
        vec![],
    )?;
    let origin = SourceOrigin::new(
        SourceUnitId::from_bytes([0xd1; 16]),
        0,
        u32::try_from(content.len())?,
    )?;
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), origin),
        DefinitionOrigin::new(DefinitionIdentity::ValueType(record_type.id()), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: record_type.id(),
                field: record_type.fields()[0].id(),
            },
            origin,
        ),
    ];
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[])?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(origins, vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        context,
    )?)
}

fn failed_apply_error(
    result: Result<ActiveDatabaseRevision, PostgresKernelError>,
    success_message: &'static str,
) -> TestResult<PostgresKernelError> {
    result.err().ok_or_else(|| failure(success_message))
}

fn candidate(source: &str, active: &ActiveDatabaseRevision) -> TestResult<DeployableRevision> {
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check(&bundle, active.catalogue());
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "compiler diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare(&report, active.pair(), active)?)
}

fn standard_application_candidate(
    source: &str,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )
    .map_err(|error| failure(format!("standard application context failed: {error}")))?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check_standard_application(&bundle, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "standard application diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
}

fn only_revision(active: &ActiveDatabaseRevision) -> TestResult<&FunctionRevisionRecord> {
    active
        .function_revisions()
        .first()
        .filter(|_| active.function_revisions().len() == 1)
        .ok_or_else(|| failure("expected exactly one active function revision"))
}

fn require_rename_semantics(
    candidate: &DeployableRevision,
    object: TypeId,
    field: FieldId,
    function: orna_core::FunctionId,
    revision: orna_core::FunctionRevisionId,
) -> TestResult<()> {
    let renamed_object = candidate
        .candidate()
        .object_type_by_id(object)
        .ok_or_else(|| failure("renamed candidate lost the original object TypeId"))?;
    let renamed_field = renamed_object
        .field_by_id(field)
        .ok_or_else(|| failure("renamed candidate lost the original FieldId"))?;
    require(
        renamed_field.name() == "primary_email"
            && candidate.candidate().functions().iter().any(|definition| {
                definition.id() == function && definition.current_revision() == revision
            }),
        "renamed candidate changed stable semantic identities",
    )?;

    let source_unit = candidate.source().units()[0].id();
    let declaration_origin = token_origin(
        source_unit,
        FIELD_RENAME_FINAL_SOURCE,
        "CREATE TYPE people.person AS OBJECT (",
        "primary_email TEXT NOT NULL",
    )?;
    let query_origin = token_origin(
        source_unit,
        FIELD_RENAME_FINAL_SOURCE,
        "AS SELECT p.",
        "primary_email",
    )?;
    let field_origin = candidate
        .origins()
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Field {
                    owner: object,
                    field,
                }
        })
        .ok_or_else(|| failure("renamed field definition origin is absent"))?;
    require(
        field_origin.source() == declaration_origin,
        "renamed field origin does not select the final CREATE TYPE token",
    )?;
    let query_reference = candidate
        .references()
        .iter()
        .find(|reference| {
            reference.source_function() == function
                && reference.kind() == DefinitionReferenceKind::QueryField
                && reference.target()
                    == DefinitionReferenceTarget::Field {
                        owner: object,
                        field,
                    }
        })
        .ok_or_else(|| failure("renamed QueryField reference is absent"))?;
    require(
        query_reference.source_revision() == revision
            && query_reference.source_origin() == query_origin,
        "renamed QueryField reference changed identity or token origin",
    )
}

fn token_origin(
    source_unit: orna_core::SourceUnitId,
    source: &str,
    anchor: &str,
    token: &str,
) -> TestResult<SourceOrigin> {
    let anchor_start = source
        .find(anchor)
        .ok_or_else(|| failure("source-origin anchor is absent"))?;
    let token_start = source[anchor_start + anchor.len()..]
        .find(token)
        .map(|offset| anchor_start + anchor.len() + offset)
        .ok_or_else(|| failure("source-origin token is absent after its anchor"))?;
    let token_end = token_start + token.len();
    Ok(SourceOrigin::new(
        source_unit,
        u32::try_from(token_start)?,
        u32::try_from(token_end)?,
    )?)
}

struct RenameProof {
    object: TypeId,
    field: FieldId,
    function: orna_core::FunctionId,
    revision: FunctionRevisionRecord,
    immutable: ImmutableRevisionRows,
    physical: PhysicalCatalogue,
    stored_object: ObjectId,
}

struct UniqueReferenceProof {
    person: TypeId,
    assignment: TypeId,
    field: FieldId,
    person_physical: PhysicalCatalogue,
    assignment_physical: PhysicalCatalogue,
}

struct UniqueTextProof {
    object: TypeId,
    nullable_field: FieldId,
    required_field: FieldId,
    physical: PhysicalCatalogue,
}

async fn require_unique_reference_state(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    proof: &UniqueReferenceProof,
    field_name: &str,
) -> TestResult<()> {
    let person = active
        .catalogue()
        .object_type_by_id(proof.person)
        .ok_or_else(|| failure("required unique reference recovery lost the person TypeId"))?;
    let assignment = active
        .catalogue()
        .object_type_by_id(proof.assignment)
        .ok_or_else(|| failure("required unique reference recovery lost the assignment TypeId"))?;
    let reference = assignment
        .field_by_id(proof.field)
        .ok_or_else(|| failure("required unique reference recovery lost the FieldId"))?;
    require(
        person.name().parts() == ["assignments", "person"]
            && assignment.name().parts() == ["assignments", "assignment"]
            && assignment.fields().len() == 1
            && reference.name() == field_name
            && reference.is_required_unique_reference()
            && reference.resolved_type() == ResolvedType::reference(proof.person),
        "required unique reference recovery changed semantic identity or field properties",
    )?;
    require(
        physical_catalogue(database, proof.person).await? == proof.person_physical
            && physical_catalogue(database, proof.assignment).await? == proof.assignment_physical,
        "required unique reference replay or rename changed physical identities",
    )
}

async fn require_unique_text_state(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    proof: &UniqueTextProof,
    nullable_name: &str,
    required_name: &str,
) -> TestResult<()> {
    let account = active
        .catalogue()
        .object_type_by_id(proof.object)
        .ok_or_else(|| failure("unique Text recovery lost the account TypeId"))?;
    let nullable = account
        .field_by_id(proof.nullable_field)
        .ok_or_else(|| failure("unique Text recovery lost the nullable FieldId"))?;
    let required = account
        .field_by_id(proof.required_field)
        .ok_or_else(|| failure("unique Text recovery lost the required FieldId"))?;
    require(
        account.name().parts() == ["accounts", "account"]
            && account.fields().len() == 2
            && nullable.name() == nullable_name
            && nullable.nullable()
            && nullable.unique()
            && nullable.resolved_type()
                == ResolvedType::scalar(StandardScalar::CharacterLargeObject)
            && required.name() == required_name
            && !required.nullable()
            && required.unique()
            && required.resolved_type()
                == ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        "unique Text replay or rename changed semantic identity or field properties",
    )?;
    require_unique_text_physical_shape(database, proof.object, proof.nullable_field).await?;
    require_unique_text_physical_shape(database, proof.object, proof.required_field).await?;
    require(
        physical_catalogue(database, proof.object).await? == proof.physical,
        "unique Text replay or rename changed stable-ID PostgreSQL physical identities",
    )
}

async fn require_unique_text_physical_shape(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let relation_name = relation(object);
        let field_name = field(field_id);
        let field_hex = format!("{:032x}", u128::from_be_bytes(field_id.to_bytes()));
        let constraint_name = format!("uq_{field_hex}");
        let row = session
            .client()
            .query_one(
                "SELECT\n\
                    a.attcollation = 'pg_catalog.\"C\"'::regcollation AS column_uses_c,\n\
                    count(con.oid) = 1\n\
                        AND bool_and(con.contype = 'u' AND con.conkey = ARRAY[a.attnum]::smallint[])\n\
                        AND bool_and(i.indisunique AND NOT i.indnullsnotdistinct)\n\
                        AND bool_and(i.indnkeyatts = 1 AND i.indnatts = 1)\n\
                        AND bool_and(i.indcollation[0] = 'pg_catalog.\"C\"'::regcollation::oid)\n\
                        AS stable_c_unique_constraint\n\
                 FROM pg_attribute a\n\
                 LEFT JOIN pg_constraint con\n\
                   ON con.conrelid = a.attrelid AND con.conname = $3\n\
                 LEFT JOIN pg_index i ON i.indexrelid = con.conindid\n\
                 WHERE a.attrelid = to_regclass($1) AND a.attname = $2\n\
                 GROUP BY a.attcollation, a.attnum",
                &[&relation_name, &field_name, &constraint_name],
            )
            .await?;
        let column_uses_c: bool = row.try_get(0)?;
        let stable_c_unique_constraint: bool = row.try_get(1)?;
        require(
            column_uses_c && stable_c_unique_constraint,
            "unique Text field did not use the required C-collated stable one-column NULLS DISTINCT constraint",
        )
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "unique Text physical inspection",
    )
}

async fn tamper_unique_text_column_collation(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = session
        .client()
        .batch_execute(&format!(
            "ALTER TABLE {relation} DROP CONSTRAINT {constraint};\n\
             ALTER TABLE {relation}\n\
               ALTER COLUMN {field} TYPE text COLLATE pg_catalog.\"default\" USING {field}::text;\n\
             ALTER TABLE {relation} ADD CONSTRAINT {constraint} UNIQUE ({field});",
            relation = relation(object),
            field = field(field_id),
            constraint = format!("uq_{:032x}", u128::from_be_bytes(field_id.to_bytes())),
        ))
        .await
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>);
    finish_test_session(
        operation,
        session.shutdown().await,
        "unique Text column collation tamper",
    )
}

async fn require_rename_state(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    proof: &RenameProof,
) -> TestResult<()> {
    let current_object = active
        .catalogue()
        .object_type_by_id(proof.object)
        .ok_or_else(|| failure("recovered rename lost the original TypeId"))?;
    let current_field = current_object
        .field_by_id(proof.field)
        .ok_or_else(|| failure("recovered rename lost the original FieldId"))?;
    require(
        current_field.name() == "primary_email"
            && only_revision(active)? == &proof.revision
            && active.catalogue().functions()[0].current_revision() == proof.revision.id()
            && active.historical_function_revisions().is_empty(),
        "recovered rename changed identity, immutable revision, or history",
    )?;
    require(
        immutable_rows(database, &proof.revision).await? == proof.immutable,
        "field rename rewrote the immutable function revision or artifact",
    )?;
    require(
        physical_catalogue(database, proof.object).await? == proof.physical,
        "field rename changed the stable-ID PostgreSQL physical catalogue",
    )?;
    require_private_text(
        database,
        proof.object,
        proof.field,
        proof.stored_object,
        "kept@example.test",
    )
    .await?;
    let result = kernel(database)?
        .execute_server_select(proof.function)
        .await?;
    require(
        result.rows().rows().len() == 1
            && result.rows().rows()[0].values()
                == [RuntimeValue::Text(String::from("kept@example.test"))],
        "renamed SERVER SELECT did not read the pre-existing private field value",
    )
}

#[derive(Debug, Eq, PartialEq)]
struct PhysicalCatalogue {
    relation: (i64, i64, String, String),
    attributes: Vec<(i16, String, i64, i32, bool, bool)>,
    constraints: Vec<(i64, String, String)>,
    indexes: Vec<(i64, String, String)>,
}

async fn physical_catalogue(
    database: &TestDatabase,
    object: TypeId,
) -> TestResult<PhysicalCatalogue> {
    let session = database.open().await?;
    let operation = async {
        let relation_name = relation(object);
        let relation_row = session
            .client()
            .query_one(
                "SELECT c.oid::bigint, c.relfilenode::bigint, c.relkind::text,
                        n.nspname || '.' || c.relname
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.oid = to_regclass($1)",
                &[&relation_name],
            )
            .await?;
        let attributes = session
            .client()
            .query(
                "SELECT a.attnum, a.attname, a.atttypid::bigint, a.atttypmod,
                        a.attnotnull, a.attisdropped
                 FROM pg_attribute a
                 WHERE a.attrelid = to_regclass($1) AND a.attnum > 0
                 ORDER BY a.attnum",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get(0)?,
                    row.try_get(1)?,
                    row.try_get(2)?,
                    row.try_get(3)?,
                    row.try_get(4)?,
                    row.try_get(5)?,
                ))
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        let constraints = session
            .client()
            .query(
                "SELECT oid::bigint, conname, pg_get_constraintdef(oid, false)
                 FROM pg_constraint WHERE conrelid = to_regclass($1) ORDER BY oid",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        let indexes = session
            .client()
            .query(
                "SELECT i.indexrelid::bigint, c.relname, pg_get_indexdef(i.indexrelid)
                 FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
                 WHERE i.indrelid = to_regclass($1) ORDER BY i.indexrelid",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        Ok(PhysicalCatalogue {
            relation: (
                relation_row.try_get(0)?,
                relation_row.try_get(1)?,
                relation_row.try_get(2)?,
                relation_row.try_get(3)?,
            ),
            attributes,
            constraints,
            indexes,
        })
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "physical catalogue inspection",
    )
}

async fn insert_private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    value: &str,
) -> TestResult<()> {
    private_text(
        database,
        object,
        field_id,
        object_id,
        PrivateTextOperation::Insert(value),
    )
    .await
}

async fn require_private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    expected: &str,
) -> TestResult<()> {
    private_text(
        database,
        object,
        field_id,
        object_id,
        PrivateTextOperation::Require(expected),
    )
    .await
}

enum PrivateTextOperation<'a> {
    Insert(&'a str),
    Require(&'a str),
}

async fn private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    operation: PrivateTextOperation<'_>,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        match operation {
            PrivateTextOperation::Insert(value) => {
                session
                    .client()
                    .execute(
                        &format!(
                            "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
                            relation(object),
                            field(field_id)
                        ),
                        &[&object_id.to_bytes().to_vec(), &value],
                    )
                    .await?;
                Ok(())
            }
            PrivateTextOperation::Require(expected) => {
                let row = session
                    .client()
                    .query_one(
                        &format!(
                            "SELECT {} FROM {} WHERE _orna_object_id = $1",
                            field(field_id),
                            relation(object)
                        ),
                        &[&object_id.to_bytes().to_vec()],
                    )
                    .await?;
                let actual: String = row.try_get(0)?;
                require(
                    actual == expected,
                    "pre-existing private field value did not survive the rename",
                )
            }
        }
    }
    .await;
    finish_test_session(operation, session.shutdown().await, "private row operation")
}

fn finish_test_session<T>(
    operation: TestResult<T>,
    shutdown: TestResult<()>,
    context: &str,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(shutdown)) => Err(failure(format!(
            "{context} failed: {operation}; session shutdown also failed: {shutdown}"
        ))),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ImmutableRevisionRows {
    revision_count: i64,
    revision_xmin: String,
    introduced_catalogue_revision_id: Vec<u8>,
    function_id: Vec<u8>,
    revision_number: i64,
    declaration_hash: Vec<u8>,
    semantic_hash: Vec<u8>,
    hash_algorithm: String,
    language_version: String,
    status: String,
    hash_contract_version: i16,
    artifact_count: i64,
    artifact_xmin: String,
    artifact_function_revision_id: Vec<u8>,
    artifact_kind: String,
    artifact_format: String,
    artifact_version: i32,
    artifact_payload: Vec<u8>,
    artifact_hash: Vec<u8>,
    artifact_hash_algorithm: String,
    artifact_hash_contract_version: i16,
}

async fn immutable_rows(
    database: &TestDatabase,
    revision: &FunctionRevisionRecord,
) -> TestResult<ImmutableRevisionRows> {
    let session = database.open().await?;
    let operation = async {
        let revision_id = revision.id().to_bytes().to_vec();
        let revision_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&revision_id],
            )
            .await?
            .try_get(0)?;
        let revision_row = session
            .client()
            .query_one(
                "SELECT xmin::text, introduced_catalogue_revision_id, function_id,
                    revision_number, content_hash, semantic_ir_hash, hash_algorithm,
                    language_version, status, hash_contract_version
             FROM _orna_kernel.function_revisions
             WHERE id = $1",
                &[&revision_id],
            )
            .await?;
        let artifact_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?
            .try_get(0)?;
        let artifact_row = session
            .client()
            .query_one(
                "SELECT xmin::text, function_revision_id, artifact_kind, format, format_version,
                    payload, content_hash, hash_algorithm, hash_contract_version
             FROM _orna_kernel.function_artifacts
             WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?;
        Ok(ImmutableRevisionRows {
            revision_count,
            revision_xmin: revision_row.try_get(0)?,
            introduced_catalogue_revision_id: revision_row.try_get(1)?,
            function_id: revision_row.try_get(2)?,
            revision_number: revision_row.try_get(3)?,
            declaration_hash: revision_row.try_get(4)?,
            semantic_hash: revision_row.try_get(5)?,
            hash_algorithm: revision_row.try_get(6)?,
            language_version: revision_row.try_get(7)?,
            status: revision_row.try_get(8)?,
            hash_contract_version: revision_row.try_get(9)?,
            artifact_count,
            artifact_xmin: artifact_row.try_get(0)?,
            artifact_function_revision_id: artifact_row.try_get(1)?,
            artifact_kind: artifact_row.try_get(2)?,
            artifact_format: artifact_row.try_get(3)?,
            artifact_version: artifact_row.try_get(4)?,
            artifact_payload: artifact_row.try_get(5)?,
            artifact_hash: artifact_row.try_get(6)?,
            artifact_hash_algorithm: artifact_row.try_get(7)?,
            artifact_hash_contract_version: artifact_row.try_get(8)?,
        })
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "immutable row inspection",
    )
}

async fn require_no_candidate_residue(
    database: &TestDatabase,
    candidate: &DeployableRevision,
    base: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let source_bundle = candidate.source().bundle().to_bytes().to_vec();
    let source_revision = candidate.source().id().to_bytes().to_vec();
    let catalogue = candidate.candidate().revision().to_bytes().to_vec();
    let source_bundle_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_bundles WHERE id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_membership_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_bundle_units WHERE bundle_id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_revision_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&source_revision],
        )
        .await?
        .try_get(0)?;
    let catalogue_and_semantic_rows: i64 = session
        .client()
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions WHERE id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_schemas WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_object_types WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_expressions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_fields WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_functions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.definition_references WHERE catalogue_revision_id = $1)",
            &[&catalogue],
        )
        .await?
        .try_get(0)?;
    let authority_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
             WHERE catalogue_revision_id = $1",
            &[&catalogue],
        )
        .await?
        .try_get(0)?;
    let mut immutable_rows = 0_i64;
    for revision in candidate.new_function_revisions() {
        let revision_id = revision.id().to_bytes().to_vec();
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
    }
    let names = candidate
        .candidate()
        .object_types()
        .iter()
        .filter(|object| base.catalogue().object_type_by_id(object.id()).is_none())
        .map(|object| relation(object.id()))
        .collect::<Vec<_>>();
    let physical_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*)
             FROM unnest($1::text[]) AS expected(name)
             WHERE to_regclass(expected.name) IS NOT NULL",
            &[&names],
        )
        .await?
        .try_get(0)?;
    session.shutdown().await?;
    require(
        authority_rows == 0,
        "losing apply left invocation_target_authorities candidate residue",
    )?;
    require(
        source_bundle_rows == 0
            && source_membership_rows == 0
            && source_revision_rows == 0
            && catalogue_and_semantic_rows == 0
            && immutable_rows == 0
            && physical_rows == 0,
        "losing apply left source, catalogue, semantic, immutable, artifact, or physical residue",
    )
}

fn require_recovered_new_candidate(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require_recovered_snapshot(candidate, active)?;
    require(
        same_members(
            active.function_revisions(),
            candidate.new_function_revisions(),
        ),
        "recovered candidate current function revisions differ",
    )?;
    require(
        active.historical_function_revisions().is_empty(),
        "new candidate apply unexpectedly recovered function history",
    )
}

fn require_recovered_snapshot(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require(
        active.pair() == candidate.candidate_pair(),
        "recovered candidate pair differs",
    )?;
    require(
        active.source() == candidate.source(),
        "recovered candidate source differs",
    )?;
    // Recovery uses durable record order, while preparation uses candidate
    // snapshot order. Stable identities and embedded ordinals carry equality.
    require(
        active.catalogue().revision() == candidate.candidate().revision()
            && same_members(
                active.catalogue().schemas(),
                candidate.candidate().schemas(),
            )
            && same_members(
                active.catalogue().object_types(),
                candidate.candidate().object_types(),
            )
            && same_members(
                active.catalogue().enum_types(),
                candidate.candidate().enum_types(),
            )
            && same_members(
                active.catalogue().record_value_types(),
                candidate.candidate().record_value_types(),
            )
            && same_members(
                active.catalogue().functions(),
                candidate.candidate().functions(),
            ),
        "recovered candidate catalogue differs",
    )?;
    require(
        active.catalogue_hash() == candidate.catalogue_hash(),
        "recovered candidate catalogue hash differs",
    )?;
    require(
        same_members(active.expressions(), candidate.expressions()),
        "recovered candidate expressions differ",
    )?;
    require(
        same_members(active.origins(), candidate.origins()),
        "recovered candidate origins differ",
    )?;
    require(
        same_members(active.references(), candidate.references()),
        "recovered candidate references differ",
    )?;
    Ok(())
}

fn same_members<T>(left: &[T], right: &[T]) -> bool
where
    T: Eq,
{
    if left.len() != right.len() {
        return false;
    }
    let mut unmatched = right.iter().collect::<Vec<_>>();
    for member in left {
        let Some(index) = unmatched.iter().position(|candidate| *candidate == member) else {
            return false;
        };
        unmatched.swap_remove(index);
    }
    unmatched.is_empty()
}

fn relation(type_id: TypeId) -> String {
    format!(
        "_orna_data.t_{:032x}",
        u128::from_be_bytes(type_id.to_bytes())
    )
}

fn field(field_id: FieldId) -> String {
    format!("f_{:032x}", u128::from_be_bytes(field_id.to_bytes()))
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

#[cfg(feature = "test-hooks")]
const NULLABLE_ADDITION_INITIAL_SOURCE: &str = "CREATE SCHEMA nullable_addition;\n\
    CREATE TYPE nullable_addition.entry AS OBJECT (stored BOOL NOT NULL);\n";

/// Builds the complete final source for one appended nullable scalar field.
#[cfg(feature = "test-hooks")]
fn nullable_addition_final_source(spelling: &str) -> String {
    format!(
        "CREATE SCHEMA nullable_addition;\n\
         CREATE TYPE nullable_addition.entry AS OBJECT (\n\
             stored BOOL NOT NULL,\n\
             added {spelling}\n\
         );\n"
    )
}

/// The native runtime value stored in one appended nullable scalar column.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Debug, PartialEq)]
enum AddedScalarValue {
    Boolean(bool),
    Integer(i32),
    BigInt(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

/// One entry in the exact six-scalar appended-field proof matrix.
#[cfg(feature = "test-hooks")]
struct NullableScalarCase {
    /// Exact source spelling of the appended field, for example `TEXT`.
    spelling: &'static str,
    /// Exact pinned standard value TypeId carried by `ResolvedType::Value`.
    value_type: TypeId,
    /// Exact PostgreSQL physical column type installed by the lowered plan.
    physical_type: &'static str,
    /// Native typed value that must round-trip through the appended column.
    explicit: AddedScalarValue,
}

/// The scalar family of one appended nullable column, used to decode rows.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddedScalarVariant {
    Boolean,
    Integer,
    BigInt,
    Float,
    Text,
    Bytes,
}

#[cfg(feature = "test-hooks")]
impl AddedScalarValue {
    fn variant(&self) -> AddedScalarVariant {
        match self {
            Self::Boolean(_) => AddedScalarVariant::Boolean,
            Self::Integer(_) => AddedScalarVariant::Integer,
            Self::BigInt(_) => AddedScalarVariant::BigInt,
            Self::Float(_) => AddedScalarVariant::Float,
            Self::Text(_) => AddedScalarVariant::Text,
            Self::Bytes(_) => AddedScalarVariant::Bytes,
        }
    }

    /// Returns this value as a PostgreSQL-bound reference.
    fn sql_value(&self) -> &(dyn tokio_postgres::types::ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Integer(value) => value,
            Self::BigInt(value) => value,
            Self::Float(value) => value,
            Self::Text(value) => value,
            Self::Bytes(value) => value,
        }
    }
}

#[cfg(feature = "test-hooks")]
impl AddedScalarVariant {
    /// Reads the appended column as the exact typed optional value.
    fn read_from(
        &self,
        row: &tokio_postgres::Row,
        index: usize,
    ) -> TestResult<Option<AddedScalarValue>> {
        let value = match self {
            Self::Boolean => row
                .try_get::<_, Option<bool>>(index)?
                .map(AddedScalarValue::Boolean),
            Self::Integer => row
                .try_get::<_, Option<i32>>(index)?
                .map(AddedScalarValue::Integer),
            Self::BigInt => row
                .try_get::<_, Option<i64>>(index)?
                .map(AddedScalarValue::BigInt),
            Self::Float => row
                .try_get::<_, Option<f64>>(index)?
                .map(AddedScalarValue::Float),
            Self::Text => row
                .try_get::<_, Option<String>>(index)?
                .map(AddedScalarValue::Text),
            Self::Bytes => row
                .try_get::<_, Option<Vec<u8>>>(index)?
                .map(AddedScalarValue::Bytes),
        };
        Ok(value)
    }
}

/// One live verified-standard-v2 journey for appending one nullable standard
/// scalar field to an existing object that already has live rows.
///
/// The journey establishes a pinned standard context through the normal
/// kernel apply path, applies the initial `nullable_addition` source, captures
/// the stable object and stored field identities, seeds one pre-transition
/// row, compiles the final source, proves through the public candidate
/// catalogue that the object and stored identities are retained and that the
/// appended field is an exact nullable `ResolvedType::Value` with a distinct
/// FieldId, then applies the final candidate and requires the recovered
/// snapshot. After the transition two more rows are seeded: one that omits
/// the new column and one that writes the explicit native value. The
/// three-row state is queried by exact ObjectId: the seeded row keeps stored
/// true with a NULL added column, the omitted row keeps stored false with
/// NULL added, and the explicit row stores true and the exact explicit value.
/// The appended column must also carry the exact nullable physical type. A
/// fresh kernel recovers the same stable identities. Replaying the exact
/// final source against the recovered revision plans no duplicate column and
/// reapplies cleanly, and a second fresh kernel recovers the replayed
/// snapshot with the same three rows.
#[cfg(feature = "test-hooks")]
async fn applies_one_appended_nullable_scalar_field(
    database: &TestDatabase,
    case: &NullableScalarCase,
) -> TestResult<()> {
    let kernel_instance = kernel(database)?;
    kernel_instance.bootstrap().await?;
    let empty = kernel_instance.recover().await?;
    let version_one = kernel_instance
        .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
        .await?;
    let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
        .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
    let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
    let initial =
        standard_application_candidate(NULLABLE_ADDITION_INITIAL_SOURCE, &version_two, &upgrade)?;
    let applied_initial = kernel_instance.apply(&initial).await?;
    require_recovered_snapshot(&initial, &applied_initial)?;

    let object_type = applied_initial
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["nullable_addition", "entry"])
        .ok_or_else(|| {
            failure("nullable_addition.entry is absent from the applied initial catalogue")
        })?
        .id();
    let stored_field = applied_initial
        .catalogue()
        .object_type_by_id(object_type)
        .ok_or_else(|| {
            failure("nullable_addition.entry is absent from the applied initial catalogue")
        })?
        .field_by_name("stored")
        .ok_or_else(|| failure("nullable_addition.entry.stored is absent"))?
        .id();
    let seeded_object = ObjectId::from_bytes([0xab; 16]);

    // Seed one row before the transition through a direct test SQL session.
    // This is test substrate only: the private physical relation and
    // identity-derived column names are the documented lower-level PostgreSQL
    // seam.
    seed_nullable_addition_row(
        database,
        object_type,
        stored_field,
        None,
        seeded_object,
        true,
        None,
    )
    .await?;

    // Compile the final source and prove through the public candidate
    // catalogue that the object and stored identities are retained and that
    // the appended field is one exact nullable ResolvedType::Value with a
    // distinct FieldId.
    let final_source = nullable_addition_final_source(case.spelling);
    let final_candidate =
        standard_application_candidate(&final_source, &applied_initial, &upgrade)?;
    let final_object = final_candidate
        .candidate()
        .object_type_by_id(object_type)
        .ok_or_else(|| failure("the final candidate lost the object TypeId"))?;
    let retained_stored = final_object
        .field_by_id(stored_field)
        .ok_or_else(|| failure("the final candidate lost the stored FieldId"))?;
    require(
        retained_stored.name() == "stored"
            && retained_stored.ordinal() == 0
            && retained_stored.resolved_type()
                == ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID)
            && !retained_stored.nullable(),
        "the final candidate must retain the exact stored value identity",
    )?;
    let added_field = final_object
        .field_by_name("added")
        .ok_or_else(|| failure("the final candidate is missing the added field"))?;
    require(
        added_field.id() != stored_field
            && added_field.ordinal() == 1
            && added_field.resolved_type() == ResolvedType::value(case.value_type)
            && added_field.nullable(),
        "the final candidate must append one exact nullable ResolvedType::Value field",
    )?;

    // Apply the final candidate and require the recovered snapshot.
    let applied_final = kernel_instance.apply(&final_candidate).await?;
    require_recovered_snapshot(&final_candidate, &applied_final)?;
    require_added_physical_column_type(database, object_type, added_field.id(), case.physical_type)
        .await?;

    // Two more fixed nonzero rows through bounded direct test SQL: one that
    // omits the appended column and one that writes it explicitly.
    let omitted_object = ObjectId::from_bytes([0xac; 16]);
    let explicit_object = ObjectId::from_bytes([0xad; 16]);
    seed_nullable_addition_row(
        database,
        object_type,
        stored_field,
        None,
        omitted_object,
        false,
        None,
    )
    .await?;
    seed_nullable_addition_row(
        database,
        object_type,
        stored_field,
        Some(added_field.id()),
        explicit_object,
        true,
        Some(&case.explicit),
    )
    .await?;
    let expected_rows = [
        (seeded_object, true, None),
        (omitted_object, false, None),
        (explicit_object, true, Some(case.explicit.clone())),
    ];
    require_nullable_addition_rows(
        database,
        object_type,
        stored_field,
        added_field.id(),
        case.explicit.variant(),
        &expected_rows,
    )
    .await?;

    // A fresh kernel recovers the same stable object, stored, and added
    // identities.
    let restarted = kernel(database)?.recover().await?;
    require_recovered_snapshot(&final_candidate, &restarted)?;
    let recovered_object = restarted
        .catalogue()
        .object_type_by_id(object_type)
        .ok_or_else(|| failure("the recovered catalogue lost the object TypeId"))?;
    let recovered_stored = recovered_object
        .field_by_id(stored_field)
        .ok_or_else(|| failure("the recovered catalogue lost the stored FieldId"))?;
    let recovered_added = recovered_object
        .field_by_id(added_field.id())
        .ok_or_else(|| failure("the recovered catalogue lost the added FieldId"))?;
    require(
        recovered_stored.name() == "stored" && recovered_added.name() == "added",
        "the recovered catalogue must retain the stable stored and added identities",
    )?;

    // Replay the exact final source against the recovered revision. Exact
    // replay may advance the immutable revision pair; the applied and
    // recovered snapshot must equal the replay candidate instead. The replay
    // candidate must retain the same object, stored, and added identities so
    // applying it plans no duplicate column, which the successful apply and
    // the unchanged row checks prove.
    let replay_candidate = standard_application_candidate(&final_source, &restarted, &upgrade)?;
    let replay_object = replay_candidate
        .candidate()
        .object_type_by_id(object_type)
        .ok_or_else(|| failure("the replay candidate lost the object TypeId"))?;
    let replay_stored = replay_object
        .field_by_id(stored_field)
        .ok_or_else(|| failure("the replay candidate lost the stored FieldId"))?;
    let replay_added = replay_object
        .field_by_id(added_field.id())
        .ok_or_else(|| failure("the replay candidate lost the added FieldId"))?;
    require(
        replay_stored.name() == "stored"
            && replay_added.name() == "added"
            && replay_added.id() == added_field.id(),
        "the replay candidate must retain the exact stored and added identities",
    )?;
    let replayed = kernel_instance.apply(&replay_candidate).await?;
    require_recovered_snapshot(&replay_candidate, &replayed)?;
    require_nullable_addition_rows(
        database,
        object_type,
        stored_field,
        added_field.id(),
        case.explicit.variant(),
        &expected_rows,
    )
    .await?;

    // One more fresh kernel recovers the exact replayed snapshot and the same
    // three private rows.
    let replayed_recovery = kernel(database)?.recover().await?;
    require_recovered_snapshot(&replay_candidate, &replayed_recovery)?;
    let replayed_recovered_object = replayed_recovery
        .catalogue()
        .object_type_by_id(object_type)
        .ok_or_else(|| failure("the replayed recovery lost the object TypeId"))?;
    let replayed_recovered_added = replayed_recovered_object
        .field_by_id(added_field.id())
        .ok_or_else(|| failure("the replayed recovery lost the added FieldId"))?;
    require(
        replayed_recovered_added.name() == "added",
        "the replayed recovery must retain the added identity",
    )?;
    require_nullable_addition_rows(
        database,
        object_type,
        stored_field,
        added_field.id(),
        case.explicit.variant(),
        &expected_rows,
    )
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_boolean_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "BOOL",
                value_type: orna_standard::BOOLEAN_TYPE_ID,
                physical_type: "boolean",
                explicit: AddedScalarValue::Boolean(true),
            },
        )
        .await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_integer_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "INTEGER",
                value_type: orna_standard::INTEGER_TYPE_ID,
                physical_type: "integer",
                explicit: AddedScalarValue::Integer(-7),
            },
        )
        .await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_bigint_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "BIGINT",
                value_type: orna_standard::BIGINT_TYPE_ID,
                physical_type: "bigint",
                explicit: AddedScalarValue::BigInt(9_007_199_254_740_991),
            },
        )
        .await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_float_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "FLOAT",
                value_type: orna_standard::FLOAT_TYPE_ID,
                physical_type: "double precision",
                explicit: AddedScalarValue::Float(1.5),
            },
        )
        .await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_text_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "TEXT",
                value_type: orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                physical_type: "text",
                explicit: AddedScalarValue::Text("scalar-text".to_owned()),
            },
        )
        .await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_one_appended_nullable_bytes_field_to_live_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        applies_one_appended_nullable_scalar_field(
            &database,
            &NullableScalarCase {
                spelling: "BYTES",
                value_type: orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
                physical_type: "bytea",
                explicit: AddedScalarValue::Bytes(vec![0, 1, 255]),
            },
        )
        .await
    })
    .await
}

/// Seeds one private nullable-addition row through a bounded direct test SQL
/// session. The appended column is written only when both the added field and
/// a typed value are supplied; otherwise the row omits it and stays NULL.
#[cfg(feature = "test-hooks")]
async fn seed_nullable_addition_row(
    database: &TestDatabase,
    object_type: TypeId,
    stored_field: FieldId,
    added_field: Option<FieldId>,
    object: ObjectId,
    stored: bool,
    added: Option<&AddedScalarValue>,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let sql = if let Some(added_field) = added_field {
            format!(
                "INSERT INTO {} (_orna_object_id, {}, {}) VALUES ($1, $2, $3)",
                relation(object_type),
                field(stored_field),
                field(added_field)
            )
        } else {
            format!(
                "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
                relation(object_type),
                field(stored_field)
            )
        };
        let result = if let Some(added) = added {
            session
                .client()
                .execute(
                    &sql,
                    &[&object.to_bytes().to_vec(), &stored, added.sql_value()],
                )
                .await
        } else {
            session
                .client()
                .execute(&sql, &[&object.to_bytes().to_vec(), &stored])
                .await
        };
        result?;
        Ok(())
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "seed a nullable-addition row",
    )
}

/// Requires the exact ordered private row state of the nullable-addition
/// relation, deterministically ordered by object identity. The appended
/// column is decoded through the exact scalar variant.
#[cfg(feature = "test-hooks")]
async fn require_nullable_addition_rows(
    database: &TestDatabase,
    object_type: TypeId,
    stored_field: FieldId,
    added_field: FieldId,
    added_variant: AddedScalarVariant,
    expected: &[(ObjectId, bool, Option<AddedScalarValue>)],
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let rows = session
            .client()
            .query(
                &format!(
                    "SELECT _orna_object_id, {}, {} FROM {} ORDER BY _orna_object_id",
                    field(stored_field),
                    field(added_field),
                    relation(object_type)
                ),
                &[],
            )
            .await?;
        if rows.len() != expected.len() {
            return Err(failure(format!(
                "expected exactly {} private rows, got {}",
                expected.len(),
                rows.len()
            )));
        }
        for (row, expected) in rows.iter().zip(expected) {
            let object: Vec<u8> = row.try_get(0)?;
            let stored: bool = row.try_get(1)?;
            let added = added_variant.read_from(row, 2)?;
            require(
                object == expected.0.to_bytes().to_vec()
                    && stored == expected.1
                    && added == expected.2,
                "private row identity or values differ from the expected state",
            )?;
        }
        Ok(())
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "require nullable-addition rows",
    )
}

/// Requires the exact nullable PostgreSQL physical type of the appended
/// identity-derived column.
#[cfg(feature = "test-hooks")]
async fn require_added_physical_column_type(
    database: &TestDatabase,
    object: TypeId,
    added_field: FieldId,
    expected: &str,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                        attribute.attnotnull
                 FROM pg_catalog.pg_attribute AS attribute
                 WHERE attribute.attrelid = pg_catalog.to_regclass($1)
                   AND attribute.attname = $2 AND NOT attribute.attisdropped",
                &[&relation(object), &field(added_field)],
            )
            .await?;
        let actual: String = row.try_get(0)?;
        let not_null: bool = row.try_get(1)?;
        require(
            actual == expected && !not_null,
            "the appended column must have the exact nullable physical type",
        )
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "appended column physical type inspection",
    )
}

/// The single live verified-standard-v2 rollback tracer for a failed
/// appended-field apply.
///
/// The tracer applies the initial `nullable_addition` source under a pinned
/// standard context, seeds one fixed row through a direct test SQL session,
/// captures the durable baseline, compiles the final source, and installs the
/// existing `FailurePoint::CatalogueSchema` trigger, which fires after
/// physical installation and before semantic persistence. The triggered apply
/// must fail with the exact failure shape, and baseline recovery must causally
/// catch any leaked extra physical column. The original row is then queried
/// through only the pre-transition stored column and must still be true.
#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn failed_appended_nullable_scalar_apply_rolls_back_column_and_live_row() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let empty = kernel_instance.recover().await?;
        let version_one = kernel_instance
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
        let initial = standard_application_candidate(
            NULLABLE_ADDITION_INITIAL_SOURCE,
            &version_two,
            &upgrade,
        )?;
        let applied_initial = kernel_instance.apply(&initial).await?;
        require_recovered_snapshot(&initial, &applied_initial)?;

        let object_type = applied_initial
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["nullable_addition", "entry"])
            .ok_or_else(|| {
                failure("nullable_addition.entry is absent from the applied initial catalogue")
            })?
            .id();
        let stored_field = applied_initial
            .catalogue()
            .object_type_by_id(object_type)
            .ok_or_else(|| {
                failure("nullable_addition.entry is absent from the applied initial catalogue")
            })?
            .field_by_name("stored")
            .ok_or_else(|| failure("nullable_addition.entry.stored is absent"))?
            .id();
        let seeded_object = ObjectId::from_bytes([0xac; 16]);
        seed_nullable_addition_row(
            &database,
            object_type,
            stored_field,
            None,
            seeded_object,
            true,
            None,
        )
        .await?;

        let final_source = nullable_addition_final_source("BOOL");
        let final_candidate =
            standard_application_candidate(&final_source, &applied_initial, &upgrade)?;
        let baseline = baseline(&database, &applied_initial).await?;
        install_failure_point(&database, FailurePoint::CatalogueSchema, &final_candidate).await?;

        let error = kernel_instance
            .apply(&final_candidate)
            .await
            .expect_err("the triggered final apply must fail");
        assert_failure_shape(FailurePoint::CatalogueSchema, &error)?;
        require_baseline(&database, &baseline, &kernel_instance).await?;

        // The original row survives through only the pre-transition column.
        let session = database.open().await?;
        let operation = async {
            let row = session
                .client()
                .query_one(
                    &format!(
                        "SELECT {} FROM {} WHERE _orna_object_id = $1",
                        field(stored_field),
                        relation(object_type)
                    ),
                    &[&seeded_object.to_bytes().to_vec()],
                )
                .await?;
            let stored: bool = row.try_get(0)?;
            require(
                stored,
                "the pre-transition stored value must survive the failed apply",
            )
        }
        .await;

        finish_test_session(
            operation,
            session.shutdown().await,
            "query the pre-transition row",
        )
    })
    .await
}

// The version-2 standard source constants below are the exact retained
// `std/types.orna` and `std/invoke.orna` shapes from the compiler reconcile
// fixtures. The fixture uses the same fixed identities, source, catalogue,
// executable, and origins, so its canonical digest is the compiled
// STANDARD_V2_CANONICAL_DIGEST golden.
#[cfg(feature = "test-hooks")]
const STD_INVOKE_SOURCE: &str = "CREATE SCHEMA std.invoke;\n\
    CREATE SERVER FUNCTION std.invoke.echo(\n\
    \x20   p_value INTEGER\n\
    )\n\
    RETURNS INTEGER\n\
    SECURITY INVOKER\n\
    TRANSACTION READ ONLY\n\
    VOLATILITY STABLE\n\
    AS\n\
    \x20   SELECT p_value;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_TYPES_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;\
    CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT \
    'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;\
    EXPORT TYPE std.types.INTEGER AS std.INTEGER;\
    EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_CANONICAL_DIGEST: [u8; 32] = [
    115, 202, 159, 209, 255, 174, 218, 69, 195, 114, 168, 108, 210, 7, 50, 127, 176, 149, 134, 145,
    229, 113, 139, 179, 237, 228, 75, 75, 94, 20, 52, 52,
];

/// The complete V2 standard fixture with the exact compiler-reconcile
/// identities and the compiled canonical digest golden. Its source revision
/// parent must exist as a durable source revision before the upgrade applies.
#[cfg(feature = "test-hooks")]
fn verified_standard_v2_fixture() -> TestResult<orna_core::revision::VerifiedStandardLibrarySnapshot>
{
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
        source_unit_content_digest(STANDARD_V2_TYPES_SOURCE)?,
    )?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        STD_INVOKE_SOURCE,
        source_unit_content_digest(STD_INVOKE_SOURCE)?,
    )?;
    let units = vec![types_unit, invoke_unit];
    let bundle = SourceBundleId::from_bytes([0x41; 16]);
    let bundle_hash = source_bundle_digest(&units)?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x42; 16]),
        Some(SourceRevisionId::from_bytes([0x43; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            bundle,
            Some(SourceRevisionId::from_bytes([0x43; 16])),
            bundle_hash,
        )?,
    )?;

    let integer = ValueTypeDefinition::primitive(
        STD_INTEGER_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "integer"])?,
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"])?,
        integer.id(),
    )?;
    let prelude = TypeBinding::prelude(PreludeTypeName::new(["integer"])?, integer.id())?;
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "invoke", "echo"])?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_INVOKE_ECHO_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"])?,
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"])?,
            ),
            SchemaDefinition::new(
                STD_INVOKE_SCHEMA_ID,
                QualifiedSemanticName::new(["std", "invoke"])?,
            ),
        ],
        vec![],
        vec![integer],
        vec![qualified, prelude],
        vec![echo],
    )?;

    let origins = standard_v2_origins(&catalogue, STD_INVOKE_SOURCE)?;
    let executable = standard_v2_executable(&catalogue, &origins)?;

    let provisional = StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes([0x44; 16]),
        StandardLibraryDigestVersion::Version2,
        source,
        "orna.language/1",
        catalogue,
        vec![executable],
        origins,
        Sha256Digest::from_bytes(STANDARD_V2_CANONICAL_DIGEST),
    )?;
    Ok(verify_standard_library_v2_snapshot(provisional)?)
}

/// Builds the exact origin sequence for both retained V2 source units. The
/// byte ranges match the parsed declaration spans of the compiler fixture.
#[cfg(feature = "test-hooks")]
fn standard_v2_origins(
    catalogue: &CatalogueSnapshot,
    invoke_source: &str,
) -> TestResult<Vec<DefinitionOrigin>> {
    let mut origins = Vec::new();
    let types = STANDARD_V2_TYPES_SOURCE;
    let schema_std_end = "CREATE SCHEMA std;".len();
    let schema_types_end = schema_std_end + "CREATE SCHEMA std.types;".len();
    let type_declaration = "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;";
    let type_start = types
        .find("CREATE TYPE")
        .ok_or_else(|| failure("missing type"))?;
    let type_end = type_start + type_declaration.len();
    let qualified_declaration = "EXPORT TYPE std.types.INTEGER AS std.INTEGER;";
    let qualified_start = types
        .find("EXPORT TYPE std.types.INTEGER AS std.INTEGER")
        .ok_or_else(|| failure("missing qualified binding"))?;
    let qualified_end = qualified_start + qualified_declaration.len();
    let prelude_declaration = "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
    let prelude_start = types
        .find("EXPORT TYPE std.INTEGER TO PRELUDE")
        .ok_or_else(|| failure("missing prelude binding"))?;
    let prelude_end = prelude_start + prelude_declaration.len();
    let types_unit = STD_TYPES_SOURCE_UNIT_ID;
    let qualified_binding = catalogue
        .type_bindings()
        .first()
        .ok_or_else(|| failure("missing qualified binding"))?;
    let prelude_binding = catalogue
        .type_bindings()
        .last()
        .ok_or_else(|| failure("missing prelude binding"))?;
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        SourceOrigin::new(types_unit, 0, u32::try_from(schema_std_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
        SourceOrigin::new(
            types_unit,
            u32::try_from(schema_std_end)?,
            u32::try_from(schema_types_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(STD_INTEGER_TYPE_ID),
        SourceOrigin::new(
            types_unit,
            u32::try_from(type_start)?,
            u32::try_from(type_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(qualified_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(qualified_start)?,
            u32::try_from(qualified_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(prelude_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(prelude_start)?,
            u32::try_from(prelude_end)?,
        )?,
    ));

    let function_start = invoke_source
        .find("CREATE SERVER FUNCTION")
        .ok_or_else(|| failure("missing function declaration"))?;
    let function_end = invoke_source.len();
    let parameter_start = invoke_source
        .find("p_value")
        .ok_or_else(|| failure("missing parameter declaration"))?;
    let parameter_end = parameter_start + "p_value INTEGER".len();
    let schema_end = "CREATE SCHEMA std.invoke;".len();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, u32::try_from(schema_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(function_start)?,
            u32::try_from(function_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        },
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(parameter_start)?,
            u32::try_from(parameter_end)?,
        )?,
    ));
    Ok(origins)
}

/// Builds the exact V2 executable: the immutable echo revision, the 44-byte
/// server parameter-echo artifact, and the three ordered references.
#[cfg(feature = "test-hooks")]
fn standard_v2_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> TestResult<StandardExecutable> {
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or_else(|| failure("missing echo function"))?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or_else(|| failure("missing echo function origin"))?
        .source();
    let declaration_content_hash = function_declaration_digest(
        &STD_INVOKE_SOURCE.as_bytes()
            [function_origin.byte_start() as usize..function_origin.byte_end() as usize],
    )?;
    let payload =
        ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)?.encode()?;
    let content_hash = artifact_payload_digest(&payload)?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-parameter-echo",
        1,
        payload,
        content_hash,
    )?;
    let parameter_integer_start = STD_INVOKE_SOURCE
        .find("INTEGER")
        .ok_or_else(|| failure("missing parameter type"))? as u32;
    let result_integer_start = STD_INVOKE_SOURCE
        .rfind("INTEGER")
        .ok_or_else(|| failure("missing result type"))? as u32;
    let body_p_value_start = STD_INVOKE_SOURCE
        .rfind("p_value")
        .ok_or_else(|| failure("missing body identifier"))? as u32;
    let integer_origin = |start: u32| -> TestResult<SourceOrigin> {
        Ok(SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            start,
            start + 7,
        )?)
    };
    let references = vec![
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            0,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(parameter_integer_start)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(result_integer_start)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            integer_origin(body_p_value_start)?,
        ),
    ];
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        "orna.language/1",
        &artifact,
        &[],
        &references,
    )?;
    let revision = FunctionRevisionRecord::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic,
        "orna.language/1",
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    Ok(StandardExecutable::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        revision,
        references,
    )?)
}

/// Installs the durable source-revision parent of the fixture V2 standard
/// source before the upgrade applies.
#[cfg(feature = "test-hooks")]
async fn install_standard_v2_parent_revision(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let bundle = vec![0x99_u8; 16];
    let parent = vec![0x43_u8; 16];
    let content_hash = vec![0x98_u8; 32];
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', 1)",
            &[&bundle, &content_hash],
        )
        .await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, NULL, $2, $3, 'sha256', 1)",
            &[&parent, &bundle, &content_hash],
        )
        .await?;
    session.shutdown().await
}

/// The companion application revision for one V2 standard upgrade. It is a
/// complete new source and catalogue revision whose hash context pins the
/// supplied verified standard snapshot.
#[cfg(feature = "test-hooks")]
fn standard_v2_application_candidate(
    active: &ActiveDatabaseRevision,
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let context = CatalogueHashContext::version_two(standard.clone());
    let bundle = SourceBundleId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x93; 16]),
        Some(active.pair().source()),
        vec![],
        bundle_hash,
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)?,
    )?;
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([0x94; 16]), vec![], vec![])?;
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        context,
    )?)
}

/// Inserts one inactive application catalogue function with its complete
/// revision chain and an optional protected invocation-audit row.
#[cfg(feature = "test-hooks")]
async fn insert_inactive_application_function(
    database: &TestDatabase,
    discriminator: u8,
    function_id: &[u8],
    revision_id: &[u8],
    name_parts: &[&str],
    audit_event: bool,
) -> TestResult<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    let session = database.open().await?;
    let bundle = vec![discriminator; 16];
    let unit = vec![discriminator + 1; 16];
    let source = vec![discriminator + 2; 16];
    let catalogue = vec![discriminator + 3; 16];
    let schema = vec![discriminator + 4; 16];
    let content_hash = vec![discriminator; 32];
    let principal = vec![discriminator + 5; 16];
    let client = session.client();
    client.batch_execute("BEGIN").await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
             VALUES ($1, $2)",
            &[&bundle, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_units
                (id, bundle_id, ordinal, logical_path, content, content_hash)
             VALUES ($1, $2, 0, 'hostile/func.orna', 'hostile', $3)",
            &[&unit, &bundle, &content_hash],
        )
        .await?;
    let has_source_bundle_units: bool = client
        .query_one(
            "SELECT to_regclass('_orna_kernel.source_bundle_units') IS NOT NULL",
            &[],
        )
        .await?
        .get(0);
    if has_source_bundle_units {
        client
            .execute(
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, 0)",
                &[&bundle, &unit],
            )
            .await?;
    }
    client
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash)
             VALUES ($1, NULL, $2, $3)",
            &[&source, &bundle, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash)
             VALUES ($1, $2, NULL, $3)",
            &[&catalogue, &source, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, ARRAY['hostile'], $3, 0, 1)",
            &[&catalogue, &schema, &unit],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, hash_algorithm, language_version, status)
             VALUES ($1, $2, $3, 1, $4, $4, 'sha256', 'orna.language/1', 'active')",
            &[
                &revision_id.to_vec(),
                &catalogue,
                &function_id.to_vec(),
                &content_hash,
            ],
        )
        .await?;
    let artifact_payload =
        ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)?.encode()?;
    let artifact_hash = artifact_payload_digest(&artifact_payload)?;
    client
        .execute(
            "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, 'server_plan', 'orna.server-parameter-echo', 1, $2, $3, 'sha256', 1)",
            &[
                &revision_id.to_vec(),
                &artifact_payload,
                &artifact_hash.to_bytes().to_vec(),
            ],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                 security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_target_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, 'server', 'invoker', 'read_only', 'stable', 'rows',
                     NULL, NULL, NULL, $5, $6, 0, 1)",
            &[
                &catalogue,
                &function_id.to_vec(),
                &schema,
                &name_parts
                    .iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>(),
                &revision_id.to_vec(),
                &unit,
            ],
        )
        .await?;
    if audit_event {
        let security_event = vec![discriminator + 6; 16];
        let invocation_event = vec![discriminator + 7; 16];
        let invocation_id = vec![discriminator + 8; 16];
        client
            .execute(
                "INSERT INTO _orna_kernel.security_audit_events
                    (event_id, event_kind, outcome, session_principal_id,
                     effective_principal_id, authorising_principal_id, function_id,
                     source_revision_id, catalogue_revision_id, denial_reason)
                 VALUES ($1, 'execute', 'allowed', $2, $2, $2, $3, $4, $5, NULL)",
                &[
                    &security_event,
                    &principal,
                    &function_id.to_vec(),
                    &source,
                    &catalogue,
                ],
            )
            .await?;
        client
            .execute(
                "INSERT INTO _orna_kernel.invocation_audit_events
                    (event_id, invocation_id, outcome, session_principal_id,
                     effective_principal_id, authorising_principal_id, function_id,
                     source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES ($1, $2, 'allowed', $3, $3, $3, $4, $5, $6, $7)",
                &[
                    &invocation_event,
                    &invocation_id,
                    &principal,
                    &function_id.to_vec(),
                    &source,
                    &catalogue,
                    &security_event,
                ],
            )
            .await?;
    }
    client.batch_execute("COMMIT").await?;
    session.shutdown().await?;
    Ok((
        catalogue,
        schema,
        function_id.to_vec(),
        revision_id.to_vec(),
    ))
}

/// Runs the migration SQL files whose numeric prefixes are in `versions`.
#[cfg(feature = "test-hooks")]
async fn run_migration_files(database: &TestDatabase, versions: &[u32]) -> TestResult<()> {
    let session = database.open().await?;
    session
        .client()
        .batch_execute("CREATE SCHEMA IF NOT EXISTS _orna_kernel; REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;")
        .await?;
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut paths = std::fs::read_dir(format!("{manifest_dir}/migrations"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("migration file name"))?;
        let Some(version) = name.get(0..4).and_then(|prefix| prefix.parse::<u32>().ok()) else {
            continue;
        };
        if !versions.contains(&version) {
            continue;
        }
        let sql = std::fs::read_to_string(&path)?;
        session.client().batch_execute(&sql).await?;
    }
    session.shutdown().await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn persists_the_v2_standard_snapshot_and_authority_atomically() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        install_standard_v2_parent_revision(&database)
            .await
            .map_err(|error| failure(format!("parent revision step: {error}")))?;
        let active = kernel_instance
            .recover()
            .await
            .map_err(|error| failure(format!("recover step: {error}")))?;
        let standard = verified_standard_v2_fixture()
            .map_err(|error| failure(format!("fixture step: {error}")))?;
        let candidate = standard_v2_application_candidate(&active, &standard)
            .map_err(|error| failure(format!("candidate step: {error}")))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await
            .map_err(|error| failure(format!("apply step: {error}")))?;
        require_standard_context(&applied, &standard)
            .map_err(|error| failure(format!("context step: {error}")))?;
        require_recovered_snapshot(&candidate, &applied)?;
        let reopened = kernel_instance.recover().await?;
        require_standard_context(&reopened, &standard)?;

        let session = database.open().await?;
        let client = session.client();
        let standard_revision = standard.revision().to_bytes().to_vec();
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let function_id = STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec();
        let revision_id = STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes().to_vec();

        let header = client
            .query_one(
                "SELECT digest_version FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&standard_revision],
            )
            .await?;
        require(
            header.try_get::<_, i16>(0)? == 2,
            "standard header must record digest version 2",
        )?;

        let units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            units.len() == 2
                && units[0].try_get::<_, i64>(0)? == 0
                && units[0].try_get::<_, String>(1)? == "std/types.orna"
                && units[1].try_get::<_, i64>(0)? == 1
                && units[1].try_get::<_, String>(1)? == "std/invoke.orna",
            "standard source units must persist both ordinals and paths",
        )?;

        let function = client
            .query_one(
                "SELECT name_parts, domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind, return_scalar_type,
                        current_function_revision_id, source_unit_id
                 FROM _orna_kernel.standard_catalogue_functions
                 WHERE standard_library_revision_id = $1 AND function_id = $2",
                &[&standard_revision, &function_id],
            )
            .await?;
        require(
            function.try_get::<_, Vec<String>>(0)?
                == vec!["std".to_owned(), "invoke".to_owned(), "echo".to_owned()]
                && function.try_get::<_, String>(1)? == "server"
                && function.try_get::<_, String>(2)? == "invoker"
                && function.try_get::<_, Option<String>>(3)? == Some("read_only".to_owned())
                && function.try_get::<_, String>(4)? == "stable"
                && function.try_get::<_, String>(5)? == "single"
                && function.try_get::<_, Option<String>>(6)? == Some("scalar".to_owned())
                && function.try_get::<_, Option<String>>(7)? == Some("integer".to_owned())
                && function.try_get::<_, Vec<u8>>(8)? == revision_id
                && function.try_get::<_, Vec<u8>>(9)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard catalogue function row must retain the exact resolved signature",
        )?;

        let parameter = client
            .query_one(
                "SELECT name, ordinal, type_kind, scalar_type, source_unit_id
                 FROM _orna_kernel.standard_catalogue_function_parameters
                 WHERE standard_library_revision_id = $1 AND function_id = $2 AND parameter_id = $3",
                &[&standard_revision, &function_id, &STD_INVOKE_ECHO_PARAMETER_ID.to_bytes().to_vec()],
            )
            .await?;
        require(
            parameter.try_get::<_, String>(0)? == "p_value"
                && parameter.try_get::<_, i64>(1)? == 0
                && parameter.try_get::<_, String>(2)? == "scalar"
                && parameter.try_get::<_, Option<String>>(3)? == Some("integer".to_owned())
                && parameter.try_get::<_, Vec<u8>>(4)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard parameter row must retain the exact ordered signature",
        )?;

        let revision_row = client
            .query_one(
                "SELECT function_id, revision_number, semantic_hash_version, language_version,
                        declaration_source_unit_id
                 FROM _orna_kernel.standard_function_revisions
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            revision_row.try_get::<_, Vec<u8>>(0)? == function_id
                && revision_row.try_get::<_, i64>(1)? == 1
                && revision_row.try_get::<_, i16>(2)? == 2
                && revision_row.try_get::<_, String>(3)? == "orna.language/1"
                && revision_row.try_get::<_, Vec<u8>>(4)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard function revision row must retain the immutable revision facts",
        )?;

        let artifact = client
            .query_one(
                "SELECT artifact_kind, format, format_version, octet_length(payload)
                 FROM _orna_kernel.standard_function_artifacts
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            artifact.try_get::<_, String>(0)? == "server_plan"
                && artifact.try_get::<_, String>(1)? == "orna.server-parameter-echo"
                && artifact.try_get::<_, i32>(2)? == 1
                && artifact.try_get::<_, i32>(3)? == 44,
            "standard artifact row must retain the exact 44-byte parameter-echo artifact",
        )?;

        let references = client
            .query(
                "SELECT ordinal, target_kind, reference_kind, target_standard_library_revision_id
                 FROM _orna_kernel.standard_definition_references
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2
                 ORDER BY ordinal",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            references.len() == 3,
            "standard reference rows must retain the exact ordered reference sequence",
        )?;
        for (index, reference) in references.iter().enumerate() {
            let i = i64::try_from(index)?;
            let pin = reference.try_get::<_, Option<Vec<u8>>>(3)?;
            require(
                reference.try_get::<_, i64>(0)? == i,
                "standard reference ordinal must be contiguous",
            )?;
            if index == 2 {
                require(
                    reference.try_get::<_, String>(1)? == "parameter"
                        && reference.try_get::<_, String>(2)? == "parameter_read"
                        && pin.is_none(),
                    "standard parameter reference must retain its scoped target",
                )?;
            } else {
                require(
                    reference.try_get::<_, String>(1)? == "value_type"
                        && reference.try_get::<_, String>(2)? == "named_type"
                        && pin == Some(standard_revision.clone()),
                    "standard value reference must pin the selected standard revision",
                )?;
            }
        }

        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&catalogue_revision, &function_id],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "standard"
                && authority.try_get::<_, Vec<u8>>(1)? == revision_id
                && authority.try_get::<_, Vec<u8>>(2)? == standard_revision,
            "the standard authority row must pin the exact executable and standard revisions",
        )?;

        let application_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND target_class = 'application'",
                &[&catalogue_revision],
            )
            .await?
            .try_get(0)?;
        require(
            application_rows == 0,
            "an empty companion revision must not write application authority rows",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn v1_standard_upgrade_writes_no_executable_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_UPGRADE_V1_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let applied = kernel.apply_standard_upgrade(&upgrade).await?;
        require_standard_context(&applied, upgrade.verified_standard_snapshot())?;

        let session = database.open().await?;
        let client = session.client();
        for table in [
            "standard_catalogue_functions",
            "standard_catalogue_function_parameters",
            "standard_function_revisions",
            "standard_function_artifacts",
            "standard_definition_references",
        ] {
            let count: i64 = client
                .query_one(&format!("SELECT count(*) FROM _orna_kernel.{table}"), &[])
                .await?
                .try_get(0)?;
            require(
                count == 0,
                "a version-one standard install must not write executable rows",
            )?;
        }
        let standard_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE target_class = 'standard'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            standard_rows == 0,
            "a version-one standard install must not write standard authority rows",
        )?;
        let expected_application_rows = applied.catalogue().functions().len() as i64;
        let application_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE target_class = 'application' AND catalogue_revision_id = $1",
                &[&applied.pair().catalogue().to_bytes().to_vec()],
            )
            .await?
            .try_get(0)?;
        require(
            application_rows == expected_application_rows,
            "the companion application revision must retain one authority row per function",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn v2_upgrade_rejects_duplicate_authority_row() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        install_standard_v2_parent_revision(&database)
            .await
            .map_err(|error| failure(format!("parent revision step: {error}")))?;
        let active = kernel_instance
            .recover()
            .await
            .map_err(|error| failure(format!("recover step: {error}")))?;
        let standard = verified_standard_v2_fixture()
            .map_err(|error| failure(format!("fixture step: {error}")))?;
        let candidate = standard_v2_application_candidate(&active, &standard)
            .map_err(|error| failure(format!("candidate step: {error}")))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await
            .map_err(|error| failure(format!("apply step: {error}")))?;
        require_standard_context(&applied, &standard)
            .map_err(|error| failure(format!("context step: {error}")))?;

        let session = database.open().await?;
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let duplicate = session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'standard', $3, $4)",
                &[
                    &catalogue_revision,
                    &STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec(),
                    &STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes().to_vec(),
                    &standard.revision().to_bytes().to_vec(),
                ],
            )
            .await;
        require(
            duplicate.is_err(),
            "a duplicate standard authority row must be rejected by the primary key",
        )?;
        let authority_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND target_class = 'standard'",
                &[&catalogue_revision],
            )
            .await?
            .try_get(0)?;
        require(
            authority_rows == 1,
            "the standard authority row must exist exactly once",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn migration_twenty_three_backfills_and_replaces_the_audit_target_fk() -> TestResult<()> {
    with_test_database(|database| async move {
        run_migration_files(&database, &(1..=22).collect::<Vec<_>>()).await?;
        let function_id = vec![0xc6; 16];
        let revision_id = vec![0xc7; 16];
        let (catalogue, _, _, _) = insert_inactive_application_function(
            &database,
            0xc6,
            &function_id,
            &revision_id,
            &["hostile", "audited"],
            true,
        )
        .await?;

        let session = database.open().await?;
        let before_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        require(before_rows == 1, "the pre-migration audit row must exist")?;
        session.shutdown().await?;

        run_migration_files(&database, &[23]).await?;

        let session = database.open().await?;
        let client = session.client();
        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&catalogue, &function_id],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "application"
                && authority.try_get::<_, Vec<u8>>(1)? == revision_id
                && authority.try_get::<_, Option<Vec<u8>>>(2)?.is_none(),
            "the migration must backfill exactly one application authority row per function",
        )?;
        let after_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            after_rows == before_rows,
            "the migration must never drop or rewrite an invocation-audit row",
        )?;
        let target_fk_points_at_authorities: bool = client
            .query_one(
                "SELECT confrelid = '_orna_kernel.invocation_target_authorities'::regclass
                 FROM pg_catalog.pg_constraint
                 WHERE conname = 'invocation_audit_events_target_fk'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            target_fk_points_at_authorities,
            "the invocation-audit target foreign key must reference the authority relation",
        )?;
        let audit_targets: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events AS audit
                 JOIN _orna_kernel.invocation_target_authorities AS authority
                   ON authority.catalogue_revision_id = audit.catalogue_revision_id
                  AND authority.function_id = audit.function_id",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            audit_targets == before_rows,
            "every existing invocation-audit target pair must resolve through the authority relation",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn migration_twenty_three_aborts_on_revision_mismatched_backfill() -> TestResult<()> {
    with_test_database(|database| async move {
        run_migration_files(&database, &(1..=22).collect::<Vec<_>>()).await?;
        insert_inactive_application_function(
            &database,
            0xd1,
            &[0xd2_u8; 16],
            &[0xd3_u8; 16],
            &["hostile", "corrupt"],
            false,
        )
        .await?;

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sql = std::fs::read_to_string(format!(
            "{manifest_dir}/migrations/0023_executable_standard_snapshots.sql"
        ))?;
        let split = sql
            .find("INSERT INTO _orna_kernel.invocation_target_authorities")
            .ok_or_else(|| failure("migration 23 backfill statement"))?;
        let ddl = &sql[..split];
        let backfill = &sql[split..];
        let session = database.open().await?;
        session.client().batch_execute(ddl).await?;
        session
            .client()
            .batch_execute(
                "CREATE FUNCTION _orna_kernel.test_corrupt_authority() RETURNS trigger
                 LANGUAGE plpgsql AS $$
                 BEGIN
                   NEW.function_revision_id := decode(repeat('ab', 16), 'hex');
                   RETURN NEW;
                 END $$;
                 CREATE TRIGGER corrupt_authority BEFORE INSERT
                 ON _orna_kernel.invocation_target_authorities
                 FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_corrupt_authority();",
            )
            .await?;
        let migration_result = session.client().batch_execute(backfill).await;
        require(
            migration_result.is_err(),
            "a revision-mismatched backfill must abort migration 23",
        )?;
        session.shutdown().await?;

        let session = database.open().await?;
        let client = session.client();
        let authority_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            authority_rows == 0,
            "an aborted backfill must leave no authority row behind",
        )?;
        let target_fk_points_at_functions: bool = client
            .query_one(
                "SELECT confrelid = '_orna_kernel.catalogue_functions'::regclass
                 FROM pg_catalog.pg_constraint
                 WHERE conname = 'invocation_audit_events_target_fk'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            target_fk_points_at_functions,
            "an aborted migration 23 must leave the invocation-audit target foreign key untouched",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

// Work ADR 0059 implementation order item 4: the live production-path V3
// install proof. The tests below prove that a fresh database installs V1,
// upgrades to V2, and upgrades to V3 through the normal compiler-backed
// pipeline (`prepare_standard_upgrade_v2_to_v3` + `apply_standard_upgrade`,
// never a test-hooks fixture), that the active revision reopens pinned to
// `orna.std/3` with the exact V3 snapshot facts, that the V1 and V2 pins
// from the earlier activations remain in the historical revision records,
// that tampered V3 standard rows fail recovery closed without changing prior
// history, and that the sealed `sys.invoke` echo dogfooding proof runs
// against the V3-pinned active revision.

const V3_PROOF_CLIENT_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
const V3_PROOF_CLIENT_ROLE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
const V3_PROOF_CLIENT_ROLE_SECOND: PrincipalId = PrincipalId::from_bytes([0x73; 16]);
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

/// Installs the complete production standard chain on a fresh database:
/// the empty base, the V1 application activation, the V1-to-V2 upgrade
/// through `prepare_standard_upgrade_v1_to_v2` + `apply_standard_upgrade`,
/// and the V2-to-V3 upgrade through `prepare_standard_upgrade_v2_to_v3` +
/// `apply_standard_upgrade`.
struct V3StandardChain {
    version_one: ActiveDatabaseRevision,
    version_two: ActiveDatabaseRevision,
    version_three: ActiveDatabaseRevision,
    version_three_upgrade: orna_standard::StandardUpgrade,
}

async fn install_v3_standard_chain(database: &TestDatabase) -> TestResult<V3StandardChain> {
    let kernel = kernel(database)?;
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
    let version_one = kernel.apply(&version_one_candidate).await?;

    let version_two_upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&version_one)
        .map_err(|error| failure(format!("V1-to-V2 upgrade preparation failed: {error}")))?;
    let version_two = kernel.apply_standard_upgrade(&version_two_upgrade).await?;
    require(
        version_two.catalogue_hash_context().version() == CatalogueHashVersion::Version2
            && version_two
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision())
                == Some(STANDARD_LIBRARY_V2_REVISION_ID),
        "the V1-to-V2 upgrade did not install a version-two context pinned to orna.std/2",
    )?;
    require_standard_context(
        &version_two,
        version_two_upgrade.verified_standard_snapshot(),
    )?;

    let version_three_upgrade = orna_standard::prepare_standard_upgrade_v2_to_v3(&version_two)
        .map_err(|error| failure(format!("V2-to-V3 upgrade preparation failed: {error}")))?;
    let version_three = kernel
        .apply_standard_upgrade(&version_three_upgrade)
        .await?;
    Ok(V3StandardChain {
        version_one,
        version_two,
        version_three,
        version_three_upgrade,
    })
}

fn user_state_plan_candidate(
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let candidate =
        standard_application_candidate(STANDARD_APPLICATION_SOURCE_EDIT, active, upgrade)?;
    let function = candidate
        .candidate()
        .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
            "app", "enabled",
        ])?)
        .ok_or_else(|| failure("existing CLIENT fixture did not contain app.enabled"))?
        .id();
    let revision = candidate
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == function)
        .ok_or_else(|| failure("existing CLIENT fixture revision did not persist"))?;
    let slot = StateSlotId::from_bytes([0xa5; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: false },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let payload = plan
        .encode()
        .map_err(|error| failure(format!("USER state plan encoding failed: {error}")))?;
    let content_hash = orna_core::canonical_hash::artifact_payload_digest(&payload)?;
    let artifact = orna_core::revision::ExecutableArtifact::new(
        orna_core::revision::ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        orna_artifact::client_plan::STATE_FORMAT_VERSION,
        payload,
        content_hash,
    )?;
    let function_definition = candidate
        .candidate()
        .function_by_id(function)
        .ok_or_else(|| failure("USER state fixture function declaration disappeared"))?;
    let function_references = candidate
        .references()
        .iter()
        .filter(|reference| reference.source_function() == function)
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = orna_core::canonical_hash::function_semantic_digest_with_version(
        revision.semantic_hash_version(),
        function_definition,
        revision.language_version(),
        &artifact,
        candidate.expressions(),
        &function_references,
    )?;
    let replacement = orna_core::revision::FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        semantic_hash,
        revision.language_version(),
        artifact,
    )
    .map_err(|error| {
        failure(format!(
            "USER state function revision rebuild failed: {error}"
        ))
    })?
    .with_semantic_hash_version(revision.semantic_hash_version());
    let new_revisions = candidate
        .new_function_revisions()
        .iter()
        .map(|item| {
            if item.function() == function {
                replacement.clone()
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>();
    let current_revisions = candidate
        .current_function_revisions()
        .ok_or_else(|| failure("V3 application candidate omitted current function revisions"))?
        .iter()
        .map(|item| {
            if item.function() == function {
                replacement.clone()
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        candidate.catalogue_hash_context(),
        candidate.candidate(),
        &current_revisions,
        candidate.expressions(),
        candidate.origins(),
        candidate.references(),
    )?;
    let content = DeployableRevisionContent::new(
        candidate.origins().to_vec(),
        candidate.expressions().to_vec(),
        new_revisions,
        candidate.references().to_vec(),
    )
    .with_current_function_revisions(current_revisions);
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        orna_core::revision::DeployableRevisionInput::new(
            candidate.expected_base(),
            candidate.source().clone(),
            candidate.parent_catalogue(),
            candidate.candidate().clone(),
            catalogue_hash,
            content,
        ),
        candidate.catalogue_hash_context().clone(),
    )?)
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_public_user_state_profiles_and_atomic_conflict_batch() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        let default_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let named_change = UserStateChange::new(
            function,
            "blue".to_owned(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        let default_key = default_change.key_without_principal();
        let named_key = named_change.key_without_principal();
        let seeded = kernel
            .write_user_state(&session, &[default_change, named_change])
            .await?;
        require(
            seeded.len() == 2
                && seeded[0].key() == &default_key
                && seeded[1].key() == &named_key
                && seeded[0].outcome() == UserStateWriteOutcome::Written { revision: 1 }
                && seeded[1].outcome() == UserStateWriteOutcome::Written { revision: 1 },
            "initial USER state write did not return exact ordered keys and revisions",
        )?;
        let initial_audits = kernel.recover_security_audit_events().await?;
        let write_audits = initial_audits
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(2)
            })
            .count();
        require(
            write_audits == 1,
            "successful USER state batch did not record one write audit",
        )?;

        let default_cells = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        let named_cells = kernel
            .load_user_state(&session, function, "blue", &[], &expected_types)
            .await?;
        require(
            default_cells.len() == 1
                && default_cells[0].key().without_principal() == default_key
                && default_cells[0].revision() == 1
                && default_cells[0].value() == &RuntimeValue::Boolean(true)
                && named_cells.len() == 1
                && named_cells[0].key().without_principal() == named_key
                && named_cells[0].revision() == 1
                && named_cells[0].value() == &RuntimeValue::Boolean(false),
            "default and named USER state profile loads did not return persisted cells",
        )?;
        let profile_load_audits = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Load)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            profile_load_audits == 2,
            "default and named USER state loads did not record two redacted load audits",
        )?;

        let revisioned_default = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        let revised = kernel
            .write_user_state(&session, &[revisioned_default])
            .await?;
        require(
            revised.len() == 1
                && revised[0].key() == &default_key
                && revised[0].outcome() == UserStateWriteOutcome::Written { revision: 2 },
            "USER state successor write did not advance the revision to two",
        )?;
        let revised_default_cells = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        require(
            revised_default_cells.len() == 1
                && revised_default_cells[0].key().without_principal() == default_key
                && revised_default_cells[0].revision() == 2
                && revised_default_cells[0].value() == &RuntimeValue::Boolean(false),
            "USER state successor load did not return revision two",
        )?;
        let audit_count_before_conflict = kernel.recover_security_audit_events().await?.len();
        let stale_default = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let fresh_named = UserStateChange::new(
            function,
            "blue".to_owned(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let stale_default_key = stale_default.key_without_principal();
        let fresh_named_key = fresh_named.key_without_principal();
        let conflicts = kernel
            .write_user_state(&session, &[stale_default, fresh_named])
            .await?;
        require(
            conflicts.len() == 2
                && conflicts[0].key() == &stale_default_key
                && conflicts[1].key() == &fresh_named_key
                && conflicts[0].outcome()
                    == UserStateWriteOutcome::Conflict {
                        current_revision: 2,
                    }
                && conflicts[1].outcome()
                    == UserStateWriteOutcome::Conflict {
                        current_revision: 1,
                    },
            "mixed USER state conflict did not return exact ordered per-key results",
        )?;
        let audits_after_conflict = kernel.recover_security_audit_events().await?;
        let write_audits_after_conflict = audits_after_conflict
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
            })
            .count();
        require(
            audits_after_conflict.len() == audit_count_before_conflict + 1
                && write_audits_after_conflict == 3,
            "mixed USER state conflict did not record its redacted write audit",
        )?;
        let default_after = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        let named_after = kernel
            .load_user_state(&session, function, "blue", &[], &expected_types)
            .await?;
        require(
            default_after.len() == 1
                && default_after[0].key().without_principal() == default_key
                && default_after[0].revision() == 2
                && default_after[0].value() == &RuntimeValue::Boolean(false)
                && named_after.len() == 1
                && named_after[0].key().without_principal() == named_key
                && named_after[0].revision() == 1
                && named_after[0].value() == &RuntimeValue::Boolean(false),
            "mixed USER state conflict changed persisted cells",
        )?;
        Ok(())
    })
    .await
}

/// Proves the active-revision lock serialises concurrent missing-cell writes
/// before persistence. One writer commits revision one and the other observes
/// the committed cell, returns the accepted ORNA0902 conflict, and appends its
/// redacted audit without leaking a database uniqueness error.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn concurrent_missing_user_state_writes_return_one_write_and_one_conflict() -> TestResult<()>
{
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let setup_kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = setup_kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("concurrent USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = setup_kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = setup_kernel.replace_security_snapshot(&security).await?;
        let first_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let second_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let first_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let second_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        require(
            first_change.expected_revision().is_none()
                && second_change.expected_revision().is_none(),
            "concurrent USER state writers did not carry expected_revision=None",
        )?;
        let expected_key = first_change.key_without_principal();
        require(
            expected_key == second_change.key_without_principal(),
            "concurrent USER state writers did not target one missing cell",
        )?;
        let first_kernel = named_kernel(&database, "orna-user-state-race-a")?;
        let second_kernel = named_kernel(&database, "orna-user-state-race-b")?;
        let first_task = tokio::spawn(async move {
            first_kernel
                .write_user_state(&first_session, std::slice::from_ref(&first_change))
                .await
        });
        let second_task = tokio::spawn(async move {
            second_kernel
                .write_user_state(&second_session, std::slice::from_ref(&second_change))
                .await
        });


        let (first_join, second_join) = tokio::time::timeout(
            APPLY_TIMEOUT,
            async { tokio::join!(first_task, second_task) },
        )
        .await
        .map_err(|_| failure("timed out waiting for concurrent USER state writers"))?;
        let first_results = first_join
            .map_err(|error| failure(format!("first USER state writer task failed: {error}")))??;
        let second_results = second_join
            .map_err(|error| failure(format!("second USER state writer task failed: {error}")))??;
        require(
            first_results.len() == 1
                && second_results.len() == 1
                && first_results[0].key() == &expected_key
                && second_results[0].key() == &expected_key,
            "concurrent USER state writes did not return one aligned result per batch",
        )?;
        let outcomes = [first_results[0].outcome(), second_results[0].outcome()];
        require(
            outcomes
                .iter()
                .filter(|outcome| **outcome == UserStateWriteOutcome::Written { revision: 1 })
                .count()
                == 1
                && outcomes
                    .iter()
                    .filter(|outcome| {
                        **outcome == UserStateWriteOutcome::Conflict {
                            current_revision: 1,
                        }
                    })
                    .count()
                    == 1,
            "concurrent USER state writes did not return one Written(1) and one ORNA0902 Conflict(1)",
        )?;

        let final_kernel = kernel(&database)?;
        let final_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let final_cells = final_kernel
            .load_user_state(
                &final_session,
                function,
                "",
                &[],
                &expected_types,
            )
            .await?;
        require(
            final_cells.len() == 1
                && final_cells[0].key().without_principal() == expected_key
                && final_cells[0].revision() == 1
                && matches!(final_cells[0].value(), RuntimeValue::Boolean(_)),
            "concurrent USER state writes left anything other than one revision-one cell",
        )?;

        let inspection = database.open().await?;
        let principal_bytes = V3_PROOF_CLIENT_USER.to_bytes().to_vec();
        let function_bytes = function.to_bytes().to_vec();
        let slot_bytes = slot.to_bytes().to_vec();
        let row = inspection
            .client()
            .query_one(
                "SELECT COUNT(*)::BIGINT, COALESCE(MAX(revision), 0)::BIGINT
                 FROM _orna_kernel.user_state_cells
                 WHERE principal_id = $1
                   AND root_function_id = $2
                   AND root_state_profile = ''
                   AND function_id = $2
                   AND function_instance_key = ''
                   AND state_slot_id = $3",
                &[&principal_bytes, &function_bytes, &slot_bytes],
            )
            .await?;
        let row_count: i64 = row.try_get(0)?;
        let max_revision: i64 = row.try_get(1)?;
        inspection.shutdown().await?;
        require(
            row_count == 1 && max_revision == 1,
            "concurrent USER state writes left partial or duplicate durable rows",
        )?;

        let write_audits = final_kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            write_audits == 2,
            "concurrent USER state writes did not leave two redacted write audits",
        )?;
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn user_state_write_linearizes_before_concurrent_security_replacement() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let setup_kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = setup_kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("linearization USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = setup_kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = setup_kernel.replace_security_snapshot(&security).await?;
        let retained_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let expected_key = change.key_without_principal();
        let disabled = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Disabled,
            )],
            vec![],
            vec![],
        )?;
        let initial_audits = setup_kernel.recover_security_audit_events().await?;
        let initial_audit_count = initial_audits.len();
        let initial_write_audits = initial_audits
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            initial_write_audits == 0,
            "linearization USER state fixture unexpectedly wrote an audit before the race",
        )?;

        install_user_state_insert_pause_trigger(&database).await?;
        let race_result: TestResult<()> = async {
            let coordinator = database.open().await?;
            coordinator
                .client()
                .query_one(
                    "SELECT pg_advisory_lock($1)",
                    &[&USER_STATE_INSERT_RACE_LOCK_KEY],
                )
                .await?;

            let writer_kernel =
                named_kernel(&database, "orna-user-state-linearization-writer")?;
            let writer_session = retained_session.clone();
            let writer_change = change.clone();
            let mut writer_task = Some(tokio::spawn(async move {
                writer_kernel
                    .write_user_state(
                        &writer_session,
                        std::slice::from_ref(&writer_change),
                    )
                    .await
            }));
            if let Err(error) =
                wait_for_advisory_wait(&database, "orna-user-state-linearization-writer").await
            {
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(error);
            }

            let replacement_kernel =
                named_kernel(&database, "orna-user-state-linearization-replacement")?;
            let replacement_snapshot = disabled.clone();
            let mut replacement_task = Some(tokio::spawn(async move {
                replacement_kernel
                    .replace_security_snapshot(&replacement_snapshot)
                    .await
            }));
            if let Err(error) = wait_for_active_lock_block(
                &database,
                "orna-user-state-linearization-writer",
                "orna-user-state-linearization-replacement",
            )
            .await
            {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(error);
            }

            if let Err(error) = coordinator
                .client()
                .query_one(
                    "SELECT pg_advisory_unlock($1)",
                    &[&USER_STATE_INSERT_RACE_LOCK_KEY],
                )
                .await
            {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(Box::new(error));
            }
            if let Err(error) = coordinator.shutdown().await {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                return Err(error);
            }

            let writer_task = writer_task
                .take()
                .ok_or_else(|| failure("linearization USER state writer task disappeared"))?;
            let replacement_task = replacement_task.take().ok_or_else(|| {
                failure("linearization security replacement task disappeared")
            })?;
            let (writer_join, replacement_join) = tokio::join!(
                wait_for_kernel_task(writer_task, "linearization USER state writer"),
                wait_for_kernel_task(replacement_task, "linearization security replacement"),
            );
            let writer_results = writer_join?;
            let replacement_snapshot = replacement_join?;
            let writer_results = writer_results?;
            let replacement_snapshot = replacement_snapshot?;

            // The replacement was observed waiting on the writer's active
            // revision lock; joining the writer first makes the commit order
            // explicit before the replacement result is accepted.
            require(
                writer_results.len() == 1
                    && writer_results[0].key() == &expected_key
                    && writer_results[0].outcome()
                        == UserStateWriteOutcome::Written { revision: 1 },
                "linearized USER state writer did not commit exactly one revision-one result",
            )?;
            let replacement_status = replacement_snapshot
                .principals()
                .find(|principal| principal.id() == V3_PROOF_CLIENT_USER)
                .map(|principal| principal.status());
            require(
                replacement_snapshot.revision() == active.pair()
                    && replacement_status == Some(PrincipalStatus::Disabled),
                "concurrent security replacement did not commit the disabled principal",
            )?;

            let final_kernel = kernel(&database)?;
            let after_replacement_audits =
                final_kernel.recover_security_audit_events().await?;
            let after_replacement_write_audits = after_replacement_audits
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::UserState
                        && decision.outcome() == SecurityAuditOutcome::Allowed
                        && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                        && decision.user_state_root_function() == Some(function)
                        && decision.user_state_cell_count() == Some(1)
                })
                .count();
            require(
                after_replacement_audits.len() == initial_audit_count + 1
                    && after_replacement_write_audits == initial_write_audits + 1,
                "security replacement left a stale or duplicate allowed USER state audit",
            )?;

            let recovered_disabled = final_kernel.recover_security_snapshot().await?;
            let recovered_status = recovered_disabled
                .principals()
                .find(|principal| principal.id() == V3_PROOF_CLIENT_USER)
                .map(|principal| principal.status());
            require(
                recovered_disabled.revision() == active.pair()
                    && recovered_status == Some(PrincipalStatus::Disabled),
                "disabled security replacement did not persist durably",
            )?;
            let denied = final_kernel
                .load_user_state(
                    &retained_session,
                    function,
                    "",
                    &[],
                    &expected_types,
                )
                .await
                .expect_err("retained disabled session must be denied on a later USER state load");
            require(
                matches!(
                    denied,
                    PostgresKernelError::StateExecuteDenied {
                        pair,
                        function: denied_function,
                        reason: ExecuteDenial::InvalidSession,
                    } if pair == active.pair()
                        && denied_function == SYS_STATE_LOAD_USER_STATE_FUNCTION_ID
                ),
                "retained disabled session returned the wrong typed USER state denial",
            )?;
            let after_denial_audits = final_kernel.recover_security_audit_events().await?;
            let after_denial_write_audits = after_denial_audits
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::UserState
                        && decision.outcome() == SecurityAuditOutcome::Allowed
                        && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                        && decision.user_state_root_function() == Some(function)
                        && decision.user_state_cell_count() == Some(1)
                })
                .count();
            require(
                after_denial_audits.len() == after_replacement_audits.len()
                    && after_denial_write_audits == after_replacement_write_audits,
                "denied retained USER state session appended an unexpected allowed audit",
            )?;

            let principal_bytes = V3_PROOF_CLIENT_USER.to_bytes().to_vec();
            let function_bytes = function.to_bytes().to_vec();
            let slot_bytes = slot.to_bytes().to_vec();
            let inspection = database.open().await?;
            let operation: TestResult<(i64, i64, i64)> = async {
                let row = inspection
                    .client()
                    .query_one(
                        "SELECT COUNT(*)::BIGINT,
                                COALESCE(MIN(revision), 0)::BIGINT,
                                COALESCE(MAX(revision), 0)::BIGINT
                         FROM _orna_kernel.user_state_cells
                         WHERE principal_id = $1
                           AND root_function_id = $2
                           AND root_state_profile = ''
                           AND function_id = $2
                           AND function_instance_key = ''
                           AND state_slot_id = $3",
                        &[&principal_bytes, &function_bytes, &slot_bytes],
                    )
                    .await?;
                Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
            }
            .await;
            let (row_count, min_revision, max_revision) = finish_test_session(
                operation,
                inspection.shutdown().await,
                "linearization USER state cell inspection",
            )?;
            require(
                row_count == 1 && min_revision == 1 && max_revision == 1,
                "linearized USER state write did not leave exactly one revision-one cell",
            )
        }
        .await;
        let cleanup_result = remove_user_state_insert_pause_trigger(&database).await;
        match (race_result, cleanup_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(test_error), Err(cleanup_error)) => Err(failure(format!(
                "USER state linearization proof failed: {test_error}; trigger cleanup also failed: {cleanup_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_the_v3_standard_install_and_reopen() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();

        // The active revision pins `orna.std/3` through a version-two
        // catalogue hash context, and the recovered snapshot matches the
        // companion application revision the upgrade prepared.
        require(
            chain.version_three.catalogue_hash_context().version()
                == CatalogueHashVersion::Version2
                && chain
                    .version_three
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID),
            "the V2-to-V3 upgrade did not pin orna.std/3 through the version-two context",
        )?;
        require_standard_context(&chain.version_three, standard)?;
        require_recovered_snapshot(
            chain.version_three_upgrade.application_revision(),
            &chain.version_three,
        )?;

        // The V3 snapshot facts: the three ordered units with their exact
        // reserved identities and logical paths, the append-only V2 source
        // parent edge, and the two output value types.
        let units = standard.source().units();
        require(
            units.len() == 3
                && units[0].ordinal() == 0
                && units[0].id() == orna_compiler::STD_TYPES_SOURCE_UNIT_ID
                && units[0].logical_path() == "std/types.orna"
                && units[1].ordinal() == 1
                && units[1].id() == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID
                && units[1].logical_path() == "std/invoke.orna"
                && units[2].ordinal() == 2
                && units[2].id() == STD_OUTPUT_SOURCE_UNIT_ID
                && units[2].logical_path() == "std/output.orna",
            "the V3 snapshot did not retain the exact three-unit bundle",
        )?;
        require(
            standard.source().bundle() == STANDARD_SOURCE_V3_BUNDLE_ID
                && standard.source().id() == STANDARD_SOURCE_V3_REVISION_ID
                && standard.source().parent() == Some(STANDARD_SOURCE_V2_REVISION_ID),
            "the V3 source revision did not retain its append-only V2 parent edge",
        )?;
        let value_types = standard.catalogue().value_types();
        let document = value_types
            .iter()
            .find(|definition| definition.id() == STD_TERMINAL_DOCUMENT_TYPE_ID)
            .ok_or_else(|| failure("the V3 snapshot is missing std.terminal.Document"))?;
        let bytestream = value_types
            .iter()
            .find(|definition| definition.id() == STD_IO_BYTE_STREAM_TYPE_ID)
            .ok_or_else(|| failure("the V3 snapshot is missing std.io.ByteStream"))?;
        require(
            document.name().parts() == ["std", "terminal", "document"]
                && document.persistence() == ValueTypePersistence::Transient
                && document.representation_contract() == STD_TERMINAL_DOCUMENT_CONTRACT
                && bytestream.name().parts() == ["std", "io", "bytestream"]
                && bytestream.persistence() == ValueTypePersistence::Transient
                && bytestream.representation_contract() == STD_IO_BYTE_STREAM_CONTRACT,
            "the V3 snapshot did not retain the two output value types",
        )?;
        require(
            standard
                .catalogue()
                .function_by_id(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID)
                .is_some()
                && standard.executables().iter().any(|executable| {
                    executable.function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        && executable.revision().id()
                            == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                }),
            "the V3 snapshot did not retain the unchanged std.invoke.echo executable",
        )?;

        // The durable V3 standard rows: header, three source units, the two
        // output schemas and value types, the echo function, its immutable
        // revision, the 44-byte parameter-echo artifact, and the exact
        // ordered reference sequence.
        let session = database.open().await?;
        let client = session.client();
        let v3_revision = standard.revision().to_bytes().to_vec();
        let v3_bundle = standard.source().bundle().to_bytes().to_vec();
        let v2_standard = chain
            .version_two
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the V2 active revision omitted its standard snapshot"))?;
        let v2_bundle = v2_standard.source().bundle().to_bytes().to_vec();
        let header = client
            .query_one(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash
                 FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&v3_revision],
            )
            .await?;
        require(
            header.try_get::<_, Vec<u8>>(0)? == standard.revision().to_bytes()
                && header.try_get::<_, Vec<u8>>(1)? == standard.source().id().to_bytes()
                && header.try_get::<_, Vec<u8>>(2)? == standard.catalogue().revision().to_bytes()
                && header.try_get::<_, i16>(3)? == 2
                && header.try_get::<_, String>(4)? == standard.language_version()
                && header.try_get::<_, Vec<u8>>(5)? == standard.digest().to_bytes(),
            "the V3 standard header row did not retain the exact digest-version-two facts",
        )?;
        let stored_units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path,
                        membership.source_unit_id, source_unit.content_hash,
                        source_unit.bundle_id, source_unit.content
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&v3_bundle],
            )
            .await?;
        let parent_units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path,
                        membership.source_unit_id, source_unit.content_hash,
                        source_unit.bundle_id, source_unit.content
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&v2_bundle],
            )
            .await?;
        require(
            parent_units.len() == 2
                && parent_units[0].try_get::<_, i64>(0)? == 0
                && parent_units[0].try_get::<_, String>(1)? == "std/types.orna"
                && parent_units[0].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_TYPES_SOURCE_UNIT_ID.to_bytes()
                && parent_units[0].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && parent_units[0].try_get::<_, Vec<u8>>(3)?
                    == stored_units[0].try_get::<_, Vec<u8>>(3)?
                && parent_units[0].try_get::<_, String>(5)?
                    == stored_units[0].try_get::<_, String>(5)?
                && parent_units[1].try_get::<_, i64>(0)? == 1
                && parent_units[1].try_get::<_, String>(1)? == "std/invoke.orna"
                && parent_units[1].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes()
                && parent_units[1].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && parent_units[1].try_get::<_, Vec<u8>>(3)?
                    == stored_units[1].try_get::<_, Vec<u8>>(3)?
                && parent_units[1].try_get::<_, String>(5)?
                    == stored_units[1].try_get::<_, String>(5)?,
            "the V2 parent source bundle lost reused source-unit membership or bytes",
        )?;
        require(
            stored_units.len() == 3
                && stored_units[0].try_get::<_, i64>(0)? == 0
                && stored_units[0].try_get::<_, String>(1)? == "std/types.orna"
                && stored_units[0].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_TYPES_SOURCE_UNIT_ID.to_bytes()
                && stored_units[0].try_get::<_, Vec<u8>>(3)? == units[0].content_hash().to_bytes()
                && stored_units[0].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && stored_units[0].try_get::<_, String>(5)? == units[0].content()
                && stored_units[1].try_get::<_, i64>(0)? == 1
                && stored_units[1].try_get::<_, String>(1)? == "std/invoke.orna"
                && stored_units[1].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes()
                && stored_units[1].try_get::<_, Vec<u8>>(3)? == units[1].content_hash().to_bytes()
                && stored_units[1].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && stored_units[1].try_get::<_, String>(5)? == units[1].content()
                && stored_units[2].try_get::<_, i64>(0)? == 2
                && stored_units[2].try_get::<_, String>(1)? == "std/output.orna"
                && stored_units[2].try_get::<_, Vec<u8>>(2)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes()
                && stored_units[2].try_get::<_, Vec<u8>>(3)? == units[2].content_hash().to_bytes()
                && stored_units[2].try_get::<_, Vec<u8>>(4)? == v3_bundle
                && stored_units[2].try_get::<_, String>(5)? == units[2].content(),
            "the V3 source units did not persist complete parent/child membership or bytes",
        )?;
        let schemas = client
            .query(
                "SELECT schema_id, name_parts FROM _orna_kernel.standard_catalogue_schemas
                 WHERE standard_library_revision_id = $1 ORDER BY schema_id",
                &[&v3_revision],
            )
            .await?;
        let terminal_schema = schemas
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_TERMINAL_SCHEMA_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the std.terminal schema row"))?;
        let io_schema = schemas
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok() == Some(STD_IO_SCHEMA_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the std.io schema row"))?;
        require(
            schemas.len() == 5
                && terminal_schema.try_get::<_, Vec<String>>(1)? == vec!["std", "terminal"]
                && io_schema.try_get::<_, Vec<String>>(1)? == vec!["std", "io"],
            "the V3 snapshot did not persist the std.terminal and std.io schemas",
        )?;
        let stored_value_types = client
            .query(
                "SELECT type_id, schema_id, name_parts, value_kind, mutability,
                        persistence, representation_contract, source_unit_id
                 FROM _orna_kernel.standard_catalogue_value_types
                 WHERE standard_library_revision_id = $1 ORDER BY type_id",
                &[&v3_revision],
            )
            .await?;
        let stored_document = stored_value_types
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_TERMINAL_DOCUMENT_TYPE_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the Document value type row"))?;
        let stored_bytestream = stored_value_types
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_IO_BYTE_STREAM_TYPE_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the ByteStream value type row"))?;
        require(
            stored_value_types.len() == 16
                && stored_document.try_get::<_, Vec<u8>>(1)? == STD_TERMINAL_SCHEMA_ID.to_bytes()
                && stored_document.try_get::<_, Vec<String>>(2)?
                    == vec!["std", "terminal", "document"]
                && stored_document.try_get::<_, String>(3)? == "opaque"
                && stored_document.try_get::<_, String>(4)? == "immutable"
                && stored_document.try_get::<_, String>(5)? == "transient"
                && stored_document.try_get::<_, String>(6)? == STD_TERMINAL_DOCUMENT_CONTRACT
                && stored_document.try_get::<_, Vec<u8>>(7)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes()
                && stored_bytestream.try_get::<_, Vec<u8>>(1)? == STD_IO_SCHEMA_ID.to_bytes()
                && stored_bytestream.try_get::<_, Vec<String>>(2)?
                    == vec!["std", "io", "bytestream"]
                && stored_bytestream.try_get::<_, String>(3)? == "opaque"
                && stored_bytestream.try_get::<_, String>(4)? == "immutable"
                && stored_bytestream.try_get::<_, String>(5)? == "transient"
                && stored_bytestream.try_get::<_, String>(6)? == STD_IO_BYTE_STREAM_CONTRACT
                && stored_bytestream.try_get::<_, Vec<u8>>(7)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes(),
            "the V3 snapshot did not persist the two output value types",
        )?;
        let function = client
            .query_one(
                "SELECT name_parts, current_function_revision_id, source_unit_id
                 FROM _orna_kernel.standard_catalogue_functions
                 WHERE standard_library_revision_id = $1 AND function_id = $2",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            function.try_get::<_, Vec<String>>(0)? == vec!["std", "invoke", "echo"]
                && function.try_get::<_, Vec<u8>>(1)?
                    == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()
                && function.try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes(),
            "the V3 snapshot did not retain the exact std.invoke.echo function row",
        )?;
        let artifact = client
            .query_one(
                "SELECT artifact_kind, format, format_version, octet_length(payload)
                 FROM _orna_kernel.standard_function_artifacts
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            artifact.try_get::<_, String>(0)? == "server_plan"
                && artifact.try_get::<_, String>(1)? == "orna.server-parameter-echo"
                && artifact.try_get::<_, i32>(2)? == 1
                && artifact.try_get::<_, i32>(3)? == 44,
            "the V3 snapshot did not retain the exact 44-byte parameter-echo artifact",
        )?;
        let references = client
            .query(
                "SELECT ordinal FROM _orna_kernel.standard_definition_references
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2
                 ORDER BY ordinal",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            references.len() == 3
                && (0..3).all(|ordinal| {
                    references
                        .get(ordinal as usize)
                        .and_then(|row| row.try_get::<_, i64>(0).ok())
                        == Some(ordinal)
                }),
            "the V3 snapshot did not persist the exact three ordered references",
        )?;
        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[
                    &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "standard"
                && authority.try_get::<_, Vec<u8>>(1)?
                    == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()
                && authority.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(standard.revision().to_bytes().to_vec()),
            "the V3 companion authority row did not pin the exact standard executable",
        )?;
        session.shutdown().await?;
        let marker = database.open().await?;
        marker
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2
                 WHERE singleton = true",
                &[
                    &chain.version_two.pair().source().to_bytes().to_vec(),
                    &chain.version_two.pair().catalogue().to_bytes().to_vec(),
                ],
            )
            .await?;
        marker.shutdown().await?;
        let recovered_parent = named_kernel(&database, "orna-v2-parent-recover")?
            .recover()
            .await?;
        require(
            recovered_parent.pair() == chain.version_two.pair()
                && recovered_parent
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V2_REVISION_ID),
            "the V2 parent standard bundle was not recoverable after the V3 upgrade",
        )?;
        let marker = database.open().await?;
        marker
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2
                 WHERE singleton = true",
                &[
                    &chain.version_three.pair().source().to_bytes().to_vec(),
                    &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                ],
            )
            .await?;
        marker.shutdown().await?;

        // Historical pins intact: the V1, V2, and V3 standard headers all
        // remain installed, and the three historical application catalogue
        // revisions retain their exact pins (V1 activation without a pin,
        // V2 companion pinned to orna.std/2, V3 companion pinned to orna.std/3).
        let session = database.open().await?;
        let client = session.client();
        let headers = client
            .query(
                "SELECT id, digest_version FROM _orna_kernel.standard_library_revisions
                 ORDER BY id",
                &[],
            )
            .await?;
        require(
            headers.len() == 3
                && headers[0].try_get::<_, Vec<u8>>(0)? == STANDARD_LIBRARY_REVISION_ID.to_bytes()
                && headers[0].try_get::<_, i16>(1)? == 1
                && headers[1].try_get::<_, Vec<u8>>(0)?
                    == STANDARD_LIBRARY_V2_REVISION_ID.to_bytes()
                && headers[1].try_get::<_, i16>(1)? == 2
                && headers[2].try_get::<_, Vec<u8>>(0)?
                    == STANDARD_LIBRARY_V3_REVISION_ID.to_bytes()
                && headers[2].try_get::<_, i16>(1)? == 2,
            "the historical V1, V2, and V3 standard headers did not all remain installed",
        )?;
        let v1_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_one.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        let v2_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_two.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        let v3_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_three.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        require(
            v1_pin.try_get::<_, i16>(0)? == 1
                && v1_pin.try_get::<_, Option<Vec<u8>>>(1)?.is_none()
                && v2_pin.try_get::<_, i16>(0)? == 2
                && v2_pin.try_get::<_, Option<Vec<u8>>>(1)?
                    == Some(STANDARD_LIBRARY_V2_REVISION_ID.to_bytes().to_vec())
                && v3_pin.try_get::<_, i16>(0)? == 2
                && v3_pin.try_get::<_, Option<Vec<u8>>>(1)?
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID.to_bytes().to_vec()),
            "the historical application revisions did not retain the exact V1, V2, and V3 pins",
        )?;
        session.shutdown().await?;

        // Reopening the database recovers the same active pair pinned to V3.
        let reopened = named_kernel(&database, "orna-v3-reopen")?.recover().await?;
        require(
            reopened.pair() == chain.version_three.pair()
                && reopened
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID),
            "reopening the installed database changed its active pair or pinned standard",
        )?;
        require_standard_context(&reopened, standard)?;
        require_recovered_snapshot(
            chain.version_three_upgrade.application_revision(),
            &reopened,
        )?;

        // Re-preparing the installed V3 upgrade fails closed with the exact
        // already-installed compiler error.
        let repeated = orna_standard::prepare_standard_upgrade_v2_to_v3(&chain.version_three)
            .expect_err("re-preparing an installed V3 standard unexpectedly succeeded");
        require(
            repeated.to_string()
                == format!(
                    "standard library {} is already installed",
                    STANDARD_LIBRARY_V3_REVISION_ID
                ),
            "re-preparing the installed V3 did not preserve the exact compiler error",
        )?;
        match repeated {
            orna_standard::StandardUpgradeError::Prepare {
                source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision },
            } => require(
                revision == STANDARD_LIBRARY_V3_REVISION_ID,
                "re-preparation reported the wrong installed standard revision",
            )?,
            error => {
                return Err(failure(format!(
                    "expected StandardLibraryAlreadyInstalled, got {error}"
                )));
            }
        }
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_sealed_echo_invocation_and_rejects_tampered_v3_rows() -> TestResult<()> {
    const ECHO_BY_NAME: i32 = 41;
    const ECHO_BY_IDENTITY: i32 = 42;

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, exactly as the V2 dogfooding proof does.
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Invoke through sys.invoke by qualified name and parameter name.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_BY_NAME,
        )?;
        let retained_name = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result_name = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_name)
            .await?;
        let invocation_name = require_echo_completion(&result_name, ECHO_BY_NAME)?;
        let events_name = match &result_name {
            SealedInvocationResult::Completed { events, .. } => events,
            _ => {
                return Err(failure(
                    "the name-addressed sealed invocation did not complete",
                ));
            }
        };

        // The completed kernel result carries the exact RESULT_VALUES Event
        // batch a server adapter delivers before CALL_COMPLETED; prove the
        // payload round-trips the sealed protocol bytes.
        let payload = encode_invocation_event_batch(&chain.version_three, &registry, events_name)?;
        let decoded = decode_invocation_event_batch(&chain.version_three, &registry, &payload)?;
        require(
            decoded == *events_name,
            "the completed Event batch did not round-trip the sealed RESULT_VALUES payload",
        )?;

        // Repeat the invocation by the fixed function and parameter
        // identities (FunctionId ...10 and ParameterId ...10).
        let by_identity = sealed_echo_request(
            InvocationRequestTarget::function_id(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID),
            InvocationParameterSelector::parameter_id(orna_compiler::STD_INVOKE_ECHO_PARAMETER_ID),
            ECHO_BY_IDENTITY,
        )?;
        let retained_identity =
            encode_invoke_request(&chain.version_three, &registry, &by_identity)?;
        let result_identity = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_identity)
            .await?;
        let invocation_identity = require_echo_completion(&result_identity, ECHO_BY_IDENTITY)?;
        require(
            invocation_name != invocation_identity,
            "the two sealed invocations reused one invocation identity",
        )?;

        // The allowed protected security and invocation audit events both
        // link the exact historical application RevisionPair whose catalogue
        // hash context pins orna.std/3.
        let security_events = kernel.recover_security_audit_events().await?;
        let allowed = security_events
            .iter()
            .filter(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().kind() == SecurityAuditKind::Execute
            })
            .collect::<Vec<_>>();
        require(
            allowed.len() == 2
                && allowed.iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(
                                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                                pair,
                            ))
                }),
            "the allowed EXECUTE evidence did not link the exact V3-pinned RevisionPair",
        )?;
        let allowed_security_ids = allowed.iter().map(|event| event.id()).collect::<Vec<_>>();
        let invocation_rows = invocation_audit_rows(&database).await?;
        require(
            invocation_rows.len() == 2
                && invocation_rows.iter().all(|row| {
                    row.outcome == "allowed"
                        && row.function
                            == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                                .to_bytes()
                                .to_vec()
                        && row.source == pair.source().to_bytes().to_vec()
                        && row.catalogue == pair.catalogue().to_bytes().to_vec()
                        && row.security_event.is_some()
                })
                && invocation_rows
                    .iter()
                    .map(|row| row.security_event.clone())
                    .collect::<Vec<_>>()
                    == allowed_security_ids
                        .iter()
                        .map(|id| Some(id.to_bytes().to_vec()))
                        .collect::<Vec<_>>(),
            "the invocation audit rows did not link the exact V3-pinned RevisionPair",
        )?;
        let authority = standard_authority_row(
            &database,
            pair.catalogue(),
            orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
        )
        .await?;
        require(
            authority.as_ref().is_some_and(|row| {
                row.target_class == "standard"
                    && row.function_revision
                        == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                            .to_bytes()
                            .to_vec()
                    && row.standard_revision == Some(standard_revision.to_bytes().to_vec())
            }),
            "the durable invocation target authority did not pin the V3 standard target",
        )?;

        // Reopen with the V3 pin: a fresh kernel recovers the same pair and
        // the same pinned standard after the sealed invocations.
        let reopened = named_kernel(&database, "orna-v3-invoke-reopen")?
            .recover()
            .await?;
        require(
            reopened.pair() == pair
                && reopened
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(standard_revision),
            "reopening the invoked database changed its active pair or pinned standard",
        )?;

        // The tamper fixtures below each fail recovery without writing or
        // changing prior history: the exact tampered fact stays tampered, the
        // active pair and every historical pin stay unchanged, and restoring
        // the row returns the database to a clean recovery.
        reject_tampered_output_unit_digest(&database, &chain).await?;
        reject_tampered_standard_revision(&database, &chain).await?;
        reject_tampered_executable_authority(&database, &chain).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_sealed_security_identity_invocation_and_audit() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let pair = chain.version_three.pair();
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let registry = registered_opaque_codecs(standard)?;
        let recovered = kernel.recover_security_snapshot().await?;
        let mut principals = recovered.principals().collect::<Vec<_>>();
        if !principals
            .iter()
            .any(|principal| principal.id() == V3_PROOF_CLIENT_USER)
        {
            principals.push(Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            ));
        }
        for role in [V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND] {
            if !principals.iter().any(|principal| principal.id() == role) {
                principals.push(Principal::new(
                    role,
                    PrincipalKind::Role,
                    PrincipalStatus::Active,
                ));
            }
        }
        let mut memberships = recovered.memberships().collect::<Vec<_>>();
        for role in [V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND] {
            if !memberships.iter().any(|membership| {
                membership.role() == role && membership.member() == V3_PROOF_CLIENT_USER
            }) {
                memberships.push(RoleMembership::new(role, V3_PROOF_CLIENT_USER));
            }
        }
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            recovered.function_targets().collect(),
            principals,
            memberships,
            recovered.execute_grants().collect(),
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(
            V3_PROOF_CLIENT_USER,
            vec![V3_PROOF_CLIENT_ROLE_SECOND, V3_PROOF_CLIENT_ROLE],
        )?;

        let requests = [
            (
                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
                "sys.security.session_principal",
            ),
            (
                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
                "sys.security.effective_principal",
            ),
        ];
        for (function, name) in requests {
            let target = if function == SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID {
                InvocationRequestTarget::qualified_name(
                    orna_core::catalogue::QualifiedSemanticName::new(name.split('.'))?,
                )?
            } else {
                InvocationRequestTarget::function_id(function)
            };
            let request = sealed_security_identity_request(target)?;
            let retained = encode_invoke_request(&chain.version_three, &registry, &request)?;
            let result = kernel
                .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
                .await?;
            require_security_identity_completion(&result, V3_PROOF_CLIENT_USER)?;
        }
        for target in [
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(
                    "sys.security.active_roles".split('.'),
                )?,
            )?,
            InvocationRequestTarget::function_id(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID),
        ] {
            let request = sealed_security_identity_request(target)?;
            let retained = encode_invoke_request(&chain.version_three, &registry, &request)?;
            let result = kernel
                .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
                .await?;
            require_active_roles_completion(
                &result,
                &[V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND],
            )?;
        }

        let security_events = kernel.recover_security_audit_events().await?;

        let allowed = security_events
            .iter()
            .filter(|event| {
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && event
                        .decision()
                        .target()
                        .is_some_and(|target| target.revision() == pair)
            })
            .collect::<Vec<_>>();
        require(
            allowed.len() == 4
                && allowed.iter().all(|event| {
                    event
                        .decision()
                        .target()
                        .map(|target| target.function())
                        .is_some_and(|function| {
                            function == SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
                                || function == SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID
                                || function == SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID
                        })
                })
                && allowed.iter().any(|event| {
                    event.decision().target().map(|target| target.function())
                        == Some(SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID)
                })
                && allowed.iter().any(|event| {
                    event.decision().target().map(|target| target.function())
                        == Some(SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID)
                })
                && allowed
                    .iter()
                    .filter(|event| {
                        event.decision().target().map(|target| target.function())
                            == Some(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID)
                    })
                    .count()
                    == 2,
            "sealed security identity invocations did not append exact EXECUTE evidence",
        )?;
        let security_ids = allowed.iter().map(|event| event.id()).collect::<Vec<_>>();
        let invocation_rows = invocation_audit_rows(&database).await?;
        require(
            invocation_rows.len() == 4
                && invocation_rows
                    .iter()
                    .all(|row| row.outcome == "allowed" && row.security_event.is_some())
                && invocation_rows
                    .iter()
                    .map(|row| row.security_event.clone())
                    .collect::<Vec<_>>()
                    == security_ids
                        .iter()
                        .map(|id| Some(id.to_bytes().to_vec()))
                        .collect::<Vec<_>>(),
            "sealed security identity invocations did not link invocation audit evidence",
        )?;
        for (function, name) in [
            (
                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
                "session_principal",
            ),
            (
                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
                "effective_principal",
            ),
            (SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, "active_roles"),
        ] {
            let authority = standard_authority_row(&database, pair.catalogue(), function).await?;
            require(
                authority.as_ref().is_some_and(|row| {
                    row.target_class == "system"
                        && row.function_revision == function.to_bytes().to_vec()
                        && row.standard_revision.is_none()
                }),
                match name {
                    "session_principal" => {
                        "session_principal must have a sealed system audit anchor"
                    }
                    "effective_principal" => {
                        "effective_principal must have a sealed system audit anchor"
                    }
                    _ => "active_roles must have a sealed system audit anchor",
                },
            )?;
        }

        // Recovery accepts the persisted sealed system invocation targets before
        // tampering, then fails closed when an authority binding no longer
        // identifies the admitted sealed system target. Restoring the binding
        // must return the same durable history to an accepted recovery path.
        kernel.recover().await?;

        // Exercise historical audit recovery without letting the active loader
        // reject the same collision first: point the active marker at the
        // already-valid V2 pair while the V3 invocation evidence remains
        // historical audit data.
        let collision_function = SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec();
        let collision_revision = vec![0xfa_u8; 16];
        let collision_hash = vec![0xfb_u8; 32];
        let collision_catalogue = pair.catalogue().to_bytes().to_vec();
        let schema_session = database.open().await?;
        let schema_row = schema_session
            .client()
            .query_one(
                "SELECT schema_id, source_unit_id
                 FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $1 AND source_unit_id IS NOT NULL
                 LIMIT 1",
                &[&collision_catalogue],
            )
            .await?;
        let schema_id: Vec<u8> = schema_row.try_get("schema_id")?;
        let source_unit_id: Vec<u8> = schema_row.try_get("source_unit_id")?;
        let revision_number: i64 = schema_session
            .client()
            .query_one(
                "SELECT COALESCE(MAX(revision_number), 0) + 1
                 FROM _orna_kernel.function_revisions
                 WHERE function_id = $1",
                &[&collision_function],
            )
            .await?
            .try_get(0)?;
        schema_session.client().batch_execute("BEGIN").await?;
        schema_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                    (id, introduced_catalogue_revision_id, function_id, revision_number,
                     content_hash, semantic_ir_hash, hash_algorithm, language_version, status)
                 VALUES ($1, $2, $3, $4, $5, $5, 'sha256', 'orna.language/1', 'active')",
                &[
                    &collision_revision,
                    &collision_catalogue,
                    &collision_function,
                    &revision_number,
                    &collision_hash,
                ],
            )
            .await?;
        schema_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                    (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                     security_mode, transaction_mode, volatility, return_shape,
                     return_type_kind, return_scalar_type, return_target_type_id,
                     current_function_revision_id, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, ARRAY['app', 'sealed_collision'], 'server', 'invoker',
                         'read_only', 'stable', 'rows', NULL, NULL, NULL, $4, $5, 0, 1)",
                &[
                    &collision_catalogue,
                    &collision_function,
                    &schema_id,
                    &collision_revision,
                    &source_unit_id,
                ],
            )
            .await?;
        schema_session.client().batch_execute("COMMIT").await?;
        schema_session.shutdown().await?;

        let v2_source = chain.version_two.pair().source().to_bytes().to_vec();
        let v2_catalogue = chain.version_two.pair().catalogue().to_bytes().to_vec();
        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2",
                &[&v2_source, &v2_catalogue],
            )
            .await?;
        require(
            changed == 1,
            "historical collision active marker update changed the wrong row count",
        )?;
        database_session.shutdown().await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.function_revisions",
                    rule: "each function revision must have exactly one versioned executable artifact",
                    ..
                }
            ),
            "sealed system catalogue collision did not fail historical recovery with the exact durable invariant",
        )?;

        let cleanup_session = database.open().await?;
        cleanup_session.client().batch_execute("BEGIN").await?;
        let deleted_revision = cleanup_session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&collision_revision],
            )
            .await?;
        let deleted_function = cleanup_session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&collision_catalogue, &collision_function],
            )
            .await?;
        require(
            deleted_revision == 1 && deleted_function == 1,
            "sealed system catalogue collision cleanup changed the wrong row count",
        )?;
        cleanup_session.client().batch_execute("COMMIT").await?;
        cleanup_session.shutdown().await?;

        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2",
                &[
                    &pair.source().to_bytes().to_vec(),
                    &collision_catalogue,
                ],
            )
            .await?;
        require(
            changed == 1,
            "historical collision active marker restore changed the wrong row count",
        )?;
        database_session.shutdown().await?;
        kernel.recover().await?;
        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'application'
                 WHERE catalogue_revision_id = $1
                   AND function_id = $2",
                &[
                    &pair.catalogue().to_bytes().to_vec(),
                    &SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            changed == 1,
            "sealed system authority tamper changed the wrong row count",
        )?;
        database_session.shutdown().await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "sealed system authority tamper did not fail with the exact durable invariant",
        )?;

        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'system'
                 WHERE catalogue_revision_id = $1
                   AND function_id = $2",
                &[
                    &pair.catalogue().to_bytes().to_vec(),
                    &SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            changed == 1,
            "sealed system authority restore changed the wrong row count",
        )?;
        database_session.shutdown().await?;
        kernel.recover().await?;
        Ok(())
    })
    .await
}

/// Proves the ADR 0064 capture surface end to end: a sealed echo invocation
/// completes, an inspection epoch captures its snapshot and trace rows in
/// the same commit, the epoch round-trips through load with the canonical
/// payload, the trace stream returns the model events with
/// `p_after_sequence` and self-observation suppression, the live
/// `state_cells` projection returns the stored cell redacted or with values
/// per the requested INSPECT classifier, and the `security_decisions`
/// projection returns the linked EXECUTE decision. A fresh recovery then
/// validates the inspection relations and the appended INSPECT audit row.
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_inspect_capture_and_projections_after_sealed_echo() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("inspect-capture-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "inspect capture live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(proves_inspect_capture_and_projections_after_sealed_echo_inner())
        })
        .map_err(|error| {
            failure(format!(
                "inspect capture live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("inspect capture live thread panicked")),
    }
}

async fn proves_inspect_capture_and_projections_after_sealed_echo_inner() -> TestResult<()> {
    const ECHO_VALUE: i32 = 41;

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, exactly as the sealed-echo proof does.
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
            vec![],
            vec![PrivilegeGrant::new(
                V3_PROOF_CLIENT_USER,
                PrivilegeClass::Inspect(InspectPrivilege::Values),
                None,
            )?],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Store one live USER state cell for the echo root so the live
        // state_cells projection has a decodable row.
        let state_slot = StateSlotId::from_bytes([0x42; 16]);
        let cell_value = encode_constructed_value(
            &chain.version_three,
            &registry,
            &RuntimeValue::Integer(ECHO_VALUE),
        )
        .map_err(|error| failure(format!("cell value encoding failed: {error}")))?;
        let database_session = database.open().await?;
        database_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES ($1, $2, '', $3, '', $4, $5, $6, 1)",
                &[
                    &V3_PROOF_CLIENT_USER.to_bytes().to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                    &state_slot.to_bytes().to_vec(),
                    &cell_value,
                    &orna_compiler::STD_INTEGER_TYPE_ID.to_bytes().to_vec(),
                ],
            )
            .await
            .map_err(|error| failure(format!("USER state cell insert failed: {error}")))?;
        let shutdown_result = database_session.shutdown().await;
        if let Err(error) = shutdown_result {
            return Err(failure(format!(
                "USER state insert session shutdown failed: {error}"
            )));
        }

        // Invoke through sys.invoke and capture the completed invocation.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_VALUE,
        )?;
        let retained = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = require_echo_completion(&result, ECHO_VALUE)?;

        // The dispatch auto-captures one structural epoch for the completed
        // invocation (ADR 0064), so the proof consumes that epoch rather than
        // capturing a second one (which would rewrite the invocation's trace
        // rows and violate the trace primary key).
        let resolved = kernel
            .find_latest_inspect_epoch(&session, invocation)
            .await?
            .ok_or_else(|| failure("the dispatch auto-captured epoch did not resolve"))?;
        let epoch_id = resolved;

        // The epoch round-trips through the canonical ORV5 payload and
        // agrees with the invocation, the pinned pair, and the owner.
        let loaded = kernel
            .load_inspect_snapshot(&session, epoch_id)
            .await?
            .ok_or_else(|| failure("the captured epoch did not load"))?;
        require(
            loaded.id() == epoch_id
                && loaded.invocation_id() == invocation
                && loaded.source_revision_id() == pair.source()
                && loaded.catalogue_revision_id() == pair.catalogue()
                && loaded.owner() == V3_PROOF_CLIENT_USER
                && loaded.root_target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && loaded.outcome() == InspectOutcomeKind::Allowed,
            "the loaded epoch did not retain the exact capture facts",
        )?;
        require(
            loaded.summary().event_count() == 3
                && loaded.summary().result()
                    == orna_core::inspect::InspectResultSummary::ValueBatch { value_count: 1 },
            "the loaded epoch did not retain the batch summary",
        )?;

        // The projections return the epoch rows after the ladder check, and
        // a request without a granted privilege fails closed.
        let nodes = kernel.inspect_invocation_nodes(&loaded, InspectPrivilege::OwnInvocation).await?;
        require(
            nodes.len() == 1
                && nodes[0].id() == invocation
                && nodes[0].kind() == orna_core::inspect::InspectInvocationNodeKind::Root
                && nodes[0].phase() == orna_core::inspect::InspectInvocationPhase::Completed
                && nodes[0].target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            "the loaded epoch did not retain the root invocation node",
        )?;
        let calls = kernel.inspect_calls(&loaded, InspectPrivilege::OwnInvocation).await?;
        require(
            calls.len() == 1
                && calls[0].invocation_id() == invocation
                && calls[0].value_count() == 1
                && calls[0].duration_nanoseconds() == 0,
            "the loaded epoch did not retain the root call row",
        )?;

        require(
            kernel
                .inspect_resources(&loaded, InspectPrivilege::OwnInvocation).await?
                .is_empty()
                && kernel
                    .inspect_ui_nodes(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty()
                && kernel
                    .inspect_presentation_candidates(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty()
                && kernel
                    .inspect_runtime_bindings(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty(),
            "the v1-empty projections returned non-empty rows",
        )?;
        let denied_audits_before = inspect_denied_audit_rows(&database).await?.len();
        let denied = kernel
            .inspect_invocation_nodes(&loaded, InspectPrivilege::Source)
            .await;
        require(
            matches!(denied, Err(PostgresKernelError::InspectDenied { .. })),
            "a projection without a granted privilege did not fail closed",
        )?;

        // The trace relation retains sequences 0..3 with the four durable
        // kinds; the model stream returns the three lifecycle events and
        // honours p_after_sequence and self-observation suppression.
        let trace_rows = inspect_trace_rows(&database, invocation).await?;
        if !(trace_rows.len() == 4
            && trace_rows[0].1 == 0
            && trace_rows[0].2 == "started"
            && trace_rows[1].1 == 1
            && trace_rows[1].2 == "value_batch"
            && trace_rows[2].1 == 2
            && trace_rows[2].2 == "completed"
            && trace_rows[3].1 == 3
            && trace_rows[3].2 == "inspect_snapshot")
        {
            return Err(failure(format!(
                "trace rows 0..3 are not exact: {trace_rows:?}"
            )));
        }
        // `p_after_sequence` is a resume cursor: `after = 0` (the spec
        // default) means "from the start" and returns the full stream
        // including the Started marker at sequence 0; a positive value
        // returns only rows strictly after it.
        let stream = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                0,
                None,
                false,
            )
            .await?;
        require(
            stream.len() == 3
                && stream[0].sequence() == 0
                && stream[0].kind() == InspectTraceEventKind::InvocationStarted
                && stream[1].sequence() == 1
                && stream[1].kind() == InspectTraceEventKind::ValueBatch
                && matches!(
                    stream[1].payload(),
                    InspectTracePayload::ValueBatch {
                        schema: None,
                        values,
                    } if values.len() == 1
                        && values[0].value() == &RuntimeValue::Integer(ECHO_VALUE)
                )
                && stream[2].sequence() == 2
                && stream[2].kind() == InspectTraceEventKind::InvocationCompleted,
            "the trace stream did not return the model lifecycle events",
        )?;
        let resumed = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                None,
                false,
            )
            .await?;
        require(
            resumed.len() == 1 && resumed[0].sequence() == 2,
            "p_after_sequence did not resume after sequence 1",
        )?;

        // An observation-produced row is suppressed by default and included
        // in the explicit include-observer mode.
        let observer = InvocationId::from_bytes([0x77; 16]);
        let observed_event = InvokeEvent::new(
            invocation,
            4,
            InvocationEventBody::Started {
                visible_principal: Some(V3_PROOF_CLIENT_USER),
            },
        )
        .map_err(|error| failure(format!("observer event construction failed: {error}")))?;
        let observed_payload = encode_constructed_value(
            &chain.version_three,
            &registry,
            &RuntimeValue::InvokeEvent(observed_event),
        )
        .map_err(|error| failure(format!("observer event encoding failed: {error}")))?;
        let database_session = database.open().await?;
        database_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES ($1, 4, 'started', $2, $3, transaction_timestamp())",
                &[
                    &invocation.to_bytes().to_vec(),
                    &observed_payload,
                    &observer.to_bytes().to_vec(),
                ],
            )
            .await
            .map_err(|error| failure(format!("observer trace row insert failed: {error}")))?;
        let shutdown_result = database_session.shutdown().await;
        if let Err(error) = shutdown_result {
            return Err(failure(format!(
                "observer row session shutdown failed: {error}"
            )));
        }
        let suppressed = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                Some(observer),
                false,
            )
            .await?;
        require(
            suppressed.len() == 1 && suppressed[0].sequence() == 2,
            "self-observation suppression did not drop the observer row",
        )?;
        let included = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                Some(observer),
                true,
            )
            .await?;
        require(
            included.len() == 2
                && included[0].sequence() == 2
                && included[1].sequence() == 4
                && included[1].observer_invocation() == Some(observer),
            "include-observer mode did not return the observer row",
        )?;

        let redacted = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::OwnInvocation,
                invocation,
                0,
                Some(observer),
                false,
            )
            .await?;
        require(
            redacted.len() == 3
                && matches!(
                    redacted[1].payload(),
                    InspectTracePayload::ValueBatchRedacted { value_count: 1 }
                ),
            "the unarmed trace must retain a redacted ValueBatch without decoded values",
        )?;
        let denied = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Source,
                invocation,
                0,
                None,
                false,
            )
            .await;
        require(
            matches!(
                denied,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a trace request without a granted privilege did not fail closed",
        )?;
        let denied_audits = inspect_denied_audit_rows(&database).await?;
        require(
            denied_audits.len() == denied_audits_before + 2
                && denied_audits[denied_audits_before..].iter().all(|audit| {
                    audit.0 == V3_PROOF_CLIENT_USER.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-privilege"
                }),
            "projection and trace denials did not append exactly one protected audit each",
        )?;

        // The live state_cells projection returns the stored cell; the typed
        // value is redacted unless the Values classifier was requested and
        // granted.
        let cells = kernel
            .inspect_state_cells(
                &loaded,
                InspectPrivilege::Values,
            )
            .await?;
        require(
            cells.len() == 1
                && cells[0].key().root_function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && cells[0].key().state_profile().is_empty()
                && cells[0].key().function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && cells[0].key().instance_key().is_empty()
                && cells[0].key().state_slot() == state_slot
                && cells[0].value_type() == orna_compiler::STD_INTEGER_TYPE_ID
                && cells[0].revision() == 1
                && cells[0].value()
                    == Some(
                        &InvokeValue::new(RuntimeValue::Integer(ECHO_VALUE)).map_err(|error| {
                            failure(format!("invoke value construction failed: {error}"))
                        })?,
                    ),
            "the state_cells projection did not return the stored cell with values",
        )?;
        let redacted = kernel
            .inspect_state_cells(
                &loaded,
                InspectPrivilege::OwnInvocation,
            )
            .await?;
        require(
            redacted.len() == 1 && redacted[0].revision() == 1 && redacted[0].value().is_none(),
            "the state_cells projection did not redact the stored value",
        )?;

        // The security_decisions projection returns the linked EXECUTE
        // decision and the INSPECT decision that captured this epoch.
        let decisions = kernel
            .inspect_security_decisions(
                &loaded,
                InspectPrivilege::OwnInvocation,
            )
            .await?;
        require(
            decisions.len() == 2
                && decisions[0].kind() == InspectSecurityDecisionKind::Execute
                && decisions[0].outcome() == InspectSecurityDecisionOutcome::Allowed
                && decisions[0].principals().contains(&V3_PROOF_CLIENT_USER)
                && decisions[0].target() == Some(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID)
                && decisions[0].denial_reason().is_none()
                && decisions[0].audit_refs().len() == 1
                && decisions[1].kind() == InspectSecurityDecisionKind::Inspect
                && decisions[1].outcome() == InspectSecurityDecisionOutcome::Allowed
                && decisions[1].principals().contains(&V3_PROOF_CLIENT_USER)
                && decisions[1].target().is_none()
                && decisions[1].denial_reason().is_none()
                && decisions[1].audit_refs().len() == 1,
            "the security_decisions projection did not return the linked EXECUTE and INSPECT decisions",
        )?;

        // A fresh recovery validates the inspection relations and the
        // appended INSPECT capture audit row.
        kernel.recover().await?;
        Ok(())
    })
    .await
}

/// Proves the ADR 0065 security-admin surface end to end: the identity
/// facts from a bound session, the `can_execute` and `has_privilege`
/// decisions against the recovered snapshot, the SecurityAdmin privilege
/// gate denying a session without the class while still recording the
/// closed denied audit, every admin mutation persisting its durable row
/// through the validated candidate, a privilege granted to an active role
/// passing the gate, disable failing closed for an unknown principal and
/// denying session formation afterwards, revoke removing the durable rows,
/// the audit rows carrying the closed `security_admin` kind for both
/// outcomes with the sealed target identities, and a fresh kernel
/// recovering the grants and the audit rows.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_security_admin_identity_checks_mutations_and_audit() -> TestResult<()> {
    const ADMIN: PrincipalId = PrincipalId::from_bytes([0x61; 16]);
    const USER: PrincipalId = PrincipalId::from_bytes([0x62; 16]);
    const ROLE: PrincipalId = PrincipalId::from_bytes([0x63; 16]);
    const NEW_USER: PrincipalId = PrincipalId::from_bytes([0x64; 16]);
    const OTHER: PrincipalId = PrincipalId::from_bytes([0x65; 16]);
    const UNKNOWN: PrincipalId = PrincipalId::from_bytes([0x66; 16]);
    const UNKNOWN_FUNCTION: FunctionId = FunctionId::from_bytes([0x67; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = version_one
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("the security-admin fixture function was not recovered"))?
            .id();

        // Seed the snapshot with one active admin holding the class-wide
        // SecurityAdmin privilege, one user with an active role and an
        // object-scoped EXECUTE privilege, and the application target.
        let security =
            SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                version_one.pair(),
                vec![SecurityFunctionTarget::application(function)],
                vec![
                    Principal::new(ADMIN, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                ],
                vec![RoleMembership::new(ROLE, USER)],
                vec![ExecuteGrant::new(USER, function)],
                vec![],
                vec![
                    PrivilegeGrant::new(ADMIN, PrivilegeClass::SecurityAdmin, None)?,
                    PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(function))?,
                ],
            )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let admin_session = security.bind_authenticated_session(ADMIN, vec![])?;
        let user_session = security.bind_authenticated_session(USER, vec![ROLE])?;

        // The identity functions return the typed facts of the bound session;
        // effective identity equals the session principal today.
        require(
            kernel.session_principal(&user_session) == USER
                && kernel.effective_principal(&user_session) == USER
                && kernel.active_roles(&user_session) == vec![ROLE],
            "the identity facts did not match the bound session",
        )?;

        // can_execute wraps authorise_execute: the granted user is allowed,
        // an existing principal without an EXECUTE grant fails closed on the
        // missing grant (an unknown principal would instead be an invalid
        // session; NEW_USER is created later in this proof).
        let allowed_execute = kernel.can_execute(USER, function).await?;
        require(
            matches!(
                &allowed_execute,
                ExecuteDecision::Allowed(authorised)
                    if authorised.session_principal() == USER
                        && authorised.authorising_principal() == USER
            ),
            "can_execute did not allow the granted user",
        )?;
        require(
            kernel.can_execute(ADMIN, function).await?
                == ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant),
            "can_execute did not deny an ungranted principal",
        )?;

        // has_privilege honours the class and the object scope: the
        // object-scoped EXECUTE grant reaches an object request only, and
        // the user holds no SecurityAdmin class.
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::Execute, Some(function))
                .await?
                == PrivilegeDecision::Allowed {
                    requested: PrivilegeClass::Execute,
                },
            "has_privilege did not allow the object-scoped execute privilege",
        )?;
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::Execute, None)
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::Execute,
                }),
            "has_privilege did not keep the object scope closed",
        )?;
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::SecurityAdmin, None)
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::SecurityAdmin,
                }),
            "has_privilege did not deny an unprivileged user",
        )?;
        require(
            kernel
                .has_privilege(ADMIN, PrivilegeClass::SecurityAdmin, Some(function))
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::SecurityAdmin,
                }),
            "has_privilege did not close SecurityAdmin object scope",
        )?;

        // The enforcement gate denies a session without the SecurityAdmin
        // class and still records the closed denied audit decision.
        let denied = kernel
            .create_principal(&user_session, NEW_USER, PrincipalKind::User)
            .await
            .expect_err("a session without SecurityAdmin must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::SecurityAdminDenied {
                    reason: PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    },
                }
            ),
            "the gate returned the wrong typed denial",
        )?;

        // The admin mutations persist their durable rows through the
        // validated candidate and return the recovered snapshot.
        let after_create = kernel
            .create_principal(&admin_session, NEW_USER, PrincipalKind::User)
            .await?;
        require(
            after_create
                .principals()
                .any(|principal| principal.id() == NEW_USER),
            "create_principal did not persist the new principal",
        )?;
        let after_role = kernel.grant_role(&admin_session, ROLE, NEW_USER).await?;
        require(
            after_role
                .memberships()
                .any(|membership| membership.role() == ROLE && membership.member() == NEW_USER),
            "grant_role did not persist the membership",
        )?;
        let scoped_security_admin = kernel
            .grant_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                Some(function),
            )
            .await
            .expect_err("object-scoped SecurityAdmin must be rejected");
        require(
            matches!(
                scoped_security_admin,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record,
                    rule: "the security_admin privilege grant must be class-wide",
                } if record == "grant_privilege"
            ),
            "grant_privilege did not reject object-scoped SecurityAdmin",
        )?;

        let after_privilege = kernel
            .grant_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                None,
            )
            .await?;
        require(
            after_privilege.privilege_grants().any(|grant| {
                grant.grantee() == NEW_USER
                    && grant.class() == PrivilegeClass::SecurityAdmin
                    && grant.is_class_wide()
            }),
            "grant_privilege did not persist the class-wide grant",
        )?;

        // A privilege granted to an active role reaches the session through
        // the gate: the user session can now create a role.
        kernel
            .grant_privilege(&admin_session, ROLE, PrivilegeClass::SecurityAdmin, None)
            .await?;
        let after_role_privilege = kernel.create_role(&user_session, OTHER).await?;
        require(
            after_role_privilege
                .principals()
                .any(|principal| principal.id() == OTHER),
            "an active role with the privilege did not pass the gate",
        )?;

        // Disabling fails closed for an unknown principal and prevents a
        // disabled principal from forming a session afterwards.
        kernel.disable_principal(&admin_session, NEW_USER).await?;
        require(
            kernel.can_execute(NEW_USER, function).await?
                == ExecuteDecision::Denied(ExecuteDenial::InvalidSession),
            "a disabled principal must not form a session",
        )?;
        let unknown_error = kernel
            .disable_principal(&admin_session, UNKNOWN)
            .await
            .expect_err("disabling an unknown principal must fail");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::DurableInvariant {
                    rule: "the principal to disable must exist",
                    ..
                }
            ),
            "disabling an unknown principal returned the wrong error",
        )?;

        // Revoke removes the durable rows.
        let after_revoke_role = kernel.revoke_role(&admin_session, ROLE, NEW_USER).await?;
        require(
            !after_revoke_role
                .memberships()
                .any(|membership| membership.role() == ROLE && membership.member() == NEW_USER),
            "revoke_role did not remove the membership",
        )?;
        let after_revoke_privilege = kernel
            .revoke_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                None,
            )
            .await?;
        require(
            !after_revoke_privilege
                .privilege_grants()
                .any(|grant| grant.grantee() == NEW_USER),
            "revoke_privilege did not remove the grant",
        )?;

        // Unknown revoke targets fail before the durable DELETE and do not
        // append an allowed mutation audit.
        let unknown_role = kernel
            .revoke_role(&admin_session, UNKNOWN, NEW_USER)
            .await
            .expect_err("revoking an unknown role must fail");
        require(
            matches!(
                unknown_role,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the role to revoke must exist",
                } if record == "revoke_role"
            ),
            "revoke_role accepted an unknown role target",
        )?;
        let unknown_member = kernel
            .revoke_role(&admin_session, ROLE, UNKNOWN)
            .await
            .expect_err("revoking an unknown role member must fail");
        require(
            matches!(
                unknown_member,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the role member to revoke must exist",
                } if record == "revoke_role"
            ),
            "revoke_role accepted an unknown member target",
        )?;
        let unknown_grantee = kernel
            .revoke_privilege(&admin_session, UNKNOWN, PrivilegeClass::SecurityAdmin, None)
            .await
            .expect_err("revoking an unknown privilege grantee must fail");
        require(
            matches!(
                unknown_grantee,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the privilege grantee must exist",
                } if record == "revoke_privilege"
            ),
            "revoke_privilege accepted an unknown grantee target",
        )?;
        let unknown_object = kernel
            .revoke_privilege(
                &admin_session,
                ADMIN,
                PrivilegeClass::Execute,
                Some(UNKNOWN_FUNCTION),
            )
            .await
            .expect_err("revoking an unknown privilege object must fail");
        require(
            matches!(
                unknown_object,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record,
                    rule: "the privilege grant object must exist",
                } if record == "revoke_privilege"
            ),
            "revoke_privilege accepted an unknown object target",
        )?;

        // The audit rows carry the closed security_admin kind for both
        // outcomes with the exact sealed target identities and the session
        // principals; argument payloads never appear.
        let events = kernel.recover_security_audit_events().await?;
        let admin_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SecurityAdmin)
            .collect::<Vec<_>>();
        require(
            admin_events.iter().any(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Denied
                    && event.decision().session_principal() == Some(USER)
                    && event.decision().security_admin_operation()
                        == Some(SecurityAdminAuditOperation::CreatePrincipal)
                    && event.decision().security_admin_target()
                        == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                    && event.decision().security_admin_denial()
                        == Some(PrivilegeDenial::MissingPrivilege {
                            requested: PrivilegeClass::SecurityAdmin,
                        })
            }),
            "the denied gate did not record its closed audit decision",
        )?;
        let allowed_creates = admin_events
            .iter()
            .filter(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().security_admin_operation()
                        == Some(SecurityAdminAuditOperation::CreatePrincipal)
                    && event.decision().session_principal() == Some(ADMIN)
                    && event.decision().security_admin_target()
                        == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
            })
            .count();
        require(
            allowed_creates == 1
                && admin_events.len() == 9
                && admin_events.iter().all(|event| {
                    event.decision().security_admin_target().is_some()
                        && event.decision().session_principal().is_some()
                }),
            "the allowed mutation audit rows did not record the closed shape",
        )?;

        // A fresh kernel recovers the same privilege grants and the audit
        // rows, proving the durable round-trip through the privilege loader
        // and the security_admin audit decoder.
        let reopened = named_kernel(&database, "orna-security-admin-reopen")?
            .recover_security_snapshot()
            .await?;
        require(
            reopened.privilege_grants().any(|grant| {
                grant.grantee() == ADMIN
                    && grant.class() == PrivilegeClass::SecurityAdmin
                    && grant.is_class_wide()
            }) && reopened.principals().any(|principal| {
                principal.id() == NEW_USER && principal.status() == PrincipalStatus::Disabled
            }),
            "a fresh kernel did not recover the persisted privilege grants",
        )?;
        let reopened_audit = named_kernel(&database, "orna-security-admin-audit-reopen")?
            .recover_security_audit_events()
            .await?;
        require(
            reopened_audit
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::SecurityAdmin)
                .count()
                == admin_events.len(),
            "a fresh kernel did not recover the security-admin audit rows",
        )?;
        Ok(())
    })
    .await
}

/// `find_latest_inspect_epoch` resolves the dispatch-auto-captured epoch for
/// a completed invocation, fails closed with `InspectDenial::MissingEpoch` when
/// no epoch exists, and fails closed with `InspectDenied` for a caller whose
/// granted ladder does not reach the epoch's owner scope.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_find_latest_inspect_epoch_resolves_the_dispatch_epoch() -> TestResult<()> {
    const ECHO_VALUE: i32 = 41;
    const FOREIGN_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0xdd; 16]);

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, mirroring the sealed-echo proof.
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![
                Principal::new(
                    V3_PROOF_CLIENT_USER,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
                Principal::new(
                    FOREIGN_PRINCIPAL,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
            ],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Invoke through sys.invoke; the sealed dispatch auto-captures one
        // structural epoch for the completed invocation.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_VALUE,
        )?;
        let retained = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = require_echo_completion(&result, ECHO_VALUE)?;

        let found = kernel
            .find_latest_inspect_epoch(&session, invocation)
            .await?;
        let epoch_id = found.ok_or_else(|| failure("the dispatched invocation has no epoch"))?;
        let loaded = kernel
            .load_inspect_snapshot(&session, epoch_id)
            .await?
            .ok_or_else(|| failure("the resolved epoch did not load"))?;
        require(
            loaded.invocation_id() == invocation
                && loaded.owner() == V3_PROOF_CLIENT_USER
                && loaded.root_target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            "find_latest_inspect_epoch resolved the wrong epoch",
        )?;

        // An explicit epoch uses the same authenticated ownership gate as the
        // latest lookup, then can be loaded by the already-authorised caller.
        let exact = kernel.find_inspect_epoch(&session, epoch_id).await?;
        require(
            exact == Some(epoch_id),
            "the owning principal must resolve its explicit inspect epoch",
        )?;

        let missing_before = inspect_denied_audit_rows(&database).await?.len();
        let unknown_epoch = kernel
            .find_inspect_epoch(&session, InspectEpochId::from_bytes([0xef; 16]))
            .await;
        require(
            matches!(
                unknown_epoch,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingEpoch
                })
            ),
            "an unknown explicit inspect epoch must fail closed as MissingEpoch",
        )?;

        // An invocation with no captured epoch also fails closed without
        // disclosing whether the invocation or epoch exists.
        let absent = kernel
            .find_latest_inspect_epoch(&session, InvocationId::from_bytes([0xee; 16]))
            .await;
        require(
            matches!(
                absent,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingEpoch
                })
            ),
            "an invocation without an epoch must fail closed as MissingEpoch",
        )?;
        let missing_audits = inspect_denied_audit_rows(&database).await?;
        require(
            missing_audits.len() == missing_before + 2
                && missing_audits[missing_before..].iter().all(|audit| {
                    audit.0 == V3_PROOF_CLIENT_USER.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-epoch"
                }),
            "missing epoch lookups did not append exactly one protected denial each",
        )?;

        // A foreign principal whose granted ladder is only OwnInvocation
        // cannot resolve the proof principal's epoch (required rung is
        // AnyInvocation) and fails closed with the closed denial reason.
        let foreign_session = security.bind_authenticated_session(FOREIGN_PRINCIPAL, vec![])?;
        let denial = kernel
            .find_latest_inspect_epoch(&foreign_session, invocation)
            .await;
        require(
            matches!(
                denial,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a foreign principal must fail closed on the ladder",
        )?;

        let exact_denial = kernel.find_inspect_epoch(&foreign_session, epoch_id).await;
        require(
            matches!(
                exact_denial,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a foreign principal must fail closed for an explicit epoch",
        )?;
        let denial_audits = inspect_denied_audit_rows(&database).await?;
        require(
            denial_audits.len() == missing_before + 4
                && denial_audits[missing_before + 2..].iter().all(|audit| {
                    audit.0 == FOREIGN_PRINCIPAL.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-privilege"
                }),
            "foreign inspect denials did not append exactly one protected audit each",
        )?;

        Ok(())
    })
    .await
}

/// Returns the protected columns for every denied INSPECT audit row.
async fn inspect_denied_audit_rows(
    database: &TestDatabase,
) -> TestResult<Vec<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, String)>> {
    let session = database.open().await?;
    let result: TestResult<Vec<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, String)>> = async {
        let rows = session
            .client()
            .query(
                "SELECT session_principal_id, effective_principal_id,
                        authorising_principal_id, denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE event_kind = 'inspect' AND outcome = 'denied'
                 ORDER BY sequence",
                &[],
            )
            .await?;
        let mut audits = Vec::with_capacity(rows.len());
        for row in &rows {
            audits.push((
                row.try_get(0)?,
                row.try_get(1)?,
                row.try_get(2)?,
                row.try_get(3)?,
            ));
        }
        Ok(audits)
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (result, shutdown_result) {
        (Ok(audits), Ok(())) => Ok(audits),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Returns `(invocation_id, sequence, kind)` for every trace row of one
/// invocation in sequence order.
async fn inspect_trace_rows(
    database: &TestDatabase,
    invocation: InvocationId,
) -> TestResult<Vec<(Vec<u8>, i64, String)>> {
    let session = database.open().await?;
    let result = async {
        let rows = session
            .client()
            .query(
                "SELECT invocation_id, sequence, kind
                 FROM _orna_kernel.inspect_trace_events
                 WHERE invocation_id = $1
                 ORDER BY sequence",
                &[&invocation.to_bytes().to_vec()],
            )
            .await?;
        let mut trace = Vec::with_capacity(rows.len());
        for row in &rows {
            trace.push((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?));
        }
        Ok(trace)
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (result, shutdown_result) {
        (Ok(trace), Ok(())) => Ok(trace),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Tamper fixture 1: the V3 `std/output.orna` unit's stored content digest is
/// replaced. Recovery reconstructs the three-unit bundle and must fail closed
/// with the exact content-hash mismatch without writing or repairing rows.
async fn reject_tampered_output_unit_digest(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let standard = chain.version_three_upgrade.verified_standard_snapshot();
    let original_content_hash: Vec<u8> = {
        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT content_hash FROM _orna_kernel.source_units WHERE id = $1",
                &[&STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec()],
            )
            .await?;
        let hash = row.try_get(0)?;
        session.shutdown().await?;
        hash
    };
    require(
        original_content_hash == standard.source().units()[2].content_hash().to_bytes(),
        "the stored output unit digest did not match the verified V3 snapshot",
    )?;
    let before = v3_durable_state(database).await?;

    let session = database.open().await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.source_units SET content_hash = $1 WHERE id = $2",
            &[
                &vec![0x77u8; 32],
                &STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "output unit tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.source_units"
                && rule == "source unit digest must match its exact UTF-8 content",
            "the wrong output unit digest did not fail with the exact source-unit invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the wrong output unit digest produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected output unit tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.source_units SET content_hash = $1 WHERE id = $2",
            &[
                &original_content_hash,
                &STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "output unit restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the output unit did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// Tamper fixture 2: the V3 standard header's source link is replaced with a
/// hostile source revision. Recovery joins the hostile source and its empty
/// bundle, cannot reconstruct the three-unit V3 source, and must fail closed
/// with the source bundle invariant without writing or repairing rows.
async fn reject_tampered_standard_revision(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let standard = chain.version_three_upgrade.verified_standard_snapshot();
    let hostile_source = SourceRevisionId::from_bytes([0xe4; 16]);
    let hostile_bundle = SourceBundleId::from_bytes([0xe5; 16]);
    require(
        hostile_source != standard.source().id() && hostile_bundle != standard.source().bundle(),
        "the hostile source revision collided with the V3 source",
    )?;
    let before = v3_durable_state(database).await?;

    let session = database.open().await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', 1)",
            &[&hostile_bundle.to_bytes().to_vec(), &vec![0xe6u8; 32]],
        )
        .await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, NULL, $2, $3, 'sha256', 1)",
            &[
                &hostile_source.to_bytes().to_vec(),
                &hostile_bundle.to_bytes().to_vec(),
                &vec![0xe7u8; 32],
            ],
        )
        .await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.standard_library_revisions
             SET source_revision_id = $1 WHERE id = $2",
            &[
                &hostile_source.to_bytes().to_vec(),
                &standard.revision().to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "standard revision tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.source_bundles"
                && rule
                    == "standard source bundle digest must match the ordered source unit records",
            "the wrong standard revision did not fail with the exact source bundle invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the wrong standard revision produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected standard revision tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.standard_library_revisions
             SET source_revision_id = $1 WHERE id = $2",
            &[
                &standard.source().id().to_bytes().to_vec(),
                &standard.revision().to_bytes().to_vec(),
            ],
        )
        .await?;
    session
        .client()
        .execute(
            "DELETE FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&hostile_source.to_bytes().to_vec()],
        )
        .await?;
    session
        .client()
        .execute(
            "DELETE FROM _orna_kernel.source_bundles WHERE id = $1",
            &[&hostile_bundle.to_bytes().to_vec()],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "standard revision restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the standard revision did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// Tamper fixture 3: the V3 companion authority row pins an executable
/// revision the verified standard does not contain. Recovery must reject the
/// audited standard target with the exact durable invariant without writing
/// or repairing rows.
async fn reject_tampered_executable_authority(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let before = v3_durable_state(database).await?;
    let session = database.open().await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.invocation_target_authorities
             SET function_revision_id = $1
             WHERE catalogue_revision_id = $2 AND function_id = $3",
            &[
                &vec![0xaau8; 16],
                &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                    .to_bytes()
                    .to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "executable authority tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.invocation_audit_events"
                && rule == "target function and pinned revision must exist together",
            "the wrong executable authority did not fail with the exact durable invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the mismatched executable produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected executable tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.invocation_target_authorities
             SET function_revision_id = $1
             WHERE catalogue_revision_id = $2 AND function_id = $3",
            &[
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                    .to_bytes()
                    .to_vec(),
                &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                    .to_bytes()
                    .to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "executable authority restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the executable authority did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// The exact durable kernel facts a failed recovery must never change: the
/// active revision pointer, every standard header, every application
/// catalogue pin, and the protected audit row counts.
#[derive(Debug, Eq, PartialEq)]
struct V3DurableState {
    active_pair: (Vec<u8>, Vec<u8>),
    standard_headers: Vec<StandardHeaderRow>,
    catalogue_pins: Vec<CataloguePinRow>,
    invocation_audit_rows: i64,
    security_audit_rows: i64,
}

#[derive(Debug, Eq, PartialEq)]
struct StandardHeaderRow {
    id: Vec<u8>,
    source_revision: Vec<u8>,
    catalogue_revision: Vec<u8>,
    digest_version: i16,
}

#[derive(Debug, Eq, PartialEq)]
struct CataloguePinRow {
    id: Vec<u8>,
    standard_library_revision: Option<Vec<u8>>,
    canonical_hash_version: i16,
}

async fn v3_durable_state(database: &TestDatabase) -> TestResult<V3DurableState> {
    let session = database.open().await?;
    let operation = async {
        let active = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision",
                &[],
            )
            .await?;
        let active_pair = (active.try_get(0)?, active.try_get(1)?);
        let headers = session
            .client()
            .query(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version
                 FROM _orna_kernel.standard_library_revisions ORDER BY id",
                &[],
            )
            .await?;
        let mut standard_headers = Vec::with_capacity(headers.len());
        for row in headers {
            standard_headers.push(StandardHeaderRow {
                id: row.try_get(0)?,
                source_revision: row.try_get(1)?,
                catalogue_revision: row.try_get(2)?,
                digest_version: row.try_get(3)?,
            });
        }
        let pins = session
            .client()
            .query(
                "SELECT id, standard_library_revision_id, canonical_hash_version
                 FROM _orna_kernel.catalogue_revisions ORDER BY id",
                &[],
            )
            .await?;
        let mut catalogue_pins = Vec::with_capacity(pins.len());
        for row in pins {
            catalogue_pins.push(CataloguePinRow {
                id: row.try_get(0)?,
                standard_library_revision: row.try_get(1)?,
                canonical_hash_version: row.try_get(2)?,
            });
        }
        let invocation_audit_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        let security_audit_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.security_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        Ok(V3DurableState {
            active_pair,
            standard_headers,
            catalogue_pins,
            invocation_audit_rows,
            security_audit_rows,
        })
    }
    .await;
    finish_test_session(operation, session.shutdown().await, "V3 durable state read")
}

async fn recovery_error(database: &TestDatabase) -> TestResult<PostgresKernelError> {
    match kernel(database)?.recover().await {
        Ok(_) => Err(failure("tampered durable state recovered successfully")),
        Err(error) => Ok(error),
    }
}

/// Builds one complete checked `sys.invoke` Request for `std.invoke.echo`.
fn sealed_echo_request(
    target: InvocationRequestTarget,
    selector: InvocationParameterSelector,
    value: i32,
) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target,
        arguments: vec![InvocationArgument::new(
            selector,
            InvokeValue::new(RuntimeValue::Integer(value))?,
        )],
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::TestRunner,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )?,
        client_offer: InvocationClientOffer::new(
            5,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            1_024,
            0,
            None,
            None,
        )?,
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })?)
}
fn sealed_security_identity_request(target: InvocationRequestTarget) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target,
        arguments: Vec::new(),
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::TestRunner,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )?,
        client_offer: InvocationClientOffer::new(
            5,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            1_024,
            0,
            None,
            None,
        )?,
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })?)
}

fn require_active_roles_completion(
    result: &SealedInvocationResult,
    roles: &[PrincipalId],
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed active-roles invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    let values = records
        .get(1)
        .and_then(|record| match record.event().body() {
            InvocationEventBody::ValueBatch {
                schema: None,
                values,
            } => Some(values),
            _ => None,
        })
        .ok_or_else(|| failure("the sealed active-roles result lacked a plain ValueBatch"))?;
    let RuntimeValue::Constructed(value) = values
        .first()
        .ok_or_else(|| failure("the sealed active-roles result had no value"))?
        .value()
    else {
        return Err(failure(
            "the sealed active-roles result was not a constructed SET",
        ));
    };
    let ConstructedValueKind::Set(elements) = value.kind() else {
        return Err(failure(
            "the sealed active-roles result did not contain a SET",
        ));
    };
    let expected = roles
        .iter()
        .copied()
        .map(|role| RuntimeValue::Reference {
            target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
            object: ObjectId::from_bytes(role.to_bytes()),
        })
        .collect::<Vec<_>>();
    require(
        records.len() == 3
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted
            && values.len() == 1
            && matches!(
                value.descriptor().kind(),
                TypeDescriptorKind::Set(child)
                    if matches!(
                        child.kind(),
                        TypeDescriptorKind::Reference(target)
                            if target == SYS_SECURITY_PRINCIPAL_TYPE_ID
                    )
            )
            && elements == expected.as_slice(),
        "the sealed active-roles result did not return the exact typed canonical SET",
    )?;
    require(
        records
            .iter()
            .all(|record| record.event().invocation_id() == *invocation),
        "the sealed active-roles events did not retain one invocation",
    )?;
    Ok(*invocation)
}

fn require_security_identity_completion(
    result: &SealedInvocationResult,
    principal: PrincipalId,
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed security identity invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    let values = records
        .get(1)
        .and_then(|record| match record.event().body() {
            InvocationEventBody::ValueBatch {
                schema: None,
                values,
            } => Some(values),
            _ => None,
        })
        .ok_or_else(|| failure("the sealed security identity result lacked a plain ValueBatch"))?;
    require(
        records.len() == 3
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted
            && values.len() == 1
            && values[0].value()
                == &RuntimeValue::Reference {
                    target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
                    object: ObjectId::from_bytes(principal.to_bytes()),
                },
        "the sealed security identity result did not return the exact principal reference",
    )?;
    require(
        records
            .iter()
            .all(|record| record.event().invocation_id() == *invocation),
        "the sealed security identity events did not retain one invocation",
    )?;
    Ok(*invocation)
}

/// Asserts one completed sealed echo invocation carried exactly
/// `InvocationStarted(0)`, `ValueBatch(1)` with the typed integer, and
/// `InvocationCompleted(2)`, and returns its invocation identity.
fn require_echo_completion(
    result: &SealedInvocationResult,
    expected: i32,
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed echo invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    require(
        records.len() == 3
            && records[0].outer_sequence() == 1
            && records[1].outer_sequence() == 2
            && records[2].outer_sequence() == 3
            && records[0].event().sequence() == 0
            && records[1].event().sequence() == 1
            && records[2].event().sequence() == 2,
        "the sealed echo stream did not carry contiguous outer and inner sequences",
    )?;
    require(
        records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
        "the sealed echo stream did not carry InvocationStarted(0), ValueBatch(1), InvocationCompleted(2)",
    )?;
    let InvocationEventBody::ValueBatch {
        schema: None,
        values,
    } = records[1].event().body()
    else {
        return Err(failure(
            "the sealed ValueBatch event did not carry a plain typed batch",
        ));
    };
    require(
        values.len() == 1 && values[0].value() == &RuntimeValue::Integer(expected),
        "the sealed ValueBatch did not carry the exact typed integer",
    )?;
    require(
        records[0].event().invocation_id() == *invocation
            && records[1].event().invocation_id() == *invocation
            && records[2].event().invocation_id() == *invocation,
        "the sealed events did not share one invocation identity",
    )?;
    Ok(*invocation)
}

struct InvocationAuditRow {
    outcome: String,
    function: Vec<u8>,
    source: Vec<u8>,
    catalogue: Vec<u8>,
    security_event: Option<Vec<u8>>,
}

async fn invocation_audit_rows(database: &TestDatabase) -> TestResult<Vec<InvocationAuditRow>> {
    let session = database.open().await?;
    let operation = async {
        let rows = session
            .client()
            .query(
                "SELECT outcome, function_id, source_revision_id,
                        catalogue_revision_id, security_audit_event_id
                 FROM _orna_kernel.invocation_audit_events
                 ORDER BY sequence",
                &[],
            )
            .await?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(InvocationAuditRow {
                outcome: row.try_get("outcome")?,
                function: row.try_get("function_id")?,
                source: row.try_get("source_revision_id")?,
                catalogue: row.try_get("catalogue_revision_id")?,
                security_event: row.try_get("security_audit_event_id")?,
            });
        }
        Ok(result)
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "invocation audit row read",
    )
}

struct StandardAuthorityRow {
    target_class: String,
    function_revision: Vec<u8>,
    standard_revision: Option<Vec<u8>>,
}

async fn standard_authority_row(
    database: &TestDatabase,
    catalogue: CatalogueRevisionId,
    function: FunctionId,
) -> TestResult<Option<StandardAuthorityRow>> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_opt(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[
                    &catalogue.to_bytes().to_vec(),
                    &function.to_bytes().to_vec(),
                ],
            )
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(StandardAuthorityRow {
            target_class: row.try_get("target_class")?,
            function_revision: row.try_get("function_revision_id")?,
            standard_revision: row.try_get("standard_library_revision_id")?,
        }))
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "standard authority row read",
    )
}
