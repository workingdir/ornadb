#![allow(clippy::redundant_closure)]
#![allow(clippy::iter_cloned_collect)]
#![cfg(unix)]

use std::{
    error::Error, io::ErrorKind, os::unix::net::UnixStream as StandardUnixStream, sync::Arc,
    time::Duration,
};

#[cfg(feature = "test-hooks")]
use orna_artifact::client_plan::ActionTargetDomain;
use orna_artifact::client_plan::{
    ClientExpressionNode, EXPRESSION_FORMAT_VERSION, ExpressionClientPlan, OPAQUE_FORMAT_VERSION,
    OpaqueClientPlan, ProceduralClientPlan, ResourceClientPlan, STATE_FORMAT_VERSION,
    StateClientPlan, StateDefault, StateScope,
};
#[cfg(feature = "test-hooks")]
use orna_client::ClientInspectError;
#[cfg(feature = "test-hooks")]
use orna_client::{
    ClientActionError, ClientActionOutcome, ClientActionState, ClientInspectRequest,
    ClientResourceStatus, ClientStateStore, complete_client_action, decode_action_payload,
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation,
    trigger_client_action,
};
use orna_client::{
    ClientExecutionError, ClientExternalContractRequest, ClientResourceCompletion,
    ClientResourceExecutor, ClientResourceRequest,
    capability::{
        LocalCapabilityArgumentSource, LocalCapabilityDeclaration, LocalCapabilityGrant,
        LocalCapabilityGrantSet, LocalCapabilityName, LocalCapabilityScope,
    },
    evaluate_client_function, evaluate_client_function_with_arguments,
    evaluate_client_function_with_arguments_and_executor, evaluate_client_function_with_grants,
    evaluate_client_function_with_grants_and_arguments,
};
#[cfg(feature = "test-hooks")]
use orna_client::{ClientResource, ClientResourceInvocationContext, ClientResourceKey};
use orna_compiler::{
    CheckedStandardLibrary, CheckedTypeId, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
    StandardApplicationCheckContext, check, check_standard_application,
    check_standard_library_source, prepare, prepare_standard_application,
};
#[cfg(feature = "test-hooks")]
use orna_core::inspect::INSPECT_RENDER_CONTRACT;
#[cfg(feature = "test-hooks")]
use orna_core::inspect_carrier::{InspectCarrierEnvelope, InspectCarrierKind};
#[cfg(feature = "test-hooks")]
use orna_core::revision::Sha256Digest;
#[cfg(feature = "test-hooks")]
use orna_core::system::{
    SYS_INSPECT_CALLS_TYPE_ID, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
    SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID, SYS_INSPECT_RESOURCES_TYPE_ID,
    SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_UI_NODES_TYPE_ID,
};
#[cfg(feature = "test-hooks")]
use orna_core::value::OpaqueCodecRegistry;
use orna_core::{
    CallSiteId, CatalogueRevisionId, FunctionId, FunctionRevisionId, InvocationId, ObjectId,
    ParameterId, PrincipalId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionVolatility, QualifiedSemanticName,
    },
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationOutputRequirement,
        InvocationParameterSelector, InvocationSinkOffer, InvocationStreamingRequirement,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    },
    invocation_binding::CliArgumentInput,
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget, DeployableRevision, DeployableRevisionContent,
        DeployableRevisionInput, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    security::{
        AuthenticatedSession, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision,
        ExecuteDenial, ExecuteGrant, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecurityAuditDecision,
        SecurityAuditDenial, SecurityAuditKind, SecurityAuditOutcome, SecurityFunctionTarget,
        SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::{SYS_INVOKE_FUNCTION_ID, SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID},
    types::{ResolvedType, TypeDescriptor},
    value::{EnumValue, FunctionArgument, OpaqueValue, RecordValue, RuntimeValue},
};
use orna_postgres::{
    AuthenticatedRawCallResult, AuthenticatedServerResourceResult, PostgresKernel,
    PostgresKernelError, ResourceCancellation, SealedInvocationResult, ServerInsertError,
    ServerMutationError, ServerUpdateError,
};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, ConnectionError, Event, MAX_RESOURCE_WINDOW,
    ProtocolConnection, RawCall, ResourceArgument, ResourceKind, ResourceRequest, ServerAction,
    ServerFrame, decode_active_server_frame, decode_constructed_invocation_event_frame,
    decode_constructed_server_frame, decode_invocation_event_batch, decode_registered_server_frame,
    decode_server_frame, encode_active_client_frame, encode_active_server_frame,
    encode_client_frame, encode_constructed_client_frame, encode_constructed_value,
    encode_invocation_event_batch, encode_invoke_request, encode_registered_client_frame,
};
#[cfg(feature = "test-hooks")]
use orna_protocol::{
    ResourceCancel, ResourceCancellationCode, ResourceClientFrame, ResourceServerFrame,
    ResourceWindowUpdate, decode_resource_client_frame, decode_resource_server_frame,
    encode_resource_client_frame, encode_resource_server_frame,
};
#[cfg(feature = "test-hooks")]
use orna_server::{
    InstalledClientResourceExecutor, RawResourceRequestAuthorizer,
    serve_local_raw_stream_with_resource_authorizer,
};
use orna_server::{
    InstalledInvokeError, InstalledInvokeErrorKind, InstalledInvokeOutcome, InstalledInvokeRequest,
    LocalAuthenticationError, LocalRawSocketError, LocalRawSocketResources,
    OpenStandardDatabaseError, RawClientDispatch, RuntimeFamily, open_standard_database,
    run_invoke_with_kernel, serve_local_raw_stream,
};
use orna_standard::{
    BOOLEAN_TYPE_ID, BYTE_STREAM_MAGIC, JSON_MAGIC, OPAQUE_TOKEN_TYPE_ID,
    STANDARD_LIBRARY_V3_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID, STD_ACTION_TYPE_ID,
    STD_IO_BYTE_STREAM_TYPE_ID, STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID,
    registered_opaque_codecs, retained_standard_library_snapshot,
    retained_standard_library_v2_snapshot, retained_standard_library_v3_snapshot,
    retained_standard_library_v6_snapshot, retained_standard_library_v10_snapshot,
    verify_standard_library_snapshot, verify_standard_library_v2_snapshot,
    verify_standard_library_v3_snapshot, verify_standard_library_v6_snapshot,
    verify_standard_library_v10_snapshot,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Barrier,
    time::{sleep, timeout},
};

use tokio_postgres::error::SqlState;
#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const RAW_CLIENT_SCHEMA_SOURCE: &str = "CREATE SCHEMA app;\n";
#[cfg(feature = "test-hooks")]
struct RecordingInstalledResourceExecutor {
    inner: InstalledClientResourceExecutor,
    execute_count: usize,
    inspect_count: usize,
    poll_count: usize,
    completed_values: Vec<(ResolvedType, RuntimeValue)>,
}

#[cfg(feature = "test-hooks")]
impl RecordingInstalledResourceExecutor {
    fn new(
        kernel: PostgresKernel,
        session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
        stream: StandardUnixStream,
        authorizer: RawResourceRequestAuthorizer,
    ) -> Self {
        Self {
            inner: InstalledClientResourceExecutor::new_with_stream_and_resource_authorizer(
                kernel, session, active, stream, authorizer,
            ),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        }
    }
}

#[cfg(feature = "test-hooks")]
impl ClientResourceExecutor for RecordingInstalledResourceExecutor {
    fn bind_current_invocation(&mut self, invocation: InvocationId) {
        self.inner.bind_current_invocation(invocation);
    }
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.execute_count += 1;
        let expected_type = request.expected_type();
        let completion = self.inner.execute(request);
        if let ClientResourceCompletion::Ready { value, .. } = &completion {
            self.completed_values.push((expected_type, value.clone()));
        }
        completion
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        self.poll_count += 1;
        self.inner.poll()
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.inner.cancel(request)
    }
    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        self.inner.abandon(request)
    }

    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        self.inspect_count += 1;
        let expected_type = match request.operation().projection_carrier_tag() {
            None => orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            Some(2) => SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            Some(3) => SYS_INSPECT_CALLS_TYPE_ID,
            Some(4) => SYS_INSPECT_RESOURCES_TYPE_ID,
            Some(5) => SYS_INSPECT_STATE_CELLS_TYPE_ID,
            Some(6) => SYS_INSPECT_UI_NODES_TYPE_ID,
            Some(7) => SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            Some(8) => SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            Some(9) => SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
            Some(_) => return Err("unexpected Inspector carrier tag".to_owned()),
        };
        let result = self.inner.inspect(request);
        if let Ok(value) = &result {
            self.completed_values
                .push((ResolvedType::Value(expected_type), value.clone()));
        }
        result
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        self.inner.external_contract(request)
    }
}
struct DeterministicStreamResourceExecutor;

impl ClientResourceExecutor for DeterministicStreamResourceExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.stream_values(vec![
            RuntimeValue::Text("stream-one".to_owned()),
            RuntimeValue::Text("stream-two".to_owned()),
        ])
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        None
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
    }
}

