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
#[path = "apply/core.rs"]
mod core;
#[path = "apply/standard_v2.rs"]
mod standard_v2;
#[path = "apply/v3.rs"]
mod v3;

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