const RAW_CLIENT_FUNCTION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.stage AS ENUM ('lead', 'qualified');\n\
    CREATE TYPE app.request AS VALUE (stage app.stage) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;\n\
    CREATE SERVER FUNCTION app.select_flag(p_flag REF app.flag)\n\
    RETURNS ROWS (selected REF app.flag, value BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT REF(selected), selected.value\n\
    FROM app.flag selected WHERE REF(selected) = p_flag;\n\
    CREATE SERVER FUNCTION app.create_flagged(p_value BOOLEAN)\n\
    RETURNS ROWS (created REF app.flag)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO app.flag AS made (value)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION app.update_false(p_flag REF app.flag)\n\
    RETURNS ROWS (updated REF app.flag)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE app.flag AS alias\n\
    SET value = FALSE\n\
    WHERE REF(alias) = p_flag\n\
    RETURNING REF(alias);\n\
    CREATE SERVER FUNCTION app.delete_flag(p_flag REF app.flag)\n\
    RETURNS ROWS (deleted BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS DELETE FROM app.flag AS alias\n\
    WHERE REF(alias) = p_flag\n\
    RETURNING TRUE;\n\
    CREATE TYPE app.assignment AS OBJECT (\n\
      owner REF app.flag NOT NULL UNIQUE, marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION app.create_assignment(p_flag REF app.flag)\n\
    RETURNS ROWS (created_assignment REF app.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO app.assignment AS made_assignment (owner, marker)\n\
    VALUES (p_flag, TRUE) RETURNING REF(made_assignment);\n\
    CREATE SERVER FUNCTION app.read_assignments()\n\
    RETURNS ROWS (marker BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT assignment.marker FROM app.assignment assignment;\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
const RAW_EXPRESSION_CLIENT_FUNCTION_SOURCE: &str =
    include_str!("fixtures/expression_client_dogfood.orna");
const RAW_ACTION_SOURCE: &str = include_str!("fixtures/action_dogfood.orna");
#[cfg(feature = "test-hooks")]
const RAW_ACTION_CLIENT_MARKER: &str = "CREATE CLIENT FUNCTION action_fixture.call";
#[cfg(feature = "test-hooks")]
fn action_source_parts() -> TestResult<(&'static str, &'static str)> {
    let client_start = RAW_ACTION_SOURCE
        .find(RAW_ACTION_CLIENT_MARKER)
        .ok_or_else(|| failure("action fixture is missing its CLIENT source marker"))?;
    let server_source = RAW_ACTION_SOURCE[..client_start]
        .strip_suffix('\n')
        .ok_or_else(|| failure("action fixture is missing the server/client source separator"))?;
    Ok((server_source, &RAW_ACTION_SOURCE[client_start..]))
}
const RAW_EXTERNAL_CAPABILITY_SOURCE: &str = "CREATE SCHEMA cap;\n\
    CREATE EXTERNAL CLIENT FUNCTION cap.read() RETURNS TEXT\n\
    RUNTIME CONTRACT 'std.fs.read@1'\n\
    REQUIRES CAPABILITY std.fs.read('/home/bob');\n";
const RAW_STREAM_RESOURCE_SERVER_SOURCE: &str = "CREATE SCHEMA resource_fixture;\n\
    CREATE TYPE resource_fixture.probe AS OBJECT (marker TEXT UNIQUE NOT NULL, sequence INT NOT NULL);\n\
    CREATE SERVER FUNCTION resource_fixture.create(p_marker TEXT, p_sequence INT)\n\
    RETURNS ROWS (created REF resource_fixture.probe) SECURITY INVOKER\n\
    TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO resource_fixture.probe AS made (marker, sequence)\n\
    VALUES (p_marker, p_sequence) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION resource_fixture.resource(p_marker TEXT)\n\
    RETURNS STREAM<TEXT> SECURITY INVOKER\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.marker FROM resource_fixture.probe probe\n\
    WHERE probe.marker = p_marker;\n\
    CREATE SERVER FUNCTION resource_fixture.all()\n\
    RETURNS STREAM<TEXT> SECURITY INVOKER\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.marker FROM resource_fixture.probe probe ORDER BY probe.sequence;\n";
const RAW_STREAM_RESOURCE_CLIENT_SOURCE: &str = "CREATE CLIENT FUNCTION resource_fixture.call(p_marker TEXT) RETURNS STREAM<TEXT> IS\n\
    BEGIN\n\
        RETURN AWAIT std.data.stream_resource(target => resource_fixture.resource,\n\
          arguments => std.call.args(p_marker => p_marker));\n\
    END;\n\
    CREATE CLIENT FUNCTION resource_fixture.root() RETURNS BOOLEAN AS TRUE;\n\
    CREATE CLIENT FUNCTION resource_fixture.call_all() RETURNS STREAM<TEXT> IS\n\
    BEGIN\n\
        RETURN AWAIT std.data.stream_resource(target => resource_fixture.all, arguments => std.call.args());\n\
    END;\n";
const RAW_PROCEDURAL_RESOURCE_SERVER_SOURCE: &str = "CREATE SCHEMA procedural_fixture;\n\
    CREATE TYPE procedural_fixture.probe AS OBJECT (marker TEXT UNIQUE NOT NULL);\n\
    CREATE SERVER FUNCTION procedural_fixture.create(p_marker TEXT)\n\
    RETURNS ROWS (created REF procedural_fixture.probe) SECURITY INVOKER\n\
    TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO procedural_fixture.probe AS made (marker)\n\
    VALUES (p_marker) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION procedural_fixture.resource(p_marker TEXT)\n\
    RETURNS STREAM<TEXT> SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.marker FROM procedural_fixture.probe probe\n\
    WHERE probe.marker = p_marker;\n";
const RAW_PROCEDURAL_RESOURCE_CLIENT_SOURCE: &str = "CREATE CLIENT FUNCTION procedural_fixture.call(p_marker TEXT) RETURNS STREAM<TEXT> IS\n\
    LET resource std.data.StreamResource<TEXT> := std.data.stream_resource(\n\
        target => procedural_fixture.resource,\n\
        arguments => std.call.args(p_marker => p_marker)\n\
    );\n\
    BEGIN\n\
        RETURN AWAIT resource;\n\
    END;\n\
    CREATE CLIENT FUNCTION procedural_fixture.host() RETURNS STREAM<TEXT> IS\n\
    LET resource std.data.StreamResource<TEXT> := std.data.stream_resource(\n\
        target => procedural_fixture.resource,\n\
        arguments => std.call.args(p_marker => 'installed-marker')\n\
    );\n\
    BEGIN\n\
        RETURN AWAIT resource;\n\
    END;\n";
const RAW_SCALAR_RESOURCE_CLIENT_SOURCE: &str =
    include_str!("fixtures/scalar_resource_dogfood.orna");
const RAW_CLIENT_INT_INSERT_SOURCE: &str = "CREATE SCHEMA raw_int_insert;\n\
    CREATE TYPE raw_int_insert.int_probe AS OBJECT (\n\
      stored INT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_int_insert.create_int(p_value INT)\n\
    RETURNS ROWS (created REF raw_int_insert.int_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_int_insert.int_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_int_insert.read_ints()\n\
    RETURNS ROWS (stored INT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT int_probe.stored FROM raw_int_insert.int_probe int_probe;\n";
/// A two-Text-field object with one exact pair creator and separate readers.
const RAW_ARGUMENT_PAIR_SOCKET_SOURCE: &str = "CREATE SCHEMA raw_argument_pair_socket;\n\
    CREATE TYPE raw_argument_pair_socket.probe AS OBJECT (\n\
      first TEXT NOT NULL, second TEXT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_argument_pair_socket.create_pair(p_first TEXT, p_second TEXT)\n\
    RETURNS ROWS (created REF raw_argument_pair_socket.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_socket.probe AS made (first, second)\n\
    VALUES (p_first, p_second) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_socket.read_first()\n\
    RETURNS ROWS (first TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.first FROM raw_argument_pair_socket.probe probe;\n\
    CREATE SERVER FUNCTION raw_argument_pair_socket.read_second()\n\
    RETURNS ROWS (second TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.second FROM raw_argument_pair_socket.probe probe;\n";
/// ADR 0050 uses a scalar and a Reference value with the selector declared
/// second. Socket calls supply the selector first to prove ParameterId binding.
const RAW_REFERENCE_VALUE_UPDATE_SOCKET_SOURCE: &str = "CREATE SCHEMA raw_reference_value_socket;\n\
    CREATE TYPE raw_reference_value_socket.probe AS OBJECT (\n\
      stored TEXT NOT NULL UNIQUE, linked REF raw_reference_value_socket.probe\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_value_socket.create_probe(p_stored TEXT)\n\
    RETURNS ROWS (created REF raw_reference_value_socket.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_value_socket.probe AS made (stored)\n\
    VALUES (p_stored) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_reference_value_socket.update_text(\n\
      p_value TEXT, p_probe REF raw_reference_value_socket.probe\n\
    ) RETURNS ROWS (updated REF raw_reference_value_socket.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_socket.probe AS changed\n\
    SET stored = p_value WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_socket.update_link(\n\
      p_value REF raw_reference_value_socket.probe, p_probe REF raw_reference_value_socket.probe\n\
    ) RETURNS ROWS (updated REF raw_reference_value_socket.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_socket.probe AS changed\n\
    SET linked = p_value WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_socket.read_stored()\n\
    RETURNS ROWS (stored TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.stored FROM raw_reference_value_socket.probe probe;\n\
    CREATE SERVER FUNCTION raw_reference_value_socket.read_links()\n\
    RETURNS ROWS (linked REF raw_reference_value_socket.probe)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.linked FROM raw_reference_value_socket.probe probe;\n";
/// ADR 0052 exposes one version-4 unique Text selector and retains a creator
/// so the local socket can create the exact byte-distinct test rows itself.
const RAW_UNIQUE_TEXT_SELECT_SOCKET_SOURCE: &str = "CREATE SCHEMA raw_unique_text_select_socket;\n\
    CREATE TYPE raw_unique_text_select_socket.person AS OBJECT (\n\
      email TEXT UNIQUE, name TEXT NOT NULL, note TEXT\n\
    );\n\
    CREATE SERVER FUNCTION raw_unique_text_select_socket.create_person(\n\
      p_email TEXT, p_name TEXT\n\
    ) RETURNS ROWS (created REF raw_unique_text_select_socket.person)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_unique_text_select_socket.person AS made (email, name)\n\
    VALUES (p_email, p_name) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_unique_text_select_socket.by_email(p_email TEXT)\n\
    RETURNS ROWS (person REF raw_unique_text_select_socket.person, name TEXT, note TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT REF(selected), selected.name, selected.note\n\
    FROM raw_unique_text_select_socket.person selected WHERE selected.email = p_email;\n\
    CREATE SERVER FUNCTION raw_unique_text_select_socket.all_people()\n\
    RETURNS ROWS (person REF raw_unique_text_select_socket.person)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT REF(person) FROM raw_unique_text_select_socket.person person;\n";
const RAW_CLIENT_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
/// The CLIENT-authoritative principal for the ADR 0060 capability gate proof.
const CAPABILITY_GATE_USER: PrincipalId = PrincipalId::from_bytes([0x6a; 16]);
const RAW_CLIENT_UNGRANTED_USER: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
const RAW_CLIENT_STALE_USER: PrincipalId = PrincipalId::from_bytes([0x73; 16]);
const BOOLEAN_EVENT_CREDIT: u64 = 42;

macro_rules! standard_context_facts {
    ($active:expr) => {{
        let active = $active;
        let selected = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the public opener did not retain a selected standard context")
        })?;
        (
            active.catalogue_hash_context().version().to_u32(),
            selected.revision().to_bytes(),
            selected.catalogue().revision().to_bytes(),
            selected.digest().to_bytes(),
        )
    }};
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn opens_reopens_and_rejects_tampered_standard_database() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("standard-database-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "standard database runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(opens_reopens_and_rejects_tampered_standard_database_inner())
        })
        .map_err(|error| failure(format!("standard database thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("standard database thread panicked")),
    }
}

async fn opens_reopens_and_rejects_tampered_standard_database_inner() -> TestResult<()> {
    let expected =
        retained_standard_library_v10_snapshot().and_then(verify_standard_library_v10_snapshot)?;
    let expected_boolean_contract = expected
        .catalogue()
        .value_type_by_id(BOOLEAN_TYPE_ID)
        .ok_or_else(|| failure("the accepted standard library is missing the Boolean value type"))?
        .representation_contract()
        .to_owned();

    with_test_database(|database| async move {
        let opened = open_standard_database(kernel(&database)?).await?;
        let initial = opened.recover().await?;
        let initial_context = standard_context_facts!(&initial);
        require(
            initial_context.0 == 2 && initial_context.1 == expected.revision().to_bytes()
                && initial_context.2 == expected.catalogue().revision().to_bytes()
                && initial_context.3 == expected.digest().to_bytes(),
            "opening a fresh database did not select the exact accepted V9 standard context",
        )?;
        let initial_pair = initial.pair();
        let initial_pointer = active_pointer(&database).await?;
        require(
            initial_pointer
                == (
                    initial_pair.source().to_bytes().to_vec(),
                    initial_pair.catalogue().to_bytes().to_vec(),
                ),
            "the fresh opener recovery pair does not match the active durable pointer",
        )?;

        let reopened = open_standard_database(kernel(&database)?).await?;
        let reopened_active = reopened.recover().await?;
        require(
            reopened_active.pair() == initial_pair
                && standard_context_facts!(&reopened_active) == initial_context,
            "reopening an installed V9 database changed its active pair or accepted context",
        )?;

        let mut reconnect_config = database.config()?;
        reconnect_config.application_name("orna-standard-database-reconnect");
        let reconnected = open_standard_database(PostgresKernel::new(reconnect_config)).await?;
        let reconnected_active = reconnected.recover().await?;
        require(
            reconnected_active.pair() == initial_pair
                && standard_context_facts!(&reconnected_active) == initial_context,
            "reconnecting to an installed V9 database changed its active pair or accepted context",
        )?;

        let tampered_contract = format!("{expected_boolean_contract}.tampered");
        let written_contract = boolean_contract(
            &database,
            expected.revision().to_bytes().to_vec(),
            BOOLEAN_TYPE_ID.to_bytes().to_vec(),
            Some(&tampered_contract),
        )
        .await?;
        require(
            written_contract == tampered_contract,
            "the standard Boolean contract tamper did not commit its exact durable value",
        )?;

        let rejection = match open_standard_database(kernel(&database)?).await {
            Ok(_) => return Err(failure("the public opener repaired or accepted the tampered standard")),
            Err(error) => error,
        };
        require(
            matches!(
                &rejection,
                OpenStandardDatabaseError::Kernel {
                    source: PostgresKernelError::CanonicalHash(_),
                }
            ),
            "the public opener did not expose the canonical standard-tamper rejection",
        )?;
        require(
            rejection.to_string()
                == "canonical durable hash failed: stored standard library digest differs from canonical facts",
            "the public opener changed the standard-tamper Display contract",
        )?;
        let kernel_source = Error::source(&rejection)
            .ok_or_else(|| failure("the public standard-tamper error lost its kernel source"))?;
        require(
            kernel_source.to_string()
                == "canonical durable hash failed: stored standard library digest differs from canonical facts",
            "the public standard-tamper error changed its kernel source",
        )?;
        let canonical_source = Error::source(kernel_source)
            .ok_or_else(|| failure("the public standard-tamper error lost its canonical source"))?;
        require(
            canonical_source.to_string() == "stored standard library digest differs from canonical facts"
                && Error::source(canonical_source).is_none(),
            "the public standard-tamper error changed its canonical source chain",
        )?;
        require(
            boolean_contract(
                &database,
                expected.revision().to_bytes().to_vec(),
                BOOLEAN_TYPE_ID.to_bytes().to_vec(),
                None,
            )
            .await?
                == tampered_contract,
            "the failed public opener repaired the tampered standard contract",
        )?;
        require(
            active_pointer(&database).await? == initial_pointer,
            "the failed public opener changed the active durable pointer",
        )
    })
    .await
}

#[path = "standard_database/invocation.rs"]
mod invocation;
#[path = "standard_database/raw_socket.rs"]
mod raw_socket;
#[path = "standard_database/resource_dispatch.rs"]
mod resource_dispatch;
#[path = "standard_database/resource_socket.rs"]
mod resource_socket;

use invocation::{installed_invoke_request, installed_invoke_run};

fn raw_call(function: FunctionId) -> RawCall {
    RawCall {
        function,
        arguments: vec![],
    }
}

/// Persists one CLIENT capability audit decision through the same protected
/// table encoding the kernel's `append_security_audit_event` uses.
///
/// The orna-client gate entry is a pure evaluator: it performs no database
/// operation, so the decision it implies is appended by the enforcement
/// layer. That layer is the crate-private `append_security_audit_event` (no
/// public kernel entry appends an arbitrary decision), so the live proof
/// writes the row directly with the kernel's exact column encoding — the
/// `capability:<qualified-name>` denial-reason detail for both outcomes —
/// and recovers it through the public `recover_security_audit_events` entry.
async fn insert_capability_audit_decision(
    database: &TestDatabase,
    decision: &SecurityAuditDecision,
) -> TestResult<()> {
    let session = database.open().await?;
    let event_id = orna_core::SecurityAuditEventId::new();
    let outcome = match decision.outcome() {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    };
    let session_principal = decision
        .session_principal()
        .ok_or_else(|| failure("capability audit decision must carry a session principal"))?
        .to_bytes()
        .to_vec();
    let target = decision
        .target()
        .ok_or_else(|| failure("capability audit decision must pin its target"))?;
    let capability = decision
        .capability_name()
        .ok_or_else(|| failure("capability audit decision must carry the redacted name"))?;
    let insertion = session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.security_audit_events
                 (event_id, event_kind, outcome, session_principal_id,
                  function_id, source_revision_id, catalogue_revision_id, denial_reason)
             VALUES ($1, 'capability', $2, $3, $4, $5, $6, $7)",
            &[
                &event_id.to_bytes().to_vec(),
                &outcome,
                &session_principal,
                &target.function().to_bytes().to_vec(),
                &target.revision().source().to_bytes().to_vec(),
                &target.revision().catalogue().to_bytes().to_vec(),
                &format!("capability:{capability}"),
            ],
        )
        .await
        .map_err(Box::<dyn Error + Send + Sync>::from);
    session.shutdown().await?;
    insertion?;
    Ok(())
}

async fn create_flag_reference(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    create_flagged: FunctionId,
    p_value: ParameterId,
    flag_type: TypeId,
    stream: u64,
) -> TestResult<RuntimeValue> {
    let result = RawClientDispatch::new(
        kernel.clone(),
        session.clone(),
        stream,
        RawCall {
            function: create_flagged,
            arguments: vec![orna_protocol::CallArgument {
                parameter: p_value,
                value: RuntimeValue::Boolean(true),
            }],
        },
    )
    .finish()
    .await;
    require(
        result.source().is_none(),
        "the reference create must not retain a kernel source",
    )?;
    let [
        ServerAction::Events {
            stream: events_stream,
            events,
        },
        ServerAction::Completed {
            stream: completed_stream,
        },
    ] = result.actions()
    else {
        return Err(failure(
            "the reference create must return one event batch and completion",
        ));
    };
    require(
        *events_stream == stream && *completed_stream == stream,
        "the reference create must use the exact stream",
    )?;
    let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
        return Err(failure("the reference create must return one reference"));
    };
    require(
        *target == flag_type && *object != ObjectId::from_bytes([0; 16]),
        "the reference create returned the wrong reference",
    )?;
    Ok(RuntimeValue::Reference {
        target: *target,
        object: *object,
    })
}

async fn read_flag_values(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    server_function: FunctionId,
    stream: u64,
) -> TestResult<Vec<RuntimeValue>> {
    let result = RawClientDispatch::new(
        kernel.clone(),
        session.clone(),
        stream,
        raw_call(server_function),
    )
    .finish()
    .await;
    require(
        result.source().is_none(),
        "the raw read must not retain a kernel source",
    )?;
    let mut values = Vec::new();
    let mut completed = false;
    for action in result.actions() {
        match action {
            ServerAction::Events {
                stream: action_stream,
                events,
            } => {
                require(
                    !completed,
                    "the raw read must not emit events after completion",
                )?;
                require(
                    *action_stream == stream,
                    "the raw read must use the exact stream",
                )?;
                for event in events {
                    let Event::Value(value) = event else {
                        return Err(failure("the raw read must return value events"));
                    };
                    values.push(value.clone());
                }
            }
            ServerAction::Completed {
                stream: action_stream,
            } => {
                require(
                    !completed,
                    "the raw read must contain exactly one completion",
                )?;
                require(
                    *action_stream == stream,
                    "the raw read must use the exact stream",
                )?;
                completed = true;
            }
            other => {
                return Err(failure(format!(
                    "the raw read returned an unexpected action {other:?}"
                )));
            }
        }
    }
    require(
        completed,
        "the raw read must terminate with exactly one completion",
    )?;
    Ok(values)
}

/// Installs the Integer tracer on top of the active revision.
///
/// The preparation candidate must retain every active catalogue definition, so
/// the source is rebuilt from all retained units and the Integer trio is
/// appended to the last unit.
async fn install_raw_int_insert_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard_upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    TypeId,
    FunctionId,
    ParameterId,
    FunctionId,
)> {
    let last_ordinal = active
        .source()
        .units()
        .len()
        .checked_sub(1)
        .ok_or_else(|| failure("raw Integer tracer has no retained source unit"))?;
    let source = SourceBundle::new(active.source().units().iter().enumerate().map(
        |(ordinal, unit)| {
            let content = if ordinal == last_ordinal {
                format!("{}\n{}", unit.content(), RAW_CLIENT_INT_INSERT_SOURCE)
            } else {
                unit.content().to_owned()
            };
            SourceUnit::new(unit.logical_path(), content)
        },
    ))?;
    let report = check_standard_application(
        &source,
        &StandardApplicationCheckContext::try_new(
            active.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?,
    );
    require(
        report.diagnostics().is_empty(),
        "raw Integer INSERT fixture did not compile",
    )?;
    let applied = kernel
        .apply(&prepare_standard_application(
            &report,
            active.pair(),
            active,
        )?)
        .await?;
    let catalogue = applied.catalogue();
    let int_probe = catalogue
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["raw_int_insert", "int_probe"])
        .expect("raw_int_insert.int_probe type is absent")
        .id();
    let create_int = catalogue
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["raw_int_insert", "create_int"])
        .expect("raw_int_insert.create_int function is absent")
        .id();
    let create_int_parameter = catalogue
        .function_by_id(create_int)
        .expect("create_int is absent from the active catalogue")
        .parameter_by_name("p_value")
        .expect("create_int.p_value is absent")
        .id();
    let read_ints = catalogue
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["raw_int_insert", "read_ints"])
        .expect("raw_int_insert.read_ints function is absent")
        .id();
    Ok((
        applied,
        int_probe,
        create_int,
        create_int_parameter,
        read_ints,
    ))
}

async fn install_raw_argument_pair_socket_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard_upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    TypeId,
    FunctionId,
    ParameterId,
    ParameterId,
    FunctionId,
    FunctionId,
)> {
    let last_ordinal = active
        .source()
        .units()
        .len()
        .checked_sub(1)
        .ok_or_else(|| failure("raw argument-pair socket fixture has no retained source unit"))?;
    let source = SourceBundle::new(active.source().units().iter().enumerate().map(
        |(ordinal, unit)| {
            let content = if ordinal == last_ordinal {
                format!("{}\n{}", unit.content(), RAW_ARGUMENT_PAIR_SOCKET_SOURCE)
            } else {
                unit.content().to_owned()
            };
            SourceUnit::new(unit.logical_path(), content)
        },
    ))?;
    let report = check_standard_application(
        &source,
        &StandardApplicationCheckContext::try_new(
            active.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?,
    );
    require(
        report.diagnostics().is_empty(),
        "raw argument-pair socket fixture did not compile",
    )?;
    let applied = kernel
        .apply(&prepare_standard_application(
            &report,
            active.pair(),
            active,
        )?)
        .await?;
    let catalogue = applied.catalogue();
    let probe = catalogue
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["raw_argument_pair_socket", "probe"])
        .ok_or_else(|| failure("raw argument-pair socket probe type is absent"))?
        .id();
    let function = |name: &[&str]| {
        catalogue
            .functions()
            .iter()
            .find(|function| function.name().parts() == name)
            .map(|function| function.id())
            .ok_or_else(|| {
                failure(format!(
                    "raw argument-pair socket function is absent: {name:?}"
                ))
            })
    };
    let create_pair = function(&["raw_argument_pair_socket", "create_pair"])?;
    let definition = catalogue
        .function_by_id(create_pair)
        .ok_or_else(|| failure("raw argument-pair socket creator is absent"))?;
    let first = definition
        .parameter_by_name("p_first")
        .ok_or_else(|| failure("raw argument-pair socket p_first is absent"))?
        .id();
    let second = definition
        .parameter_by_name("p_second")
        .ok_or_else(|| failure("raw argument-pair socket p_second is absent"))?
        .id();
    require(
        first != second,
        "raw argument-pair socket parameters must have distinct identities",
    )?;
    let read_first = function(&["raw_argument_pair_socket", "read_first"])?;
    let read_second = function(&["raw_argument_pair_socket", "read_second"])?;
    Ok((
        applied,
        probe,
        create_pair,
        first,
        second,
        read_first,
        read_second,
    ))
}

async fn install_raw_unique_text_select_socket_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard_upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    TypeId,
    FunctionId,
    ParameterId,
    ParameterId,
    FunctionId,
    ParameterId,
    FunctionId,
)> {
    let last_ordinal = active
        .source()
        .units()
        .len()
        .checked_sub(1)
        .ok_or_else(|| failure("raw unique Text selector fixture has no retained source unit"))?;
    let source = SourceBundle::new(active.source().units().iter().enumerate().map(
        |(ordinal, unit)| {
            let content = if ordinal == last_ordinal {
                format!(
                    "{}\n{}",
                    unit.content(),
                    RAW_UNIQUE_TEXT_SELECT_SOCKET_SOURCE
                )
            } else {
                unit.content().to_owned()
            };
            SourceUnit::new(unit.logical_path(), content)
        },
    ))?;
    let report = check_standard_application(
        &source,
        &StandardApplicationCheckContext::try_new(
            active.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?,
    );
    require(
        report.diagnostics().is_empty(),
        "raw unique Text selector fixture did not compile",
    )?;
    let applied = kernel
        .apply(&prepare_standard_application(
            &report,
            active.pair(),
            active,
        )?)
        .await?;
    let catalogue = applied.catalogue();
    let person = catalogue
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["raw_unique_text_select_socket", "person"])
        .ok_or_else(|| failure("raw unique Text selector person type is absent"))?
        .id();
    let function = |name: &str| {
        catalogue
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["raw_unique_text_select_socket", name])
            .map(|function| function.id())
            .ok_or_else(|| {
                failure(format!(
                    "raw unique Text selector function is absent: {name}"
                ))
            })
    };
    let parameter = |function: FunctionId, name: &str| {
        catalogue
            .function_by_id(function)
            .and_then(|definition| definition.parameter_by_name(name))
            .map(|parameter| parameter.id())
            .ok_or_else(|| {
                failure(format!(
                    "raw unique Text selector parameter is absent: {name}"
                ))
            })
    };
    let create = function("create_person")?;
    let by_email = function("by_email")?;
    let all_people = function("all_people")?;
    let create_email = parameter(create, "p_email")?;
    let create_name = parameter(create, "p_name")?;
    let email = parameter(by_email, "p_email")?;
    require(
        active
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == by_email)
            .is_none()
            && applied
                .function_revisions()
                .iter()
                .find(|revision| revision.function() == by_email)
                .is_some_and(|revision| revision.artifact().version() == 4),
        "raw unique Text selector did not retain its sealed version-4 plan",
    )?;
    Ok((
        applied,
        person,
        create,
        create_email,
        create_name,
        by_email,
        email,
        all_people,
    ))
}

async fn install_raw_reference_value_update_socket_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard_upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    TypeId,
    FunctionId,
    ParameterId,
    FunctionId,
    ParameterId,
    ParameterId,
    FunctionId,
    ParameterId,
    ParameterId,
    FunctionId,
    FunctionId,
)> {
    let last_ordinal = active
        .source()
        .units()
        .len()
        .checked_sub(1)
        .ok_or_else(|| failure("raw reference value socket fixture has no retained source unit"))?;
    let source = SourceBundle::new(active.source().units().iter().enumerate().map(
        |(ordinal, unit)| {
            let content = if ordinal == last_ordinal {
                format!(
                    "{}\n{}",
                    unit.content(),
                    RAW_REFERENCE_VALUE_UPDATE_SOCKET_SOURCE
                )
            } else {
                unit.content().to_owned()
            };
            SourceUnit::new(unit.logical_path(), content)
        },
    ))?;
    let report = check_standard_application(
        &source,
        &StandardApplicationCheckContext::try_new(
            active.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?,
    );
    require(
        report.diagnostics().is_empty(),
        "raw reference value socket fixture did not compile",
    )?;
    let applied = kernel
        .apply(&prepare_standard_application(
            &report,
            active.pair(),
            active,
        )?)
        .await?;
    let catalogue = applied.catalogue();
    let probe = catalogue
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["raw_reference_value_socket", "probe"])
        .ok_or_else(|| failure("raw reference value socket probe type is absent"))?
        .id();
    let function = |name: &str| {
        catalogue
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["raw_reference_value_socket", name])
            .map(|function| function.id())
            .ok_or_else(|| {
                failure(format!(
                    "raw reference value socket function is absent: {name}"
                ))
            })
    };
    let parameter = |function: FunctionId, name: &str| {
        catalogue
            .function_by_id(function)
            .and_then(|definition| definition.parameter_by_name(name))
            .map(|parameter| parameter.id())
            .ok_or_else(|| {
                failure(format!(
                    "raw reference value socket parameter is absent: {name}"
                ))
            })
    };
    let create = function("create_probe")?;
    let update_text = function("update_text")?;
    let update_link = function("update_link")?;
    let create_stored = parameter(create, "p_stored")?;
    let text_value = parameter(update_text, "p_value")?;
    let text_selector = parameter(update_text, "p_probe")?;
    let link_value = parameter(update_link, "p_value")?;
    let link_selector = parameter(update_link, "p_probe")?;
    let read_stored = function("read_stored")?;
    let read_links = function("read_links")?;
    Ok((
        applied,
        probe,
        create,
        create_stored,
        update_text,
        text_value,
        text_selector,
        update_link,
        link_value,
        link_selector,
        read_stored,
        read_links,
    ))
}

async fn send_active_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream
        .write_all(&encode_active_client_frame(active, frame)?)
        .await?;
    Ok(())
}

async fn send_catalogue_protocol_frame(
    stream: &mut UnixStream,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream
        .write_all(&orna_protocol::encode_catalogue_client_frame(
            catalogue, frame,
        )?)
        .await?;
    Ok(())
}

async fn send_registered_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream
        .write_all(&encode_registered_client_frame(active, registry, frame)?)
        .await?;
    Ok(())
}

async fn send_constructed_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream
        .write_all(&encode_constructed_client_frame(active, registry, frame)?)
        .await?;
    Ok(())
}

async fn read_active_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(decode_active_server_frame(active, &encoded)?)
}

async fn read_registered_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(decode_registered_server_frame(active, registry, &encoded)?)
}

async fn read_constructed_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(decode_constructed_server_frame(active, registry, &encoded)?)
}
async fn read_constructed_invocation_protocol_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(
        decode_constructed_invocation_event_frame(active, registry, &encoded)
            .or_else(|_| decode_constructed_server_frame(active, registry, &encoded))?,
    )
}

async fn send_legacy_protocol_frame(
    stream: &mut UnixStream,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream.write_all(&encode_client_frame(frame)?).await?;
    Ok(())
}

async fn read_legacy_protocol_frame(stream: &mut UnixStream) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(decode_server_frame(&encoded)?)
}

async fn read_catalogue_protocol_frame(
    stream: &mut UnixStream,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(orna_protocol::decode_catalogue_server_frame(
        catalogue, &encoded,
    )?)
}

async fn read_encoded_protocol_frame(stream: &mut UnixStream) -> TestResult<Vec<u8>> {
    let mut header = [0_u8; 18];
    stream.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header[14..18].try_into()?) as usize;
    let mut encoded = header.to_vec();
    encoded.resize(18 + length, 0);
    stream.read_exact(&mut encoded[18..]).await?;
    Ok(encoded)
}

fn canonical_value_suffix(encoded: &[u8], marker: &[u8; 4]) -> TestResult<Vec<u8>> {
    let offset = encoded
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| failure("raw SERVER event is missing its selected value marker"))?;
    Ok(encoded[offset + marker.len()..].to_vec())
}

fn constructed_list_argument_frame(
    stream: u64,
    parameter: ParameterId,
    boolean_type: TypeId,
) -> Vec<u8> {
    let mut child = b"ORV5".to_vec();
    child.push(0x02);
    child.extend_from_slice(&boolean_type.to_bytes());
    child.extend_from_slice(&1_u32.to_be_bytes());
    child.push(1);

    let mut value_payload = 18_u16.to_be_bytes().to_vec();
    value_payload.extend_from_slice(&[0x02, 0x00]);
    value_payload.extend_from_slice(&boolean_type.to_bytes());
    value_payload.extend_from_slice(&1_u32.to_be_bytes());
    value_payload.extend_from_slice(&(child.len() as u32).to_be_bytes());
    value_payload.extend_from_slice(&child);

    let mut value = b"ORV5".to_vec();
    value.push(0x0d);
    value.extend_from_slice(&[0; 16]);
    value.extend_from_slice(&(value_payload.len() as u32).to_be_bytes());
    value.extend_from_slice(&value_payload);

    let mut frame_payload = parameter.to_bytes().to_vec();
    frame_payload.extend_from_slice(&value);
    let mut frame = b"ORF5\x02\x00".to_vec();
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(&(frame_payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&frame_payload);
    frame
}

async fn require_invalid_local_raw_hello(
    kernel: &PostgresKernel,
    hello: [u8; 12],
) -> TestResult<()> {
    let (server, client) = StandardUnixStream::pair()?;
    client.set_nonblocking(true)?;
    let mut client = UnixStream::from_std(client)?;
    let rejected = tokio::spawn(serve_local_raw_stream(
        kernel.clone(),
        server,
        LocalRawSocketResources::new(),
    ));
    let operation = async {
        client.write_all(&hello).await?;
        let mut response = [0_u8; 1];
        require(
            timeout(Duration::from_secs(1), client.read(&mut response)).await?? == 0,
            "invalid protocol-5 hello returned an acknowledgement or partial frame",
        )
    }
    .await;
    let shutdown = client.shutdown().await.map_err(Into::into);
    let rejection = rejected.await?;
    finish_session(operation, shutdown, "invalid protocol-5 hello cleanup")?;
    require(
        matches!(rejection, Err(LocalRawSocketError::InvalidHello)),
        "invalid protocol-5 hello did not close at the public handshake boundary",
    )
}
#[cfg(feature = "test-hooks")]
const RESOURCE_WIRE_HEADER_LENGTH: usize = 21;

#[cfg(feature = "test-hooks")]
async fn read_resource_client_frame_from_socket(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> TestResult<ResourceClientFrame> {
    let mut header = [0_u8; RESOURCE_WIRE_HEADER_LENGTH];
    stream.read_exact(&mut header).await?;
    let payload_length = u32::from_be_bytes(header[17..21].try_into()?) as usize;
    let mut encoded = header.to_vec();
    encoded.resize(RESOURCE_WIRE_HEADER_LENGTH + payload_length, 0);
    stream
        .read_exact(&mut encoded[RESOURCE_WIRE_HEADER_LENGTH..])
        .await?;
    Ok(decode_resource_client_frame(active, registry, &encoded)?)
}

#[cfg(feature = "test-hooks")]
async fn send_resource_server_frame_to_socket(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ResourceServerFrame,
) -> TestResult<()> {
    let encoded = encode_resource_server_frame(active, registry, frame)?;
    stream.write_all(&encoded).await?;
    Ok(())
}

#[cfg(feature = "test-hooks")]
async fn read_resource_server_frame_with_encoded(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> TestResult<(Vec<u8>, ResourceServerFrame)> {
    let mut header = [0_u8; RESOURCE_WIRE_HEADER_LENGTH];
    stream.read_exact(&mut header).await?;
    let payload_length = u32::from_be_bytes(header[17..21].try_into()?) as usize;
    let mut encoded = header.to_vec();
    encoded.resize(RESOURCE_WIRE_HEADER_LENGTH + payload_length, 0);
    stream
        .read_exact(&mut encoded[RESOURCE_WIRE_HEADER_LENGTH..])
        .await?;
    let frame = decode_resource_server_frame(active, registry, &encoded)?;
    Ok((encoded, frame))
}

#[cfg(feature = "test-hooks")]
async fn read_resource_server_frame_from_socket(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> TestResult<ResourceServerFrame> {
    Ok(
        read_resource_server_frame_with_encoded(stream, active, registry)
            .await?
            .1,
    )
}

#[cfg(feature = "test-hooks")]
async fn send_resource_client_frame_to_socket(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ResourceClientFrame,
) -> TestResult<()> {
    let encoded = encode_resource_client_frame(active, registry, frame)?;
    stream.write_all(&encoded).await?;
    Ok(())
}

#[cfg(feature = "test-hooks")]
fn exact_resource_value_bytes(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ResourceServerFrame,
) -> TestResult<usize> {
    let ResourceServerFrame::Values(mut values) = frame.clone() else {
        return Err(failure(
            "exact resource byte accounting requires a values frame",
        ));
    };
    values.byte_count = 0;
    match encode_resource_server_frame(active, registry, &ResourceServerFrame::Values(values)) {
        Err(orna_protocol::FrameCodecError::ResourceByteCountMismatch { actual, .. }) => Ok(actual),
        Ok(_) => Err(failure("resource values encoder accepted zero byte credit")),
        Err(error) => Err(failure(format!(
            "resource values byte accounting failed: {error:?}"
        ))),
    }
}

fn require_dispatch_failure(
    result: &orna_server::RawClientDispatchResult,
    stream: u64,
    failure: CallFailure,
    private_source_matches: bool,
    message: &'static str,
) -> TestResult<()> {
    require(
        private_source_matches && result.actions() == [ServerAction::Failed { stream, failure }],
        message,
    )
}

async fn install_raw_client_fixture(
    kernel: &PostgresKernel,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    orna_standard::StandardUpgrade,
    FunctionId,
    FunctionId,
)> {
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let schema = SourceBundle::new([SourceUnit::new("schema.orna", RAW_CLIENT_SCHEMA_SOURCE)])?;
    let report = check(&schema, empty.catalogue());
    require(
        report.diagnostics().is_empty(),
        "raw CLIENT fixture schema did not compile",
    )?;
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let standard_upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&standard_upgrade).await?;
    let context = StandardApplicationCheckContext::try_new(
        version_two.catalogue(),
        standard_upgrade.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new("main.orna", RAW_CLIENT_FUNCTION_SOURCE)])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "raw CLIENT fixture functions did not compile",
    )?;
    let active = kernel
        .apply(&prepare_standard_application(
            &report,
            version_two.pair(),
            &version_two,
        )?)
        .await?;
    let client = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app", "enabled"])
        .ok_or_else(|| failure("raw CLIENT fixture is missing its CLIENT function"))?
        .id();
    let server = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app", "read"])
        .ok_or_else(|| failure("raw CLIENT fixture is missing its SERVER function"))?
        .id();
    Ok((active, standard_upgrade, client, server))
}

#[cfg(feature = "test-hooks")]
async fn install_raw_client_fixture_v4(
    kernel: &PostgresKernel,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    orna_standard::StandardUpgrade,
    FunctionId,
    FunctionId,
)> {
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let schema = SourceBundle::new([SourceUnit::new("schema.orna", RAW_CLIENT_SCHEMA_SOURCE)])?;
    let report = check(&schema, empty.catalogue());
    require(
        report.diagnostics().is_empty(),
        "raw V4 CLIENT fixture schema did not compile",
    )?;
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let standard_upgrade_v2 = orna_standard::prepare_standard_upgrade_v1_to_v2(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&standard_upgrade_v2).await?;
    let standard_upgrade_v3 = orna_standard::prepare_standard_upgrade_v2_to_v3(&version_two)?;
    let version_three = kernel.apply_standard_upgrade(&standard_upgrade_v3).await?;
    let standard_upgrade_v4 = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = kernel.apply_standard_upgrade(&standard_upgrade_v4).await?;
    let context = StandardApplicationCheckContext::try_new(
        version_four.catalogue(),
        standard_upgrade_v4.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new("main.orna", RAW_CLIENT_FUNCTION_SOURCE)])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "raw V4 CLIENT fixture functions did not compile",
    )?;
    let active = kernel
        .apply(&prepare_standard_application(
            &report,
            version_four.pair(),
            &version_four,
        )?)
        .await?;
    let client = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app", "enabled"])
        .ok_or_else(|| failure("raw V4 CLIENT fixture is missing its CLIENT function"))?
        .id();
    let server = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app", "read"])
        .ok_or_else(|| failure("raw V4 CLIENT fixture is missing its SERVER function"))?
        .id();
    Ok((active, standard_upgrade_v4, client, server))
}
async fn install_scalar_resource_client_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard: &CheckedStandardLibrary,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    CallSiteId,
)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision,
                         body: &str|
     -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new(
                "resource_fixture.sql",
                body.to_owned(),
            )])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("scalar resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(
            active
                .source()
                .units()
                .iter()
                .enumerate()
                .map(|(ordinal, unit)| {
                    let content = if ordinal == last_ordinal {
                        format!("{}\n{}", unit.content(), body)
                    } else {
                        unit.content().to_owned()
                    };
                    SourceUnit::new(unit.logical_path(), content)
                }),
        )?)
    };
    let client_context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let client_source = append_source(active, RAW_SCALAR_RESOURCE_CLIENT_SOURCE)?;
    let client_report = check_standard_application(&client_source, &client_context);
    if !client_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "scalar CLIENT resource fixture did not compile: {:?}",
            client_report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&client_report, active.pair(), active)
        .map_err(|error| failure(format!("scalar client prepare failed: {error:?}")))?;
    let target_revision = prepared.candidate_pair();
    let active = kernel
        .apply(&prepared)
        .await
        .map_err(|error| failure(format!("scalar client apply failed: {error:?}")))?;
    let target_definition = standard
        .verified_snapshot()
        .catalogue()
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or_else(|| failure("installed standard is missing std.invoke.echo"))?;
    let target_type = match target_definition.return_type() {
        FunctionReturn::Single(ResolvedType::Scalar(orna_core::types::StandardScalar::Integer)) => {
            ResolvedType::Value(orna_standard::STD_INTEGER_TYPE_ID)
        }
        _ => return Err(failure("std.invoke.echo is not a single INTEGER result")),
    };
    let target = target_definition.id();
    let client_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["scalar_fixture", "call"])
        .ok_or_else(|| failure("scalar resource fixture is missing its CLIENT function"))?;
    require(
        client_definition.return_type() == &FunctionReturn::Single(target_type),
        "scalar CLIENT resource fixture did not retain the checked INTEGER result",
    )?;
    let client = client_definition.id();
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == client)
        .ok_or_else(|| failure("scalar CLIENT resource fixture is missing its revision"))?;
    let plan = ResourceClientPlan::decode(revision.artifact().payload())?;
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        return Err(failure(
            "scalar CLIENT resource plan is not an awaited resource",
        ));
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        return Err(failure(
            "scalar CLIENT resource plan is not a resource operation",
        ));
    };
    if !(operation.kind() == orna_artifact::client_plan::ResourceKind::Scalar
        && operation.target() == target
        && operation.target_revision() == target_revision
        && operation.arguments().len() == 1
        && operation.arguments()[0].0 == STD_INVOKE_ECHO_PARAMETER_ID
        && operation.arguments()[0].1 == (ClientExpressionNode::Integer { value: 43 }))
    {
        return Err(failure(format!(
            "scalar CLIENT resource plan metadata mismatch: kind={:?} target={:?}/{:?} revision={:?}/{:?} arguments={:?}",
            operation.kind(),
            operation.target(),
            target,
            operation.target_revision(),
            target_revision,
            operation.arguments(),
        )));
    }
    Ok((active, client, target, operation.call_site_id()))
}

async fn install_procedural_resource_client_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard: &CheckedStandardLibrary,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    ParameterId,
)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision,
                         body: &str|
     -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new(
                "resource_fixture.sql",
                body.to_owned(),
            )])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("procedural resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(
            active
                .source()
                .units()
                .iter()
                .enumerate()
                .map(|(ordinal, unit)| {
                    let content = if ordinal == last_ordinal {
                        format!("{}\n{}", unit.content(), body)
                    } else {
                        unit.content().to_owned()
                    };
                    SourceUnit::new(unit.logical_path(), content)
                }),
        )?)
    };
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let server_source = append_source(active, RAW_PROCEDURAL_RESOURCE_SERVER_SOURCE)?;
    let server_report = check_standard_application(&server_source, &context);
    if !server_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "procedural SERVER resource fixture did not compile: {:?}",
            server_report.diagnostics(),
        )));
    }
    let active = kernel
        .apply(
            &prepare_standard_application(&server_report, active.pair(), active)
                .map_err(|error| failure(format!("procedural server prepare failed: {error:?}")))?,
        )
        .await
        .map_err(|error| failure(format!("procedural server apply failed: {error:?}")))?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let client_source = append_source(&active, RAW_PROCEDURAL_RESOURCE_CLIENT_SOURCE)?;
    let client_report = check_standard_application(&client_source, &context);
    if !client_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "procedural CLIENT resource fixture did not compile: {:?}",
            client_report.diagnostics(),
        )));
    }
    let active = kernel
        .apply(
            &prepare_standard_application(&client_report, active.pair(), &active)
                .map_err(|error| failure(format!("procedural client prepare failed: {error:?}")))?,
        )
        .await
        .map_err(|error| failure(format!("procedural client apply failed: {error:?}")))?;
    let target_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["procedural_fixture", "resource"])
        .ok_or_else(|| failure("procedural resource fixture is missing its SERVER target"))?;
    let target_parameter = target_definition
        .parameters()
        .first()
        .ok_or_else(|| failure("procedural resource target is missing p_marker"))?
        .id();
    let target = target_definition.id();
    let client_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["procedural_fixture", "call"])
        .ok_or_else(|| failure("procedural resource fixture is missing its CLIENT function"))?;
    let target_type = match target_definition.return_type() {
        FunctionReturn::Stream(resolved_type) => *resolved_type,
        _ => return Err(failure("procedural resource target is not a TEXT stream")),
    };
    require(
        client_definition.return_type() == &FunctionReturn::Stream(target_type),
        "procedural CLIENT fixture did not retain the target-derived TEXT stream result",
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == client_definition.id())
        .ok_or_else(|| failure("procedural CLIENT fixture is missing its function revision"))?;
    let plan = ProceduralClientPlan::decode(revision.artifact().payload())?;
    require(
        plan.locals().len() == 1 && plan.statements().len() == 1,
        "procedural CLIENT artifact did not retain one resource LET",
    )?;
    let ClientExpressionNode::Resource { operation } = plan.statements()[0].expression() else {
        return Err(failure(
            "procedural CLIENT LET did not retain a resource operation",
        ));
    };
    let ClientExpressionNode::Await { expression } = plan.return_expression() else {
        return Err(failure("procedural CLIENT return did not retain AWAIT"));
    };
    let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
        return Err(failure(
            "procedural CLIENT AWAIT did not retain the resource local read",
        ));
    };
    require(
        *local == plan.locals()[0].local_id(),
        "procedural CLIENT AWAIT did not retain the declared resource local",
    )?;
    require(
        operation.kind() == orna_artifact::client_plan::ResourceKind::Stream
            && operation.target() == target
            && operation.target_revision() == active.pair()
            && operation.arguments().len() == 1
            && operation.arguments()[0].0 == target_parameter,
        "procedural CLIENT artifact lost stream target or pinned revision metadata",
    )?;
    let client_parameter = client_definition
        .parameters()
        .first()
        .ok_or_else(|| failure("procedural CLIENT fixture is missing p_marker"))?
        .id();
    let client = client_definition.id();
    Ok((active, client, target, client_parameter))
}

async fn install_stream_resource_client_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard: &CheckedStandardLibrary,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    ParameterId,
    CallSiteId,
)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision,
                         body: &str|
     -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new(
                "resource_fixture.sql",
                body.to_owned(),
            )])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("stream resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(
            active
                .source()
                .units()
                .iter()
                .enumerate()
                .map(|(ordinal, unit)| {
                    let content = if ordinal == last_ordinal {
                        format!("{}\n{}", unit.content(), body)
                    } else {
                        unit.content().to_owned()
                    };
                    SourceUnit::new(unit.logical_path(), content)
                }),
        )?)
    };
    let server_context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let server_source = append_source(active, RAW_STREAM_RESOURCE_SERVER_SOURCE)?;
    let server_report = check_standard_application(&server_source, &server_context);
    if !server_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "stream SERVER resource fixture did not compile: {:?}",
            server_report.diagnostics(),
        )));
    }
    let active = kernel
        .apply(
            &prepare_standard_application(&server_report, active.pair(), active)
                .map_err(|error| failure(format!("server prepare failed: {error:?}")))?,
        )
        .await
        .map_err(|error| failure(format!("server apply failed: {error:?}")))?;
    let client_context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let client_source = append_source(&active, RAW_STREAM_RESOURCE_CLIENT_SOURCE)?;
    let client_report = check_standard_application(&client_source, &client_context);
    if !client_report.diagnostics().is_empty() {
        let target_present = active
            .catalogue()
            .function_by_name(
                &QualifiedSemanticName::new(["resource_fixture", "resource"]).unwrap(),
            )
            .is_some();
        return Err(failure(format!(
            "stream CLIENT resource fixture did not compile: target_present={target_present}, units={:?}, diagnostics={:?}",
            active
                .source()
                .units()
                .iter()
                .map(|unit| (unit.logical_path(), unit.content().len()))
                .collect::<Vec<_>>(),
            client_report.diagnostics(),
        )));
    }
    let active = kernel
        .apply(
            &prepare_standard_application(&client_report, active.pair(), &active)
                .map_err(|error| failure(format!("client prepare failed: {error:?}")))?,
        )
        .await
        .map_err(|error| failure(format!("client apply failed: {error:?}")))?;
    let target = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "resource"])
        .ok_or_else(|| failure("resource fixture is missing resource_fixture.resource"))?;
    let target_parameter = target
        .parameters()
        .first()
        .ok_or_else(|| failure("resource fixture target is missing p_marker"))?
        .id();
    let target_result_type = match target.return_type() {
        FunctionReturn::Stream(resolved_type) => *resolved_type,
        _ => return Err(failure("resource fixture target is not a TEXT stream")),
    };
    let target = target.id();
    let client_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "call"])
        .ok_or_else(|| failure("stream CLIENT resource fixture is missing resource.call"))?;
    require(
        client_definition.return_type() == &FunctionReturn::Stream(target_result_type),
        "stream CLIENT resource fixture did not retain the checked STREAM<TEXT> result",
    )?;
    let client = client_definition.id();
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == client)
        .ok_or_else(|| failure("stream CLIENT resource fixture is missing its revision"))?;
    let plan = ResourceClientPlan::decode(revision.artifact().payload())?;
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        return Err(failure(
            "stream CLIENT resource plan is not an awaited resource",
        ));
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        return Err(failure(
            "stream CLIENT resource plan is not a resource operation",
        ));
    };
    require(
        operation.kind() == orna_artifact::client_plan::ResourceKind::Stream
            && operation.target() == target
            && operation.target_revision() == active.pair()
            && operation.arguments().len() == 1
            && operation.arguments()[0].0 == target_parameter,
        "stream CLIENT resource plan did not retain canonical target metadata",
    )?;

    Ok((
        active,
        client,
        target,
        target_parameter,
        operation.call_site_id(),
    ))
}

async fn install_expression_client_fixture(
    kernel: &PostgresKernel,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    FunctionId,
)> {
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let schema = SourceBundle::new([SourceUnit::new("schema.orna", RAW_CLIENT_SCHEMA_SOURCE)])?;
    let report = check(&schema, empty.catalogue());
    require(
        report.diagnostics().is_empty(),
        "expression CLIENT fixture schema did not compile",
    )?;
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let standard_upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&standard_upgrade).await?;
    let context = StandardApplicationCheckContext::try_new(
        version_two.catalogue(),
        standard_upgrade.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new(
        "expression.orna",
        RAW_EXPRESSION_CLIENT_FUNCTION_SOURCE,
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "expression CLIENT fixture functions did not compile",
    )?;
    let active = kernel
        .apply(&prepare_standard_application(
            &report,
            version_two.pair(),
            &version_two,
        )?)
        .await?;
    let function_id = |parts: &[&str]| {
        active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == parts)
            .map(FunctionDefinition::id)
    };
    let literal = function_id(&["expr", "literal"])
        .ok_or_else(|| failure("expression CLIENT fixture is missing expr.literal"))?;
    let composed = function_id(&["expr", "composed"])
        .ok_or_else(|| failure("expression CLIENT fixture is missing expr.composed"))?;
    let external = function_id(&["expr", "external"])
        .ok_or_else(|| failure("expression CLIENT fixture is missing expr.external"))?;
    Ok((active, literal, composed, external))
}
#[cfg(feature = "test-hooks")]
async fn install_action_client_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    standard: &CheckedStandardLibrary,
) -> TestResult<(
    orna_core::revision::ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    ParameterId,
    ParameterId,
    FunctionId,
    FunctionId,
    ParameterId,
    ParameterId,
)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision,
                         body: &str|
     -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new(
                "action_fixture.orna",
                body.to_owned(),
            )])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("action fixture has no retained source unit"))?;
        Ok(SourceBundle::new(
            active
                .source()
                .units()
                .iter()
                .enumerate()
                .map(|(ordinal, unit)| {
                    let content = if ordinal == last_ordinal {
                        format!("{}\n{}", unit.content(), body)
                    } else {
                        unit.content().to_owned()
                    };
                    SourceUnit::new(unit.logical_path(), content)
                }),
        )?)
    };
    let (server_source_body, client_source_body) = action_source_parts()?;
    let server_source = append_source(active, server_source_body)?;
    let server_context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let server_report = check_standard_application(&server_source, &server_context);
    if !server_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "SERVER action fixture did not compile: {:?}",
            server_report.diagnostics()
        )));
    }
    let active = kernel
        .apply(&prepare_standard_application(
            &server_report,
            active.pair(),
            active,
        )?)
        .await?;
    let client_source = append_source(&active, client_source_body)?;
    let client_context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
    let client_report = check_standard_application(&client_source, &client_context);
    if !client_report.diagnostics().is_empty() {
        return Err(failure(format!(
            "CLIENT action fixture did not compile: {:?}",
            client_report.diagnostics()
        )));
    }
    let active = kernel
        .apply(&prepare_standard_application(
            &client_report,
            active.pair(),
            &active,
        )?)
        .await?;
    let target_definition = standard
        .verified_snapshot()
        .catalogue()
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or_else(|| failure("installed standard is missing std.invoke.echo SERVER target"))?;
    let target_parameter = target_definition
        .parameter_by_name("p_value")
        .ok_or_else(|| failure("action fixture SERVER target is missing p_value"))?
        .id();
    let client_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "call"])
        .ok_or_else(|| failure("action fixture is missing its CLIENT function"))?;
    let client = client_definition.id();
    let client_parameter = client_definition
        .parameter_by_name("p_value")
        .ok_or_else(|| failure("action fixture CLIENT function is missing p_value"))?
        .id();
    let target = target_definition.id();
    let local_target_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "local"])
        .ok_or_else(|| failure("action fixture is missing its local CLIENT target"))?;
    let local_target = local_target_definition.id();
    let local_target_parameter = local_target_definition
        .parameter_by_name("p_value")
        .ok_or_else(|| failure("action fixture local CLIENT target is missing p_value"))?
        .id();
    let local_client_definition = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "call_local"])
        .ok_or_else(|| failure("action fixture is missing its local action CLIENT function"))?;
    let local_client = local_client_definition.id();
    let local_client_parameter = local_client_definition
        .parameter_by_name("p_value")
        .ok_or_else(|| failure("action fixture local action CLIENT function is missing p_value"))?
        .id();
    Ok((
        active,
        client,
        target,
        client_parameter,
        target_parameter,
        local_client,
        local_target,
        local_client_parameter,
        local_target_parameter,
    ))
}

async fn install_external_capability_fixture(
    kernel: &PostgresKernel,
) -> TestResult<(orna_core::revision::ActiveDatabaseRevision, FunctionId)> {
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let schema = SourceBundle::new([SourceUnit::new("schema.orna", RAW_CLIENT_SCHEMA_SOURCE)])?;
    let report = check(&schema, empty.catalogue());
    require(
        report.diagnostics().is_empty(),
        "external capability fixture schema did not compile",
    )?;
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let standard_upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&standard_upgrade).await?;
    let context = StandardApplicationCheckContext::try_new(
        version_two.catalogue(),
        standard_upgrade.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new(
        "external-capability.orna",
        RAW_EXTERNAL_CAPABILITY_SOURCE,
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "external capability CLIENT fixture did not compile",
    )?;
    let active = kernel
        .apply(&prepare_standard_application(
            &report,
            version_two.pair(),
            &version_two,
        )?)
        .await?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["cap", "read"])
        .ok_or_else(|| failure("external capability fixture is missing cap.read"))?
        .id();
    Ok((active, function))
}

/// Installs `orna.std/3` as the active standard from the empty base (ADR 0057
/// step 10).
///
/// The live proof uses a test-only empty-base/source-chain seeding seam because
/// the retained V3 snapshot requires its V1 and V2 source parents. The
/// compiler-backed V2-to-V3 upgrade remains the production path; this helper
/// prepares the same V3 snapshot through the test-hooks persistence seam so
/// the sealed route's opaque codec registry binds the
/// `std.terminal.Document` and `std.io.ByteStream` codecs.
async fn install_v3_standard(
    kernel: &PostgresKernel,
    empty: &ActiveDatabaseRevision,
    database: &TestDatabase,
) -> TestResult<ActiveDatabaseRevision> {
    seed_standard_source_chain(database).await?;
    let snapshot = retained_standard_library_v3_snapshot()?;
    let verified = verify_standard_library_v3_snapshot(snapshot)?;
    let candidate = v3_standard_upgrade_candidate(empty, &verified)?;
    Ok(kernel
        .apply_test_standard_upgrade(&candidate, &verified)
        .await?)
}

/// Installs the accepted V5 standard through the compiler-backed V3-to-V4
/// and V4-to-V5 append-only upgrade path.
async fn install_v5_standard(
    kernel: &PostgresKernel,
    empty: &ActiveDatabaseRevision,
    database: &TestDatabase,
) -> TestResult<ActiveDatabaseRevision> {
    let version_three = install_v3_standard(kernel, empty, database).await?;
    let upgrade_v4 = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = kernel.apply_standard_upgrade(&upgrade_v4).await?;
    let upgrade_v5 = orna_standard::prepare_standard_upgrade_v4_to_v5(&version_four)?;
    Ok(kernel.apply_standard_upgrade(&upgrade_v5).await?)
}

/// Persists the retained `orna.std/1` and `orna.std/2` source bundles and
/// revisions so the V3 snapshot's reserved source parent chain satisfies the
/// kernel's foreign keys.
///
/// The V3 source revision descends from the V2 source revision, whose parent
/// is the V1 source revision. The standard-install persistence path inserts
/// those rows in the same transaction only for the V1 edge of a V2 install
/// (`persist_retained_v1_standard_parent`), so a V3 install from the empty
/// base must seed the two historical source records exactly as the accepted
/// V2 install would persist them. Only the source bundles and revisions are
/// seeded; the kernel's V3 persist inserts the V3 bundle, units, and revision
/// rows itself.
async fn seed_standard_source_chain(database: &TestDatabase) -> TestResult<()> {
    let version_one = verify_standard_library_snapshot(retained_standard_library_snapshot()?)?;
    let version_two =
        verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let sources = [version_one.source(), version_two.source()];
    let session = database.open().await?;
    for source in sources {
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles
                    (id, content_hash, hash_algorithm, hash_contract_version)
                 VALUES ($1, $2, 'sha256', 1)",
                &[
                    &source.bundle().to_bytes().to_vec(),
                    &source.bundle_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    for source in sources {
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash,
                     hash_algorithm, hash_contract_version)
                 VALUES ($1, $2, $3, $4, 'sha256', 1)",
                &[
                    &source.id().to_bytes().to_vec(),
                    &source.parent().map(|parent| parent.to_bytes().to_vec()),
                    &source.bundle().to_bytes().to_vec(),
                    &source.revision_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    session.shutdown().await
}

/// Builds the V3 standard-upgrade application candidate for the empty base.
///
/// The candidate mirrors the compiler's standard-upgrade lowering exactly:
/// fresh non-reserved application source and catalogue identities, the
/// active (empty) source units, no functions, origins, or references, and
/// the verified V3 snapshot pinned through the version-two catalogue hash
/// context. The kernel persists the standard's own retained rows alongside
/// the candidate.
fn v3_standard_upgrade_candidate(
    empty: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let source_bundle = SourceBundleId::from_bytes([0xb1; 16]);
    let source_revision = SourceRevisionId::from_bytes([0xb2; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        Some(empty.pair().source()),
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, Some(empty.pair().source()), bundle_hash)?,
    )?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0xb3; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            empty.pair(),
            source,
            empty.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new())
                .with_current_function_revisions(Vec::new()),
        ),
        context,
    )?)
}

async fn install_opaque_client_fixture(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    checked_standard: &orna_compiler::CheckedStandardLibrary,
    function: FunctionId,
    payload: [u8; 16],
) -> TestResult<orna_core::revision::ActiveDatabaseRevision> {
    let last_ordinal = active
        .source()
        .units()
        .len()
        .checked_sub(1)
        .ok_or_else(|| failure("opaque CLIENT fixture has no retained source unit"))?;
    let source = SourceBundle::new(active.source().units().iter().enumerate().map(
        |(ordinal, unit)| {
            let content = if ordinal == last_ordinal {
                format!("{}\n", unit.content())
            } else {
                unit.content().to_owned()
            };
            SourceUnit::new(unit.logical_path(), content)
        },
    ))?;
    let report = check_standard_application(
        &source,
        &StandardApplicationCheckContext::try_new(active.catalogue(), checked_standard)?,
    );
    require(
        report.diagnostics().is_empty(),
        "opaque CLIENT source-only precursor did not compile",
    )?;
    let precursor = prepare_standard_application(&report, active.pair(), active)?;
    require(
        precursor.new_function_revisions().is_empty(),
        "opaque CLIENT source-only precursor changed executable semantics",
    )?;
    let previous = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function)
        .ok_or_else(|| failure("opaque CLIENT fixture is missing its prior function revision"))?;
    let function_origin = precursor
        .origins()
        .iter()
        .find_map(|origin| {
            (origin.identity() == DefinitionIdentity::Function(function)).then_some(origin.source())
        })
        .ok_or_else(|| failure("opaque CLIENT fixture is missing its function origin"))?;
    let function_revision = FunctionRevisionId::from_bytes([0x78; 16]);
    require(
        active
            .function_revisions()
            .iter()
            .all(|revision| revision.id() != function_revision),
        "opaque CLIENT fixture revision identity collides with active state",
    )?;
    let prior_definition = precursor
        .candidate()
        .function_by_id(function)
        .ok_or_else(|| failure("opaque CLIENT fixture is missing its function definition"))?;
    let opaque_definition = FunctionDefinition::new(
        function,
        prior_definition.name().clone(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::value(OPAQUE_TOKEN_TYPE_ID)),
        function_revision,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let functions = precursor
        .candidate()
        .functions()
        .iter()
        .map(|definition| {
            if definition.id() == function {
                opaque_definition.clone()
            } else {
                definition.clone()
            }
        })
        .collect();
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        precursor.candidate().revision(),
        precursor.candidate().schemas().to_vec(),
        precursor.candidate().object_types().to_vec(),
        precursor.candidate().value_types().to_vec(),
        precursor.candidate().enum_types().to_vec(),
        precursor.candidate().record_value_types().to_vec(),
        precursor.candidate().type_bindings().to_vec(),
        functions,
    )?;
    let reference = DefinitionReference::new(
        function,
        function_revision,
        0,
        DefinitionReferenceTarget::ValueType(OPAQUE_TOKEN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        function_origin,
    );
    let plan = OpaqueClientPlan::return_opaque(OPAQUE_TOKEN_TYPE_ID, payload).encode()?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        OPAQUE_FORMAT_VERSION,
        plan.clone(),
        artifact_payload_digest(&plan)?,
    )?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &opaque_definition,
        previous.language_version(),
        &artifact,
        precursor.expressions(),
        std::slice::from_ref(&reference),
    )?;
    let opaque_revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        previous.revision_number() + 1,
        function_origin,
        previous.declaration_content_hash(),
        semantic_hash,
        previous.language_version(),
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let current_revisions = precursor
        .current_function_revisions()
        .ok_or_else(|| failure("opaque CLIENT precursor omitted current revision evidence"))?
        .iter()
        .map(|revision| {
            if revision.function() == function {
                opaque_revision.clone()
            } else {
                revision.clone()
            }
        })
        .collect::<Vec<_>>();
    let mut references = precursor
        .references()
        .iter()
        .filter(|candidate| candidate.source_function() != function)
        .cloned()
        .collect::<Vec<_>>();
    references.push(reference);
    let context = precursor.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        &current_revisions,
        precursor.expressions(),
        precursor.origins(),
        &references,
    )?;
    let candidate = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            precursor.expected_base(),
            precursor.source().clone(),
            precursor.parent_catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(
                precursor.origins().to_vec(),
                precursor.expressions().to_vec(),
                vec![opaque_revision],
                references,
            )
            .with_current_function_revisions(current_revisions),
        ),
        context,
    )?;
    Ok(kernel.apply(&candidate).await?)
}

fn raw_client_record(
    active: &orna_core::revision::ActiveDatabaseRevision,
) -> TestResult<RuntimeValue> {
    let record_type = active
        .catalogue()
        .record_value_types()
        .first()
        .ok_or_else(|| failure("raw CLIENT fixture is missing its record value type"))?;
    let enum_type = active
        .catalogue()
        .enum_types()
        .first()
        .ok_or_else(|| failure("raw CLIENT fixture is missing its enum type"))?;
    Ok(RuntimeValue::Record(RecordValue::new(
        active,
        record_type.id(),
        [(
            "stage".to_owned(),
            RuntimeValue::Enum(EnumValue::new(
                active.catalogue(),
                enum_type.id(),
                "qualified",
            )?),
        )],
    )?))
}

async fn insert_raw_server_flag(
    database: &TestDatabase,
    active: &orna_core::revision::ActiveDatabaseRevision,
    object_byte: u8,
    value: bool,
) -> TestResult<()> {
    let object = active
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["app", "flag"])
        .ok_or_else(|| failure("raw SERVER fixture is missing app.flag"))?;
    let field = object
        .fields()
        .iter()
        .find(|field| field.name() == "value")
        .ok_or_else(|| failure("raw SERVER fixture is missing app.flag.value"))?;
    let table = format!("t_{:032x}", u128::from_be_bytes(object.id().to_bytes()));
    let column = format!("f_{:032x}", u128::from_be_bytes(field.id().to_bytes()));
    let object_id = format!("{:032x}", u128::from_be_bytes([object_byte; 16]));
    run_database_statement(
        database,
        &format!(
            "INSERT INTO _orna_data.{table} (_orna_object_id, {column}) VALUES (decode('{object_id}', 'hex'), {value})"
        ),
    )
    .await
}

async fn insert_expression_item_row(
    database: &TestDatabase,
    active: &orna_core::revision::ActiveDatabaseRevision,
    object_type: TypeId,
    title_field: orna_core::FieldId,
    object_id: ObjectId,
    title: &str,
) -> TestResult<()> {
    let object = active
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.id() == object_type)
        .ok_or_else(|| failure("expression CLIENT fixture object identity is not active"))?;
    let field = object
        .fields()
        .iter()
        .find(|field| field.id() == title_field)
        .ok_or_else(|| failure("expression CLIENT fixture field identity is not active"))?;
    let table = format!("t_{:032x}", u128::from_be_bytes(object.id().to_bytes()));
    let column = format!("f_{:032x}", u128::from_be_bytes(field.id().to_bytes()));
    let statement =
        format!("INSERT INTO _orna_data.{table} (_orna_object_id, {column}) VALUES ($1, $2)");
    let session = database.open().await?;
    let object_bytes = object_id.to_bytes().to_vec();
    let operation: TestResult<()> = async {
        session
            .client()
            .execute(&statement, &[&object_bytes, &title])
            .await?;
        Ok(())
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "expression CLIENT object insert",
    )
}

async fn run_database_statement(database: &TestDatabase, statement: &str) -> TestResult<()> {
    let session = database.open().await?;
    let operation = session
        .client()
        .batch_execute(statement)
        .await
        .map_err(Into::into);
    finish_session(
        operation,
        session.shutdown().await,
        "raw CLIENT test statement",
    )
}

async fn security_audit_count(database: &TestDatabase) -> TestResult<i64> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.security_audit_events",
                &[],
            )
            .await?;
        Ok(row.try_get(0)?)
    }
    .await;
    finish_session(operation, session.shutdown().await, "security audit count")
}

#[cfg(feature = "test-hooks")]
async fn assert_resource_audit_rows(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    call_site: CallSiteId,
    target: FunctionId,
    all_target: FunctionId,
    expected_parent: InvocationId,
) -> TestResult<()> {
    let audit_session = database.open().await?;
    let audit_operation = async {
        let rows = audit_session
            .client()
            .query(
                "SELECT resource.request_id, resource.parent_invocation_id,
                        resource.call_site_id, resource.nested_invocation_id,
                        resource.target_function_id, resource.source_revision_id,
                        resource.catalogue_revision_id, resource.session_principal_id,
                        resource.decision_outcome, resource.terminal_outcome,
                        resource.item_count, resource.byte_count,
                        invocation.outcome AS invocation_outcome
                 FROM _orna_kernel.resource_audit_events AS resource
                 LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                   ON invocation.invocation_id = resource.nested_invocation_id
                 ORDER BY resource.request_id",
                &[],
            )
            .await?;
        require(
            rows.len() == 7,
            "resource audit did not retain one terminal row for each request",
        )?;
        let expected = [
            ([0x51; 16], Some(all_target), "allowed", "completed", Some(2_i64), Some(80_i64)),
            ([0x53; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
            ([0x55; 16], Some(all_target), "allowed", "completed", Some(2_i64), Some(80_i64)),
            ([0x57; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
            ([0x61; 16], Some(all_target), "allowed", "cancelled", None, None),
            ([0x71; 16], Some(target), "denied", "failed", None, None),
            ([0x81; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
        ];
        for (index, row) in rows.iter().enumerate() {
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("catalogue_revision_id")?;
            let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
            let (request, target, decision, terminal, count, bytes) = expected[index];
            let target = target.map(|function| function.to_bytes().to_vec());
            let accepted = decision == "allowed";
            let nested_identity_matches = match (&nested_invocation_id, accepted) {
                (Some(nested), true) => nested.len() == 16,
                (None, false) => true,
                _ => false,
            };
            require(
                request_id == request
                    && parent_invocation_id == expected_parent.to_bytes().to_vec()
                    && call_site_id == call_site.to_bytes().to_vec()
                    && nested_identity_matches
                    && target_function_id == target
                    && source_revision_id
                        == target.as_ref().map(|_| active.pair().source().to_bytes().to_vec())
                    && catalogue_revision_id
                        == target
                            .as_ref()
                            .map(|_| active.pair().catalogue().to_bytes().to_vec())
                    && session_principal_id == RAW_CLIENT_USER.to_bytes().to_vec()
                    && decision_outcome == decision
                    && terminal_outcome == terminal
                    && item_count == count
                    && byte_count == bytes
                    && invocation_outcome.as_deref() == accepted.then_some(decision),
                "resource audit row did not preserve exact request correlation and terminal outcome",
            )?;
        }
        Ok(())
    }
    .await;
    finish_session(
        audit_operation,
        audit_session.shutdown().await,
        "resource audit correlation",
    )
}

async fn require_no_database_sessions(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        // PostgreSQL can retain a just-closed backend row briefly after the
        // client driver has completed its shutdown handshake.
        match tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let row = session
                    .client()
                    .query_one(
                        "SELECT count(*) FROM pg_stat_activity
                         WHERE datname = current_database() AND pid <> pg_backend_pid()",
                        &[],
                    )
                    .await?;
                let count: i64 = row.try_get(0)?;
                if count == 0 {
                    return Ok::<(), Box<dyn Error + Send + Sync>>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(failure(
                "raw CLIENT dispatch leaked a PostgreSQL database session",
            )),
        }
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "database session leak check",
    )
}

/// Builds one complete checked `sys.invoke` Request for a no-argument scalar CLIENT fixture.
fn sealed_scalar_resource_request(client: FunctionId) -> TestResult<InvokeRequest> {
    sealed_request(InvocationRequestTarget::function_id(client), Vec::new())
}

/// Builds one complete checked `sys.invoke` Request for `std.invoke.echo`.
fn sealed_echo_request(
    target: InvocationRequestTarget,
    selector: InvocationParameterSelector,
    value: i32,
) -> TestResult<InvokeRequest> {
    sealed_request(
        target,
        vec![InvocationArgument::new(
            selector,
            InvokeValue::new(RuntimeValue::Integer(value))?,
        )],
    )
}

fn sealed_request(
    target: InvocationRequestTarget,
    arguments: Vec<InvocationArgument>,
) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target,
        arguments,
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

/// Creates an authenticated parent invocation for resource provenance tests.
async fn create_authenticated_parent_invocation(
    kernel: &PostgresKernel,
    active: &ActiveDatabaseRevision,
    session: &AuthenticatedSession,
    request: InvokeRequest,
) -> TestResult<InvocationId> {
    let registry = active
        .catalogue_hash_context()
        .standard()
        .map(registered_opaque_codecs)
        .transpose()?
        .ok_or_else(|| failure("the parent invocation requires the verified standard snapshot"))?;
    let retained = encode_invoke_request(active, &registry, &request)?;
    let kernel = kernel.clone();
    let session = session.clone();
    let worker = std::thread::Builder::new()
        .name("orna-test-parent-invocation".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || -> TestResult<SealedInvocationResult> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| failure(format!("parent invocation runtime failed: {error}")))?;
            Ok(runtime.block_on(kernel.dispatch_sealed_sys_invoke(&session, 5, &retained))?)
        })
        .map_err(|error| failure(format!("parent invocation thread failed: {error}")))?;
    let result = worker
        .join()
        .map_err(|_| failure("parent invocation thread panicked"))??;
    match result {
        SealedInvocationResult::Completed { invocation, .. } => Ok(invocation),
        result => Err(failure(format!(
            "the authenticated parent invocation did not complete: {result:?}",
        ))),
    }
}

/// Builds one complete checked `sys.invoke` Request for `std.json.encode`.
fn sealed_json_encode_request(value: OpaqueValue) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(STD_JSON_ENCODE_FUNCTION_ID),
        arguments: vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(STD_JSON_ENCODE_PARAMETER_ID),
            InvokeValue::new(RuntimeValue::Opaque(value))?,
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

/// Asserts one completed sealed JSON presenter invocation returned the exact
/// application/json ByteStream payload.
fn require_json_encode_completion(
    result: &SealedInvocationResult,
    expected_payload: &[u8],
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed JSON presenter invocation did not complete with its Event batch",
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
        "the sealed JSON presenter stream did not carry contiguous sequences",
    )?;
    require(
        records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
        "the sealed JSON presenter stream did not carry the expected event kinds",
    )?;
    let InvocationEventBody::ValueBatch {
        schema: None,
        values,
    } = records[1].event().body()
    else {
        return Err(failure(
            "the sealed JSON presenter ValueBatch did not carry a plain typed batch",
        ));
    };
    require(
        values.len() == 1,
        "the sealed JSON presenter ValueBatch did not carry one result",
    )?;
    let RuntimeValue::Opaque(value) = values[0].value() else {
        return Err(failure(
            "the sealed JSON presenter result was not an opaque ByteStream",
        ));
    };
    require(
        value.opaque_type() == STD_IO_BYTE_STREAM_TYPE_ID
            && value.canonical_payload() == expected_payload,
        "the sealed JSON presenter did not return the exact application/json ByteStream",
    )?;
    require(
        records[0].event().invocation_id() == *invocation
            && records[1].event().invocation_id() == *invocation
            && records[2].event().invocation_id() == *invocation,
        "the sealed JSON presenter events did not share one invocation identity",
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
    finish_session(
        operation,
        session.shutdown().await,
        "invocation audit row read",
    )
}

async fn invocation_audit_count(database: &TestDatabase) -> TestResult<i64> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?;
        Ok(row.try_get(0)?)
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "invocation audit count",
    )
}

/// Returns whether one invocation audit row links exactly the supplied
/// security audit event identity.
async fn invocation_audit_security_link(
    database: &TestDatabase,
    invocation: InvocationId,
    expected: Option<[u8; 16]>,
) -> TestResult<bool> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT security_audit_event_id
                 FROM _orna_kernel.invocation_audit_events
                 WHERE invocation_id = $1",
                &[&invocation.to_bytes().to_vec()],
            )
            .await?;
        let actual: Option<Vec<u8>> = row.try_get(0)?;
        Ok(actual == expected.map(|bytes| bytes.to_vec()))
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "invocation audit security link read",
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
    finish_session(
        operation,
        session.shutdown().await,
        "standard authority row read",
    )
}

async fn recovery_error(database: &TestDatabase) -> TestResult<PostgresKernelError> {
    match kernel(database)?.recover().await {
        Ok(_) => Err(failure("tampered durable state recovered successfully")),
        Err(error) => Ok(error),
    }
}

fn id_hex(bytes: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(bytes))
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(database.connection_string().parse()?)
}

async fn active_pointer(database: &TestDatabase) -> TestResult<(Vec<u8>, Vec<u8>)> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id FROM _orna_kernel.active_revision",
                &[],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "active pointer inspection",
    )
}

async fn boolean_contract(
    database: &TestDatabase,
    standard_revision: Vec<u8>,
    boolean_type: Vec<u8>,
    replacement: Option<&str>,
) -> TestResult<String> {
    let session = database.open().await?;
    let operation = async {
        if let Some(replacement) = replacement {
            let affected = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET representation_contract = $3
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &boolean_type, &replacement],
                )
                .await?;
            require(
                affected == 1,
                "the standard Boolean contract tamper did not select exactly one row",
            )?;
        }
        read_boolean_contract_from_client(session.client(), &standard_revision, &boolean_type).await
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "standard Boolean contract operation",
    )
}

async fn read_boolean_contract_from_client(
    client: &tokio_postgres::Client,
    standard_revision: &[u8],
    boolean_type: &[u8],
) -> TestResult<String> {
    let row = client
        .query_one(
            "SELECT representation_contract
             FROM _orna_kernel.standard_catalogue_value_types
             WHERE standard_library_revision_id = $1 AND type_id = $2",
            &[&standard_revision, &boolean_type],
        )
        .await?;
    Ok(row.try_get(0)?)
}

fn finish_session<T>(
    operation: TestResult<T>,
    shutdown: TestResult<()>,
    label: &str,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(shutdown_error)) => Err(failure(format!(
            "{label} failed: {operation_error}; connection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn offline_empty_version_two_active(
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<ActiveDatabaseRevision> {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("")?,
    )?;
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit))?;
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)?,
    )?;
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )?;
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;
    Ok(ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )?)
}

fn offline_active_from_prepared(
    prepared: &DeployableRevision,
) -> TestResult<ActiveDatabaseRevision> {
    let current_function_revisions = prepared
        .current_function_revisions()
        .ok_or_else(|| failure("offline prepared expression fixture has no current revisions"))?
        .to_vec();
    let context = prepared.catalogue_hash_context().clone();
    Ok(ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                current_function_revisions,
                prepared.origins().to_vec(),
                prepared.references().to_vec(),
            ),
        ),
        context,
    )?)
}

#[test]
fn checks_accepted_scalar_resource_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/scalar_resource_dogfood.orna",
        include_str!("fixtures/scalar_resource_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "accepted scalar resource fixture did not check",
    )?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted scalar resource fixture produced no checked bundle"))?;
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["scalar_fixture", "call"]),
        "accepted scalar resource fixture is missing scalar_fixture.call",
    )
}

#[test]
fn checks_accepted_stream_resource_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_dogfood.orna",
        include_str!("fixtures/stream_resource_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        "accepted stream resource fixture did not check",
    )?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted stream resource fixture produced no checked bundle"))?;
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["stream_fixture", "read"]),
        "accepted stream resource fixture is missing stream_fixture.read",
    )
}
#[test]
fn checks_accepted_expression_client_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/expression_client_dogfood.orna",
        include_str!("fixtures/expression_client_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted expression CLIENT fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted expression CLIENT fixture produced no checked bundle"))?;
    for name in [
        "literal",
        "composed",
        "ref_composed",
        "param_composed",
        "external",
    ] {
        require(
            checked
                .client_functions()
                .any(|function| function.name().parts() == ["expr", name]),
            "accepted expression CLIENT fixture is missing a declared function",
        )?;
    }
    let composed = checked
        .client_functions()
        .find(|function| function.name().parts() == ["expr", "composed"])
        .ok_or_else(|| failure("accepted expression CLIENT fixture is missing expr.composed"))?;
    require(
        composed
            .references()
            .iter()
            .any(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall),
        "accepted expression CLIENT fixture did not retain expr.literal as a function call reference",
    )?;
    let external = checked
        .client_functions()
        .find(|function| function.name().parts() == ["expr", "external"])
        .ok_or_else(|| failure("accepted expression CLIENT fixture is missing expr.external"))?;
    require(
        external.references().is_empty(),
        "accepted external CLIENT contract unexpectedly retained executable references",
    )?;

    let ref_composed = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "ref_composed"])
        .ok_or_else(|| {
            failure("prepared expression CLIENT fixture is missing expr.ref_composed")
        })?;
    let item_type = active
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.name().parts() == ["expr", "item"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.item"))?;
    let title_field = item_type
        .field_by_name("title")
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.item.title"))?;
    let ref_parameter = ref_composed
        .parameters()
        .first()
        .ok_or_else(|| failure("prepared expr.ref_composed is missing p_item"))?;
    require(
        ref_parameter.resolved_type() == ResolvedType::reference(item_type.id()),
        "prepared expr.ref_composed lost its REF expr.item parameter type",
    )?;
    require(
        active.references().iter().any(|reference| {
            reference.source_function() == ref_composed.id()
                && reference.kind() == DefinitionReferenceKind::ObjectReference
                && reference.target() == DefinitionReferenceTarget::ObjectType(item_type.id())
        }),
        "prepared expr.ref_composed lost its object-reference metadata",
    )?;
    let ref_revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == ref_composed.id())
        .ok_or_else(|| failure("prepared expr.ref_composed is missing its function revision"))?;
    require(
        ref_revision.artifact().version() == EXPRESSION_FORMAT_VERSION,
        "prepared expr.ref_composed did not produce a version-three expression plan",
    )?;
    let ref_plan = ExpressionClientPlan::decode(ref_revision.artifact().payload())?;
    let ClientExpressionNode::Concat { left, right } = ref_plan.expression() else {
        return Err(failure(
            "expr.ref_composed plan lost its outer concatenation",
        ));
    };
    let ClientExpressionNode::Concat {
        left: field_path,
        right: bang,
    } = left.as_ref()
    else {
        return Err(failure(
            "expr.ref_composed plan lost its left-associative concatenation",
        ));
    };
    let ClientExpressionNode::FieldPath { root, fields } = field_path.as_ref() else {
        return Err(failure("expr.ref_composed plan lost its REF field path"));
    };
    require(
        *root == ref_parameter.id() && fields.len() == 1 && fields[0] == title_field.id(),
        "expr.ref_composed plan did not retain p_item.title field identity",
    )?;
    require(
        matches!(bang.as_ref(), ClientExpressionNode::String { value } if value == "!"),
        "expr.ref_composed plan lost the first suffix",
    )?;
    require(
        matches!(right.as_ref(), ClientExpressionNode::String { value } if value == "?"),
        "expr.ref_composed plan lost the second suffix",
    )?;
    let literal_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "literal"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.literal"))?
        .id();
    let composed_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "composed"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.composed"))?
        .id();
    let param_composed_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "param_composed"])
        .ok_or_else(|| {
            failure("prepared expression CLIENT fixture is missing expr.param_composed")
        })?
        .id();
    let external_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["expr", "external"])
        .ok_or_else(|| failure("prepared expression CLIENT fixture is missing expr.external"))?
        .id();
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![
            ExecuteGrant::new(RAW_CLIENT_USER, literal_id),
            ExecuteGrant::new(RAW_CLIENT_USER, composed_id),
            ExecuteGrant::new(RAW_CLIENT_USER, param_composed_id),
            ExecuteGrant::new(RAW_CLIENT_USER, external_id),
        ],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let composed_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(composed_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT composed authorisation was denied: {reason:?}"
            )));
        }
    };
    let composed_result = evaluate_client_function(&active, &composed_authorisation)?;
    require(
        composed_result.value() == &RuntimeValue::Text("hello world".to_owned()),
        "offline expression CLIENT composed evaluation returned the wrong value",
    )?;
    let param_authorisation = match security.authorise_execute(
        &session,
        InvocationTarget::new(param_composed_id, active.pair()),
    ) {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT param_composed authorisation was denied: {reason:?}"
            )));
        }
    };
    let param_function = active
        .catalogue()
        .function_by_id(param_composed_id)
        .ok_or_else(|| failure("prepared expression CLIENT fixture lost expr.param_composed"))?;
    let param_parameter = param_function
        .parameters()
        .first()
        .ok_or_else(|| failure("prepared expr.param_composed is missing p_suffix"))?;
    let param_argument = FunctionArgument::new(
        param_parameter.id(),
        RuntimeValue::Text(" world".to_owned()),
    )?;
    let param_result = evaluate_client_function_with_grants_and_arguments(
        &active,
        &param_authorisation,
        std::slice::from_ref(&param_argument),
        &[],
        &LocalCapabilityGrantSet::new(),
    )?;
    require(
        param_result.value() == &RuntimeValue::Text("hello world".to_owned()),
        "offline parameterized CLIENT evaluation returned the wrong typed result",
    )?;
    let literal_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(literal_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline expression CLIENT literal authorisation was denied: {reason:?}"
            )));
        }
    };
    let literal_result = evaluate_client_function(&active, &literal_authorisation)?;
    require(
        literal_result.value() == &RuntimeValue::Text("hello".to_owned()),
        "offline expression CLIENT literal evaluation returned the wrong value",
    )?;
    require(
        literal_result.context().function() == literal_id
            && literal_result.context().pair() == active.pair(),
        "offline expression CLIENT literal result retained the wrong invocation context",
    )?;
    let external_authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(external_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline external CLIENT authorisation was denied: {reason:?}"
            )));
        }
    };
    let external_error = evaluate_client_function(&active, &external_authorisation)
        .expect_err("offline external CLIENT evaluation unexpectedly completed");
    require(
        matches!(
            external_error,
            ClientExecutionError::ExternalContract { identity, .. }
                if identity == "expr.runtime@1"
        ),
        "offline external CLIENT evaluation did not fail closed on expr.runtime@1",
    )
}

#[test]
fn checks_accepted_client_state_fixture_plan_metadata_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let upgrade_v3 = orna_standard::prepare_standard_upgrade_v2_to_v3(&base)?;
    let version_three = offline_active_from_prepared(upgrade_v3.application_revision())?;
    let upgrade_v4 = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = offline_active_from_prepared(upgrade_v4.application_revision())?;
    let context = StandardApplicationCheckContext::try_new(
        version_four.catalogue(),
        upgrade_v4.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_state_dogfood.orna",
        include_str!("fixtures/client_state_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT state fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }

    let prepared = prepare_standard_application(&report, version_four.pair(), &version_four)?;
    let active = offline_active_from_prepared(&prepared)?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["state_fixture", "scalar"])
        .ok_or_else(|| failure("prepared CLIENT state fixture is missing state_fixture.scalar"))?;
    require(
        function.return_type() == &FunctionReturn::Single(ResolvedType::Value(BOOLEAN_TYPE_ID)),
        "prepared CLIENT state fixture did not retain its Boolean scalar return",
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function.id())
        .ok_or_else(|| failure("prepared CLIENT state fixture is missing its function revision"))?;
    let plan = StateClientPlan::decode(revision.artifact().payload())?;
    require(
        plan.format_version() == STATE_FORMAT_VERSION,
        "CLIENT state fixture did not produce a version-four plan",
    )?;
    let slots = plan.slots();
    require(
        slots.len() == 3,
        "CLIENT state fixture plan did not retain three declarations",
    )?;
    require(
        slots.iter().all(|slot| slot.type_id() == BOOLEAN_TYPE_ID),
        "CLIENT state fixture plan did not retain scalar Boolean slot types",
    )?;
    require(
        slots[0].scope() == StateScope::Local
            && matches!(
                slots[0].default(),
                StateDefault::Expression(ClientExpressionNode::Boolean { value: true })
            ),
        "CLIENT state fixture did not retain the LOCAL expression default in order",
    )?;
    require(
        slots[1].scope() == StateScope::Session && slots[1].default() == &StateDefault::Null,
        "CLIENT state fixture did not retain the SESSION NULL default in order",
    )?;
    require(
        slots[2].scope() == StateScope::User && slots[2].default() == &StateDefault::Unset,
        "CLIENT state fixture did not retain the USER unset default in order",
    )?;
    let slot_ids = slots
        .iter()
        .map(|slot| slot.state_slot_id())
        .collect::<Vec<_>>();
    require(
        slot_ids.iter().all(|id| id.to_bytes() != [0; 16])
            && slot_ids[0] != slot_ids[1]
            && slot_ids[0] != slot_ids[2]
            && slot_ids[1] != slot_ids[2],
        "CLIENT state fixture plan did not retain distinct non-zero state slot IDs",
    )?;

    Ok(())
}

#[test]
fn checks_and_evaluates_accepted_client_local_assignment_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_local_assignment_dogfood.orna",
        include_str!("fixtures/client_local_assignment_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT local assignment fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report.checked_bundle().ok_or_else(|| {
        failure("accepted CLIENT local assignment fixture produced no checked bundle")
    })?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["local_assignment_fixture", "assigned"])
        .ok_or_else(|| failure("accepted CLIENT local assignment fixture is missing assigned"))?;
    let function_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name() == function.name())
        .map(FunctionDefinition::id)
        .ok_or_else(|| {
            failure("prepared CLIENT local assignment fixture is missing its function definition")
        })?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function_id)
        .ok_or_else(|| {
            failure("prepared CLIENT local assignment fixture is missing its function revision")
        })?;
    let plan = ProceduralClientPlan::decode(revision.artifact().payload())?;
    require(
        plan.format_version() == 7
            && plan.locals().len() == 1
            && plan.locals()[0].type_id() == orna_standard::INTEGER_TYPE_ID
            && plan.statements().len() == 2,
        "CLIENT local assignment artifact did not retain version-seven LET and assignment",
    )?;
    let local = plan.locals()[0].local_id();
    require(
        matches!(
            &plan.statements()[0],
            orna_artifact::client_plan::ClientStatement::Let {
                local: statement_local,
                ..
            } if *statement_local == local
        ),
        "CLIENT local assignment artifact did not retain the typed LET",
    )?;
    require(
        matches!(
            &plan.statements()[1],
            orna_artifact::client_plan::ClientStatement::Assignment {
                local: statement_local,
                ..
            } if *statement_local == local
        ),
        "CLIENT local assignment artifact did not retain the plain assignment",
    )?;
    require(
        matches!(
            plan.return_expression(),
            ClientExpressionNode::LocalRead { local: return_local } if *return_local == local
        ),
        "CLIENT local assignment artifact did not return the assigned local",
    )?;

    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(RAW_CLIENT_USER, function_id)],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(function_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline CLIENT local assignment authorisation was denied: {reason:?}"
            )));
        }
    };
    let result = evaluate_client_function(&active, &authorisation)?;
    require(
        result.value() == &RuntimeValue::Integer(42),
        "offline CLIENT local assignment evaluation returned the wrong value",
    )
}

#[test]
fn exposes_checked_client_body_kind_for_rust_introspection() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_introspection_dogfood.orna",
        include_str!("fixtures/client_introspection_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "introspection fixture did not check: {:?}",
            report.diagnostics()
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("missing checked bundle"))?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["introspection_demo", "compute"])
        .ok_or_else(|| failure("missing checked introspection function"))?;
    require(
        function.body_kind() == orna_compiler::CheckedClientBodyKind::ControlFlow,
        "Rust introspection did not expose the checked control-flow body kind",
    )
}

#[test]
fn checks_and_evaluates_accepted_client_control_flow_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/client_control_flow_dogfood.orna",
        include_str!("fixtures/client_control_flow_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted CLIENT control-flow fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let checked = report.checked_bundle().ok_or_else(|| {
        failure("accepted CLIENT control-flow fixture produced no checked bundle")
    })?;
    let function = checked
        .client_functions()
        .find(|function| function.name().parts() == ["console_demo", "bounded_counter"])
        .ok_or_else(|| {
            failure("accepted CLIENT control-flow fixture is missing console_demo.bounded_counter")
        })?;
    let function_id = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name() == function.name())
        .map(FunctionDefinition::id)
        .ok_or_else(|| {
            failure("prepared CLIENT control-flow fixture is missing its function definition")
        })?;
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        functions,
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(RAW_CLIENT_USER, function_id)],
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security
        .authorise_execute(&session, InvocationTarget::new(function_id, active.pair()))
    {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "offline CLIENT control-flow authorisation was denied: {reason:?}"
            )));
        }
    };
    let result = evaluate_client_function(&active, &authorisation)?;
    require(
        result.value() == &RuntimeValue::Integer(5),
        "offline CLIENT control-flow evaluation returned the wrong value",
    )
}
#[test]
fn checks_and_evaluates_accepted_ui_constructor_showcase_roots_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v10_snapshot(retained_standard_library_v10_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/ui_constructor_showcase_dogfood.orna",
        include_str!("fixtures/ui_constructor_showcase_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted UI constructor showcase did not check: {:?}",
            report.diagnostics()
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;

    let function_ids = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        function_ids.iter().copied().collect(),
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        function_ids
            .iter()
            .copied()
            .map(|function| ExecuteGrant::new(RAW_CLIENT_USER, function))
            .collect(),
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

    let roots: [(&str, &str, &[&str]); 3] = [
        (
            "main",
            "UI Constructor Showcase",
            &[
                "std.ui.tabs",
                "std.ui.column",
                "std.ui.panel",
                "std.ui.text",
            ],
        ),
        (
            "input_window",
            "Input Constructor Showcase",
            &["std.ui.text_input"],
        ),
        (
            "control_window",
            "Button Constructor Showcase",
            &["std.ui.row", "std.ui.button"],
        ),
    ];
    for (root_name, expected_title, expected_contracts) in roots {
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["ui_constructor_showcase", root_name])
            .ok_or_else(|| {
                failure(format!(
                    "prepared UI constructor showcase is missing {root_name}"
                ))
            })?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(function.id(), active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(reason) => {
                return Err(failure(format!(
                    "UI constructor showcase root {root_name} authorisation was denied: {reason:?}"
                )));
            }
        };

        let expected_title = expected_title.to_owned();
        let expected_contracts = expected_contracts
            .iter()
            .map(|contract| (*contract).to_owned())
            .collect::<Vec<_>>();
        let window_calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let provider_window_calls = window_calls.clone();
        let mut executor = orna_client::DeterministicClientResourceExecutor::new(
            |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
                Err("resource executor was not used".to_owned())
            },
        )
        .with_external_contract(
            move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
                provider_window_calls.set(provider_window_calls.get() + 1);
                assert_eq!(
                    request.identity(),
                    orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
                );
                assert_eq!(request.arguments().len(), 2);
                assert_eq!(
                    request.arguments()[0].0,
                    orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID
                );
                assert_eq!(
                    request.arguments()[0].1,
                    RuntimeValue::Text(expected_title.clone())
                );
                assert_eq!(
                    request.arguments()[1].0,
                    orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID
                );
                let RuntimeValue::Opaque(content) = &request.arguments()[1].1 else {
                    panic!("std.ui.window content argument was not an opaque UI value");
                };
                assert_eq!(content.opaque_type(), orna_standard::STD_UI_TYPE_ID);

                let payload = content.canonical_payload();
                let magic = orna_standard::UI_MAGIC.as_bytes();
                let prefix_length = magic.len() + 4;
                assert!(
                    payload.len() >= prefix_length && payload.starts_with(magic),
                    "std.ui.window content did not use canonical ORNA-UI/1 framing"
                );
                let body_length = u32::from_be_bytes(
                    payload[magic.len()..prefix_length]
                        .try_into()
                        .expect("the UI body length is exactly four bytes"),
                ) as usize;
                assert_eq!(
                    payload.len(),
                    prefix_length + body_length,
                    "std.ui.window content framing had trailing or truncated bytes"
                );
                let body_bytes = &payload[prefix_length..];
                let body: serde_json::Value =
                    serde_json::from_slice(body_bytes).expect("UI content body must be JSON");
                assert_eq!(
                    serde_json::to_vec(&body).expect("UI content body must re-encode"),
                    body_bytes,
                    "std.ui.window content body was not canonical JSON"
                );
                let mut node = &body;
                for (index, expected_contract) in expected_contracts.iter().enumerate() {
                    assert_eq!(
                        node.get("kind").and_then(serde_json::Value::as_str),
                        Some("node")
                    );
                    let contract = node
                        .get("contract")
                        .and_then(serde_json::Value::as_object)
                        .expect("UI content node must carry a contract");
                    assert_eq!(
                        contract.get("id").and_then(serde_json::Value::as_str),
                        Some(expected_contract.as_str())
                    );
                    assert_eq!(
                        contract.get("name").and_then(serde_json::Value::as_str),
                        Some(expected_contract.as_str())
                    );
                    assert_eq!(
                        contract.get("version").and_then(serde_json::Value::as_str),
                        Some("1.0")
                    );
                    if index + 1 < expected_contracts.len() {
                        let children = node
                            .get("slots")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|slots| slots.get("content"))
                            .and_then(serde_json::Value::as_array)
                            .expect("container UI node must carry a content slot");
                        assert_eq!(children.len(), 1);
                        node = children
                            .first()
                            .expect("container UI content slot must have one child");
                    }
                }
                Ok(RuntimeValue::Opaque(content.clone()))
            },
        );
        let result = evaluate_client_function_with_arguments_and_executor(
            &active,
            &authorisation,
            &[],
            &mut executor,
        )?;
        require(
            window_calls.get() == 1,
            "UI constructor showcase root did not reach std.ui.window exactly once",
        )?;
        let RuntimeValue::Opaque(ui) = result.value() else {
            return Err(failure(format!(
                "UI constructor showcase root {root_name} did not return an opaque UI value"
            )));
        };
        require(
            ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
            "UI constructor showcase root returned the wrong opaque type",
        )?;
        let payload = ui.canonical_payload();
        let magic = orna_standard::UI_MAGIC.as_bytes();
        let prefix_length = magic.len() + 4;
        require(
            payload.len() >= prefix_length && payload.starts_with(magic),
            "UI constructor showcase root returned a non-canonical UI frame",
        )?;
        let body_length = u32::from_be_bytes(
            payload[magic.len()..prefix_length]
                .try_into()
                .map_err(|_| failure("UI result body length was truncated"))?,
        ) as usize;
        require(
            payload.len() == prefix_length + body_length,
            "UI constructor showcase root returned trailing or truncated UI bytes",
        )?;
        let body = &payload[prefix_length..];
        let decoded: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| failure(format!("UI result body was not JSON: {error}")))?;
        require(
            serde_json::to_vec(&decoded)
                .map_err(|error| failure(format!("UI result body did not re-encode: {error}")))?
                == body,
            "UI constructor showcase root returned non-canonical JSON",
        )?;
    }
    Ok(())
}

#[test]
fn checks_and_evaluates_accepted_static_studio_shell_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v10_snapshot(retained_standard_library_v10_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/studio_static_app_dogfood.orna",
        include_str!("fixtures/studio_static_app_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "static Studio shell did not check: {:?}",
            report.diagnostics()
        )));
    }
    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    let active = offline_active_from_prepared(&prepared)?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["studio_static_app", "main"])
        .ok_or_else(|| failure("prepared static Studio shell is missing studio_static_app.main"))?;
    let function_ids = active
        .catalogue()
        .functions()
        .iter()
        .map(FunctionDefinition::id)
        .collect::<Vec<_>>();
    let security = SecuritySnapshot::new(
        active.pair(),
        function_ids.iter().copied().collect(),
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        function_ids
            .iter()
            .copied()
            .map(|function| ExecuteGrant::new(RAW_CLIENT_USER, function))
            .collect(),
    )?;
    let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
    let authorisation = match security.authorise_execute(
        &session,
        InvocationTarget::new(function.id(), active.pair()),
    ) {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(reason) => {
            return Err(failure(format!(
                "static Studio shell authorisation was denied: {reason:?}"
            )));
        }
    };

    let window_calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let provider_window_calls = window_calls.clone();
    let mut executor = orna_client::DeterministicClientResourceExecutor::new(
        |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
            Err("static Studio shell used an unexpected resource executor".to_owned())
        },
    )
    .with_external_contract(
        move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
            provider_window_calls.set(provider_window_calls.get() + 1);
            assert_eq!(
                request.identity(),
                orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
            );
            assert_eq!(request.arguments().len(), 2);
            assert_eq!(
                request.arguments()[0].0,
                orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID
            );
            assert_eq!(
                request.arguments()[0].1,
                RuntimeValue::Text("Orna Studio".to_owned())
            );
            assert_eq!(
                request.arguments()[1].0,
                orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID
            );
            let RuntimeValue::Opaque(content) = &request.arguments()[1].1 else {
                panic!("static Studio shell content was not an opaque UI value");
            };
            assert_eq!(content.opaque_type(), orna_standard::STD_UI_TYPE_ID);

            let payload = content.canonical_payload();
            let magic = orna_standard::UI_MAGIC.as_bytes();
            let prefix_length = magic.len() + 4;
            assert!(payload.starts_with(magic));
            let body_length = u32::from_be_bytes(
                payload[magic.len()..prefix_length]
                    .try_into()
                    .expect("the UI body length is exactly four bytes"),
            ) as usize;
            assert_eq!(payload.len(), prefix_length + body_length);
            let body_bytes = &payload[prefix_length..];
            let body: serde_json::Value =
                serde_json::from_slice(body_bytes).expect("static Studio UI body must be JSON");
            assert_eq!(
                serde_json::to_vec(&body).expect("static Studio UI body must re-encode"),
                body_bytes
            );

            let mut node = &body;
            for (index, expected_contract) in ["std.ui.column", "std.ui.row", "std.ui.text"]
                .iter()
                .enumerate()
            {
                let contract = node
                    .get("contract")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|contract| contract.get("id"))
                    .and_then(serde_json::Value::as_str);
                assert_eq!(contract, Some(*expected_contract));
                assert_eq!(
                    node.get("contract")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|contract| contract.get("name"))
                        .and_then(serde_json::Value::as_str),
                    Some(*expected_contract)
                );
                assert_eq!(
                    node.get("contract")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|contract| contract.get("version"))
                        .and_then(serde_json::Value::as_str),
                    Some("1.0")
                );
                if index < 2 {
                    node = node
                        .get("slots")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|slots| slots.get("content"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|children| children.first())
                        .expect("static Studio container must have one content child");
                }
            }
            Ok(RuntimeValue::Opaque(content.clone()))
        },
    );
    let result = evaluate_client_function_with_arguments_and_executor(
        &active,
        &authorisation,
        &[],
        &mut executor,
    )?;
    require(
        window_calls.get() == 1,
        "static Studio shell did not reach std.ui.window exactly once",
    )?;
    let RuntimeValue::Opaque(ui) = result.value() else {
        return Err(failure(
            "static Studio shell did not return an opaque UI value",
        ));
    };
    require(
        ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
        "static Studio shell returned the wrong opaque type",
    )?;
    let payload = ui.canonical_payload();
    require(
        payload.starts_with(orna_standard::UI_MAGIC.as_bytes()),
        "static Studio shell returned a non-canonical UI frame",
    )?;
    Ok(())
}

#[test]
fn checks_and_prepares_server_function_dogfood_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let base = offline_empty_version_two_active(standard.verified_snapshot())?;
    let context = StandardApplicationCheckContext::try_new(base.catalogue(), &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/server_function_dogfood.orna",
        include_str!("fixtures/server_function_dogfood.orna"),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted SERVER dogfood fixture did not check: {:?}",
            report.diagnostics(),
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted SERVER dogfood fixture produced no checked bundle"))?;
    require(
        checked.server_functions().len() == 5,
        "accepted SERVER dogfood fixture produced an unexpected function count",
    )?;
    for name in ["read", "distinct_values", "stream", "read_item", "update"] {
        require(
            checked
                .server_functions()
                .any(|function| function.name().parts() == ["dogfood", name]),
            "accepted SERVER dogfood fixture is missing a declared function",
        )?;
    }

    let prepared = prepare_standard_application(&report, base.pair(), &base)?;
    require(
        prepared.expected_base() == base.pair(),
        "prepared SERVER dogfood fixture retained the wrong expected base pair",
    )?;
    let expected_candidate_pair =
        RevisionPair::new(prepared.source().id(), prepared.candidate().revision());
    require(
        prepared.candidate_pair() == expected_candidate_pair,
        "prepared SERVER dogfood fixture produced the wrong candidate pair",
    )?;
    require(
        prepared.candidate_pair() != base.pair(),
        "prepared SERVER dogfood fixture did not advance the revision pair",
    )?;
    let active = offline_active_from_prepared(&prepared)?;
    require(
        active.pair() == expected_candidate_pair,
        "prepared SERVER dogfood fixture could not become the expected active pair",
    )
}

#[test]
fn checks_accepted_action_fixture_offline() -> TestResult<()> {
    let snapshot = verify_standard_library_v6_snapshot(retained_standard_library_v6_snapshot()?)?;
    let standard = check_standard_library_source(&snapshot)?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = StandardApplicationCheckContext::try_new(&catalogue, &standard)?;
    let source = SourceBundle::new([SourceUnit::new(
        "fixtures/action_dogfood.orna",
        RAW_ACTION_SOURCE.to_owned(),
    )])?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "accepted V6 action fixture did not check: {:?}",
            report.diagnostics()
        )));
    }
    let checked = report
        .checked_bundle()
        .ok_or_else(|| failure("accepted V6 action fixture produced no checked bundle"))?;
    let call = checked
        .client_functions()
        .find(|function| function.name().parts() == ["action_fixture", "call"])
        .ok_or_else(|| failure("accepted V6 action fixture is missing action_fixture.call"))?;
    let call_local = checked
        .client_functions()
        .find(|function| function.name().parts() == ["action_fixture", "call_local"])
        .ok_or_else(|| {
            failure("accepted V6 action fixture is missing action_fixture.call_local")
        })?;
    if !(matches!(call.return_type().named_type(), Some(CheckedTypeId::Existing(type_id)) if type_id == STD_ACTION_TYPE_ID)
        && matches!(call_local.return_type().named_type(), Some(CheckedTypeId::Existing(type_id)) if type_id == STD_ACTION_TYPE_ID))
    {
        return Err(failure(format!(
            "accepted V6 action fixture did not retain std.Action return shape: call={:?}, local={:?}",
            call.return_type(),
            call_local.return_type(),
        )));
    }
    require(
        checked
            .client_functions()
            .any(|function| function.name().parts() == ["action_fixture", "local"]),
        "accepted V6 action fixture is missing local CLIENT action target",
    )
}

/// Proves the Compose-gated kernel source-apply candidate, audit, and invoke path for an accepted application SERVER function.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_kernel_source_apply_candidate_audit_and_invoke_path() -> TestResult<()> {
    const INPUT: i32 = 73;

    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let standard_upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let installed_standard = kernel.apply_standard_upgrade(&standard_upgrade).await?;
        let standard = installed_standard
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the installed standard snapshot was not pinned"))?;
        let context = StandardApplicationCheckContext::try_new(
            installed_standard.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?;
        let source = SourceBundle::new([SourceUnit::new(
            "fixtures/server_function_dogfood.orna",
            include_str!("fixtures/server_function_dogfood.orna"),
        )])?;
        let report = check_standard_application(&source, &context);
        if !report.diagnostics().is_empty() {
            return Err(failure(format!(
                "the accepted SERVER dogfood source did not check: {:?}",
                report.diagnostics(),
            )));
        }
        let candidate = prepare_standard_application(
            &report,
            installed_standard.pair(),
            &installed_standard,
        )?;
        let expected_candidate_pair = candidate.candidate_pair();
        let active = kernel.apply_source_apply(&candidate).await?;
        require(
            active.pair() == expected_candidate_pair,
            "kernel-applied SERVER dogfood source apply did not activate the committed candidate",
        )?;
        let audit_events = kernel.recover_security_audit_events().await?;
        let source_apply_events = audit_events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SourceApply)
            .collect::<Vec<_>>();
        require(
            audit_events.len() == 1 && source_apply_events.len() == 1,
            "kernel-applied SERVER dogfood source apply did not record exactly one protected SourceApply event",
        )?;
        let source_apply_decision = source_apply_events[0].decision();
        require(
            source_apply_decision.outcome() == SecurityAuditOutcome::Allowed
                && source_apply_decision.session_principal()
                    == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && source_apply_decision.source_apply_candidate() == Some(expected_candidate_pair)
                && source_apply_decision.target().is_none()
                && source_apply_decision.denial().is_none(),
            "kernel-applied SERVER dogfood SourceApply audit detail did not match the committed candidate",
        )?;
        let read_id = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "read"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.read"))?
            .id();
        let stream_id = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "stream"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.stream"))?
            .id();
        let distinct_id = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "distinct_values"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.distinct_values"))?
            .id();
        let read_item = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "read_item"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.read_item"))?;
        let read_item_id = read_item.id();
        let read_item_parameter_id = read_item
            .parameter_by_name("p_item")
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.read_item.p_item"))?
            .id();
        let update = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "update"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.update"))?;
        let update_id = update.id();
        let update_parameter_id = update
            .parameter_by_name("p_item")
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.update.p_item"))?
            .id();
        let update_value_parameter_id = update
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.update.p_value"))?
            .id();
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.push(SecurityFunctionTarget::verified_standard(
            STD_INVOKE_ECHO_FUNCTION_ID,
            standard.revision(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        ));
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, read_id),
                ExecuteGrant::new(RAW_CLIENT_USER, stream_id),
                ExecuteGrant::new(RAW_CLIENT_USER, distinct_id),
                ExecuteGrant::new(RAW_CLIENT_USER, read_item_id),
                ExecuteGrant::new(RAW_CLIENT_USER, update_id),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let object = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["dogfood", "item"])
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.item"))?;
        let field = object
            .fields()
            .iter()
            .find(|field| field.name() == "value")
            .ok_or_else(|| failure("the kernel-applied dogfood source is missing dogfood.item.value"))?;
        let table = format!("t_{:032x}", u128::from_be_bytes(object.id().to_bytes()));
        let column = format!("f_{:032x}", u128::from_be_bytes(field.id().to_bytes()));
        let object_id = ObjectId::from_bytes([0x91; 16]);
        let object_id_hex = format!("{:032x}", u128::from_be_bytes(object_id.to_bytes()));
        let canonical_reference = format!("@dogfood.item/{}", object_id.canonical());
        run_database_statement(
            &database,
            &format!(
                "INSERT INTO _orna_data.{table} (_orna_object_id, {column}) VALUES (decode('{object_id_hex}', 'hex'), {INPUT})"
            ),
        )
        .await?;
        let registry = registered_opaque_codecs(standard)?;
        let mut expected = encode_constructed_value(
            &active,
            &registry,
            &RuntimeValue::Integer(INPUT),
        )?;
        expected.push(b'\n');
        let mut expected_reference = encode_constructed_value(
            &active,
            &registry,
            &RuntimeValue::Reference {
                target: object.id(),
                object: object_id,
            },
        )?;
        expected_reference.push(b'\n');

        let (read_outcome, read_stdout, read_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                    "dogfood", "read",
                ])?)?,
                vec![],
                true,
                false,
            ),
        )
        .await?;
        if read_outcome != Ok(InstalledInvokeOutcome::Completed) {
            return Err(failure(format!(
                "the kernel-applied SERVER dogfood read invocation did not complete: {:?}, stdout={:?}, stderr={:?}",
                read_outcome, read_stdout, read_stderr,
            )));
        }
        require(
            read_stdout == expected,
            "the kernel-applied SERVER dogfood read invocation returned the wrong value",
        )?;
        require(
            read_stderr.is_empty(),
            "the quiet SERVER dogfood read invocation wrote progress diagnostics",
        )?;
        let (stream_outcome, stream_stdout, stream_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(stream_id),
                vec![],
                true,
                false,
            ),
        )
        .await?;
        if stream_outcome != Ok(InstalledInvokeOutcome::Completed) {
            return Err(failure(format!(
                "the kernel-applied SERVER dogfood stream invocation did not complete: {:?}, stdout={:?}, stderr={:?}",
                stream_outcome, stream_stdout, stream_stderr,
            )));
        }
        require(
            stream_stdout == expected,
            "the kernel-applied SERVER dogfood stream invocation returned the wrong values",
        )?;
        require(
            stream_stderr.is_empty(),
            "the quiet SERVER dogfood stream invocation wrote progress diagnostics",
        )?;

        let (update_outcome, update_stdout, update_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(update_id),
                vec![
                    CliArgumentInput::Canonical {
                        parameter: update_parameter_id.canonical(),
                        value: canonical_reference.clone(),
                    },
                    CliArgumentInput::Canonical {
                        parameter: update_value_parameter_id.canonical(),
                        value: "74".to_owned(),
                    },
                ],
                true,
                false,
            ),
        )
        .await?;
        if update_outcome != Ok(InstalledInvokeOutcome::Completed) {
            return Err(failure(format!(
                "the kernel-applied SERVER dogfood update invocation did not complete: {:?}, stdout={:?}, stderr={:?}",
                update_outcome, update_stdout, update_stderr,
            )));
        }
        require(
            update_stdout == expected_reference,
            "the kernel-applied SERVER dogfood update invocation returned the wrong reference",
        )?;
        require(
            update_stderr.is_empty(),
            "the quiet SERVER dogfood update invocation wrote progress diagnostics",
        )?;

        let mut expected_updated = encode_constructed_value(
            &active,
            &registry,
            &RuntimeValue::Integer(74),
        )?;
        expected_updated.push(b'\n');
        let (read_item_outcome, read_item_stdout, read_item_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(read_item_id),
                vec![CliArgumentInput::Canonical {
                    parameter: read_item_parameter_id.canonical(),
                    value: canonical_reference,
                }],
                true,
                false,
            ),
        )
        .await?;
        if read_item_outcome != Ok(InstalledInvokeOutcome::Completed) {
            return Err(failure(format!(
                "the kernel-applied SERVER dogfood read_item invocation did not complete: {:?}, stdout={:?}, stderr={:?}",
                read_item_outcome, read_item_stdout, read_item_stderr,
            )));
        }
        require(
            read_item_stdout == expected_updated,
            "the kernel-applied SERVER dogfood read_item invocation returned the wrong value",
        )?;
        require(
            read_item_stderr.is_empty(),
            "the quiet SERVER dogfood read_item invocation wrote progress diagnostics",
        )?;
        let missing_reference = format!(
            "@dogfood.item/{}",
            ObjectId::from_bytes([0x93; 16]).canonical()
        );
        let (missing_outcome, missing_stdout, missing_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(update_id),
                vec![
                    CliArgumentInput::Canonical {
                        parameter: update_parameter_id.canonical(),
                        value: missing_reference,
                    },
                    CliArgumentInput::Canonical {
                        parameter: update_value_parameter_id.canonical(),
                        value: "75".to_owned(),
                    },
                ],
                true,
                false,
            ),
        )
        .await?;
        require(
            missing_outcome == Ok(InstalledInvokeOutcome::Completed)
                && missing_stdout.is_empty()
                && missing_stderr.is_empty(),
            "the kernel-applied SERVER dogfood missing update did not commit an empty result",
        )?;

        let duplicate_object_id = format!("{:032x}", u128::from_be_bytes([0x92; 16]));
        run_database_statement(
            &database,
            &format!(
                "INSERT INTO _orna_data.{table} (_orna_object_id, {column}) VALUES (decode('{duplicate_object_id}', 'hex'), 74)"
            ),
        )
        .await?;
        let mut expected_distinct = encode_constructed_value(
            &active,
            &registry,
            &RuntimeValue::Integer(74),
        )?;
        expected_distinct.push(b'\n');
        let (distinct_outcome, distinct_stdout, distinct_stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                    "dogfood", "distinct_values",
                ])?)?,
                vec![],
                true,
                false,
            ),
        )
        .await?;
        require(
            distinct_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the kernel-applied SERVER dogfood distinct_values invocation did not complete",
        )?;
        require(
            distinct_stdout == expected_distinct,
            "the kernel-applied SERVER dogfood distinct_values invocation returned the wrong value",
        )?;
        require(
            distinct_stderr.is_empty(),
            "the quiet SERVER dogfood distinct_values invocation wrote progress diagnostics",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the accepted Boolean expression CLIENT form survives the
/// user-facing source/check/install/grant/invoke path.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_client_boolean_expression_dogfood_source_through_orna_invoke()
-> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let standard_upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let active = kernel.apply_standard_upgrade(&standard_upgrade).await?;
        let context = StandardApplicationCheckContext::try_new(
            active.catalogue(),
            standard_upgrade.checked_standard_library(),
        )?;
        let source = SourceBundle::new([SourceUnit::new(
            "fixtures/client_function_dogfood.orna",
            include_str!("fixtures/client_function_dogfood.orna"),
        )])?;
        let report = check_standard_application(&source, &context);
        if !report.diagnostics().is_empty() {
            return Err(failure(format!(
                "the accepted Boolean CLIENT dogfood source did not check: {:?}",
                report.diagnostics(),
            )));
        }
        let active = kernel
            .apply(&prepare_standard_application(
                &report,
                active.pair(),
                &active,
            )?)
            .await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["client_dogfood", "enabled"])
            .ok_or_else(|| {
                failure("the installed CLIENT dogfood source is missing client_dogfood.enabled")
            })?
            .id();
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        if let Some(standard) = active.catalogue_hash_context().standard() {
            function_targets.extend(
                standard
                    .catalogue()
                    .functions()
                    .iter()
                    .map(FunctionDefinition::id),
            );
        }
        function_targets.sort_unstable();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, function)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let registry = active
            .catalogue_hash_context()
            .standard()
            .map(orna_standard::registered_opaque_codecs)
            .transpose()?
            .ok_or_else(|| failure("the Boolean CLIENT fixture has no standard context"))?;
        let mut expected =
            encode_constructed_value(&active, &registry, &RuntimeValue::Boolean(true))?;
        expected.push(b'\n');
        let (outcome, stdout, stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                    "client_dogfood",
                    "enabled",
                ])?)?,
                vec![],
                true,
                false,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInvokeOutcome::Completed),
            "the installed Boolean CLIENT dogfood invocation did not complete",
        )?;
        require(
            stdout == expected,
            "the installed Boolean CLIENT dogfood invocation returned the wrong value",
        )?;
        require(
            stderr.is_empty(),
            "the quiet Boolean CLIENT dogfood invocation wrote progress diagnostics",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
const RAW_ORDINARY_INSPECTOR_SOURCE: &str = include_str!("fixtures/client_inspector_dogfood.orna");
#[cfg(feature = "test-hooks")]
const RAW_FORGED_INSPECTOR_SOURCE: &str = r#"
CREATE CLIENT FUNCTION inspector_app.forged_renderer(
    p_snapshot sys.inspect.snapshot,
    p_invocation_nodes sys.inspect.invocation_nodes,
    p_calls sys.inspect.calls,
    p_resources sys.inspect.resources,
    p_state_cells sys.inspect.state_cells,
    p_ui_nodes sys.inspect.ui_nodes,
    p_presentation_candidates sys.inspect.presentation_candidates,
    p_runtime_bindings sys.inspect.runtime_bindings,
    p_security_decisions sys.inspect.security_decisions
) RETURNS std.ui.UI IS
BEGIN
    RETURN inspector_app.inspector_renderer(
        p_snapshot => p_snapshot,
        p_invocation_nodes => p_invocation_nodes,
        p_calls => p_calls,
        p_resources => p_resources,
        p_state_cells => p_state_cells,
        p_ui_nodes => p_ui_nodes,
        p_presentation_candidates => p_presentation_candidates,
        p_runtime_bindings => p_runtime_bindings,
        p_security_decisions => p_security_decisions
    );
END;
"#;

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_ordinary_client_inspector_through_installed_evaluator() -> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
    const MAX_UI_BODY_BYTES: usize = 64 * 1024;

    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        let (mut active, standard_upgrade, _fixture_client, _fixture_server) =
            install_raw_client_fixture_v4(&kernel).await?;
        let standard = standard_upgrade.checked_standard_library();
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("ordinary Inspector fixture has no retained source unit"))?;
        let source = SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}\n{}", unit.content(), RAW_ORDINARY_INSPECTOR_SOURCE, RAW_FORGED_INSPECTOR_SOURCE)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?;
        let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)?;
        let report = check_standard_application(&source, &context);
        if !report.diagnostics().is_empty() {
            return Err(failure(format!(
                "ordinary Inspector source did not compile: {:?}",
                report.diagnostics()
            )));
        }
        active = kernel
            .apply(&prepare_standard_application(&report, active.pair(), &active)?)
            .await
            .map_err(|error| failure(format!("ordinary Inspector source install failed: {error:?}")))?;
        let inspector_renderer = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "inspector_renderer"])
            .ok_or_else(|| failure("installed Inspector renderer function is missing"))?;
        require(
            matches!(
                inspector_renderer.return_type(),
                FunctionReturn::Single(ResolvedType::Value(type_id))
                    if *type_id == orna_standard::STD_UI_TYPE_ID
            ),
            "installed Inspector renderer return type did not retain the sealed UI value identity",
        )?;
        let expected_renderer_parameters = [
            ("p_snapshot", SYS_INSPECT_SNAPSHOT_TYPE_ID),
            ("p_invocation_nodes", SYS_INSPECT_INVOCATION_NODES_TYPE_ID),
            ("p_calls", SYS_INSPECT_CALLS_TYPE_ID),
            ("p_resources", SYS_INSPECT_RESOURCES_TYPE_ID),
            ("p_state_cells", SYS_INSPECT_STATE_CELLS_TYPE_ID),
            ("p_ui_nodes", SYS_INSPECT_UI_NODES_TYPE_ID),
            (
                "p_presentation_candidates",
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            ),
            ("p_runtime_bindings", SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID),
            ("p_security_decisions", SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID),
        ];
        require(
            inspector_renderer.parameters().len() == expected_renderer_parameters.len()
                && inspector_renderer
                    .parameters()
                    .iter()
                    .zip(expected_renderer_parameters)
                    .all(|(parameter, (name, type_id))| {
                        parameter.name() == name
                            && parameter.resolved_type() == ResolvedType::Value(type_id)
                    }),
            "installed Inspector renderer parameters did not retain sealed value identities",
        )?;
        let forged_renderer = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "forged_renderer"])
            .ok_or_else(|| failure("installed forged Inspector renderer function is missing"))?;
        let forged_renderer_id = forged_renderer.id();
        let forged_renderer_parameter_ids = forged_renderer
            .parameters()
            .iter()
            .map(|parameter| parameter.id())
            .collect::<Vec<_>>();

        let inspector = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "inspector"])
            .ok_or_else(|| failure("installed ordinary Inspector function is missing"))?;
        let inspector_parameter = inspector
            .parameter_by_name("p_target")
            .ok_or_else(|| failure("ordinary Inspector is missing p_target"))?
            .id();
        let inspector = inspector.id();
        let target = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().function_by_id(STD_INVOKE_ECHO_FUNCTION_ID))
            .ok_or_else(|| failure("installed standard is missing std.invoke.echo"))?;
        let registry = registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("ordinary Inspector has no standard context"))?,
        )?;
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            active
                .catalogue()
                .functions()
                .iter()
                .map(|function| SecurityFunctionTarget::application(function.id()))
                .chain(std::iter::once(SecurityFunctionTarget::verified_standard(
                    target.id(),
                    standard.verified_snapshot().revision(),
                    target.current_revision(),
                )))
                .collect(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, inspector),
                ExecuteGrant::new(RAW_CLIENT_USER, forged_renderer_id),
                ExecuteGrant::new(RAW_CLIENT_USER, target.id()),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let target_request = sealed_echo_request(
            InvocationRequestTarget::function_id(target.id()),
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            41,
        )?;
        let retained = encode_invoke_request(&active, &registry, &target_request)?;
        let target_result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let target_invocation = require_echo_completion(&target_result, 41)?;
        let target_argument = FunctionArgument::new(
            inspector_parameter,
            RuntimeValue::Reference {
                target: orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: ObjectId::from_bytes(target_invocation.to_bytes()),
            },
        )?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(inspector, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!("ordinary Inspector grant was denied: {denial:?}")))
            }
        };
        let mut executor = RecordingInstalledResourceExecutor {
            inner: InstalledClientResourceExecutor::new(
                kernel.clone(),
                session.clone(),
                active.clone(),
            ),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        };
        // Reuse one enclosing invocation identity so the two runs share a client
        // epoch while each snapshot request receives a fresh server epoch.
        let deterministic_parent = InvocationId::from_bytes([0x58; 16]);
        let grants = LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
        executor.bind_current_invocation(deterministic_parent);
        let result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
            &[],
            &grants,
            &mut state,
            deterministic_parent,
            &mut executor,
        )?;
        let RuntimeValue::Opaque(ui) = result.value() else {
            return Err(failure("ordinary Inspector did not return an opaque std.ui.UI value"));
        };
        require(
            ui.opaque_type() == orna_standard::STD_UI_TYPE_ID
                && !matches!(result.value(), RuntimeValue::Boolean(_)),
            "ordinary Inspector returned a Boolean or arbitrary opaque value",
        )?;
        let payload = ui.canonical_payload().to_vec();
        let magic = orna_standard::UI_MAGIC.as_bytes();
        require(
            payload.len() >= magic.len() + 4 && payload.starts_with(magic),
            "ordinary Inspector UI payload did not start with canonical ORNA-UI/1 framing",
        )?;
        let length_start = magic.len();
        let body_length = u32::from_be_bytes(
            payload[length_start..length_start + 4]
                .try_into()
                .map_err(|_| failure("ordinary Inspector UI length prefix was truncated"))?,
        ) as usize;
        require(
            body_length <= MAX_UI_BODY_BYTES
                && payload.len() == length_start + 4 + body_length,
            "ordinary Inspector UI payload length was not bounded and exact",
        )?;
        let body = &payload[length_start + 4..];
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| failure(format!("ordinary Inspector UI body was not JSON: {error}")))?;
        require(
            json.is_object(),
            "ordinary Inspector UI body was not a canonical JSON object",
        )?;
        require(
            json.get("kind").and_then(serde_json::Value::as_str) == Some("node"),
            "ordinary Inspector UI kind was not the canonical node shape",
        )?;
        let contract = json
            .get("contract")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI contract was missing"))?;
        require(
            contract.len() == 3
                && contract.get("id").and_then(serde_json::Value::as_str)
                    == Some("std.ui.window")
                && contract.get("name").and_then(serde_json::Value::as_str)
                    == Some("std.ui.window")
                && contract.get("version").and_then(serde_json::Value::as_str) == Some("1.0"),
            "ordinary Inspector UI contract id, name, or version drifted from ORNA-UI/1",
        )?;
        require(
            json.get("call_site_id") == Some(&serde_json::Value::Null)
                && json.get("function_instance_id") == Some(&serde_json::Value::Null),
            "ordinary Inspector UI call-site identity shape was not canonical",
        )?;
        let key = json
            .get("key")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI key was missing"))?;
        require(
            key.len() == 2
                && key.get("type").and_then(serde_json::Value::as_str)
                    == Some("std.types.text")
                && key
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value.starts_with("inspector-")),
            "ordinary Inspector UI key did not retain the canonical text shape",
        )?;
        require(
            json.get("slots")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|object| object.is_empty()),
            "ordinary Inspector UI slots were not the canonical empty object",
        )?;
        require(
            json.get("actions")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|object| object.is_empty()),
            "ordinary Inspector UI actions were not the canonical empty object",
        )?;

        let expected_carrier_kinds = [
            InspectCarrierKind::Snapshot,
            InspectCarrierKind::InvocationNodes,
            InspectCarrierKind::Calls,
            InspectCarrierKind::Resources,
            InspectCarrierKind::StateCells,
            InspectCarrierKind::UiNodes,
            InspectCarrierKind::PresentationCandidates,
            InspectCarrierKind::RuntimeBindings,
            InspectCarrierKind::SecurityDecisions,
        ];
        let expected_row_counts = [1usize, 1, 1, 3, 0, 0, 0, 0, 2];
        require(
            executor.inspect_count == expected_carrier_kinds.len()
                && executor.completed_values.len() == expected_carrier_kinds.len()
                && executor
                    .completed_values
                    .iter()
                    .zip(expected_carrier_kinds.iter())
                    .all(|((expected_type, value), expected_kind)| {
                        *expected_type == ResolvedType::Value(expected_kind.type_id())
                            && matches!(
                                value,
                                RuntimeValue::Opaque(value)
                                    if value.opaque_type() == expected_kind.type_id()
                            )
                    }),
            "ordinary Inspector did not deliver the complete ordered nine-carrier set to the renderer",
        )?;
        let mut shared_server_epoch = None;
        let mut observed_row_counts = Vec::with_capacity(expected_carrier_kinds.len());
        for (((expected_type, value), expected_kind), expected_rows) in executor
            .completed_values
            .iter()
            .zip(expected_carrier_kinds.iter())
            .zip(expected_row_counts)
        {
            require(
                *expected_type == ResolvedType::Value(expected_kind.type_id()),
                "ordinary Inspector carrier result type drifted from its sealed identity",
            )?;
            let RuntimeValue::Opaque(value) = value else {
                return Err(failure("ordinary Inspector carrier result was not opaque"));
            };
            let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
                .map_err(|error| failure(format!("ordinary Inspector carrier envelope was invalid: {error}")))?;
            require(
                envelope.carrier_kind() == *expected_kind
                    && envelope.source_revision_id() == active.pair().source()
                    && envelope.catalogue_revision_id() == active.pair().catalogue()
                    && envelope.rows().len() == expected_rows,
                "ordinary Inspector carrier lost its kind, active revisions, or fixture row count",
            )?;
            if let Some(expected_epoch) = shared_server_epoch {
                require(
                    envelope.server_epoch_id() == expected_epoch,
                    "ordinary Inspector carriers did not share one server epoch",
                )?;
            } else {
                shared_server_epoch = Some(envelope.server_epoch_id());
            }
            observed_row_counts.push(envelope.rows().len());
        }
        let shared_server_epoch =
            shared_server_epoch.ok_or_else(|| failure("ordinary Inspector produced no server epoch"))?;
        require(
            observed_row_counts == expected_row_counts,
            "ordinary Inspector carrier row counts were not deterministic for the echo fixture",
        )?;
        let properties = json
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI properties were missing"))?;
        let ui_server_epoch = properties
            .get("server_epoch")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI server_epoch property was missing"))?;
        require(
            ui_server_epoch == shared_server_epoch.to_string(),
            "ordinary Inspector UI server_epoch did not match the shared carrier epoch",
        )?;
        let ui_client_epoch = properties
            .get("client_epoch")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI client_epoch property was missing"))?;
        require(
            ui_client_epoch == result.context().client_epoch_id().invocation_id().to_string(),
            "ordinary Inspector UI client_epoch did not match the evaluated request context",
        )?;
        let ui_carrier_rows = properties
            .get("carrier_rows")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI carrier_rows property was missing"))?;
        require(
            ui_carrier_rows == "1,1,1,3,0,0,0,0,2",
            "ordinary Inspector UI carrier_rows did not match the echo fixture",
        )?;
        // The installed evaluator returns the canonical ORNA-UI/1 value. The
        // private headless runtime fixture is covered by orna-client's own
        // `#[cfg(test)]` conformance suite; this installed proof does not
        // expose that fixture through a normal dependency feature.

        let first_carriers = executor.completed_values.clone();
        let forged_arguments = forged_renderer_parameter_ids
            .iter()
            .zip(first_carriers.iter())
            .map(|(parameter, (_, value))| FunctionArgument::new(*parameter, value.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let forged_authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(forged_renderer_id, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "forged Inspector renderer grant was denied: {denial:?}"
                )))
            }
        };
        let mut forged_state = ClientStateStore::new();
        let forged_result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &forged_authorisation,
                &forged_arguments,
                &[],
                &grants,
                &mut forged_state,
                deterministic_parent,
                &mut executor,
            )?;
        require(
            matches!(forged_result.value(), RuntimeValue::Opaque(value) if value.opaque_type() == orna_standard::STD_UI_TYPE_ID),
            "normal same-signature renderer did not retain the accepted external UI path",
        )?;
        let forged_contract_arguments = forged_renderer_parameter_ids
            .iter()
            .zip(first_carriers.iter())
            .map(|(parameter, (_, value))| (*parameter, value.clone()))
            .collect::<Vec<_>>();
        let forged_request = ClientExternalContractRequest::new(
            *forged_result.context(),
            INSPECT_RENDER_CONTRACT,
            forged_contract_arguments,
        );
        require(
            executor.inner.external_contract(forged_request)
                == Err("inspect.malformed_carrier".to_owned()),
            "normal same-signature renderer context obtained ORNA-UI from valid carriers",
        )?;
        let mut second_executor = RecordingInstalledResourceExecutor {
            inner: InstalledClientResourceExecutor::new(kernel.clone(), session, active.clone()),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        };
        let mut second_state = ClientStateStore::new();
        second_executor.bind_current_invocation(deterministic_parent);
        let second_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
            &[],
            &grants,
            &mut second_state,
            deterministic_parent,
            &mut second_executor,
        )?;
        let RuntimeValue::Opaque(second_ui) = second_result.value() else {
            return Err(failure("ordinary Inspector repeat did not return an opaque std.ui.UI value"));
        };
        require(
            second_ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
            "ordinary Inspector repeat did not return an opaque std.ui.UI value",
        )?;
        require(
            second_executor.completed_values.len() == expected_carrier_kinds.len(),
            "ordinary Inspector repeat did not deliver the complete carrier set",
        )?;
        let mut second_server_epoch = None;
        for (((expected_type, value), expected_kind), expected_rows) in second_executor
            .completed_values
            .iter()
            .zip(expected_carrier_kinds.iter())
            .zip(expected_row_counts)
        {
            require(
                *expected_type == ResolvedType::Value(expected_kind.type_id()),
                "ordinary Inspector repeat carrier type drifted from its sealed identity",
            )?;
            let RuntimeValue::Opaque(value) = value else {
                return Err(failure("ordinary Inspector repeat carrier was not opaque"));
            };
            let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
                .map_err(|error| {
                    failure(format!(
                        "ordinary Inspector repeat carrier envelope was invalid: {error}"
                    ))
                })?;
            require(
                envelope.carrier_kind() == *expected_kind
                    && envelope.source_revision_id() == active.pair().source()
                    && envelope.catalogue_revision_id() == active.pair().catalogue()
                    && envelope.rows().len() == expected_rows,
                "ordinary Inspector repeat carrier lost its kind, revisions, or row count",
            )?;
            if let Some(expected_epoch) = second_server_epoch {
                require(
                    envelope.server_epoch_id() == expected_epoch,
                    "ordinary Inspector repeat carriers did not share one server epoch",
                )?;
            } else {
                second_server_epoch = Some(envelope.server_epoch_id());
            }
        }
        let second_server_epoch =
            second_server_epoch.ok_or_else(|| failure("ordinary Inspector repeat had no epoch"))?;
        require(
            second_server_epoch != shared_server_epoch,
            "repeated Inspector snapshots reused the previous immutable server epoch",
        )?;
        let second_payload = second_ui.canonical_payload();
        let second_prefix_length = orna_standard::UI_MAGIC.len() + 4;
        require(
            second_payload.len() >= second_prefix_length,
            "ordinary Inspector repeat UI length prefix was truncated",
        )?;
        let second_body_length = u32::from_be_bytes(
            second_payload[orna_standard::UI_MAGIC.len()..second_prefix_length]
                .try_into()
                .map_err(|_| failure("ordinary Inspector repeat UI length was truncated"))?,
        ) as usize;
        require(
            second_payload.len() == second_prefix_length + second_body_length,
            "ordinary Inspector repeat UI framing was not exact",
        )?;
        let second_json: serde_json::Value =
            serde_json::from_slice(&second_payload[second_prefix_length..])
                .map_err(|error| failure(format!("ordinary Inspector repeat UI was not JSON: {error}")))?;
        let second_ui_server_epoch = second_json
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .and_then(|properties| properties.get("server_epoch"))
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector repeat server_epoch property was missing"))?;
        require(
            second_ui_server_epoch == second_server_epoch.to_string()
                && second_ui_server_epoch != ui_server_epoch,
            "ordinary Inspector repeat UI did not expose its fresh server epoch",
        )?;

        let unavailable = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
        );
        require(
            matches!(
                unavailable,
                Err(ClientExecutionError::Inspect {
                    source: ClientInspectError::Failed(code),
                    ..
                }) if code == "inspect.runtime_unavailable"
            ),
            "ordinary Inspector without an executor did not fail closed",
        )
    })
    .await
}
