#![cfg(unix)]

use std::{
    error::Error, io::ErrorKind, os::unix::net::UnixStream as StandardUnixStream, sync::Arc,
    time::Duration,
};

#[cfg(feature = "test-hooks")]
use orna_artifact::client_plan::ActionTargetDomain;
use orna_artifact::client_plan::{
    ClientExpressionNode, OPAQUE_FORMAT_VERSION, OpaqueClientPlan, ProceduralClientPlan,
    ResourceClientPlan,
};
use orna_client::{
    ClientExecutionError, ClientInspectError, ClientResourceCompletion, ClientResourceExecutor,
    capability::{
        LocalCapabilityArgumentSource, LocalCapabilityDeclaration, LocalCapabilityGrant,
        LocalCapabilityGrantSet, LocalCapabilityName, LocalCapabilityScope,
    },
    ClientResourceRequest, evaluate_client_function,
    evaluate_client_function_with_arguments,
    evaluate_client_function_with_arguments_and_executor,
    evaluate_client_function_with_grants,
};
#[cfg(feature = "test-hooks")]
use orna_client::{
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation,
    ClientActionError, ClientActionOutcome, ClientActionState, ClientExternalContractRequest,
    ClientInspectRequest, ClientResourceStatus, ClientStateStore, complete_client_action,
    decode_action_payload, trigger_client_action,
};
use orna_compiler::{
    CheckedStandardLibrary, STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, StandardApplicationCheckContext, check,
    check_standard_application, check_standard_library_source, prepare, prepare_standard_application,
};
use orna_core::{
    CallSiteId, CatalogueRevisionId, FunctionId, FunctionRevisionId, InvocationId, ObjectId,
    ParameterId, PrincipalId, SourceBundleId, SourceRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionVolatility, QualifiedSemanticName,
    },
    inspect_carrier::{InspectCarrierEnvelope, InspectCarrierKind},
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationParameterSelector,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    },
    invocation_binding::CliArgumentInput,
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionIdentity, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
        DeployableRevisionContent, DeployableRevisionInput, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair,
        StoredSourceRevision, VerifiedStandardLibrarySnapshot,
    },
    security::{
        AuthenticatedSession, ExecuteDecision, ExecuteDenial, ExecuteGrant, InvocationTarget,
        LocalPeerAuthenticationError, LocalPeerCredential, Principal, PrincipalKind,
        PrincipalStatus, SecurityAuditDecision, SecurityAuditDenial, SecurityAuditKind,
        SecurityAuditOutcome, SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::{
        SYS_INSPECT_CALLS_TYPE_ID, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID, SYS_INSPECT_RESOURCES_TYPE_ID,
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_UI_NODES_TYPE_ID,
        SYS_INVOKE_FUNCTION_ID,
    },
    types::{ResolvedType, TypeDescriptor},
    value::{EnumValue, FunctionArgument, OpaqueValue, RecordValue, RuntimeValue},
};
#[cfg(feature = "test-hooks")]
use orna_core::value::OpaqueCodecRegistry;
use orna_postgres::{
    AuthenticatedRawCallResult, AuthenticatedServerResourceResult, PostgresKernel,
    ResourceCancellation,
    PostgresKernelError, SealedInvocationResult, ServerInsertError, ServerMutationError,
    ServerUpdateError,
};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, ConnectionError, Event, MAX_RESOURCE_WINDOW, ProtocolConnection,
    RawCall, ResourceArgument, ResourceKind, ResourceRequest, ServerAction, ServerFrame,
    decode_active_server_frame, decode_constructed_server_frame, decode_invocation_event_batch,
    decode_registered_server_frame, decode_server_frame, encode_active_client_frame,
    encode_active_server_frame, encode_client_frame, encode_constructed_client_frame,
    encode_constructed_value, encode_invocation_event_batch, encode_invoke_request,
    encode_registered_client_frame,
};
#[cfg(feature = "test-hooks")]
use orna_protocol::{
    ResourceCancel, ResourceCancellationCode, ResourceClientFrame, ResourceServerFrame,
    ResourceWindowUpdate, decode_resource_server_frame, encode_resource_client_frame,
    encode_resource_server_frame,
};
use orna_server::{
    InstalledInvokeError, InstalledInvokeErrorKind, InstalledInvokeOutcome, InstalledInvokeRequest,
    LocalAuthenticationError, LocalRawSocketError, LocalRawSocketResources,
    OpenStandardDatabaseError, RawClientDispatch, open_standard_database, run_invoke_with_kernel,
    serve_local_raw_stream,
};
#[cfg(feature = "test-hooks")]
use orna_server::{
    InstalledClientResourceExecutor, RawResourceRequestAuthorizer,
    serve_local_raw_stream_with_resource_authorizer,
};
use orna_standard::{
    BOOLEAN_TYPE_ID, BYTE_STREAM_MAGIC, JSON_MAGIC, OPAQUE_TOKEN_TYPE_ID,
    STANDARD_LIBRARY_V3_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID, STD_IO_BYTE_STREAM_TYPE_ID,
    STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_REVISION_ID, STD_JSON_ENCODE_PARAMETER_ID,
    STD_JSON_VALUE_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID, registered_opaque_codecs,
    retained_standard_library_snapshot, retained_standard_library_v2_snapshot,
    retained_standard_library_v3_snapshot, verify_standard_library_snapshot,
    verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
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
                kernel,
                session,
                active,
                stream,
                authorizer,
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
const RAW_EXPRESSION_CLIENT_FUNCTION_SOURCE: &str = "CREATE SCHEMA expr;\n\
    CREATE CLIENT FUNCTION expr.literal() RETURNS TEXT AS 'hello';\n\
    CREATE CLIENT FUNCTION expr.composed() RETURNS TEXT AS expr.literal() || ' world';\n\
    CREATE EXTERNAL CLIENT FUNCTION expr.external() RETURNS TEXT\n\
    RUNTIME CONTRACT 'expr.runtime@1';\n";
#[cfg(feature = "test-hooks")]
const RAW_ACTION_SERVER_SOURCE: &str = "CREATE SCHEMA action_fixture;\n";
#[cfg(feature = "test-hooks")]
const RAW_ACTION_CLIENT_SOURCE: &str = "CREATE CLIENT FUNCTION action_fixture.call(p_value INTEGER)\n\
    RETURNS std.Action\n\
    AS std.action.call(\n\
      target => std.invoke.echo,\n\
      arguments => std.call.args(p_value => p_value)\n\
    );\n";
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
const RAW_STREAM_RESOURCE_CLIENT_SOURCE: &str = "CREATE CLIENT FUNCTION resource_fixture.call(p_marker TEXT) RETURNS STREAM<TEXT> AS\n\
    AWAIT std.data.stream_resource(target => resource_fixture.resource,\n\
      arguments => std.call.args(p_marker => p_marker));\n";
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
const RAW_SCALAR_RESOURCE_CLIENT_SOURCE: &str = "CREATE SCHEMA scalar_fixture;\n\
    CREATE CLIENT FUNCTION scalar_fixture.call() RETURNS INTEGER AS\n\
    AWAIT std.data.resource(target => std.invoke.echo,\n\
      arguments => std.call.args(p_value => 43));\n";
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

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn opens_reopens_and_rejects_tampered_standard_database() -> TestResult<()> {
    let expected =
        retained_standard_library_snapshot().and_then(verify_standard_library_snapshot)?;
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
            "opening a fresh database did not select the exact accepted version-two standard context",
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
            "opening an installed version-two database changed its active pair or accepted context",
        )?;

        let mut reconnect_config = database.config()?;
        reconnect_config.application_name("orna-standard-database-reconnect");
        let reconnected = open_standard_database(PostgresKernel::new(reconnect_config)).await?;
        let reconnected_active = reconnected.recover().await?;
        require(
            reconnected_active.pair() == initial_pair
                && standard_context_facts!(&reconnected_active) == initial_context,
            "reconnecting to an installed version-two database changed its active pair or accepted context",
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

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn dispatches_raw_client_calls_through_security_audit_and_evaluation() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(
                    RAW_CLIENT_USER,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
                Principal::new(
                    RAW_CLIENT_UNGRANTED_USER,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
            ],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let ungranted = security.bind_authenticated_session(RAW_CLIENT_UNGRANTED_USER, vec![])?;

        let success = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            1,
            raw_call(client_function),
        );
        let success_invocation = success.invocation();
        require(
            success.accepted_action()
                == ServerAction::Accepted {
                    stream: 1,
                    invocation: success_invocation,
                },
            "raw CLIENT dispatch changed its accepted action",
        )?;
        let success = success.finish().await;
        require(
            success.source().is_none()
                && success.actions()
                    == [
                        ServerAction::Events {
                            stream: 1,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 1 },
                    ],
            "authorised raw CLIENT dispatch returned the wrong public value actions",
        )?;

        let empty_server = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            2,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            empty_server.source().is_none()
                && empty_server.actions() == [ServerAction::Completed { stream: 2 }],
            "zero-row raw SERVER dispatch did not complete without an empty event batch",
        )?;

        let denied =
            RawClientDispatch::new(kernel.clone(), ungranted, 3, raw_call(client_function))
                .finish()
                .await;
        require_dispatch_failure(
            &denied,
            3,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "missing raw CLIENT grant did not retain its private typed denial",
        )?;

        let unknown_function = FunctionId::from_bytes([0x74; 16]);
        let unknown = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            raw_call(unknown_function),
        )
        .finish()
        .await;
        require_dispatch_failure(
            &unknown,
            4,
            CallFailure::ExecuteDenied,
            matches!(
                unknown.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::UnknownFunction,
                    ..
                })
            ),
            "unknown raw CLIENT target did not retain its private typed denial",
        )?;

        let stale_snapshot = SecuritySnapshot::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x75; 16]),
                CatalogueRevisionId::from_bytes([0x76; 16]),
            ),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_STALE_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let stale_session =
            stale_snapshot.bind_authenticated_session(RAW_CLIENT_STALE_USER, vec![])?;
        let stale =
            RawClientDispatch::new(kernel.clone(), stale_session, 5, raw_call(client_function))
                .finish()
                .await;
        require_dispatch_failure(
            &stale,
            5,
            CallFailure::ExecuteDenied,
            matches!(
                stale.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "stale raw CLIENT session did not retain its private typed denial",
        )?;

        insert_raw_server_flag(&database, &active, 0x7f, true).await?;
        let server_value = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            9,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            server_value.source().is_none()
                && server_value.actions()
                    == [
                        ServerAction::Events {
                            stream: 9,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 9 },
                    ],
            "one-row raw SERVER dispatch did not return its exact typed value",
        )?;

        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 6
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[2].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[2].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair()))
                && events[3].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction))
                && events[3].decision().target()
                    == Some(InvocationTarget::new(unknown_function, active.pair()))
                && events[4].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::InvalidSession))
                && events[4].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "raw CLIENT and SERVER dispatch changed the exact durable audit sequence",
        )?;

        let revoked = SecuritySnapshot::new(
            active.pair(),
            functions,
            granted.principals().collect(),
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let revoked_dispatch =
            RawClientDispatch::new(kernel.clone(), session, 6, raw_call(client_function));
        let cancelled = revoked_dispatch.finish().await;
        require(
            cancelled.action_after_cancellation() == ServerAction::Cancelled { stream: 6 },
            "post-completion cancellation did not replace the clean revoked-grant denial",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 7
                && events[6].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    )),
            "completed revoked-grant dispatch did not retain its durable execute decision",
        )?;

        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.security_audit_events
             ADD CONSTRAINT security_audit_events_dispatch_test_reject_execute
             CHECK (event_kind <> 'execute') NOT VALID",
        )
        .await?;
        let audit_count = security_audit_count(&database).await?;
        let audit_failure = RawClientDispatch::new(
            kernel.clone(),
            revoked.bind_authenticated_session(RAW_CLIENT_USER, vec![])?,
            7,
            raw_call(client_function),
        )
        .finish()
        .await;
        require_dispatch_failure(
            &audit_failure,
            7,
            CallFailure::InternalFailure,
            matches!(
                audit_failure.source(),
                Some(PostgresKernelError::Database(_))
            ),
            "audit insertion failure did not become a closed internal failure",
        )?;
        require(
            audit_failure.action_after_cancellation()
                == ServerAction::Failed {
                    stream: 7,
                    failure: CallFailure::InternalFailure,
                }
                && security_audit_count(&database).await? == audit_count,
            "cancellation masked an audit failure or the failed transaction fabricated evidence",
        )?;
        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.security_audit_events
             DROP CONSTRAINT security_audit_events_dispatch_test_reject_execute",
        )
        .await?;
        let record_call = RawCall {
            function: client_function,
            arguments: vec![orna_protocol::CallArgument {
                parameter: orna_core::ParameterId::from_bytes([0x74; 16]),
                value: raw_client_record(&active)?,
            }],
        };
        let audit_count = security_audit_count(&database).await?;
        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.active_revision
             RENAME TO active_revision_preflight_failure",
        )
        .await?;
        let preflight_failure = RawClientDispatch::new(
            kernel.clone(),
            revoked.bind_authenticated_session(RAW_CLIENT_USER, vec![])?,
            8,
            record_call,
        )
        .finish()
        .await;
        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.active_revision_preflight_failure
             RENAME TO active_revision",
        )
        .await?;
        require_dispatch_failure(
            &preflight_failure,
            8,
            CallFailure::InternalFailure,
            matches!(
                preflight_failure.source(),
                Some(PostgresKernelError::Database(_))
            ),
            "record preflight recovery failure did not retain its private kernel source",
        )?;
        require(
            security_audit_count(&database).await? == audit_count,
            "record preflight failure fabricated execute audit evidence",
        )?;

        let granted = kernel.replace_security_snapshot(&granted).await?;
        let pinned_session = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reached = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let dispatch_kernel = kernel.clone();
        let dispatch_reached = reached.clone();
        let dispatch_resume = resume.clone();
        let pinned_dispatch = tokio::spawn(async move {
            dispatch_kernel
                .dispatch_authenticated_raw_call_with_test_barrier(
                    &pinned_session,
                    server_function,
                    dispatch_reached,
                    dispatch_resume,
                )
                .await
        });
        reached.wait().await;

        let changed_source = RAW_CLIENT_FUNCTION_SOURCE.replace("RETURN TRUE", "RETURN FALSE");
        let changed_bundle = SourceBundle::new([SourceUnit::new("main.orna", changed_source)])?;
        let changed_report = check_standard_application(
            &changed_bundle,
            &StandardApplicationCheckContext::try_new(
                active.catalogue(),
                standard_upgrade.checked_standard_library(),
            )?,
        );
        require(
            changed_report.diagnostics().is_empty(),
            "raw dispatch snapshot-race revision did not compile",
        )?;
        let changed = kernel
            .apply(&prepare_standard_application(
                &changed_report,
                active.pair(),
                &active,
            )?)
            .await?;
        let changed_security = SecuritySnapshot::new(
            changed.pair(),
            changed
                .catalogue()
                .functions()
                .iter()
                .map(|function| function.id())
                .collect(),
            granted.principals().collect(),
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&changed_security).await?;
        resume.wait().await;
        require(
            pinned_dispatch.await??
                == AuthenticatedRawCallResult::Server(vec![RuntimeValue::Boolean(true)]),
            "raw dispatch mixed a concurrently replaced active or security revision into its pinned snapshot",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == usize::try_from(audit_count)? + 1
                && events.last().is_some_and(|event| {
                    event.decision().outcome() == SecurityAuditOutcome::Allowed
                        && event.decision().target()
                            == Some(InvocationTarget::new(server_function, active.pair()))
                }),
            "raw dispatch snapshot race did not bind its audit decision to the recovered revision",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_the_capability_gate_end_to_end() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;

        // Bootstrap the standard snapshot and install one zero-capability
        // CLIENT function (`app.enabled`, Boolean body) through the accepted
        // V1-to-V2 standard pipeline.
        let (active, _standard_upgrade, client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;

        // The CLIENT-authoritative security snapshot grants EXECUTE on the
        // CLIENT function to a fresh principal. The allow evidence becomes
        // the `AuthorisedInvocation` the gate evaluates under, exactly as
        // the client-authoritative ADR 0060 path supplies it.
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                CAPABILITY_GATE_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(CAPABILITY_GATE_USER, client_function)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(CAPABILITY_GATE_USER, vec![])?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(client_function, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(_) => {
                return Err(failure(
                    "the live security grant did not allow the CLIENT function",
                ));
            }
        };
        require(
            authorisation.target().revision() == active.pair()
                && authorisation.target().function() == client_function,
            "the live authorisation did not pin the recovered active CLIENT function",
        )?;

        // The accepted Boolean CLIENT bodies declare zero capabilities, so
        // the live proof supplies the gate's requirements as caller-supplied
        // declarations (ADR 0060 defers durable persistence of requirements
        // on the function revision). The declared path argument is the
        // unredacted value; only the qualified name may ever escape the gate.
        let declaration = LocalCapabilityDeclaration::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityArgumentSource::Text("/home/bob".to_owned()),
        );

        // Case A: the granted capability admits evaluation.
        let grant = LocalCapabilityGrant::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let result = evaluate_client_function_with_grants(
            &active,
            &authorisation,
            std::slice::from_ref(&declaration),
            &grants,
        )
        .map_err(|source| failure(source.to_string()))?;
        require(
            result.value() == &RuntimeValue::Boolean(true),
            "the granted CLIENT function did not evaluate to its Boolean value",
        )?;

        // The allowed capability decision is audited with the redacted name.
        let allowed = SecurityAuditDecision::capability_allowed(
            &session,
            InvocationTarget::new(client_function, active.pair()),
            "std.fs.read",
        )?;
        insert_capability_audit_decision(&database, &allowed).await?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.last().is_some_and(|event| {
                event.decision() == &allowed
                    && event.decision().kind() == SecurityAuditKind::Capability
                    && event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().session_principal() == Some(CAPABILITY_GATE_USER)
                    && event.decision().target()
                        == Some(InvocationTarget::new(client_function, active.pair()))
                    && event.decision().capability_name() == Some("std.fs.read")
                    && event.decision().denial().is_none()
            }),
            "the allowed capability decision did not persist redacted with its exact evidence",
        )?;

        // Case B: the missing grant denies closed with only the qualified
        // name — no path, host, or secret argument value escapes.
        let empty = LocalCapabilityGrantSet::new();
        let denied = match evaluate_client_function_with_grants(
            &active,
            &authorisation,
            std::slice::from_ref(&declaration),
            &empty,
        ) {
            Ok(_) => {
                return Err(failure(
                    "the missing grant did not deny the CLIENT function",
                ));
            }
            Err(error) => error,
        };
        require(
            matches!(
                &denied,
                ClientExecutionError::CapabilityDenied { context, capability }
                    if context.function() == client_function
                        && context.pair() == active.pair()
                        && capability == "std.fs.read"
            ),
            "the denied capability did not carry only the qualified name and context",
        )?;
        require(
            !denied.to_string().contains("/home/bob"),
            "the closed denial leaked the path-scope argument",
        )?;

        // The denied capability decision is audited with the redacted name.
        let denied_decision = SecurityAuditDecision::capability_denied(
            &session,
            InvocationTarget::new(client_function, active.pair()),
            "std.fs.read",
        )?;
        insert_capability_audit_decision(&database, &denied_decision).await?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.last().is_some_and(|event| {
                event.decision() == &denied_decision
                    && event.decision().kind() == SecurityAuditKind::Capability
                    && event.decision().outcome() == SecurityAuditOutcome::Denied
                    && event.decision().denial()
                        == Some(SecurityAuditDenial::Capability {
                            capability: "std.fs.read".to_owned(),
                        })
                    && event.decision().session_principal() == Some(CAPABILITY_GATE_USER)
                    && event.decision().target()
                        == Some(InvocationTarget::new(client_function, active.pair()))
                    && event.decision().capability_name() == Some("std.fs.read")
            }),
            "the denied capability decision did not persist redacted with its exact evidence",
        )?;

        // The durable audit rows carry exactly the redacted
        // `capability:<name>` encoding — never the argument value.
        let session = database.open().await?;
        let stored: Vec<String> = session
            .client()
            .query(
                "SELECT denial_reason FROM _orna_kernel.security_audit_events
                 ORDER BY sequence",
                &[],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        session.shutdown().await?;
        require(
            stored == ["capability:std.fs.read", "capability:std.fs.read"],
            "the durable capability audit rows changed their redacted encoding",
        )?;

        // Case C: the same zero-declaration CLIENT function evaluates
        // unchanged through the unguarded entry (which delegates with empty
        // declarations and an empty grant set) and through the granted entry
        // with an empty declaration list.
        let unguarded = evaluate_client_function(&active, &authorisation)
            .map_err(|source| failure(source.to_string()))?;
        let granted_unguarded =
            evaluate_client_function_with_grants(&active, &authorisation, &[], &empty)
                .map_err(|source| failure(source.to_string()))?;
        require(
            unguarded.value() == &RuntimeValue::Boolean(true)
                && granted_unguarded.value() == &RuntimeValue::Boolean(true),
            "the zero-declaration CLIENT function changed through the gate",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 2
                && events
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Capability),
            "the zero-declaration evaluations appended audit evidence",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn sealed_sys_invoke_entry_is_unavailable_after_system_authorisation() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
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
            vec![],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        let system_entry = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            1,
            raw_call(SYS_INVOKE_FUNCTION_ID),
        )
        .finish()
        .await;
        require_dispatch_failure(
            &system_entry,
            1,
            CallFailure::TargetUnavailable,
            matches!(
                system_entry.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, rule })
                    if *function == SYS_INVOKE_FUNCTION_ID
                        && *rule == "sys.invoke requires its sealed request carrier"
            ),
            "the sealed sys.invoke entry did not close as an unavailable raw target",
        )?;

        let ordinary_unknown = FunctionId::from_bytes([0x74; 16]);
        let unknown =
            RawClientDispatch::new(kernel.clone(), session, 2, raw_call(ordinary_unknown))
                .finish()
                .await;
        require_dispatch_failure(
            &unknown,
            2,
            CallFailure::ExecuteDenied,
            matches!(
                unknown.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::UnknownFunction,
                    ..
                })
            ),
            "an unknown ordinary target did not retain its private execute denial",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 2
                && audits[0].decision().kind() == SecurityAuditKind::Execute
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[0].decision().target()
                    == Some(InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()))
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[1].decision().target()
                    == Some(InvocationTarget::new(ordinary_unknown, active.pair()))
                && audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction)),
            "sealed system entry changed the exact durable audit sequence",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_standard_invocation_dogfooding_through_sealed_sys_invoke() -> TestResult<()> {
    const ECHO_BY_NAME: i32 = 41;
    const ECHO_BY_IDENTITY: i32 = 42;
    const RAW_DENIED_VALUE: i32 = 7;
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;

        // Install orna.std/2 through the normal installed-source path: the
        // V1-to-V2 upgrade retains and verifies V1 first, then atomically
        // applies the executable V2 snapshot and its companion application
        // revision.
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;
        let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the V1-to-V2 upgrade did not pin a verified standard snapshot")
        })?;
        require(
            standard.revision() == orna_standard::STANDARD_LIBRARY_V2_REVISION_ID
                && standard
                    .catalogue()
                    .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
                    .is_some()
                && standard.executables().iter().any(|executable| {
                    executable.function() == STD_INVOKE_ECHO_FUNCTION_ID
                        && executable.revision().id() == STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                }),
            "the installed V2 snapshot did not retain the exact std.invoke.echo executable",
        )?;
        let pair = active.pair();
        let standard_revision = standard.revision();
        let registry = orna_standard::registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo (FunctionId ...10) to the caller.
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, STD_INVOKE_ECHO_FUNCTION_ID)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // Invoke through sys.invoke by qualified name and parameter name.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "echo",
            ])?)?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_BY_NAME,
        )?;
        let retained_name = encode_invoke_request(&active, &registry, &by_name)?;
        let result_name = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_name)
            .await?;
        let invocation_name = require_echo_completion(&result_name, ECHO_BY_NAME)?;
        let events_name = match &result_name {
            SealedInvocationResult::Completed { events, .. } => events,
            _ => return Err(failure("the name-addressed sealed invocation did not complete")),
        };

        // The completed kernel result carries the exact RESULT_VALUES Event
        // batch a server adapter delivers before CALL_COMPLETED; prove the
        // payload round-trips the sealed protocol bytes.
        let payload = encode_invocation_event_batch(&active, &registry, events_name)?;
        let decoded = decode_invocation_event_batch(&active, &registry, &payload)?;
        require(
            decoded == *events_name,
            "the completed Event batch did not round-trip the sealed RESULT_VALUES payload",
        )?;

        // Repeat the invocation by the fixed function and parameter
        // identities (FunctionId ...10 and ParameterId ...10).
        let by_identity = sealed_echo_request(
            InvocationRequestTarget::function_id(STD_INVOKE_ECHO_FUNCTION_ID),
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            ECHO_BY_IDENTITY,
        )?;
        let retained_identity = encode_invoke_request(&active, &registry, &by_identity)?;
        let result_identity = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_identity)
            .await?;
        let invocation_identity = require_echo_completion(&result_identity, ECHO_BY_IDENTITY)?;
        require(
            invocation_name != invocation_identity,
            "the two sealed invocations reused one invocation identity",
        )?;

        // A direct raw call to the same standard target returns EXECUTE_DENIED,
        // records exactly one denied decision, and executes no artifact.
        kernel.replace_security_snapshot(&security).await?;

        let security_events_before = kernel.recover_security_audit_events().await?;
        let raw = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                STD_INVOKE_ECHO_FUNCTION_ID,
                &[FunctionArgument::new(
                    STD_INVOKE_ECHO_PARAMETER_ID,
                    RuntimeValue::Integer(RAW_DENIED_VALUE),
                )?],
            )
            .await;
        require(
            matches!(
                &raw,
                Err(PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function,
                    reason: ExecuteDenial::UnknownFunction,
                }) if *denied_pair == pair && *function == STD_INVOKE_ECHO_FUNCTION_ID
            ),
            "the direct raw call to the standard target did not return EXECUTE_DENIED",
        )?;
        let security_events_after = kernel.recover_security_audit_events().await?;
        require(
            security_events_after.len() == security_events_before.len() + 1
                && security_events_after
                    .last()
                    .map(|event| event.decision().outcome())
                    == Some(SecurityAuditOutcome::Denied)
                && security_events_after.last().map(|event| event.decision().target())
                    == Some(Some(InvocationTarget::new(STD_INVOKE_ECHO_FUNCTION_ID, pair)))
                && security_events_after.last().map(|event| event.decision().denial())
                    == Some(Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction))),
            "the raw denial did not record exactly one denied EXECUTE decision",
        )?;
        require(
            invocation_audit_count(&database).await? == 2,
            "the raw denial executed an artifact or recorded an invocation decision",
        )?;

        // The allowed protected security and invocation audit events both
        // link to the exact historical application RevisionPair whose
        // catalogue hash context pins orna.std/2. Each sealed invocation
        // also appends an allowed INSPECT decision from the ADR 0064
        // capture seam; the EXECUTE evidence is the two allowed decisions.
        let security_events = kernel.recover_security_audit_events().await?;
        let allowed_execute = security_events
            .iter()
            .filter(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().kind() == SecurityAuditKind::Execute
            })
            .collect::<Vec<_>>();
        require(
            allowed_execute.len() == 2
                && allowed_execute.iter().all(|event| {
                    event.decision().session_principal() == Some(RAW_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(STD_INVOKE_ECHO_FUNCTION_ID, pair))
                }),
            "the allowed EXECUTE evidence did not link the exact historical application RevisionPair",
        )?;
        let allowed_security_ids = allowed_execute
            .iter()
            .map(|event| event.id())
            .collect::<Vec<_>>();
        let invocation_rows = invocation_audit_rows(&database).await?;
        require(
            invocation_rows.len() == 2
                && invocation_rows.iter().all(|row| {
                    row.outcome == "allowed"
                        && row.function == STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec()
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
            "the invocation audit rows did not link the exact historical RevisionPair and EXECUTE evidence",
        )?;
        let authority = standard_authority_row(
            &database,
            pair.catalogue(),
            STD_INVOKE_ECHO_FUNCTION_ID,
        )
        .await?;
        require(
            authority.as_ref().is_some_and(|row| {
                row.target_class == "standard"
                    && row.function_revision
                        == STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes().to_vec()
                    && row.standard_revision == Some(standard_revision.to_bytes().to_vec())
            }),
            "the durable invocation target authority did not pin the standard target",
        )?;

        // Restart/reopen succeeds with the valid rows and the same pair.
        let reopened = PostgresKernel::new(database.config()?);
        let reopened_active = reopened.recover().await?;
        require(
            reopened_active.pair() == pair
                && reopened_active
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(standard_revision),
            "reopening the installed database changed its active pair or pinned standard",
        )?;

        // The tamper fixtures below each fail recovery without writing or
        // changing prior history. The three invocation-audit foreign keys are
        // dropped only so the tamper statements can express the corrupted
        // durable state; recovery validation does not depend on them.
        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 DROP CONSTRAINT invocation_audit_events_target_fk,
                 DROP CONSTRAINT invocation_audit_events_revision_pair_fk,
                 DROP CONSTRAINT invocation_audit_events_security_evidence_fk;",
        )
        .await?;

        // 1. Absent standard target: the authority row for std.invoke.echo is
        //    deleted, so recovery cannot resolve the standard target.
        run_database_statement(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
            ),
        )
        .await?;
        let absent = recovery_error(&database).await?;
        require(
            matches!(
                &absent,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "the absent standard target did not fail recovery closed",
        )?;
        run_database_statement(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                     (catalogue_revision_id, function_id, target_class,
                      function_revision_id, standard_library_revision_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'), 'standard',
                         decode('{}', 'hex'), decode('{}', 'hex'));",
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()),
                id_hex(standard_revision.to_bytes()),
            ),
        )
        .await?;
        PostgresKernel::new(database.config()?).recover().await?;

        // 2. Wrong standard executable revision: the authority row pins an
        //    executable revision that the verified standard does not contain.
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode(repeat('aa', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
            ),
        )
        .await?;
        let wrong_revision = recovery_error(&database).await?;
        require(
            matches!(
                &wrong_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "the wrong standard executable revision did not fail recovery closed",
        )?;
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                id_hex(STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()),
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
            ),
        )
        .await?;
        PostgresKernel::new(database.config()?).recover().await?;

        // 3. Unlinked security evidence: the invocation audit row points at a
        //    security audit event that does not exist.
        let original_security_event = allowed_security_ids[1];
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_audit_events
                 SET security_audit_event_id = decode(repeat('bb', 16), 'hex')
                 WHERE invocation_id = decode('{}', 'hex');",
                id_hex(invocation_identity.to_bytes()),
            ),
        )
        .await?;
        let unlinked = recovery_error(&database).await?;
        require(
            matches!(
                &unlinked,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "linked security audit evidence is missing",
                    ..
                }
            ),
            "the unlinked security evidence did not fail recovery closed",
        )?;
        require(
            invocation_audit_security_link(
                &database,
                invocation_identity,
                Some([0xbb; 16]),
            )
            .await?,
            "the failed recovery repaired the unlinked security evidence",
        )?;
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_audit_events
                 SET security_audit_event_id = decode('{}', 'hex')
                 WHERE invocation_id = decode('{}', 'hex');",
                id_hex(original_security_event.to_bytes()),
                id_hex(invocation_identity.to_bytes()),
            ),
        )
        .await?;
        PostgresKernel::new(database.config()?).recover().await?;

        // 4. Mismatched application revision pair: both protected rows point
        //    at a revision pair that does not pin orna.std/2, so recovery
        //    cannot resolve the standard target through the historical pin.
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = decode(repeat('cc', 16), 'hex'),
                     catalogue_revision_id = decode(repeat('dd', 16), 'hex')
                 WHERE function_id = decode('{}', 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET source_revision_id = decode(repeat('cc', 16), 'hex'),
                     catalogue_revision_id = decode(repeat('dd', 16), 'hex')
                 WHERE function_id = decode('{}', 'hex');",
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
            ),
        )
        .await?;
        let mismatched = recovery_error(&database).await?;
        require(
            matches!(
                &mismatched,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "the mismatched application revision pair did not fail recovery closed",
        )?;
        run_database_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = decode('{}', 'hex'),
                     catalogue_revision_id = decode('{}', 'hex')
                 WHERE function_id = decode('{}', 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET source_revision_id = decode('{}', 'hex'),
                     catalogue_revision_id = decode('{}', 'hex')
                 WHERE function_id = decode('{}', 'hex');",
                id_hex(pair.source().to_bytes()),
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
                id_hex(pair.source().to_bytes()),
                id_hex(pair.catalogue().to_bytes()),
                id_hex(STD_INVOKE_ECHO_FUNCTION_ID.to_bytes()),
            ),
        )
        .await?;
        PostgresKernel::new(database.config()?).recover().await?;

        // 5. Extra disclosure-bearing audit column: recovery rejects the
        //    relation shape before it trusts any audit row.
        run_database_statement(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 ADD COLUMN request_payload bytea;",
        )
        .await?;
        let disclosure = recovery_error(&database).await?;
        require(
            matches!(
                &disclosure,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "invocation audit relation has unsupported disclosure-bearing columns",
                    ..
                }
            ),
            "the disclosure-bearing audit column did not fail recovery closed",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the installed `orna invoke` command path end to end (ADR 0056 step
/// 5) against the Compose PostgreSQL kernel.
///
/// The host seam [`orna_server::run_invoke_with_kernel`] runs the exact
/// public host flow — reflect, bind, build the sealed request, authenticate
/// the local peer UID, dispatch through `sys.invoke`, and render — with the
/// test kernel injected in place of the fixed private instance. The command
/// parser is unit-covered; this proof drives the complete command path with
/// the request structs the parser produces.
///
/// The proof asserts:
/// - name invocation (`std.invoke.echo`, parameter name `p_value`) and
///   identity invocation (canonical `FunctionId ...10` / `ParameterId ...10`)
///   both complete, with stdout carrying exactly the canonical ORV5 value
///   record and stderr carrying the progress diagnostics;
/// - `--no-progress` keeps the value on stdout and writes no progress lines;
/// - usage and conversion failures (unknown parameter, invalid value,
///   unknown flag, unresolvable target, extra positional) return the exit-2
///   usage class without executing any artifact and without appending audit
///   evidence;
/// - `--explain` prints the plan and neither dispatches nor audits;
/// - revoking the EXECUTE grant returns the denied outcome (exit 4) with one
///   denied decision appended and one denied invocation-audit row.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_orna_invoke_end_to_end_against_postgres() -> TestResult<()> {
    const ECHO_BY_NAME: i32 = 41;
    const ECHO_BY_IDENTITY: i32 = 42;

    with_test_database(|database| async move {
        // The host authenticates the invoking process's effective UID, so the
        // security snapshot must map that exact UID to the granted principal.
        let uid = nix::unistd::geteuid().as_raw();

        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;
        let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the V1-to-V2 upgrade did not pin a verified standard snapshot")
        })?;
        require(
            standard.revision() == orna_standard::STANDARD_LIBRARY_V2_REVISION_ID
                && standard
                    .catalogue()
                    .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
                    .is_some(),
            "the installed V2 snapshot did not retain std.invoke.echo",
        )?;
        let pair = active.pair();
        let standard_revision = standard.revision();
        let registry = orna_standard::registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the local peer principal and
        // map the test process UID to it, exactly as the installed instance
        // would for the invoking user.
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                RAW_CLIENT_USER,
                STD_INVOKE_ECHO_FUNCTION_ID,
            )],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let security_events_before = kernel.recover_security_audit_events().await?;
        let invocation_rows_before = invocation_audit_count(&database).await?;

        // One canonical value record: the ORV5 constructed encoding followed
        // by the newline the renderer writes after every stdout value.
        fn canonical_integer_record(
            active: &orna_core::revision::ActiveDatabaseRevision,
            registry: &orna_core::value::OpaqueCodecRegistry,
            value: i32,
        ) -> TestResult<Vec<u8>> {
            let mut record =
                encode_constructed_value(active, registry, &RuntimeValue::Integer(value))?;
            record.push(b'\n');
            Ok(record)
        }

        // Invoke by qualified name and parameter name (`std.invoke.echo` with
        // `--arg p_value=41`).
        let by_name = installed_invoke_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "echo",
            ])?)?,
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: ECHO_BY_NAME.to_string(),
            }],
            false,
            false,
        );
        let (name_outcome, name_stdout, name_stderr) =
            installed_invoke_run(&database, by_name).await?;
        require(
            name_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the name-addressed installed invoke did not complete",
        )?;
        require(
            name_stdout == canonical_integer_record(&active, &registry, ECHO_BY_NAME)?,
            "the name-addressed stdout did not carry exactly the canonical value record",
        )?;
        let name_stderr = String::from_utf8(name_stderr)
            .map_err(|_| failure("the name-addressed stderr was not UTF-8 text"))?;
        require(
            name_stderr.contains("orna: invoke: invocation started")
                && name_stderr.contains("orna: invoke: invocation completed in"),
            "the name-addressed stderr did not carry the progress diagnostics",
        )?;

        // Invoke by the canonical function and parameter identities
        // (`FunctionId ...10` with `--arg parameter:<...10>=42`).
        let by_identity = installed_invoke_request(
            InvocationRequestTarget::function_id(STD_INVOKE_ECHO_FUNCTION_ID),
            vec![CliArgumentInput::Canonical {
                parameter: STD_INVOKE_ECHO_PARAMETER_ID.canonical(),
                value: ECHO_BY_IDENTITY.to_string(),
            }],
            false,
            false,
        );
        let (identity_outcome, identity_stdout, identity_stderr) =
            installed_invoke_run(&database, by_identity).await?;
        require(
            identity_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the identity-addressed installed invoke did not complete",
        )?;
        require(
            identity_stdout == canonical_integer_record(&active, &registry, ECHO_BY_IDENTITY)?,
            "the identity-addressed stdout did not carry exactly the canonical value record",
        )?;
        let identity_stderr = String::from_utf8(identity_stderr)
            .map_err(|_| failure("the identity-addressed stderr was not UTF-8 text"))?;
        require(
            identity_stderr.contains("orna: invoke: invocation started")
                && identity_stderr.contains("orna: invoke: invocation completed in"),
            "the identity-addressed stderr did not carry the progress diagnostics",
        )?;

        // `--no-progress` keeps the value on stdout and suppresses every
        // progress diagnostic.
        let no_progress = installed_invoke_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "echo",
            ])?)?,
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: ECHO_BY_NAME.to_string(),
            }],
            true,
            false,
        );
        let (quiet_outcome, quiet_stdout, quiet_stderr) =
            installed_invoke_run(&database, no_progress).await?;
        require(
            quiet_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --no-progress installed invoke did not complete",
        )?;
        require(
            quiet_stdout == canonical_integer_record(&active, &registry, ECHO_BY_NAME)?,
            "the --no-progress stdout did not carry exactly the canonical value record",
        )?;
        require(
            quiet_stderr.is_empty(),
            "the --no-progress stderr carried progress diagnostics",
        )?;

        // Three completed invocations appended three authentication-allowed,
        // three EXECUTE-allowed, and three INSPECT-allowed security events
        // (the ADR 0064 capture seam audits each auto-captured epoch), plus
        // three allowed invocation-audit rows linking the exact historical
        // RevisionPair.
        let security_events_after_invocations =
            kernel.recover_security_audit_events().await?;
        require(
            security_events_after_invocations.len() == security_events_before.len() + 9,
            "the three completed invocations did not append exactly nine security events",
        )?;
        require(
            security_events_after_invocations[security_events_before.len()..]
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
                .all(|event| {
                    event.decision().outcome() == SecurityAuditOutcome::Allowed
                        && event.decision().session_principal() == Some(RAW_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(
                                STD_INVOKE_ECHO_FUNCTION_ID,
                                pair,
                            ))
                }),
            "the completed invocations did not append three allowed EXECUTE decisions",
        )?;
        require(
            invocation_audit_count(&database).await? == invocation_rows_before + 3,
            "the completed invocations did not append exactly three invocation-audit rows",
        )?;
        let completed_rows = invocation_audit_rows(&database).await?;
        require(
            completed_rows.len() == invocation_rows_before as usize + 3
                && completed_rows[invocation_rows_before as usize..]
                    .iter()
                    .all(|row| {
                        row.outcome == "allowed"
                            && row.function == STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec()
                            && row.source == pair.source().to_bytes().to_vec()
                            && row.catalogue == pair.catalogue().to_bytes().to_vec()
                            && row.security_event.is_some()
                    }),
            "the completed invocations did not record allowed invocation-audit rows for std.invoke.echo",
        )?;

        // Usage and conversion failures return the exit-2 usage class without
        // dispatching and without appending any audit evidence: a bad `--arg`
        // (unknown parameter), an invalid value, an unknown flag, a target
        // absent from both catalogues (the host-level missing-target shape;
        // absent-target parsing is unit-covered), and an extra positional
        // argument.
        let usage_shapes = [
            vec![CliArgumentInput::Canonical {
                parameter: "p_bogus".to_owned(),
                value: "1".to_owned(),
            }],
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: "not-an-int".to_owned(),
            }],
            vec![CliArgumentInput::Friendly {
                name: "bogus".to_owned(),
                value: "x".to_owned(),
            }],
            vec![CliArgumentInput::Positional("extra".to_owned())],
        ];
        for arguments in usage_shapes {
            let request = installed_invoke_request(
                InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                    "std", "invoke", "echo",
                ])?)?,
                arguments,
                false,
                false,
            );
            let (outcome, stdout, stderr) = installed_invoke_run(&database, request).await?;
            require(
                matches!(outcome, Err(error) if error.kind() == InstalledInvokeErrorKind::Usage),
                "a usage failure did not return the exit-2 usage class",
            )?;
            require(
                stdout.is_empty() && stderr.is_empty(),
                "a usage failure wrote to a command channel before failing",
            )?;
        }
        let missing_target = installed_invoke_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "missing",
            ])?)?,
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: "1".to_owned(),
            }],
            false,
            false,
        );
        let (missing_outcome, missing_stdout, missing_stderr) =
            installed_invoke_run(&database, missing_target).await?;
        require(
            matches!(missing_outcome, Err(error) if error.kind() == InstalledInvokeErrorKind::Usage),
            "the unresolvable target did not return the exit-2 usage class",
        )?;
        require(
            missing_stdout.is_empty() && missing_stderr.is_empty(),
            "the unresolvable target wrote to a command channel before failing",
        )?;
        require(
            kernel.recover_security_audit_events().await?.len()
                == security_events_after_invocations.len()
                && invocation_audit_count(&database).await? == invocation_rows_before + 3,
            "a usage failure dispatched an artifact or appended audit evidence",
        )?;

        // `--explain` prints the resolution and sealed request plan to stdout,
        // exits success, and neither dispatches nor audits.
        let explain = installed_invoke_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "echo",
            ])?)?,
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: "41".to_owned(),
            }],
            false,
            true,
        );
        let (explain_outcome, explain_stdout, explain_stderr) =
            installed_invoke_run(&database, explain).await?;
        require(
            explain_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --explain installed invoke did not exit success",
        )?;
        let plan = String::from_utf8(explain_stdout)
            .map_err(|_| failure("the --explain plan was not UTF-8 text"))?;
        require(
            plan.contains("target: std.invoke.echo (function:")
                && plan.contains("revision:")
                && plan.contains("(pinned to verified standard")
                && plan.contains("domain: Server")
                && plan.contains("p_value (parameter:")
                && plan.contains(": INTEGER")
                && plan.contains("return: INTEGER")
                && plan.contains("request:")
                && plan.contains("caller:")
                && plan.contains("offer: protocol 5")
                && plan.contains("trace: Off")
                && plan.contains("output: none"),
            "the --explain plan did not carry the resolution and sealed request facts",
        )?;
        require(
            explain_stderr.is_empty(),
            "the --explain run wrote to stderr",
        )?;
        require(
            kernel.recover_security_audit_events().await?.len()
                == security_events_after_invocations.len()
                && invocation_audit_count(&database).await? == invocation_rows_before + 3,
            "--explain dispatched an artifact or appended audit evidence",
        )?;

        // Revoke the EXECUTE grant: the same command now returns the denied
        // outcome (exit 4) with one denied decision and one denied
        // invocation-audit row appended.
        let revoked = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let denied_request = installed_invoke_request(
            InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                "std", "invoke", "echo",
            ])?)?,
            vec![CliArgumentInput::Canonical {
                parameter: "p_value".to_owned(),
                value: "7".to_owned(),
            }],
            false,
            false,
        );
        let (denied_outcome, denied_stdout, denied_stderr) =
            installed_invoke_run(&database, denied_request).await?;
        require(
            denied_outcome == Ok(InstalledInvokeOutcome::Denied),
            "the revoked installed invoke did not return the exit-4 denied outcome",
        )?;
        require(
            denied_stdout.is_empty(),
            "the denied installed invoke wrote a value to stdout",
        )?;
        require(
            String::from_utf8(denied_stderr)
                .map_err(|_| failure("the denied stderr was not UTF-8 text"))?
                == "orna: invoke: invocation denied\n",
            "the denied installed invoke did not print exactly one redacted denial line",
        )?;
        let security_events_after_denied = kernel.recover_security_audit_events().await?;
        require(
            security_events_after_denied.len() == security_events_after_invocations.len() + 2
                && security_events_after_denied
                    .last()
                    .map(|event| event.decision().outcome())
                    == Some(SecurityAuditOutcome::Denied)
                && security_events_after_denied
                    .last()
                    .map(|event| event.decision().kind())
                    == Some(SecurityAuditKind::Execute)
                && security_events_after_denied
                    .last()
                    .map(|event| event.decision().target())
                    == Some(Some(InvocationTarget::new(STD_INVOKE_ECHO_FUNCTION_ID, pair))),
            "the denied invoke did not append exactly one denied EXECUTE decision",
        )?;
        let denied_rows = invocation_audit_rows(&database).await?;
        require(
            denied_rows.len() == invocation_rows_before as usize + 4
                && denied_rows.last().map(|row| row.outcome.as_str()) == Some("denied")
                && denied_rows
                    .last()
                    .map(|row| row.function == STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec())
                    == Some(true),
            "the denied invoke did not append one denied invocation-audit row",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_scalar_client_resource_pending_continues_through_installed_evaluator() -> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel
            .bootstrap()
            .await
            .map_err(|error| failure(format!("bootstrap failed: {error:?}")))?;
        let empty = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover empty database failed: {error:?}")))?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)
            .map_err(|error| failure(format!("prepare V1-to-V2 upgrade failed: {error:?}")))?;
        let active = kernel
            .apply_standard_upgrade(&upgrade)
            .await
            .map_err(|error| failure(format!("apply V1-to-V2 upgrade failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("scalar resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client, target, _call_site) =
            install_scalar_resource_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.push(SecurityFunctionTarget::verified_standard(
            target,
            standard.verified_snapshot().revision(),
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
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("scalar resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let request = sealed_scalar_resource_request(client)?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor = RecordingInstalledResourceExecutor::new(
            kernel.clone(),
            session.clone(),
            active.clone(),
            client_stream,
            authorizer,
        );
        let dispatch = kernel
            .dispatch_sealed_sys_invoke_with_resource_executor(
                &session,
                CONNECTION_PROTOCOL_MAJOR,
                &retained,
                Some(&mut executor),
            )
            .await
            .map_err(|error| failure(format!("scalar pending dispatch failed: {error:?}")));
        let execute_count = executor.execute_count;
        let poll_count = executor.poll_count;
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let dispatch = finish_session(dispatch, connection, "scalar pending socket cleanup")?;
        let SealedInvocationResult::Completed { events, .. } = dispatch else {
            return Err(failure("scalar pending resource did not complete the sealed invocation"));
        };
        let records = events.records();
        require(
            records.len() == 3
                && records[0].event().kind() == InvocationEventKind::InvocationStarted
                && records[1].event().kind() == InvocationEventKind::ValueBatch
                && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
            "scalar pending resource did not retain the completed invocation event sequence",
        )?;
        let InvocationEventBody::ValueBatch { schema: None, values } = records[1].event().body() else {
            return Err(failure("scalar pending resource completion did not carry a plain typed batch"));
        };
        require(
            values.len() == 1
                && values[0].value() == &RuntimeValue::Integer(43),
            "scalar pending resource completion was not typed INTEGER",
        )?;
        require(
            execute_count == 1 && poll_count > 0,
            "scalar pending resource did not execute once and continue through poll",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_procedural_client_resource_through_installed_evaluator() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = open_standard_database(kernel(&database)?)
            .await
            .map_err(|error| failure(format!("open standard database failed: {error:?}")))?;
        let active = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover installed standard failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("procedural resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client, target, parameter) =
            install_procedural_resource_client_fixture(&kernel, &active, &standard).await?;
        let host = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["procedural_fixture", "host"])
            .ok_or_else(|| failure("procedural resource fixture is missing its host CLIENT function"))?
            .id();
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["procedural_fixture", "create"])
            .ok_or_else(|| failure("procedural resource fixture is missing its create function"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("procedural resource create disappeared from the catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("procedural resource create has no p_marker parameter"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[FunctionArgument::new(
                    create_parameter,
                    RuntimeValue::Text("installed-marker".to_owned()),
                )?],
            )
            .await
            .map_err(|error| failure(format!("insert procedural resource fixture row failed: {error:?}")))?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, host),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(client, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed procedural CLIENT grant was denied: {denial:?}"
                )))
            }
        };
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("installed-marker".to_owned()),
        )?;
        let mut executor = DeterministicStreamResourceExecutor;
        let result = evaluate_client_function_with_arguments_and_executor(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
            &mut executor,
        )?;
        let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        ))?;
        let list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![
                RuntimeValue::Text("stream-one".to_owned()),
                RuntimeValue::Text("stream-two".to_owned()),
            ],
        )?;
        let expected_value = RuntimeValue::option(
            &active,
            TypeDescriptor::option(list_descriptor)?,
            Some(list),
        )?;
        require(
            result.value() == &expected_value,
            "procedural CLIENT LET/AWAIT did not return the expected typed stream value",
        )?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("procedural resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let host_list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        ))?;
        let host_list = RuntimeValue::list(
            &active,
            host_list_descriptor.clone(),
            vec![RuntimeValue::Text("installed-marker".to_owned())],
        )?;
        let host_value = RuntimeValue::option(
            &active,
            TypeDescriptor::option(host_list_descriptor)?,
            Some(host_list),
        )?;
        let mut expected = encode_constructed_value(&active, &registry, &host_value)?;
        expected.push(b'\n');
        let host_revision = active
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == host)
            .ok_or_else(|| failure("procedural resource host is missing its function revision"))?;
        let host_plan = ProceduralClientPlan::decode(host_revision.artifact().payload())?;
        let ClientExpressionNode::Resource { operation } = host_plan.statements()[0].expression() else {
            return Err(failure("procedural resource host did not retain a resource operation"));
        };
        let ClientExpressionNode::Await { expression } = host_plan.return_expression() else {
            return Err(failure("procedural resource host did not retain AWAIT"));
        };
        let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
            return Err(failure("procedural resource host AWAIT did not retain the resource local read"));
        };
        require(
            *local == host_plan.locals()[0].local_id(),
            "procedural resource host AWAIT did not retain its resource local",
        )?;
        let host_call_site = operation.call_site_id();
        let invoke_target = InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
            "procedural_fixture",
            "host",
        ])?)?;
        let (outcome, stdout, stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(invoke_target, vec![], true, false),
        )
        .await?;
        require(
            outcome == Ok(InstalledInvokeOutcome::Completed)
                && stdout == expected
                && stderr.is_empty(),
            "installed invoke did not execute the SERVER resource through its host executor",
        )?;
        let target_bytes = target.to_bytes().to_vec();
        let host_bytes = host.to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation.invocation_id, resource.parent_invocation_id,
                            resource.request_id, resource.call_site_id,
                            invocation.outcome, resource.decision_outcome,
                            resource.terminal_outcome
                     FROM _orna_kernel.resource_audit_events AS resource
                     JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.parent_invocation_id
                     WHERE resource.target_function_id = $1
                       AND invocation.function_id = $2
                     ORDER BY resource.sequence DESC
                     LIMIT 1",
                    &[&target_bytes, &host_bytes],
                )
                .await?;
            let root_invocation_id: Vec<u8> = row.try_get("invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let invocation_outcome: String = row.try_get("outcome")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            require(
                root_invocation_id.len() == 16
                    && parent_invocation_id == root_invocation_id
                    && request_id.len() == 16
                    && request_id != root_invocation_id
                    && call_site_id == host_call_site.to_bytes().to_vec(),
                "installed resource audit lost the exact root invocation or compiled call-site identity",
            )?;
            require(
                invocation_outcome == "allowed"
                    && decision_outcome == "allowed"
                    && terminal_outcome == "completed",
                "installed root/resource sequence did not retain allowed terminal audit evidence",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "installed resource identity audit",
        )?;

        let unavailable = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
        );
        require(
            matches!(
                unavailable,
                Err(ClientExecutionError::ResourceEvaluation {
                    source: orna_client::ClientResourceExecutionError::ExecutorUnavailable,
                    ..
                })
            ),
            "procedural resource without a caller-owned executor did not fail closed",
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_expression_client_functions_through_installed_invoke() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        let (active, literal, composed, external) =
            install_expression_client_fixture(&kernel).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, literal),
                ExecuteGrant::new(RAW_CLIENT_USER, composed),
                ExecuteGrant::new(RAW_CLIENT_USER, external),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let registry = active
            .catalogue_hash_context()
            .standard()
            .map(|standard| orna_standard::registered_opaque_codecs(standard))
            .transpose()?
            .ok_or_else(|| failure("the expression CLIENT fixture has no standard context"))?;
        let expected = {
            let mut record = encode_constructed_value(
                &active,
                &registry,
                &RuntimeValue::Text("hello world".into()),
            )?;
            record.push(b'\n');
            record
        };
        let target = |name: &'static str| -> TestResult<InvocationRequestTarget> {
            Ok(InvocationRequestTarget::qualified_name(
                QualifiedSemanticName::new(["expr", name])?,
            )?)
        };

        let (outcome, stdout, stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(target("composed")?, vec![], true, false),
        )
        .await?;
        require(
            outcome == Ok(InstalledInvokeOutcome::Completed)
                && stdout == expected
                && stderr.is_empty(),
            "the installed invoke path did not evaluate the expression CLIENT call and concat",
        )?;

        let (outcome, stdout, stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(target("external")?, vec![], true, false),
        )
        .await?;
        let error = outcome
            .err()
            .ok_or_else(|| failure("the external CLIENT contract unexpectedly completed"))?;
        require(
            error.kind() == InstalledInvokeErrorKind::Internal
                && error.message() == "sealed dispatch failed"
                && !error.message().contains("expr.runtime@1")
                && stdout.is_empty()
                && stderr.is_empty(),
            "the external CLIENT contract did not fail closed through installed invoke",
        )
    })
    .await
}
#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_server_action_resource_trigger_through_authenticated_executor() -> TestResult<()> {
    with_test_database(|database| async move {
        // This proof starts at the authenticated resource-trigger contract.
        // The sealed sys.invoke path evaluates a CLIENT root and returns its
        // opaque std.Action value, but it does not expose the
        // ClientExecutionContext or trigger that action. The direct sealed
        // SERVER dogfood proof above covers the outer sealed gate; keeping
        // those seams separate avoids inventing an action-trigger API here.
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_five = install_v5_standard(&kernel, &empty, &database).await?;
        let upgrade_v6 = orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five)
            .map_err(|error| failure(format!("prepare V5-to-V6 standard upgrade failed: {error:?}")))?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client, target, client_parameter, target_parameter) =
            install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(
            standard
                .verified_snapshot()
                .executables()
                .iter()
                .map(|executable| {
                    SecurityFunctionTarget::verified_standard(
                        executable.function(),
                        standard.verified_snapshot().revision(),
                        executable.revision().id(),
                    )
                }),
        );
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
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(client, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!("installed action grant was denied: {denial:?}")))
            }
        };
        let argument = FunctionArgument::new(
            client_parameter,
            RuntimeValue::Integer(43),
        )?;
        let result = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure("action CLIENT function did not return an opaque action value"));
        };
        let descriptor = decode_action_payload(&active, action.canonical_payload())?;
        require(
            descriptor.domain() == ActionTargetDomain::Server
                && descriptor.target() == target
                && descriptor.target_revision() == active.pair()
                && descriptor.arguments().len() == 1
                && descriptor.arguments()[0].parameter() == target_parameter
                && descriptor.arguments()[0].value() == argument.value(),
            "action value lost its authenticated SERVER target or canonical argument",
        )?;
        let mut action_state = ClientActionState::default();
        let mut state = ClientStateStore::default();
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor =
            orna_server::InstalledClientResourceExecutor::new_with_stream_and_resource_authorizer(
                kernel.clone(),
                session,
                active.clone(),
                client_stream,
                authorizer,
            );
        let action_result = trigger_client_action(
            &active,
            result.value(),
            &authorisation,
            result.context(),
            &mut action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut state,
            &mut executor,
        );
        let action_result = finish_pending_client_action(
            &active,
            &mut action_state,
            &mut executor,
            action_result,
        )
        .await
        .map_err(|error| failure(format!("installed action resource completion failed: {error:?}")));
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let outcome = finish_session(action_result, connection, "installed action socket cleanup")?;
        require(
            outcome == ClientActionOutcome::Completed
                && matches!(action_state.status(), ClientResourceStatus::Idle),
            "authenticated SERVER action did not complete through the installed executor",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let target_bytes = target.to_bytes().to_vec();
        let call_site_bytes = descriptor.call_site().to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT parent_invocation_id, nested_invocation_id, request_id,
                            call_site_id, target_function_id, source_revision_id,
                            catalogue_revision_id, decision_outcome, terminal_outcome,
                            item_count, byte_count
                     FROM _orna_kernel.resource_audit_events
                     WHERE parent_invocation_id = $1 AND target_function_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&parent_invocation_id, &target_bytes],
                )
                .await?;
            let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
            let nested_invocation: Vec<u8> = row.try_get("nested_invocation_id")?;
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let call_site: Vec<u8> = row.try_get("call_site_id")?;
            let audited_target: Vec<u8> = row.try_get("target_function_id")?;
            let source_revision: Vec<u8> = row.try_get("source_revision_id")?;
            let catalogue_revision: Vec<u8> = row.try_get("catalogue_revision_id")?;
            let decision: &str = row.try_get("decision_outcome")?;
            let terminal: &str = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            require(
                parent == parent_invocation_id
                    && nested_invocation.len() == 16
                    && request_id.len() == 16
                    && call_site == call_site_bytes
                    && audited_target == target_bytes
                    && source_revision == active.pair().source().to_bytes().to_vec()
                    && catalogue_revision == active.pair().catalogue().to_bytes().to_vec()
                    && decision == "allowed"
                    && terminal == "completed"
                    && item_count == Some(1)
                    && byte_count.is_some(),
                "SERVER action did not retain its authenticated redacted resource audit evidence",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "installed action resource audit",
        )
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_server_action_denial_stays_inside_authenticated_resource_trigger() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_five = install_v5_standard(&kernel, &empty, &database).await?;
        let upgrade_v6 = orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five)
            .map_err(|error| failure(format!("prepare V5-to-V6 standard upgrade failed: {error:?}")))?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action denial fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("action denial standard source check failed: {error:?}")))?;
        let (active, client, target, client_parameter, target_parameter) =
            install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(
            standard
                .verified_snapshot()
                .executables()
                .iter()
                .map(|executable| {
                    SecurityFunctionTarget::verified_standard(
                        executable.function(),
                        standard.verified_snapshot().revision(),
                        executable.revision().id(),
                    )
                }),
        );
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, client)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(client, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "action denial client grant was denied: {denial:?}"
                )))
            }
        };
        let argument = FunctionArgument::new(
            client_parameter,
            RuntimeValue::Integer(43),
        )?;
        let result = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure("action denial CLIENT function did not return an opaque action value"));
        };
        let descriptor = decode_action_payload(&active, action.canonical_payload())?;
        require(
            descriptor.domain() == ActionTargetDomain::Server
                && descriptor.target() == target
                && descriptor.target_revision() == active.pair()
                && descriptor.arguments().len() == 1
                && descriptor.arguments()[0].parameter() == target_parameter
                && descriptor.arguments()[0].value() == argument.value(),
            "action denial value lost its authenticated SERVER target or canonical argument",
        )?;
        let security_events_before = kernel.recover_security_audit_events().await?;
        let invocation_rows_before = invocation_audit_rows(&database).await?;
        let mut action_state = ClientActionState::default();
        let mut state = ClientStateStore::default();
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor =
            orna_server::InstalledClientResourceExecutor::new_with_stream_and_resource_authorizer(
                kernel.clone(),
                session,
                active.clone(),
                client_stream,
                authorizer,
            );
        let action_result = trigger_client_action(
            &active,
            result.value(),
            &authorisation,
            result.context(),
            &mut action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut state,
            &mut executor,
        );
        let action_result = finish_pending_client_action(
            &active,
            &mut action_state,
            &mut executor,
            action_result,
        )
        .await
        .map_err(|error| failure(format!("installed action resource completion failed: {error:?}")));
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let outcome = finish_session(action_result, connection, "denied action socket cleanup")?;
        require(
            matches!(
                outcome,
                ClientActionOutcome::Failed { code } if code == "action.failed"
            ) && matches!(action_state.status(), ClientResourceStatus::Idle),
            "denied SERVER action did not fail closed through the installed executor",
        )?;
        let security_events_after = kernel.recover_security_audit_events().await?;
        let appended_security_events = security_events_after
            .get(security_events_before.len()..)
            .unwrap_or_default();
        require(
            appended_security_events
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.outcome() == SecurityAuditOutcome::Denied
                })
                .count()
                == 1
                && security_events_after.last().is_some_and(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.outcome() == SecurityAuditOutcome::Denied
                        && decision.target() == Some(InvocationTarget::new(target, active.pair()))
                        && decision.denial()
                            == Some(SecurityAuditDenial::Execute(
                                ExecuteDenial::MissingExecuteGrant,
                            ))
                        && decision.effective_principal().is_none()
                        && decision.authorising_principal().is_none()
                }),
                "denied SERVER action did not append one redacted EXECUTE denial",
            )?;
        let invocation_rows_after = invocation_audit_rows(&database).await?;
        require(
            invocation_rows_after.len() == invocation_rows_before.len() + 1
                && invocation_rows_after.last().is_some_and(|row| {
                    row.outcome == "denied"
                        && row.function == target.to_bytes().to_vec()
                        && row.source == active.pair().source().to_bytes().to_vec()
                        && row.catalogue == active.pair().catalogue().to_bytes().to_vec()
                        && row.security_event.is_some()
                }),
            "denied SERVER action did not append its linked invocation audit",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT parent_invocation_id, target_function_id, source_revision_id,
                            catalogue_revision_id, decision_outcome, terminal_outcome,
                            item_count, byte_count
                     FROM _orna_kernel.resource_audit_events
                     WHERE parent_invocation_id = $1
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&parent_invocation_id],
                )
                .await?;
            let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
            let audited_target: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let decision: &str = row.try_get("decision_outcome")?;
            let terminal: &str = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            require(
                parent == parent_invocation_id
                    && audited_target == Some(target.to_bytes().to_vec())
                    && source_revision == Some(active.pair().source().to_bytes().to_vec())
                    && catalogue_revision == Some(active.pair().catalogue().to_bytes().to_vec())
                    && decision == "denied"
                    && terminal == "failed"
                    && item_count.is_none()
                    && byte_count.is_none(),
                "denied SERVER action did not retain its authenticated target identity",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "denied action resource audit",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_kernel_capability_gate_for_external_client_contract() -> TestResult<()> {
    with_test_database(|database| async move {
        let grant = LocalCapabilityGrant::new(
            LocalCapabilityName::StdFsRead,
            LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let granted_kernel = kernel(&database)?.with_capability_grants(grants);
        let (active, function) = install_external_capability_fixture(&granted_kernel).await?;
        let uid = nix::unistd::geteuid().as_raw();
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, function)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = granted_kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        let allowed = granted_kernel
            .dispatch_authenticated_raw_call(&session, function)
            .await;
        require(
            matches!(
                allowed,
                Err(PostgresKernelError::ClientExecution(
                    ClientExecutionError::ExternalContract { identity, .. }
                )) if identity == "std.fs.read@1"
            ),
            "the granted external CLIENT contract did not pass the capability gate",
        )?;
        let events = granted_kernel.recover_security_audit_events().await?;
        require(
            events.iter().any(|event| {
                event.decision().kind() == SecurityAuditKind::Capability
                    && event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().capability_name() == Some("std.fs.read")
                    && event.decision().denial().is_none()
            }),
            "the granted CLIENT capability did not append an allowed audit decision",
        )?;

        let denied_kernel = kernel(&database)?;
        let denied = denied_kernel
            .dispatch_authenticated_raw_call(&session, function)
            .await;
        require(
            matches!(
                denied,
                Err(PostgresKernelError::ClientExecution(
                    ClientExecutionError::CapabilityDenied { ref capability, .. }
                )) if capability == "std.fs.read"
            ),
            "the external CLIENT contract did not fail closed without its local grant",
        )?;
        require(
            !denied
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("/home/bob")),
            "the denied CLIENT capability exposed its path scope",
        )?;
        let events = denied_kernel.recover_security_audit_events().await?;
        require(
            events.iter().any(|event| {
                event.decision().kind() == SecurityAuditKind::Capability
                    && event.decision().outcome() == SecurityAuditOutcome::Denied
                    && event.decision().capability_name() == Some("std.fs.read")
                    && event.decision().denial().is_some()
            }),
            "the denied CLIENT capability did not append a redacted audit decision",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_v5_json_value_and_encode_through_installed_sealed_invoke() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = install_v5_standard(&kernel, &empty, &database).await?;
        let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the V5 install did not pin a verified standard snapshot")
        })?;
        require(
            standard.revision() == STANDARD_LIBRARY_V5_REVISION_ID
                && standard
                    .catalogue()
                    .type_definition_by_id(STD_JSON_VALUE_TYPE_ID)
                    .is_some()
                && standard
                    .catalogue()
                    .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
                    .is_some()
                && standard
                    .catalogue()
                    .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
                    .is_some()
                && standard.executables().iter().any(|executable| {
                    executable.function() == STD_JSON_ENCODE_FUNCTION_ID
                        && executable.revision().id() == STD_JSON_ENCODE_FUNCTION_REVISION_ID
                }),
            "the installed orna.std/5 snapshot did not retain the JSON value and presenter",
        )?;
        let registry = registered_opaque_codecs(standard)?;
        let body = br#"{"items":[1,2],"ok":true}"#;
        let mut json_payload = Vec::from(JSON_MAGIC.as_bytes());
        json_payload.extend_from_slice(
            &u32::try_from(body.len())
                .expect("the JSON body length fits the canonical frame")
                .to_be_bytes(),
        );
        json_payload.extend_from_slice(body);
        let json_value = OpaqueValue::new(
            &active,
            &registry,
            STD_JSON_VALUE_TYPE_ID,
            &json_payload,
        )?;
        require(
            json_value.canonical_payload() == json_payload.as_slice(),
            "the V5 JSON codec did not retain the canonical value payload",
        )?;

        let mut expected_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected_payload.extend_from_slice(&16_u32.to_be_bytes());
        expected_payload.extend_from_slice(b"application/json");
        expected_payload.extend_from_slice(
            &u32::try_from(body.len())
                .expect("the JSON body length fits the byte-stream frame")
                .to_be_bytes(),
        );
        expected_payload.extend_from_slice(body);

        let pair = active.pair();
        let standard_revision = standard.revision();
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            pair,
            vec![
                SecurityFunctionTarget::verified_standard(
                    STD_INVOKE_ECHO_FUNCTION_ID,
                    standard_revision,
                    STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
                ),
                SecurityFunctionTarget::verified_standard(
                    STD_JSON_ENCODE_FUNCTION_ID,
                    standard_revision,
                    STD_JSON_ENCODE_FUNCTION_REVISION_ID,
                ),
            ],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, STD_JSON_ENCODE_FUNCTION_ID)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let request = sealed_json_encode_request(json_value)?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained)
            .await?;
        require_json_encode_completion(&result, &expected_payload)?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_output_through_orna_invoke_against_postgres() -> TestResult<()> {
    const ECHO_JSON: i32 = 41;
    const ECHO_TABLE: i32 = 42;
    const ECHO_CSV: i32 = 43;

    with_test_database(|database| async move {
        // The host authenticates the invoking process's effective UID, so the
        // security snapshot must map that exact UID to the granted principal.
        let uid = nix::unistd::geteuid().as_raw();

        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = install_v3_standard(&kernel, &empty, &database).await?;
        let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the V3 install did not pin a verified standard snapshot")
        })?;
        require(
            standard.revision() == STANDARD_LIBRARY_V3_REVISION_ID
                && standard
                    .catalogue()
                    .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
                    .is_some()
                && standard
                    .catalogue()
                    .type_definition_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
                    .is_some()
                && standard
                    .catalogue()
                    .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
                    .is_some(),
            "the installed orna.std/3 snapshot did not retain the echo function and output types",
        )?;
        let pair = active.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the local peer principal and
        // map the test process UID to it, exactly as the installed instance
        // would for the invoking user.
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, STD_INVOKE_ECHO_FUNCTION_ID)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let security_events_before = kernel.recover_security_audit_events().await?;
        let invocation_rows_before = invocation_audit_count(&database).await?;

        // One canonical value record: the ORV5 constructed encoding followed
        // by the newline the renderer writes after every non-presented value.
        fn canonical_integer_record(
            active: &ActiveDatabaseRevision,
            registry: &orna_core::value::OpaqueCodecRegistry,
            value: i32,
        ) -> TestResult<Vec<u8>> {
            let mut record =
                encode_constructed_value(active, registry, &RuntimeValue::Integer(value))?;
            record.push(b'\n');
            Ok(record)
        }

        // `--output json` resolves the `json` alias to std.json.encode, which
        // wraps the canonical INTEGER 41 in an `application/json` ByteStream.
        // The tty runtime writes the raw stream bytes to stdout: exactly `41`
        // with no envelope and no progress interleave; the progress
        // diagnostics stay on stderr (ADR 0057 steps 7-10).
        let (json_outcome, json_stdout, json_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_JSON, Some("json".to_owned()))?,
        )
        .await?;
        require(
            json_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --output json installed invoke did not complete",
        )?;
        require(
            json_stdout == b"41",
            "the --output json stdout did not carry exactly the JSON bytes",
        )?;
        let json_stderr = String::from_utf8(json_stderr)
            .map_err(|_| failure("the --output json stderr was not UTF-8 text"))?;
        require(
            json_stderr.contains("orna: invoke: invocation started")
                && json_stderr.contains("orna: invoke: invocation completed in"),
            "the --output json stderr did not carry the progress diagnostics",
        )?;

        // `--output table` resolves the `table` alias to
        // std.terminal.present_table, which renders the one-column `result`
        // row set as a terminal Document. The tty runtime writes the document
        // text to stdout: exactly the header, separator, aligned row, trailing
        // count, and final newline; the progress diagnostics stay on stderr.
        let (table_outcome, table_stdout, table_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_TABLE, Some("table".to_owned()))?,
        )
        .await?;
        require(
            table_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --output table installed invoke did not complete",
        )?;
        require(
            table_stdout == b"result\n------\n42\n(1 row)\n",
            "the --output table stdout did not carry exactly the terminal document",
        )?;
        let table_stderr = String::from_utf8(table_stderr)
            .map_err(|_| failure("the --output table stderr was not UTF-8 text"))?;
        require(
            table_stderr.contains("orna: invoke: invocation started")
                && table_stderr.contains("orna: invoke: invocation completed in"),
            "the --output table stderr did not carry the progress diagnostics",
        )?;

        // `--output csv` resolves the `csv` alias to std.csv.encode (work
        // ADR 0067), which wraps the canonical INTEGER 43 in a `text/csv`
        // ByteStream: the one-column `result` row set renders as the header
        // row, the value row, and the final newline. The tty runtime writes
        // the raw stream bytes to stdout: exactly `result\n43\n` with no
        // envelope and no progress interleave; the progress diagnostics stay
        // on stderr.
        let (csv_outcome, csv_stdout, csv_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_CSV, Some("csv".to_owned()))?,
        )
        .await?;
        require(
            csv_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --output csv installed invoke did not complete",
        )?;
        require(
            csv_stdout == b"result\n43\n",
            "the --output csv stdout did not carry exactly the CSV bytes",
        )?;
        let csv_stderr = String::from_utf8(csv_stderr)
            .map_err(|_| failure("the --output csv stderr was not UTF-8 text"))?;
        require(
            csv_stderr.contains("orna: invoke: invocation started")
                && csv_stderr.contains("orna: invoke: invocation completed in"),
            "the --output csv stderr did not carry the progress diagnostics",
        )?;

        // An unmatchable requirement (`application/xml` has no registered
        // presenter) fails closed with the presentation error class (spec
        // exit 5, `ORNA0702`): no presenter artifact executes, no value
        // reaches stdout, and no diagnostic reaches stderr.
        let (xml_outcome, xml_stdout, xml_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_JSON, Some("application/xml".to_owned()))?,
        )
        .await?;
        require(
            matches!(
                xml_outcome,
                Err(error) if error.kind() == InstalledInvokeErrorKind::Presentation
            ),
            "the unmatchable output requirement did not return the exit-5 presentation class",
        )?;
        require(
            xml_stdout.is_empty() && xml_stderr.is_empty(),
            "the unmatchable output requirement wrote to a command channel",
        )?;

        // The no-requirement path is unchanged: the canonical value record on
        // stdout and the progress diagnostics on stderr (milestone 5).
        let (bare_outcome, bare_stdout, bare_stderr) =
            installed_invoke_run(&database, echo_invoke_request(ECHO_JSON, None)?).await?;
        require(
            bare_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the no-requirement installed invoke did not complete",
        )?;
        require(
            bare_stdout == canonical_integer_record(&active, &registry, ECHO_JSON)?,
            "the no-requirement stdout did not carry exactly the canonical value record",
        )?;
        let bare_stderr = String::from_utf8(bare_stderr)
            .map_err(|_| failure("the no-requirement stderr was not UTF-8 text"))?;
        require(
            bare_stderr.contains("orna: invoke: invocation started")
                && bare_stderr.contains("orna: invoke: invocation completed in"),
            "the no-requirement stderr did not carry the progress diagnostics",
        )?;

        // The four completed invocations (json, table, csv, bare) each
        // appended one authentication event, one allowed EXECUTE decision
        // against the exact V3-pinned echo target, and one INSPECT decision
        // from the ADR 0064 capture seam, plus one allowed invocation-audit
        // row. The unmatchable-requirement failure appends its
        // authentication event and the allowed EXECUTE evidence, which the
        // sealed dispatch now commits (work ADR 0059 fix): the failure still
        // captures no epoch, so it adds no INSPECT event and no
        // invocation-audit row.
        let security_events_after = kernel.recover_security_audit_events().await?;
        require(
            security_events_after.len() == security_events_before.len() + 14,
            "the five installed invocations did not append exactly fourteen security events",
        )?;
        let appended = &security_events_after[security_events_before.len()..];
        require(
            appended
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
                .all(|event| {
                    event.decision().outcome() == SecurityAuditOutcome::Allowed
                        && event.decision().session_principal() == Some(RAW_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(STD_INVOKE_ECHO_FUNCTION_ID, pair))
                }),
            "the installed invocations did not append five allowed EXECUTE decisions",
        )?;
        require(
            appended
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
                .count()
                == 5
                && appended
                    .iter()
                    .filter(|event| event.decision().kind() == SecurityAuditKind::Inspect)
                    .count()
                    == 4
                && appended
                    .iter()
                    .all(|event| event.decision().outcome() == SecurityAuditOutcome::Allowed),
            "the five installed invocations appended a denied or partial security decision",
        )?;
        // Five invocations total (json, table, csv, the unmatchable-
        // requirement failure, and bare). The failure path now commits its
        // linked invocation-audit evidence as well, so all five rows are
        // present.
        require(
            invocation_audit_count(&database).await? == invocation_rows_before + 5,
            "the five installed invocations did not append exactly five invocation-audit rows",
        )?;
        let completed_rows = invocation_audit_rows(&database).await?;
        require(
            completed_rows.len() == invocation_rows_before as usize + 5
                && completed_rows[invocation_rows_before as usize..]
                    .iter()
                    .all(|row| {
                        row.outcome == "allowed"
                            && row.function == STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec()
                            && row.source == pair.source().to_bytes().to_vec()
                            && row.catalogue == pair.catalogue().to_bytes().to_vec()
                            && row.security_event.is_some()
                    }),
            "the installed invocations did not record allowed invocation-audit rows for std.invoke.echo",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// Builds one installed `orna invoke` request against `std.invoke.echo` with
/// an optional raw `--output <alias|media-type|type-name>` value (ADR 0057
/// step 10).
fn echo_invoke_request(value: i32, output: Option<String>) -> TestResult<InstalledInvokeRequest> {
    Ok(InstalledInvokeRequest::new(
        InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
            "std", "invoke", "echo",
        ])?)?,
        vec![CliArgumentInput::Canonical {
            parameter: "p_value".to_owned(),
            value: value.to_string(),
        }],
        output,
        None,
        false,
        false,
        None,
    ))
}

/// Builds one installed `orna invoke` command request the way the command
/// parser would after stripping option prefixes (ADR 0056 step 4).
fn installed_invoke_request(
    target: InvocationRequestTarget,
    arguments: Vec<CliArgumentInput>,
    no_progress: bool,
    explain: bool,
) -> InstalledInvokeRequest {
    InstalledInvokeRequest::new(target, arguments, None, None, no_progress, explain, None)
}

/// Runs one installed `orna invoke` command through the exact host flow
/// against the Compose PostgreSQL test kernel, returning the outcome or
/// failure class plus the exact bytes each channel received.
async fn installed_invoke_run(
    database: &TestDatabase,
    request: InstalledInvokeRequest,
) -> TestResult<(
    Result<InstalledInvokeOutcome, InstalledInvokeError>,
    Vec<u8>,
    Vec<u8>,
)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome =
        run_invoke_with_kernel(kernel(database)?, request, &mut stdout, &mut stderr).await;
    Ok((outcome, stdout, stderr))
}

#[cfg(feature = "test-hooks")]
async fn finish_pending_client_action(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    executor: &mut dyn ClientResourceExecutor,
    mut result: Result<ClientActionOutcome, ClientActionError>,
) -> Result<ClientActionOutcome, ClientActionError> {
    loop {
        if !matches!(result, Err(ClientActionError::Pending)) {
            return result;
        }
        let completion = loop {
            if let Some(completion) = executor.poll() {
                break completion;
            }
            sleep(Duration::from_millis(10)).await;
        };
        result = complete_client_action(active, action_state, completion);
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_argument_authority_denies_then_grants_and_audits_each_dispatch() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw argument fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the raw argument fixture is missing app.create_flagged"))?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_value.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_value,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // A read-only grant denies the parameterised INSERT before any row.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, server_function)],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            1,
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
        require_dispatch_failure(
            &denied,
            1,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw argument INSERT was not denied before dispatch",
        )?;
        let empty = RawClientDispatch::new(kernel.clone(), session, 2, raw_call(server_function))
            .finish()
            .await;
        require(
            empty.source().is_none() && empty.actions() == [ServerAction::Completed { stream: 2 }],
            "the denied raw argument INSERT must leave the read empty",
        )?;

        // Grant the INSERT and the read together, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // TRUE with the exact discovered ParameterId returns one reference.
        let inserted_true = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
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
        let [
            ServerAction::Events { stream: 3, events },
            ServerAction::Completed { stream: 3 },
        ] = inserted_true.actions()
        else {
            return Err(failure(
                "the TRUE raw argument INSERT must return one event batch and completion",
            ));
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the TRUE raw argument INSERT did not return one reference",
            ));
        };
        require(
            *target == flag_type && *object != ObjectId::from_bytes([0; 16]),
            "the TRUE raw argument INSERT returned the wrong reference",
        )?;
        let true_object = *object;

        // The parameter-free read observes the stored TRUE row.
        let read_true = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            read_true.source().is_none()
                && read_true.actions()
                    == [
                        ServerAction::Events {
                            stream: 4,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 4 },
                    ],
            "the TRUE raw argument INSERT did not become visible to the read",
        )?;

        // A wrong ParameterId closes as TARGET_UNAVAILABLE with a retained
        // private create_flagged source, stays non-operational under
        // cancellation, and adds no row.
        let wrong_target = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            5,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: RuntimeValue::Boolean(true),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong_target,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong_target.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_flagged
            ),
            "a wrong raw argument ParameterId did not close as an unavailable target",
        )?;
        require(
            wrong_target.action_after_cancellation() == ServerAction::Cancelled { stream: 5 },
            "a wrong raw argument ParameterId must remain non-operational under cancellation",
        )?;
        let unchanged = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            unchanged.source().is_none()
                && unchanged.actions()
                    == [
                        ServerAction::Events {
                            stream: 6,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 6 },
                    ],
            "a wrong raw argument ParameterId must not add any row",
        )?;

        // FALSE with the exact discovered ParameterId returns a second
        // distinct nonzero object reference for the same app.flag type.
        let inserted_false = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            7,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_value,
                    value: RuntimeValue::Boolean(false),
                }],
            },
        )
        .finish()
        .await;
        let [
            ServerAction::Events { stream: 7, events },
            ServerAction::Completed { stream: 7 },
        ] = inserted_false.actions()
        else {
            return Err(failure(
                "the FALSE raw argument INSERT must return one event batch and completion",
            ));
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the FALSE raw argument INSERT did not return one reference",
            ));
        };
        require(
            *target == flag_type
                && *object != ObjectId::from_bytes([0; 16])
                && *object != true_object,
            "the FALSE raw argument INSERT returned the wrong distinct reference",
        )?;

        // The read returns exactly one TRUE and one FALSE in no particular
        // row order.
        let read_both =
            RawClientDispatch::new(kernel.clone(), session, 8, raw_call(server_function))
                .finish()
                .await;
        let [
            ServerAction::Events { stream: 8, events },
            ServerAction::Events {
                stream: 8,
                events: second_events,
            },
            ServerAction::Completed { stream: 8 },
        ] = read_both.actions()
        else {
            return Err(failure(
                "the argument SELECT must return two event batches and completion",
            ));
        };
        let [Event::Value(first_value)] = events.as_slice() else {
            return Err(failure(
                "the argument SELECT did not return one first value",
            ));
        };
        let [Event::Value(second_value)] = second_events.as_slice() else {
            return Err(failure(
                "the argument SELECT did not return one second value",
            ));
        };
        let ordered_ok = (first_value == &RuntimeValue::Boolean(true)
            && second_value == &RuntimeValue::Boolean(false))
            || (first_value == &RuntimeValue::Boolean(false)
                && second_value == &RuntimeValue::Boolean(true));
        require(
            ordered_ok,
            "the argument SELECT must return exactly one TRUE and one FALSE in any order",
        )?;

        // Eight execute audits in dispatch order: the pre-grant denial, then
        // every allowed dispatch including the wrong-parameter closure whose
        // allowed audit survived its savepoint rollback.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8,
            "raw argument authority audit count differs",
        )?;
        require(
            events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Denied
                && events[0].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "raw argument authority changed the exact durable audit sequence",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the raw socket-facing dispatch boundary exposes only the approved
/// version-2 identity-selected SERVER SELECT form.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_identity_selected_server_read_authorises_binds_and_redacts() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, legacy_read) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.create_flagged"))?
            .id();
        let select_flag = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "select_flag"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.select_flag"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let select_parameter = active
            .catalogue()
            .function_by_id(select_flag)
            .ok_or_else(|| failure("select_flag is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("select_flag.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = select_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != select_parameter,
            "the deliberately wrong identity-read parameter must differ from the declaration",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Create one selected object before the selector grant exists.
        let writer_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, create_flagged)],
        )?;
        let security = kernel.replace_security_snapshot(&writer_only).await?;
        let writer = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference = create_flag_reference(
            &kernel,
            &writer,
            create_flagged,
            create_parameter,
            flag_type,
            1,
        )
        .await?;

        // The protocol only exposes the public denial. The private cause
        // proves authorisation occurred before reference binding.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            writer,
            2,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            2,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted identity-selected raw read was not redacted",
        )?;

        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, legacy_read),
                ExecuteGrant::new(RAW_CLIENT_USER, select_flag),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // The selected row flattens its two ORF1 values in declared column
        // order, then emits the normal completion action.
        let selected = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        let expected_selected = [
            ServerAction::Events {
                stream: 3,
                events: vec![Event::Value(reference.clone())],
            },
            ServerAction::Events {
                stream: 3,
                events: vec![Event::Value(RuntimeValue::Boolean(true))],
            },
            ServerAction::Completed { stream: 3 },
        ];
        require(
            selected.source().is_none() && selected.actions() == expected_selected,
            "the identity-selected raw read did not preserve projected ORF1 value order",
        )?;

        let absent = RuntimeValue::Reference {
            target: flag_type,
            object: ObjectId::from_bytes([0x6d; 16]),
        };
        let no_row = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: absent.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            no_row.source().is_none()
                && no_row.actions() == [ServerAction::Completed { stream: 4 }],
            "an absent same-type reference must complete without raw values",
        )?;

        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            5,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == select_flag
            ),
            "a wrong identity-selected ParameterId did not close as unavailable",
        )?;

        // The existing zero-argument read remains available, but a Reference
        // cannot open its version-1 path.
        let legacy =
            RawClientDispatch::new(kernel.clone(), session.clone(), 6, raw_call(legacy_read))
                .finish()
                .await;
        require(
            legacy.source().is_none()
                && legacy.actions()
                    == [
                        ServerAction::Events {
                            stream: 6,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 6 },
                    ],
            "the legacy parameter-free raw read changed during identity selection",
        )?;
        let legacy_argument = RawClientDispatch::new(
            kernel.clone(),
            session,
            7,
            RawCall {
                function: legacy_read,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: absent.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &legacy_argument,
            7,
            CallFailure::TargetUnavailable,
            matches!(
                legacy_argument.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == legacy_read
            ),
            "a Reference argument opened the legacy raw read path",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 7
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && audits[1].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target()
                    == Some(InvocationTarget::new(legacy_read, active.pair()))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target()
                    == Some(InvocationTarget::new(legacy_read, active.pair())),
            "identity-selected raw read changed its durable authorisation audit sequence",
        )?;

        // Exercise the same public closure through the local authenticated
        // socket. The direct dispatcher calls above retain the private typed
        // causes; these frames prove that the protocol does not disclose them.
        let uid = nix::unistd::getuid().as_raw();
        let socket_functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let denied_socket_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            socket_functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel
            .replace_security_snapshot(&denied_socket_security)
            .await?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_socket_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "identity-selected socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: select_parameter,
                    value: reference.clone(),
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "identity-selected socket did not accept the denied call",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "identity-selected socket disclosed or changed ExecuteDenied",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_socket_operation,
            finish_session(
                shutdown,
                connection,
                "identity-selected denied socket cleanup",
            ),
            "identity-selected denied socket operation",
        )?;

        let granted_socket_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            socket_functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, select_flag)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel
            .replace_security_snapshot(&granted_socket_security)
            .await?;
        let reference_event_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 2,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(reference.clone()),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_socket_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "identity-selected granted socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: select_parameter,
                    value: reference.clone(),
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: reference_event_credit,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "identity-selected socket did not accept the granted call",
            )?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(reference.clone())
                ),
                "identity-selected socket did not emit the first projected Reference",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "identity-selected socket emitted its second projection without byte credit",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: BOOLEAN_EVENT_CREDIT,
                },
            )
            .await?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 2
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "identity-selected socket did not resume with the Boolean projection",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "identity-selected socket did not complete after both projected values",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 3,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: select_parameter,
                    value: absent.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 3, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 3 },
                "identity-selected socket did not close an absent reference without values",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 4,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 4,
                    parameter: wrong_parameter,
                    value: reference.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 4 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 4, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 4,
                        failure: CallFailure::TargetUnavailable,
                    },
                "identity-selected socket disclosed or changed TargetUnavailable",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 5,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: select_parameter,
                    value: reference,
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
                ClientFrame::CallCancel { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 5, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 5 },
                "identity-selected socket did not close the cancelled reference call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_socket_operation,
            finish_session(
                shutdown,
                connection,
                "identity-selected granted socket cleanup",
            ),
            "identity-selected granted socket operation",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn server_raw_reference_mutation_authority_selection_and_audit() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the reference fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the reference fixture is missing app.create_flagged"))?
            .id();
        let update_false = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "update_false"])
            .ok_or_else(|| failure("the reference fixture is missing app.update_false"))?
            .id();
        let delete_flag = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "delete_flag"])
            .ok_or_else(|| failure("the reference fixture is missing app.delete_flag"))?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let p_flag = active
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("update_false.p_flag is absent from the active catalogue"))?
            .id();
        let delete_parameter = active
            .catalogue()
            .function_by_id(delete_flag)
            .ok_or_else(|| failure("delete_flag is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("delete_flag.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_flag.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_flag,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Grant only the writer and the reader; the reference mutations stay
        // unauthorised for the denial proof.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // Create two distinct rows and retain both exact references.
        let first =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 1).await?;
        let second =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 2).await?;
        require(
            first != second,
            "the two created references must be distinct",
        )?;

        // The identical invalid binding is denied before its grant, proving
        // authorisation precedes argument binding.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: second.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            3,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted invalid-binding UPDATE was not denied before dispatch",
        )?;

        // Grant create, read, update, and delete, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, update_false),
                ExecuteGrant::new(RAW_CLIENT_USER, delete_flag),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // UPDATE selects the first row and returns the identical reference.
        let updated = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            updated.source().is_none(),
            "the Reference UPDATE must not retain a kernel source",
        )?;
        let [
            ServerAction::Events { stream: 4, events },
            ServerAction::Completed { stream: 4 },
        ] = updated.actions()
        else {
            return Err(failure(
                "the Reference UPDATE must return one event batch and completion",
            ));
        };
        let [Event::Value(updated_reference)] = events.as_slice() else {
            return Err(failure("the Reference UPDATE must return one reference"));
        };
        require(
            *updated_reference == first,
            "the Reference UPDATE must return exactly the identical input reference",
        )?;

        // The reader returns exactly one FALSE and one TRUE in no row order.
        let mixed = read_flag_values(&kernel, &session, server_function, 5).await?;
        require(
            mixed.len() == 2
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1,
            "the Reference UPDATE must select exactly one row",
        )?;

        // The same invalid binding closes as unavailable after the grant, and
        // the preserved read proves the second row stayed TRUE: an erroneous
        // post-grant execution would have made it FALSE.
        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: second.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            6,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == update_false
            ),
            "a wrong UPDATE ParameterId did not close as an unavailable target",
        )?;
        let preserved = read_flag_values(&kernel, &session, server_function, 7).await?;
        require(
            preserved.len() == 2
                && preserved
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1
                && preserved
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1,
            "the wrong UPDATE ParameterId must preserve both rows",
        )?;

        // DELETE the first row and prove the reader keeps only the second.
        let deleted = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            8,
            RawCall {
                function: delete_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: delete_parameter,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            deleted.source().is_none()
                && deleted.actions()
                    == [
                        ServerAction::Events {
                            stream: 8,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 8 },
                    ],
            "the reference DELETE must return exactly one TRUE value",
        )?;
        let one_true = read_flag_values(&kernel, &session, server_function, 9).await?;
        require(
            one_true == [RuntimeValue::Boolean(true)],
            "the reference DELETE must leave exactly the second row TRUE",
        )?;

        // Repeated DELETE and UPDATE of the deleted reference both complete
        // with no value events.
        let repeated = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            10,
            RawCall {
                function: delete_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: delete_parameter,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            repeated.source().is_none()
                && repeated.actions() == [ServerAction::Completed { stream: 10 }],
            "the repeated reference DELETE must complete with no value events",
        )?;
        let deleted_update = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            11,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            deleted_update.source().is_none()
                && deleted_update.actions() == [ServerAction::Completed { stream: 11 }],
            "the UPDATE of the deleted reference must complete with no value events",
        )?;

        // The final read shows only the second TRUE row.
        let final_read = read_flag_values(&kernel, &session, server_function, 12).await?;
        require(
            final_read == [RuntimeValue::Boolean(true)],
            "the final read must show only the second TRUE row",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with exact outcomes and targets in dispatch order.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 12,
            "server reference mutation audit count differs",
        )?;
        require(
            events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Denied
                && events[2].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[2].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(delete_flag, active.pair()))
                && events[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[8].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[9].decision().target()
                    == Some(InvocationTarget::new(delete_flag, active.pair()))
                && events[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[10].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[11].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "server reference mutation changed the exact durable audit sequence",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == active.pair(),
            "server reference mutations must not change the active revision pair",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// One authenticated raw reference-INSERT authority journey through the
/// public server adapter.
///
/// The test installs the shared raw CLIENT fixture plus the additive
/// `app.assignment` unique-reference pair, discovers every identity from the
/// active catalogue, and creates one real owner reference through the public
/// adapter. A wrong-parameter reference call is denied before its grant and
/// creates no assignment. After the grant, the same wrong parameter closes as
/// `CallFailure::TargetUnavailable` without adding a row, a correct reference
/// call succeeds and returns one assignment reference, and the duplicate call
/// is redacted as public `CallFailure::InternalFailure` while retaining the
/// private typed `UniqueReferenceConflict` source. The public reader exposes
/// exactly one dependent row after the duplicate. The exact audit
/// outcome/target sequence and the unchanged active revision are asserted.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn server_raw_reference_insert_authority() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw reference INSERT fixture is missing app.flag"))?
            .id();
        let assignment_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "assignment"])
            .ok_or_else(|| failure("the raw reference INSERT fixture is missing app.assignment"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.create_flagged")
            })?
            .id();
        let create_assignment = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_assignment"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.create_assignment")
            })?
            .id();
        let read_assignments = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "read_assignments"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.read_assignments")
            })?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let p_flag = active
            .catalogue()
            .function_by_id(create_assignment)
            .ok_or_else(|| failure("create_assignment is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("create_assignment.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_flag.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_flag,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Grant only the owner create, the reader, and the assignment reader;
        // the assignment create stays unauthorised for the denial proof.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, read_assignments),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // One real owner reference through the public adapter.
        let owner =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 1).await?;

        // The identical wrong-parameter call is denied before its grant,
        // proving authorisation precedes argument validation.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            2,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            2,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw reference INSERT was not denied before dispatch",
        )?;
        let zero_before = read_flag_values(&kernel, &session, read_assignments, 3).await?;
        require(
            zero_before.is_empty(),
            "the denied raw reference INSERT must leave zero assignments",
        )?;

        // Grant the assignment create, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, read_assignments),
                ExecuteGrant::new(RAW_CLIENT_USER, create_assignment),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // The wrong parameter closes as an unavailable target after the grant
        // without adding any assignment row.
        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            4,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_assignment
            ),
            "a wrong INSERT ParameterId did not close as an unavailable target",
        )?;
        let zero_after_wrong = read_flag_values(&kernel, &session, read_assignments, 5).await?;
        require(
            zero_after_wrong.is_empty(),
            "the wrong INSERT ParameterId must not add any assignment row",
        )?;

        // The correct reference call succeeds and returns one assignment
        // reference whose target differs from the owner type.
        let created = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            created.source().is_none(),
            "the raw reference INSERT must not retain a kernel source",
        )?;
        let [
            ServerAction::Events {
                stream: events_stream,
                events,
            },
            ServerAction::Completed {
                stream: completed_stream,
            },
        ] = created.actions()
        else {
            return Err(failure(
                "the raw reference INSERT must return one event batch and completion",
            ));
        };
        require(
            *events_stream == 6 && *completed_stream == 6,
            "the raw reference INSERT must use the exact stream",
        )?;
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the raw reference INSERT must return one assignment reference",
            ));
        };
        require(
            *target == assignment_type
                && *target != flag_type
                && *object != ObjectId::from_bytes([0; 16]),
            "the assignment reference must name the assignment type and a real nonzero row",
        )?;

        // The duplicate call is redacted as a public internal failure while
        // retaining the private typed unique-reference conflict source.
        let duplicate = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            7,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &duplicate,
            7,
            CallFailure::InternalFailure,
            matches!(
                duplicate.source(),
                Some(PostgresKernelError::ServerInsert(
                    ServerInsertError::NotCommitted { source: inner, .. }
                )) if matches!(inner.as_ref(), ServerMutationError::UniqueReferenceConflict { .. })
            ),
            "the duplicate raw reference INSERT was not redacted with its private conflict source",
        )?;

        // The public reader exposes exactly one dependent row after the
        // duplicate.
        let one = read_flag_values(&kernel, &session, read_assignments, 8).await?;
        require(
            one == [RuntimeValue::Boolean(true)],
            "the public reader must expose exactly one TRUE assignment after the duplicate",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with exact outcomes and targets in dispatch order.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8
                && events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Denied
                && events[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[1].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair())),
            "raw reference INSERT changed the exact durable audit sequence",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == active.pair(),
            "raw reference INSERTs must not change the active revision pair",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn server_raw_integer_dispatch_denies_then_grants_and_audits_exact_values() -> TestResult<()>
{
    // One Integer tracer through the public adapter. The PostgreSQL scalar
    // matrix already proves the kernel bind and value contract.
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;
        let (active, int_probe, create_int, create_int_parameter, read_ints) =
            install_raw_int_insert_fixture(&kernel, &active, &standard_upgrade).await?;
        let mut wrong_parameter_bytes = create_int_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let snapshot = |grants: Vec<ExecuteGrant>| {
            SecuritySnapshot::new(
                active.pair(),
                active
                    .catalogue()
                    .functions()
                    .iter()
                    .map(|function| function.id())
                    .collect::<Vec<_>>(),
                vec![principal],
                vec![],
                grants,
            )
            .expect("the raw Integer test security snapshot is valid")
        };
        let dispatch = |session: &AuthenticatedSession,
                        stream: u64,
                        parameter: ParameterId,
                        value: RuntimeValue| {
            RawClientDispatch::new(
                kernel.clone(),
                session.clone(),
                stream,
                RawCall {
                    function: create_int,
                    arguments: vec![orna_protocol::CallArgument { parameter, value }],
                },
            )
        };

        // The wrong-parameter call is denied before its grant, proving
        // authorisation precedes argument validation.
        let read_only = kernel
            .replace_security_snapshot(&snapshot(vec![ExecuteGrant::new(
                RAW_CLIENT_USER,
                read_ints,
            )]))
            .await?;
        let session = read_only.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let denied = dispatch(&session, 1, wrong_parameter, RuntimeValue::Integer(7))
            .finish()
            .await;
        require_dispatch_failure(
            &denied,
            1,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw Integer INSERT was not denied before argument binding",
        )?;

        // Grant the INSERT, store one exact value, and read it back.
        let granted = kernel
            .replace_security_snapshot(&snapshot(vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_int),
                ExecuteGrant::new(RAW_CLIENT_USER, read_ints),
            ]))
            .await?;
        let session = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let inserted = dispatch(&session, 3, create_int_parameter, RuntimeValue::Integer(-7))
            .finish()
            .await;
        let events = match inserted.actions() {
            [
                ServerAction::Events { events, .. },
                ServerAction::Completed { .. },
            ] => events,
            _ => {
                return Err(failure(
                    "the raw Integer INSERT must return one event batch and completion",
                ));
            }
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the raw Integer INSERT did not return one reference",
            ));
        };
        require(
            *target == int_probe && *object != ObjectId::from_bytes([0; 16]),
            "the raw Integer INSERT returned the wrong reference",
        )?;

        // The wrong parameter closes redacted as an unavailable target. The
        // final read proves the exact stored value and that neither the
        // denied nor the wrong call added a row.
        let wrong = dispatch(&session, 5, wrong_parameter, RuntimeValue::Integer(9))
            .finish()
            .await;
        require_dispatch_failure(
            &wrong,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_int
            ),
            "a wrong raw Integer ParameterId did not close as an unavailable target",
        )?;
        require(
            read_flag_values(&kernel, &session, read_ints, 6).await? == [RuntimeValue::Integer(-7)],
            "the final read must show exactly the stored value and no extra row",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with the exact outcome, target, and principal in dispatch order.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 4
                && audits.iter().enumerate().all(|(index, event)| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.session_principal() == Some(RAW_CLIENT_USER)
                        && decision.outcome()
                            == [
                                SecurityAuditOutcome::Denied,
                                SecurityAuditOutcome::Allowed,
                                SecurityAuditOutcome::Allowed,
                                SecurityAuditOutcome::Allowed,
                            ][index]
                        && decision.target().map(InvocationTarget::function)
                            == Some([create_int, create_int, create_int, read_ints][index])
                }),
            "raw Integer dispatch changed the exact durable audit sequence",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_stream_resource_dispatches_allowed_and_denied_with_redacted_audit() -> TestResult<()> {
    const RESOURCE_VALUE: &str = "resource-value";
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (
            active,
            standard_upgrade,
            _client_function,
            _server_function,
        ) = install_raw_client_fixture(&kernel).await?;
        let (active, _resource_client, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, standard_upgrade.checked_standard_library())
                .await
                .map_err(|error| failure(format!("install stream resource fixture failed: {error:?}")))?;
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("stream resource fixture is missing resource_fixture.create"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("resource_fixture.create.p_marker is absent from the active catalogue"))?
            .id();
        let sequence_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("resource_fixture.create.p_sequence is absent from the active catalogue"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_parameter,
                        RuntimeValue::Text(RESOURCE_VALUE.into()),
                    )?,
                    FunctionArgument::new(sequence_parameter, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| failure(format!("insert stream resource fixture row failed: {error:?}")))?;
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let request = ResourceRequest {
            stream_id: 73,
            request_id: InvocationId::from_bytes([0x31; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x32; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RESOURCE_VALUE.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let snapshot = |grants| {
            SecuritySnapshot::new_with_function_targets(
                active.pair(),
                functions
                    .iter()
                    .copied()
                    .map(SecurityFunctionTarget::application)
                    .collect(),
                vec![principal],
                vec![],
                grants,
            )
            .expect("the stream resource security snapshot is valid")
        };

        let allowed = kernel
            .replace_security_snapshot(&snapshot(vec![ExecuteGrant::new(RAW_CLIENT_USER, target)]))
            .await?;
        let session = allowed.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let completed = kernel
            .dispatch_authenticated_server_resource(&session, &request)
            .await?;
        let nested = match completed {
            AuthenticatedServerResourceResult::Completed {
                stream_id,
                request_id,
                nested_invocation_id,
                target_revision,
                resource_kind,
                values,
            } => {
                require(stream_id == request.stream_id, "stream resource changed stream identity")?;
                require(request_id == request.request_id, "stream resource changed request identity")?;
                require(target_revision == active.pair(), "stream resource changed active revision")?;
                require(resource_kind == ResourceKind::Stream, "stream resource changed result kind")?;
                require(values == [RuntimeValue::Text(RESOURCE_VALUE.into())], "stream resource returned the wrong text")?;
                require(
                    nested_invocation_id != InvocationId::from_bytes([0; 16])
                        && nested_invocation_id != request.request_id
                        && nested_invocation_id != request.parent_invocation_id,
                    "stream resource did not generate a nested invocation identity",
                )?;
                nested_invocation_id
            }
            AuthenticatedServerResourceResult::Failed { failure: call_failure, .. } => {
                return Err(failure(format!("stream resource unexpectedly failed: {call_failure:?}")));
            }
        };
        let duplicate = kernel
            .dispatch_authenticated_server_resource(&session, &request)
            .await?;
        require(
            duplicate
                == AuthenticatedServerResourceResult::Failed {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    failure: CallFailure::InternalFailure,
                },
            "resource request identity was reused after its first dispatch",
        )?;

        let denied_request = ResourceRequest {
            request_id: InvocationId::new(),
            ..request.clone()
        };
        let denied = kernel
            .replace_security_snapshot(&snapshot(vec![]))
            .await?;
        let denied_session = denied.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let failed = kernel
            .dispatch_authenticated_server_resource(&denied_session, &denied_request)
            .await?;
        require(
            failed
                == AuthenticatedServerResourceResult::Failed {
                    stream_id: denied_request.stream_id,
                    request_id: denied_request.request_id,
                    failure: CallFailure::ExecuteDenied,
                },
            "stream resource without its EXECUTE grant was not denied",
        )?;


        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 2
                && audits[0].decision().kind() == SecurityAuditKind::Execute
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[0].decision().target() == Some(InvocationTarget::new(target, active.pair()))
                && audits[0].decision().effective_principal() == Some(RAW_CLIENT_USER)
                && audits[0].decision().authorising_principal() == Some(RAW_CLIENT_USER)
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[1].decision().target() == Some(InvocationTarget::new(target, active.pair()))
                && audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::MissingExecuteGrant))
                && audits[1].decision().effective_principal().is_none()
                && audits[1].decision().authorising_principal().is_none(),
            "stream resource audit evidence exposed an unredacted decision",
        )?;
        let audit_text = format!("{audits:?}");
        require(
            !audit_text.contains(&format!("Integer({RESOURCE_VALUE})"))
                && !audit_text.contains(&nested.canonical()),
            "resource audit evidence retained raw argument or result detail",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_resource_worker_failure_is_compensated_once() -> TestResult<()> {
    const RESOURCE_INPUT: &str = "resource-worker-input";
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (
            active,
            standard_upgrade,
            _client_function,
            _server_function,
        ) = install_raw_client_fixture(&kernel).await?;
        let (active, _resource_client, target, parameter, call_site) =
            install_stream_resource_client_fixture(
                &kernel,
                &active,
                standard_upgrade.checked_standard_library(),
            )
            .await
            .map_err(|error| failure(format!("install stream resource fixture failed: {error:?}")))?;
        let request = ResourceRequest {
            stream_id: 201,
            request_id: InvocationId::from_bytes([0xa1; 16]),
            parent_invocation_id: InvocationId::from_bytes([0xa2; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RESOURCE_INPUT.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            functions
                .iter()
                .copied()
                .map(SecurityFunctionTarget::application)
                .collect(),
            vec![principal],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, target)],
        )?;
        let active = kernel.replace_security_snapshot(&security).await?;
        let session = active.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let cancellation = ResourceCancellation::new();
        let worker_failure = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &request,
                &cancellation,
            )
            .await;
        require(
            matches!(
                worker_failure,
                Err(PostgresKernelError::Database(source))
                    if source.as_db_error().is_some_and(|database| {
                        database.code() == &SqlState::UNDEFINED_COLUMN
                            && database.message().contains("no_such_resource_producer_column")
                    })
            ),
            "forced producer failure did not preserve the injected undefined-column SQLSTATE",
        )?;

        let request_bytes = request.request_id.to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let rows = audit_session
                .client()
                .query(
                    "SELECT resource.nested_invocation_id,
                            resource.parent_invocation_id,
                            resource.call_site_id,
                            resource.target_function_id,
                            resource.source_revision_id,
                            resource.catalogue_revision_id,
                            resource.session_principal_id,
                            resource.decision_outcome,
                            resource.terminal_outcome,
                            resource.item_count,
                            resource.byte_count,
                            invocation.outcome AS invocation_outcome,
                            invocation.session_principal_id AS invocation_session_principal_id,
                            invocation.effective_principal_id AS invocation_effective_principal_id,
                            invocation.authorising_principal_id AS invocation_authorising_principal_id,
                            invocation.function_id AS invocation_function_id,
                            invocation.source_revision_id AS invocation_source_revision_id,
                            invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                            invocation.security_audit_event_id AS invocation_security_audit_event_id,
                            row_to_json(resource)::text AS resource_json,
                            row_to_json(invocation)::text AS invocation_json
                     FROM _orna_kernel.resource_audit_events AS resource
                     JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.nested_invocation_id
                     WHERE resource.request_id = $1",
                    &[&request_bytes],
                )
                .await?;
            require(rows.len() == 1, "worker failure did not leave exactly one resource audit row")?;
            let row = &rows[0];
            let nested_invocation_id: Vec<u8> = row.try_get("nested_invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            let invocation_outcome: String = row.try_get("invocation_outcome")?;
            let invocation_session_principal_id: Vec<u8> =
                row.try_get("invocation_session_principal_id")?;
            let invocation_effective_principal_id: Option<Vec<u8>> =
                row.try_get("invocation_effective_principal_id")?;
            let invocation_authorising_principal_id: Option<Vec<u8>> =
                row.try_get("invocation_authorising_principal_id")?;
            let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
            let invocation_source_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_source_revision_id")?;
            let invocation_catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_catalogue_revision_id")?;
            let invocation_security_audit_event_id: Option<Vec<u8>> =
                row.try_get("invocation_security_audit_event_id")?;
            let resource_json: String = row.try_get("resource_json")?;
            let invocation_json: String = row.try_get("invocation_json")?;
            require(
                nested_invocation_id.len() == 16
                    && nested_invocation_id.iter().any(|byte| *byte != 0)
                    && nested_invocation_id != request.request_id.to_bytes().to_vec()
                    && parent_invocation_id != nested_invocation_id
                    && parent_invocation_id == request.parent_invocation_id.to_bytes().to_vec()
                    && call_site_id == request.call_site_id.to_bytes().to_vec()
                    && target_function_id.is_none()
                    && source_revision_id.is_none()
                    && catalogue_revision_id.is_none()
                    && session_principal_id == RAW_CLIENT_USER.to_bytes().to_vec()
                    && decision_outcome == "denied"
                    && terminal_outcome == "failed"
                    && item_count.is_none()
                    && byte_count.is_none()
                    && invocation_outcome == "denied"
                    && invocation_session_principal_id == RAW_CLIENT_USER.to_bytes().to_vec()
                    && invocation_effective_principal_id.is_none()
                    && invocation_authorising_principal_id.is_none()
                    && invocation_function_id.is_none()
                    && invocation_source_revision_id.is_none()
                    && invocation_catalogue_revision_id.is_none()
                    && invocation_security_audit_event_id.is_none()
                    && !resource_json.contains(RESOURCE_INPUT)
                    && !invocation_json.contains(RESOURCE_INPUT),
                "worker compensation exposed target or retained non-redacted audit state",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "worker compensation audit",
        )?;
        let mut post_request = request.clone();
        post_request.stream_id += 1;
        post_request.request_id = InvocationId::from_bytes([0xb1; 16]);
        let post_start = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_failure(
                &session,
                &post_request,
                &ResourceCancellation::new(),
            )
            .await?;
        match post_start {
            orna_postgres::AuthenticatedServerResourceStart::Accepted(producer) => drop(producer),
            orna_postgres::AuthenticatedServerResourceStart::Failed { .. } => {
                return Err(failure("forced post-acceptance failure did not publish acceptance"));
            }
        }
        let post_request_bytes = post_request.request_id.to_bytes().to_vec();
        let post_audit_session = database.open().await?;
        let post_audit_operation = async {
            let row = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(row) = post_audit_session
                        .client()
                        .query_opt(
                            "SELECT resource.nested_invocation_id,
                                    resource.parent_invocation_id,
                                    resource.call_site_id,
                                    resource.target_function_id,
                                    resource.source_revision_id,
                                    resource.catalogue_revision_id,
                                    resource.decision_outcome,
                                    resource.terminal_outcome,
                                    invocation.outcome AS invocation_outcome,
                                    invocation.function_id AS invocation_function_id,
                                    invocation.source_revision_id AS invocation_source_revision_id,
                                    invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                    row_to_json(resource)::text AS resource_json,
                                    row_to_json(invocation)::text AS invocation_json
                             FROM _orna_kernel.resource_audit_events AS resource
                             JOIN _orna_kernel.invocation_audit_events AS invocation
                               ON invocation.invocation_id = resource.nested_invocation_id
                             WHERE resource.request_id = $1",
                            &[&post_request_bytes],
                        )
                        .await?
                    {
                        return Ok::<_, tokio_postgres::Error>(row);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("post-acceptance worker failure did not leave an audit row"))??;
            let nested_invocation_id: Vec<u8> = row.try_get("nested_invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let invocation_outcome: String = row.try_get("invocation_outcome")?;
            let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
            let invocation_source_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_source_revision_id")?;
            let invocation_catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_catalogue_revision_id")?;
            let resource_json: String = row.try_get("resource_json")?;
            let invocation_json: String = row.try_get("invocation_json")?;
            require(
                nested_invocation_id.len() == 16
                    && nested_invocation_id != post_request.request_id.to_bytes().to_vec()
                    && parent_invocation_id == post_request.parent_invocation_id.to_bytes().to_vec()
                    && call_site_id == post_request.call_site_id.to_bytes().to_vec()
                    && target_function_id == Some(target.to_bytes().to_vec())
                    && source_revision_id == Some(post_request.target_revision.source().to_bytes().to_vec())
                    && catalogue_revision_id
                        == Some(post_request.target_revision.catalogue().to_bytes().to_vec())
                    && decision_outcome == "allowed"
                    && terminal_outcome == "failed"
                    && invocation_outcome == "allowed"
                    && invocation_function_id == Some(target.to_bytes().to_vec())
                    && invocation_source_revision_id
                        == Some(post_request.target_revision.source().to_bytes().to_vec())
                    && invocation_catalogue_revision_id
                        == Some(post_request.target_revision.catalogue().to_bytes().to_vec())
                    && !resource_json.contains(RESOURCE_INPUT)
                    && !invocation_json.contains(RESOURCE_INPUT),
                "post-acceptance worker failure did not preserve bounded allowed identity evidence",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        finish_session(
            post_audit_operation,
            post_audit_session.shutdown().await,
            "post-acceptance worker compensation audit",
        )?;
        let mut post_audit_request = request.clone();
        post_audit_request.stream_id += 2;
        post_audit_request.request_id = InvocationId::from_bytes([0xc1; 16]);
        let post_audit_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_audit_failure(
                &session,
                &post_audit_request,
                &ResourceCancellation::new(),
            )
            .await?;
        match post_audit_result {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == post_audit_request.stream_id
                    && request_id == post_audit_request.request_id
                    && failure == CallFailure::InternalFailure,
                "post-acceptance audit failure did not return its redacted failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("post-acceptance audit failure was accepted"));
            }
        }
        assert_resource_compensation_audit_row(
            &database,
            &post_audit_request,
            Some(target),
            "allowed",
            "failed",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;

        let mut post_cancel_request = request.clone();
        post_cancel_request.stream_id += 3;
        post_cancel_request.request_id = InvocationId::from_bytes([0xc2; 16]);
        let post_cancel = ResourceCancellation::new();
        let post_cancel_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_audit_failure(
                &session,
                &post_cancel_request,
                &post_cancel,
            )
            .await?;
        match post_cancel_result {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == post_cancel_request.stream_id
                    && request_id == post_cancel_request.request_id
                    && failure == CallFailure::InternalFailure,
                "cancelled post-acceptance audit failure did not return its redacted failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("cancelled post-acceptance audit failure was accepted"));
            }
        }
        require(
            post_cancel.is_requested() && !post_cancel.try_begin_commit(),
            "cancelled post-acceptance audit compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &post_cancel_request,
            Some(target),
            "allowed",
            "cancelled",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;
        let mut cancelled_exit_request = request.clone();
        cancelled_exit_request.stream_id += 4;
        cancelled_exit_request.request_id = InvocationId::from_bytes([0xc4; 16]);
        let cancelled_exit = ResourceCancellation::new();
        let cancelled_exit_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_exit_audit_failure(
                &session,
                &cancelled_exit_request,
                &cancelled_exit,
            )
            .await?;
        match cancelled_exit_result {
            orna_postgres::AuthenticatedServerResourceStart::Accepted(producer) => drop(producer),
            orna_postgres::AuthenticatedServerResourceStart::Failed { .. } => {
                return Err(failure("cancelled producer exit audit did not publish acceptance"));
            }
        }
        require(
            cancelled_exit.is_requested() && !cancelled_exit.try_begin_commit(),
            "cancelled producer exit compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &cancelled_exit_request,
            Some(target),
            "allowed",
            "cancelled",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;

        let mut finalizer_cancel_request = request.clone();
        finalizer_cancel_request.stream_id += 5;
        finalizer_cancel_request.request_id = InvocationId::from_bytes([0xc3; 16]);
        let finalizer_cancel = ResourceCancellation::new();
        require(
            finalizer_cancel.request_cancel(),
            "pre-finalizer cancellation did not win the cancellation race",
        )?;
        let finalizer_cancel_result = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &finalizer_cancel_request,
                &finalizer_cancel,
            )
            .await;
        require(
            finalizer_cancel_result.is_err(),
            "pre-finalizer cancellation unexpectedly returned a public failure value",
        )?;
        require(
            finalizer_cancel.is_requested() && !finalizer_cancel.try_begin_commit(),
            "finalizer cancellation compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &finalizer_cancel_request,
            None,
            "denied",
            "cancelled",
            "denied",
            RESOURCE_INPUT,
        )
        .await?;

        let duplicate = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &request,
                &ResourceCancellation::new(),
            )
            .await?;
        match duplicate {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == request.stream_id
                    && request_id == request.request_id
                    && failure == CallFailure::InternalFailure,
                "reusing a compensated resource request did not return its redacted duplicate failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("duplicate resource request was accepted"));
            }
        }
        let count_session = database.open().await?;
        let count_operation = async {
            let row = count_session
                .client()
                .query_one(
                    "SELECT
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events
                           WHERE request_id = $1) AS resource_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events AS resource
                            JOIN _orna_kernel.invocation_audit_events AS invocation
                              ON invocation.invocation_id = resource.nested_invocation_id
                           WHERE resource.request_id = $1) AS invocation_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_request_history
                           WHERE request_id = $1) AS history_count",
                    &[&request_bytes],
                )
                .await?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let history_count: i64 = row.try_get("history_count")?;
            require(
                resource_count == 1 && invocation_count == 1 && history_count == 1,
                "duplicate resource request inserted extra resource, invocation, or history rows",
            )
        }
        .await;
        finish_session(
            count_operation,
            count_session.shutdown().await,
            "worker compensation duplicate count",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

async fn assert_resource_compensation_audit_row(
    database: &TestDatabase,
    request: &ResourceRequest,
    expected_target: Option<FunctionId>,
    expected_decision: &str,
    expected_terminal: &str,
    expected_invocation_outcome: &str,
    raw_marker: &str,
) -> TestResult<()> {
    let request_bytes = request.request_id.to_bytes().to_vec();
    let audit_session = database.open().await?;
    let audit_operation = async {
        let row = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(row) = audit_session
                    .client()
                    .query_opt(
                        "SELECT resource.nested_invocation_id,
                                resource.parent_invocation_id,
                                resource.call_site_id,
                                resource.target_function_id,
                                resource.source_revision_id,
                                resource.catalogue_revision_id,
                                resource.session_principal_id,
                                resource.decision_outcome,
                                resource.terminal_outcome,
                                invocation.outcome AS invocation_outcome,
                                invocation.session_principal_id AS invocation_session_principal_id,
                                invocation.effective_principal_id AS invocation_effective_principal_id,
                                invocation.authorising_principal_id AS invocation_authorising_principal_id,
                                invocation.function_id AS invocation_function_id,
                                invocation.source_revision_id AS invocation_source_revision_id,
                                invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                invocation.security_audit_event_id AS invocation_security_audit_event_id,
                                row_to_json(resource)::text AS resource_json,
                                row_to_json(invocation)::text AS invocation_json
                         FROM _orna_kernel.resource_audit_events AS resource
                         JOIN _orna_kernel.invocation_audit_events AS invocation
                           ON invocation.invocation_id = resource.nested_invocation_id
                         WHERE resource.request_id = $1",
                        &[&request_bytes],
                    )
                    .await?
                {
                    return Ok::<_, tokio_postgres::Error>(row);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| failure("resource compensation did not leave its audit row"))??;
        let nested_invocation_id: Vec<u8> = row.try_get("nested_invocation_id")?;
        let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
        let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
        let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
        let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
        let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
        let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
        let decision_outcome: String = row.try_get("decision_outcome")?;
        let terminal_outcome: String = row.try_get("terminal_outcome")?;
        let invocation_outcome: String = row.try_get("invocation_outcome")?;
        let invocation_session_principal_id: Vec<u8> =
            row.try_get("invocation_session_principal_id")?;
        let invocation_effective_principal_id: Option<Vec<u8>> =
            row.try_get("invocation_effective_principal_id")?;
        let invocation_authorising_principal_id: Option<Vec<u8>> =
            row.try_get("invocation_authorising_principal_id")?;
        let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
        let invocation_source_revision_id: Option<Vec<u8>> =
            row.try_get("invocation_source_revision_id")?;
        let invocation_catalogue_revision_id: Option<Vec<u8>> =
            row.try_get("invocation_catalogue_revision_id")?;
        let invocation_security_audit_event_id: Option<Vec<u8>> =
            row.try_get("invocation_security_audit_event_id")?;
        let resource_json: String = row.try_get("resource_json")?;
        let invocation_json: String = row.try_get("invocation_json")?;
        let target_bytes = expected_target.map(|target| target.to_bytes().to_vec());
        let source_bytes = expected_target.map(|_| request.target_revision.source().to_bytes().to_vec());
        let catalogue_bytes =
            expected_target.map(|_| request.target_revision.catalogue().to_bytes().to_vec());
        let principal_bytes = RAW_CLIENT_USER.to_bytes().to_vec();
        require(
            nested_invocation_id.len() == 16
                && nested_invocation_id != request.request_id.to_bytes().to_vec()
                && parent_invocation_id != nested_invocation_id
                && parent_invocation_id == request.parent_invocation_id.to_bytes().to_vec()
                && call_site_id == request.call_site_id.to_bytes().to_vec()
                && target_function_id == target_bytes
                && source_revision_id == source_bytes
                && catalogue_revision_id == catalogue_bytes
                && session_principal_id == principal_bytes
                && decision_outcome == expected_decision
                && terminal_outcome == expected_terminal
                && invocation_outcome == expected_invocation_outcome
                && invocation_session_principal_id == principal_bytes
                && invocation_effective_principal_id
                    == expected_target.map(|_| RAW_CLIENT_USER.to_bytes().to_vec())
                && invocation_authorising_principal_id
                    == expected_target.map(|_| RAW_CLIENT_USER.to_bytes().to_vec())
                && invocation_function_id == target_bytes
                && invocation_source_revision_id == source_bytes
                && invocation_catalogue_revision_id == catalogue_bytes
                && invocation_security_audit_event_id.is_some() == expected_target.is_some()
                && !resource_json.contains(raw_marker)
                && !invocation_json.contains(raw_marker),
            "resource compensation changed bounded identity, audit, or redaction evidence",
        )?;
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    finish_session(
        audit_operation,
        audit_session.shutdown().await,
        "resource compensation audit",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn direct_scalar_resource_holds_active_revision_lock_through_execution() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("scalar resource lock proof has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)?;
        let (active, _client, target, call_site) =
            install_scalar_resource_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.push(SecurityFunctionTarget::verified_standard(
            target,
            standard.verified_snapshot().revision(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        ));
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![principal],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, target)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let request = ResourceRequest {
            stream_id: 91,
            request_id: InvocationId::from_bytes([0x91; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Single,
            arguments: vec![ResourceArgument {
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                value: RuntimeValue::Integer(43),
            }],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };

        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("scalar resource lock proof has no source unit"))?;
        let changed_source = SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n-- direct scalar resource lock interleave", unit.content())
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?;
        let changed_report = check_standard_application(
            &changed_source,
            &StandardApplicationCheckContext::try_new(active.catalogue(), &standard)?,
        );
        require(
            changed_report.diagnostics().is_empty(),
            "direct scalar lock interleave source-only apply did not compile",
        )?;
        let changed = prepare_standard_application(&changed_report, active.pair(), &active)?;
        let changed_pair = changed.candidate_pair();

        let reached = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let dispatch_kernel = kernel.clone();
        let dispatch_session = session.clone();
        let dispatch_request = request.clone();
        let dispatch_reached = reached.clone();
        let dispatch_resume = resume.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_kernel
                .dispatch_authenticated_server_resource_with_test_barrier(
                    &dispatch_session,
                    &dispatch_request,
                    dispatch_reached,
                    dispatch_resume,
                )
                .await
        });
        timeout(Duration::from_secs(5), reached.wait())
            .await
            .map_err(|_| failure("direct scalar resource dispatch did not reach validation barrier"))?;

        let waiter = database.open().await?;
        let apply_kernel = kernel.clone();
        let mut apply = tokio::spawn(async move { apply_kernel.apply(&changed).await });
        let apply_waiting = timeout(Duration::from_secs(5), async {
            loop {
                if apply.is_finished() {
                    return Ok::<bool, tokio_postgres::Error>(false);
                }
                let waiting = waiter
                    .client()
                    .query_one(
                        "SELECT EXISTS (
                             SELECT 1
                             FROM pg_stat_activity AS waiting
                            WHERE waiting.pid <> pg_backend_pid()
                              AND waiting.wait_event_type = 'Lock'
                              AND waiting.query LIKE '%_orna_kernel.active_revision%'
                              AND cardinality(pg_blocking_pids(waiting.pid)) > 0
                         )",
                        &[],
                    )
                    .await?
                    .get::<_, bool>(0);
                if waiting {
                    return Ok(true);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| failure("apply did not reach the active-revision lock wait"))??;

        resume.wait().await;
        let dispatched = timeout(Duration::from_secs(5), dispatch)
            .await
            .map_err(|_| failure("direct scalar resource dispatch did not resume"))?;
        let dispatched = dispatched.map_err(|error| {
            failure(format!("direct scalar resource task failed: {error}"))
        })?;
        let dispatched = dispatched?;
        let applied = timeout(Duration::from_secs(5), &mut apply)
            .await
            .map_err(|_| failure("source-only apply did not resume after resource completion"))?;
        let applied = applied
            .map_err(|error| failure(format!("source-only apply task failed: {error}")))?;
        let applied = applied?;
        waiter.shutdown().await?;

        require(
            apply_waiting,
            "source-only apply committed while direct scalar resource execution was paused",
        )?;
        require(
            applied.pair() == changed_pair,
            "source-only apply did not commit its replacement active revision",
        )?;
        match dispatched {
            AuthenticatedServerResourceResult::Completed {
                target_revision,
                resource_kind,
                values,
                ..
            } => {
                require(
                    target_revision == active.pair()
                        && resource_kind == ResourceKind::Single
                        && values == [RuntimeValue::Integer(43)],
                    "direct scalar resource did not execute against its locked active revision",
                )?;
            }
            AuthenticatedServerResourceResult::Failed {
                failure: call_failure,
                ..
            } => {
                return Err(failure(format!(
                    "direct scalar resource unexpectedly failed: {call_failure:?}"
                )));
            }
        }
        require(
            applied.pair() != active.pair(),
            "source-only apply did not advance the active revision pair",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn installed_resource_socket_delivers_values_and_enforces_windows_and_grants() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = open_standard_database(kernel(&database)?)
            .await
            .map_err(|error| failure(format!("open standard database failed: {error:?}")))?;
        let active = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover installed standard failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("installed resource fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, _client_function, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard)
                .await
                .map_err(|error| failure(format!("install installed stream fixture failed: {error:?}")))?;
        let all_target = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "all"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.all"))?
            .id();
        let probe_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["resource_fixture", "probe"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.probe"))?
            .id();
        let probe_relation = format!(
            "_orna_data.t_{:032x}",
            u128::from_be_bytes(probe_type.to_bytes()),
        );
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.create"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("resource_fixture.create.p_marker is absent from the active catalogue"))?
            .id();
        let sequence_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("resource_fixture.create.p_sequence is absent from the active catalogue"))?
            .id();
        for (sequence, marker) in ["resource-value", "resource-value-2"].into_iter().enumerate() {
            kernel
                .execute_server_insert(
                    create,
                    &[
                        FunctionArgument::new(
                            create_parameter,
                            RuntimeValue::Text(marker.into()),
                        )?,
                        FunctionArgument::new(
                            sequence_parameter,
                            RuntimeValue::Integer((sequence + 1) as i32),
                        )?,
                    ],
                )
                .await
                .map_err(|error| failure(format!("insert resource fixture row failed: {error:?}")))?;
        }

        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let registry = registered_opaque_codecs(&standard_source)?;
        let granted_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, all_target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted_security).await?;


        let first_value_bytes = exact_resource_value_bytes(
            &active,
            &registry,
            &ResourceServerFrame::Values(orna_protocol::ResourceValues {
                stream_id: 2,
                request_id: InvocationId::from_bytes([0x51; 16]),
                batch_sequence: 0,
                item_count: 1,
                byte_count: 0,
                values: vec![RuntimeValue::Text("resource-value".into())],
            }),
        )?;
        let stream_request = ResourceRequest {
            stream_id: 2,
            request_id: InvocationId::from_bytes([0x51; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x52; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let item_barrier_request = ResourceRequest {
            stream_id: 3,
            request_id: InvocationId::from_bytes([0x53; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x54; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let byte_request = ResourceRequest {
            stream_id: 4,
            request_id: InvocationId::from_bytes([0x55; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x56; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: MAX_RESOURCE_WINDOW,
            byte_window: first_value_bytes as u64,
        };
        let byte_barrier_request = ResourceRequest {
            stream_id: 5,
            request_id: InvocationId::from_bytes([0x57; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x58; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        for request in [
            &stream_request,
            &item_barrier_request,
            &byte_request,
            &byte_barrier_request,
        ] {
            require(
                authorizer.expect(request),
                "installed resource socket test could not register its request",
            )?;
        }
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer,
        ));
        let stream_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "resource stream socket did not complete the constructed handshake",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(stream_request.clone()),
            )
            .await?;
            let accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            if !matches!(
                &accepted,
                ResourceServerFrame::Accepted(frame) if frame.stream_id == 2
            ) {
                return Err(failure(format!(
                    "resource stream socket returned an unexpected acceptance frame: {accepted:?}",
                )));
            }
            let (_, first_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &first_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 2
                            && frame.batch_sequence == 0
                            && frame.item_count == 1
                            && frame.byte_count as usize == first_value_bytes
                            && frame.values == [RuntimeValue::Text("resource-value".into())]
                ),
                "resource stream socket did not return the exact first item-credit batch",
            )?;

            // The barrier has one byte of credit, so it cannot publish its
            // value until the test restores that credit.
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 2,
                    request_id: stream_request.request_id,
                    add_items: 0,
                    add_bytes: first_value_bytes as u64,
                }),
            )
            .await?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(item_barrier_request.clone()),
            )
            .await?;
            let item_barrier_accepted =
                timeout(
                    Duration::from_secs(5),
                    read_resource_server_frame_from_socket(&mut client, &active, &registry),
                )
                .await
                .map_err(|_| failure("item barrier acceptance timed out"))??;
            require(
                matches!(
                    item_barrier_accepted,
                    ResourceServerFrame::Accepted(frame) if frame.stream_id == item_barrier_request.stream_id
                ),
                "item-only restoration released a stream with exhausted item credit",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 2,
                    request_id: stream_request.request_id,
                    add_items: 1,
                    add_bytes: 0,
                }),
            )
            .await?;
            let (_, second_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            let expected_second_bytes = exact_resource_value_bytes(&active, &registry, &second_values)?;
            require(
                matches!(
                    &second_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 2
                            && frame.batch_sequence == 1
                            && frame.item_count == 1
                            && frame.byte_count as usize == expected_second_bytes
                            && frame.values == [RuntimeValue::Text("resource-value-2".into())]
                ),
                "item-credit restoration did not resume the exact second batch",
            )?;
            let completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    completed,
                    ResourceServerFrame::Completed(frame)
                        if frame.stream_id == 2
                            && frame.final_batch_sequence == 1
                            && frame.total_items == 2
                ),
                "resource stream socket did not complete after item-credit restoration",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: item_barrier_request.stream_id,
                    request_id: item_barrier_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW - 1,
                }),
            )
            .await?;
            let barrier_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_values, ResourceServerFrame::Values(frame) if frame.stream_id == 3),
                "item-credit barrier stream did not receive its typed SERVER result",
            )?;
            let barrier_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 3),
                "item-credit barrier stream did not complete",
            )?;

            // Start a second stream with exactly one value's byte credit but
            // ample item credit. Restoring only item credit must not release it.
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(byte_request.clone()),
            )
            .await?;
            let accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(accepted, ResourceServerFrame::Accepted(frame) if frame.stream_id == 4),
                "resource stream socket did not accept the byte-credit request",
            )?;
            let (_, first_byte_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &first_byte_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 4
                            && frame.batch_sequence == 0
                            && frame.item_count == 1
                            && frame.byte_count as usize == first_value_bytes
                            && frame.values == [RuntimeValue::Text("resource-value".into())]
                ),
                "byte-credit request did not consume exactly its initial byte credit",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 4,
                    request_id: byte_request.request_id,
                    add_items: 1,
                    add_bytes: 0,
                }),
            )
            .await?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(byte_barrier_request.clone()),
            )
            .await?;
            let byte_barrier_accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    byte_barrier_accepted,
                    ResourceServerFrame::Accepted(frame) if frame.stream_id == byte_barrier_request.stream_id
                ),
                "byte-credit restoration released a stream with exhausted byte credit",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 4,
                    request_id: byte_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW,
                }),
            )
            .await?;
            let (_, second_byte_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            let expected_second_byte_values =
                exact_resource_value_bytes(&active, &registry, &second_byte_values)?;
            require(
                matches!(
                    &second_byte_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 4
                            && frame.batch_sequence == 1
                            && frame.item_count == 1
                            && frame.byte_count as usize == expected_second_byte_values
                            && frame.values == [RuntimeValue::Text("resource-value-2".into())]
                ),
                "byte-credit restoration did not resume the exact second batch",
            )?;
            let completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 4 && frame.total_items == 2),
                "byte-credit stream did not complete after restoration",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: byte_barrier_request.stream_id,
                    request_id: byte_barrier_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW - 1,
                }),
            )
            .await?;
            let barrier_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_values, ResourceServerFrame::Values(frame) if frame.stream_id == 5),
                "byte-credit barrier stream did not receive its typed SERVER result",
            )?;
            let barrier_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 5),
                "byte-credit barrier stream did not complete",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            stream_operation,
            finish_session(shutdown, connection, "stream resource socket cleanup"),
            "stream resource socket operation",
        )?;

        // Authenticate and recover the socket before taking the fixture lock.
        // Catalogue recovery reads the fixture relation metadata.
        let waiter_session = database.open().await?;
        let cancellation_request = ResourceRequest {
            stream_id: 6,
            request_id: InvocationId::from_bytes([0x61; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x62; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&cancellation_request),
            "cancellation resource socket test could not register its request",
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let cancellation_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "cancellation socket did not complete the constructed handshake",
            )?;
            let locker = database.open().await?;
            locker
                .client()
                .batch_execute(&format!("BEGIN; LOCK TABLE {probe_relation} IN ACCESS EXCLUSIVE MODE;"))
                .await?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(cancellation_request.clone()),
            )
            .await?;
            let lock_waiting = timeout(Duration::from_secs(5), async {
                loop {
                    let waiting = waiter_session
                        .client()
                        .query_one(
                            &format!(
                                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE relation = '{probe_relation}'::regclass AND NOT granted)",
                            ),
                            &[],
                        )
                        .await?
                        .get::<_, bool>(0);
                    if waiting {
                        return Ok::<(), tokio_postgres::Error>(());
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("resource dispatch did not reach its lock wait state"))?;
            lock_waiting?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id: cancellation_request.stream_id,
                    request_id: cancellation_request.request_id,
                    reason: ResourceCancellationCode::ClientRequested,
                }),
            )
            .await?;
            locker.shutdown().await?;
            let cancelled = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    cancelled,
                    ResourceServerFrame::Cancelled(frame)
                        if frame.stream_id == cancellation_request.stream_id
                            && frame.request_id == cancellation_request.request_id
                            && frame.reason == ResourceCancellationCode::ClientRequested
                ),
                "active authenticated resource dispatch did not terminate as cancelled",
            )?;

            let replacement_request = ResourceRequest {
                stream_id: 8,
                request_id: InvocationId::from_bytes([0x81; 16]),
                parent_invocation_id: InvocationId::from_bytes([0x82; 16]),
                call_site_id: call_site,
                state_profile: String::new(),
                function_instance_key: String::new(),
                target_function_id: target,
                target_revision: active.pair(),
                generation: 1,
                resource_kind: ResourceKind::Stream,
                arguments: vec![ResourceArgument { parameter, value: RuntimeValue::Text("resource-value".into()) }],
                item_window: 1,
                byte_window: MAX_RESOURCE_WINDOW,
            };
            require(
                authorizer.expect(&replacement_request),
                "replacement resource socket test could not register its request",
            )?;
            send_resource_client_frame_to_socket(&mut client, &active, &registry, &ResourceClientFrame::Request(replacement_request.clone())).await?;
            let replacement_accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_accepted, ResourceServerFrame::Accepted(frame) if frame.stream_id == replacement_request.stream_id), "resource executor was not reusable after cancellation")?;
            let replacement_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_values, ResourceServerFrame::Values(frame) if frame.stream_id == replacement_request.stream_id && frame.values == [RuntimeValue::Text("resource-value".into())]), "replacement request did not return its typed value")?;
            let replacement_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == replacement_request.stream_id && frame.total_items == 1), "replacement request did not complete after cancellation")
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let waiter_shutdown = waiter_session.shutdown();
        finish_session(
            cancellation_operation,
            finish_session(
                shutdown,
                connection,
                "cancellation resource socket cleanup",
            ),
            "cancellation resource socket operation",
        )?;
        waiter_shutdown.await?;

        let denied_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied_security).await?;
        let denied_request = ResourceRequest {
            stream_id: 7,
            request_id: InvocationId::from_bytes([0x71; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x72; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&denied_request),
            "denied resource socket test could not register its request",
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer,
        ));
        let denied_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "denied resource socket did not complete the constructed handshake",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(denied_request.clone()),
            )
            .await?;
            let failed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    failed,
                    ResourceServerFrame::Failed(frame)
                        if frame.stream_id == denied_request.stream_id
                            && frame.request_id == denied_request.request_id
                            && frame.failure == CallFailure::ExecuteDenied
                ),
                "resource socket did not return execute denial",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "denied resource socket cleanup"),
            "denied resource socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let stream_allows = audits.iter().filter(|audit| {
            let decision = audit.decision();
            decision.kind() == SecurityAuditKind::Execute
                && decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.target() == Some(InvocationTarget::new(all_target, active.pair()))
        }).count();
        let denied = audits.iter().find(|audit| {
            let decision = audit.decision();
            decision.kind() == SecurityAuditKind::Execute
                && decision.outcome() == SecurityAuditOutcome::Denied
                && decision.target() == Some(InvocationTarget::new(target, active.pair()))
        });
        // The cancelled request loses its uncommitted allowed decision when
        // cancellation aborts the blocked execution transaction.
        require(
            stream_allows >= 2
                && denied.is_some_and(|audit| {
                    audit.decision().denial()
                        == Some(SecurityAuditDenial::Execute(ExecuteDenial::MissingExecuteGrant))
                    && audit.decision().effective_principal().is_none()
                    && audit.decision().authorising_principal().is_none()
                }),
            "resource socket audit evidence did not record stream allows and redacted denial",
        )?;
        let audit_text = format!("{audits:?}");
        require(
            !audit_text.contains("resource-value")
                && !audit_text.contains("resource-value-2"),
            "resource audit evidence retained raw argument or result detail",
        )?;

        assert_resource_audit_rows(&database, &active, call_site, target, all_target).await?;

        // The terminal rows must remain queryable after the current kernel and
        // audit session are gone and the installed standard is recovered by a
        // fresh kernel instance.
        drop(kernel);
        let recovered_kernel = open_standard_database(database.connection_string().parse()?)
            .await
            .map_err(|error| failure(format!("reopen standard database failed: {error:?}")))?;
        let recovered_active = recovered_kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover installed standard after reopen failed: {error:?}")))?;
        require(
            recovered_active.pair() == active.pair(),
            "fresh recovery returned a different active revision pair",
        )?;
        assert_resource_audit_rows(
            &database,
            &recovered_active,
            call_site,
            target,
            all_target,
        )
        .await?;
        drop(recovered_kernel);

        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_argument_pair_socket_binds_reverse_order_by_parameter_identity() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (active, probe, create_pair, first, second, read_first, read_second) =
            install_raw_argument_pair_socket_fixture(&kernel, &active, &standard_upgrade).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied_security).await?;

        // The local peer is authenticated. Denial still wins over the reversed
        // same-typed values and their parameter identities.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "argument-pair denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("denied second")),
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("denied first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "argument-pair denied socket disclosed a target or value fact",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "argument-pair denied socket cleanup"),
            "argument-pair denied socket operation",
        )?;

        let granted_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_pair),
                ExecuteGrant::new(RAW_CLIENT_USER, read_first),
                ExecuteGrant::new(RAW_CLIENT_USER, read_second),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted_security).await?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 2,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: probe,
                            object: ObjectId::from_bytes([0x11; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "argument-pair granted socket returned the wrong acknowledgement",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("stored second")),
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("stored first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "argument-pair granted socket did not accept the complete reverse-order pair",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "argument-pair socket emitted a Reference without result-value credit",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
            )
            .await?;
            let created = read_active_protocol_frame(&mut client, &active).await?;
            let ServerFrame::EventBatch {
                stream: 2, events, ..
            } = created
            else {
                return Err(failure(
                    "argument-pair socket did not emit one Reference event",
                ));
            };
            let [
                orna_protocol::EventRecord {
                    sequence: 1,
                    event: Event::Value(RuntimeValue::Reference { target, object }),
                },
            ] = events.as_slice()
            else {
                return Err(failure(
                    "argument-pair socket returned the wrong create event",
                ));
            };
            require(
                *target == probe && *object != ObjectId::from_bytes([0; 16]),
                "argument-pair socket returned the wrong created Reference",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "argument-pair socket did not complete the credited create",
            )?;

            // The retained zero/one paths, every malformed pair shape, and a
            // pair on a read target stay redacted after accepted framing.
            let mut wrong_bytes = first.to_bytes();
            wrong_bytes[0] ^= 0x01;
            let wrong = ParameterId::from_bytes(wrong_bytes);
            let mut third_bytes = second.to_bytes();
            third_bytes[0] ^= 0x01;
            let third = ParameterId::from_bytes(third_bytes);
            require(
                third != first && third != second && third != wrong,
                "the third pair parameter must be a distinct synthetic identity",
            )?;
            let rejected = [
                (3, create_pair, vec![]),
                (
                    4,
                    create_pair,
                    vec![(first, RuntimeValue::Text(String::from("missing second")))],
                ),
                (
                    5,
                    create_pair,
                    vec![
                        (wrong, RuntimeValue::Text(String::from("wrong"))),
                        (second, RuntimeValue::Text(String::from("second"))),
                    ],
                ),
                (
                    6,
                    create_pair,
                    vec![
                        (first, RuntimeValue::Text(String::from("third first"))),
                        (second, RuntimeValue::Text(String::from("third second"))),
                        (third, RuntimeValue::Text(String::from("third extra"))),
                    ],
                ),
                (
                    7,
                    create_pair,
                    vec![
                        (first, RuntimeValue::Integer(7)),
                        (second, RuntimeValue::Text(String::from("typed second"))),
                    ],
                ),
                (
                    8,
                    read_first,
                    vec![
                        (first, RuntimeValue::Text(String::from("non-insert first"))),
                        (
                            second,
                            RuntimeValue::Text(String::from("non-insert second")),
                        ),
                    ],
                ),
            ];
            for (stream, function, arguments) in rejected {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: actual, .. } if actual == stream
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream,
                            failure: CallFailure::TargetUnavailable,
                        },
                    "argument-pair socket changed a closed target into public detail",
                )?;
            }

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 9,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("cancel second")),
                },
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("cancel first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 9 },
                ClientFrame::CallCancel { stream: 9 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 9, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 9 },
                "argument-pair socket did not cancel the accepted complete pair",
            )?;

            for (stream, function, expected) in [
                (
                    10,
                    read_first,
                    [
                        RuntimeValue::Text(String::from("stored first")),
                        RuntimeValue::Text(String::from("cancel first")),
                    ],
                ),
                (
                    11,
                    read_second,
                    [
                        RuntimeValue::Text(String::from("stored second")),
                        RuntimeValue::Text(String::from("cancel second")),
                    ],
                ),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                let accepted = read_active_protocol_frame(&mut client, &active).await?;
                let first_event = read_active_protocol_frame(&mut client, &active).await?;
                let second_event = read_active_protocol_frame(&mut client, &active).await?;
                let completed = read_active_protocol_frame(&mut client, &active).await?;
                let text_value = |frame: &ServerFrame, sequence| match frame {
                    ServerFrame::EventBatch {
                        stream: actual,
                        events,
                        ..
                    } if *actual == stream
                        && events.len() == 1
                        && events[0].sequence == sequence => match &events[0].event {
                        Event::Value(RuntimeValue::Text(value)) => Some(value.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let first_value = text_value(&first_event, 1);
                let second_value = text_value(&second_event, 2);
                let expected_first = match &expected[0] {
                    RuntimeValue::Text(value) => value.as_str(),
                    _ => unreachable!("the reader oracle uses only Text values"),
                };
                let expected_second = match &expected[1] {
                    RuntimeValue::Text(value) => value.as_str(),
                    _ => unreachable!("the reader oracle uses only Text values"),
                };
                let values_match = (first_value.as_deref() == Some(expected_first)
                    && second_value.as_deref() == Some(expected_second))
                    || (first_value.as_deref() == Some(expected_second)
                        && second_value.as_deref() == Some(expected_first));
                if !matches!(accepted, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                    || !values_match
                    || !matches!(completed, ServerFrame::CallCompleted { stream: actual } if actual == stream)
                {
                    return Err(failure(format!(
                        "argument-pair socket read {stream} returned {accepted:?}, {first_event:?}, {second_event:?}, {completed:?}"
                    )));
                }
            }

            // Duplicate ParameterIds close the protocol connection before a
            // completed RawCall exists. They emit no accepted frame, disclose
            // no target, add no audit, and cannot add a row.
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::CallRawStart {
                    stream: 12,
                    function: create_pair,
                },
            )
            .await?;
            for value in ["duplicate first", "duplicate second"] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgument {
                        stream: 12,
                        parameter: first,
                        value: RuntimeValue::Text(String::from(value)),
                    },
                )
                .await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await,
                    Err(error) if error.to_string() == "early eof"
                ),
                "a duplicate argument pair did not fail closed before dispatch",
            )?;
            Ok(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await?;
        finish_session(
            granted_operation,
            shutdown,
            "argument-pair granted socket operation",
        )?;
        require(
            matches!(connection, Err(LocalRawSocketError::Connection { .. })),
            "a duplicate argument pair did not close the protocol connection",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let actual = audits
            .iter()
            .map(|event| {
                let decision = event.decision();
                (decision.kind(), decision.outcome(), decision.target())
            })
            .collect::<Vec<_>>();
        let allowed = SecurityAuditOutcome::Allowed;
        let expected = vec![
                    (SecurityAuditKind::Authentication, allowed, None),
                    (
                        SecurityAuditKind::Execute,
                        SecurityAuditOutcome::Denied,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (SecurityAuditKind::Authentication, allowed, None),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_first, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_first, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_second, active.pair())),
                    ),
        ];
        if actual != expected {
            return Err(failure(format!(
                "argument-pair socket audit sequence changed: actual {actual:?}; expected {expected:?}"
            )));
        }
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0050 dispatch"]
async fn raw_reference_value_update_socket_retains_pair_authority() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (
            active,
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
        ) = install_raw_reference_value_update_socket_fixture(&kernel, &active, &standard_upgrade)
            .await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied).await?;

        // Authentication is local-peer binding. An ungranted pair must not
        // disclose the selected Reference or the private update shape.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "reference value denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 1, function: update_text },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: text_selector,
                    value: RuntimeValue::Reference {
                        target: probe,
                        object: ObjectId::from_bytes([0x31; 16]),
                    },
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: text_value,
                    value: RuntimeValue::Text(String::from("denied")),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "reference value denied socket disclosed an unavailable target fact",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "reference value denied socket cleanup"),
            "reference value denied socket operation",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create),
                ExecuteGrant::new(RAW_CLIENT_USER, update_text),
                ExecuteGrant::new(RAW_CLIENT_USER, update_link),
                ExecuteGrant::new(RAW_CLIENT_USER, read_stored),
                ExecuteGrant::new(RAW_CLIENT_USER, read_links),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let direct_session = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 3,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: probe,
                            object: ObjectId::from_bytes([0x32; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "reference value granted socket returned the wrong acknowledgement",
            )?;

            let mut created = Vec::new();
            for (stream, stored) in [(1, "first"), (2, "second")] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function: create },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_stored,
                        value: RuntimeValue::Text(String::from(stored)),
                    },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "reference value socket did not accept the create call",
                )?;
                let event = read_active_protocol_frame(&mut client, &active).await?;
                let ServerFrame::EventBatch { events, .. } = event else {
                    return Err(failure("reference value socket create did not return an event batch"));
                };
                let [orna_protocol::EventRecord {
                    event: Event::Value(RuntimeValue::Reference { target, object }), ..
                }] = events.as_slice() else {
                    return Err(failure("reference value socket create did not return one Reference"));
                };
                require(
                    *target == probe && *object != ObjectId::from_bytes([0; 16]),
                    "reference value socket create returned the wrong Reference",
                )?;
                created.push(RuntimeValue::Reference { target: *target, object: *object });
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream },
                    "reference value socket create did not complete",
                )?;
            }
            let [first, second] = created.as_slice() else {
                return Err(failure("reference value socket did not create two rows"));
            };

            // The raw socket closes both conflict forms to the same public
            // failure. The trusted direct dispatcher retains the typed
            // source, because no socket frame may expose it.
            let duplicate_value = RuntimeValue::Text(String::from("second"));
            let duplicate_insert = RawClientDispatch::new(
                kernel.clone(),
                direct_session.clone(),
                12,
                RawCall {
                    function: create,
                    arguments: vec![orna_protocol::CallArgument {
                        parameter: create_stored,
                        value: duplicate_value.clone(),
                    }],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate_insert,
                12,
                CallFailure::InternalFailure,
                matches!(
                    duplicate_insert.source(),
                    Some(PostgresKernelError::ServerInsert(
                        ServerInsertError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "a duplicate raw Text INSERT did not retain its private typed conflict",
            )?;
            let duplicate_update = RawClientDispatch::new(
                kernel.clone(),
                direct_session.clone(),
                13,
                RawCall {
                    function: update_text,
                    arguments: vec![
                        orna_protocol::CallArgument {
                            parameter: text_selector,
                            value: first.clone(),
                        },
                        orna_protocol::CallArgument {
                            parameter: text_value,
                            value: duplicate_value.clone(),
                        },
                    ],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate_update,
                13,
                CallFailure::InternalFailure,
                matches!(
                    duplicate_update.source(),
                    Some(PostgresKernelError::ServerUpdate(
                        ServerUpdateError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "a duplicate raw Text UPDATE did not retain its private typed conflict",
            )?;

            for (stream, function, arguments) in [
                (
                    3,
                    create,
                    vec![(create_stored, duplicate_value.clone())],
                ),
                (
                    4,
                    update_text,
                    vec![
                        (text_selector, first.clone()),
                        (text_value, duplicate_value.clone()),
                    ],
                ),
            ] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: actual, .. } if actual == stream
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream,
                            failure: CallFailure::InternalFailure,
                        },
                    "a duplicate raw Text mutation disclosed a private conflict fact",
                )?;
                require(
                    timeout(
                        Duration::from_millis(50),
                        read_active_protocol_frame(&mut client, &active),
                    )
                    .await
                    .is_err(),
                    "a duplicate raw Text mutation emitted a value frame after its terminal failure",
                )?;
            }

            // Selector-first framing reverses the declaration order. The
            // accepted call emits no value until exact result credit arrives.
            for frame in [
                ClientFrame::CallRawStart { stream: 5, function: update_text },
                ClientFrame::CallArgument { stream: 5, parameter: text_selector, value: first.clone() },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: text_value,
                    value: RuntimeValue::Text(String::from("changed")),
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 5, .. }),
                "reference value socket did not accept the scalar pair UPDATE",
            )?;
            sleep(Duration::from_millis(50)).await;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active)
                )
                .await
                .is_err(),
                "reference value socket emitted an UPDATE result without credit",
            )?;
            send_active_protocol_frame(&mut client, &active, &ClientFrame::WindowUpdate {
                stream: 5,
                channel: Channel::ResultValues,
                credit: reference_credit
                    .checked_sub(1)
                    .ok_or_else(|| failure("reference value event credit must be nonzero"))?,
            })
            .await?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active)
                )
                .await
                .is_err(),
                "reference value socket emitted an UPDATE result before its exact credit boundary",
            )?;
            send_active_protocol_frame(&mut client, &active, &ClientFrame::WindowUpdate {
                stream: 5, channel: Channel::ResultValues, credit: 1,
            }).await?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch { stream: 5, events, .. }
                        if events.len() == 1 && events[0].sequence == 1
                            && events[0].event == Event::Value(first.clone())
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 5 },
                "reference value socket scalar UPDATE did not return the exact selector",
            )?;

            let RuntimeValue::Reference { object: first_object, .. } = first else {
                return Err(failure("first reference value socket row is not a Reference"));
            };
            let RuntimeValue::Reference { object: second_object, .. } = second else {
                return Err(failure("second reference value socket row is not a Reference"));
            };
            let absent_object = [[0x41; 16], [0x42; 16], [0x43; 16]]
                .into_iter()
                .map(ObjectId::from_bytes)
                .find(|candidate| candidate != first_object && candidate != second_object)
                .ok_or_else(|| failure("reference value socket has no absent object identity"))?;
            for frame in [
                ClientFrame::CallRawStart { stream: 6, function: update_text },
                ClientFrame::CallArgument { stream: 6, parameter: text_value, value: RuntimeValue::Text(String::from("absent")) },
                ClientFrame::CallArgument {
                    stream: 6,
                    parameter: text_selector,
                    value: RuntimeValue::Reference { target: probe, object: absent_object },
                },
                ClientFrame::WindowUpdate { stream: 6, channel: Channel::ResultValues, credit: 1024 },
                ClientFrame::CallArgumentsComplete { stream: 6 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 6, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 6 },
                "reference value socket absent selector did not complete empty",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 7, function: update_link },
                ClientFrame::CallArgument { stream: 7, parameter: link_selector, value: second.clone() },
                ClientFrame::CallArgument { stream: 7, parameter: link_value, value: first.clone() },
                ClientFrame::WindowUpdate { stream: 7, channel: Channel::ResultValues, credit: 1024 },
                ClientFrame::CallArgumentsComplete { stream: 7 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 7, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 7, events, .. }
                            if events.len() == 1 && events[0].event == Event::Value(second.clone())
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 7 },
                "reference value socket Reference UPDATE did not bind the selected row",
            )?;

            // Cancellation remains public. The accepted mutation commits, so
            // the later socket read is the durable oracle.
            for frame in [
                ClientFrame::CallRawStart { stream: 8, function: update_text },
                ClientFrame::CallArgument { stream: 8, parameter: text_selector, value: first.clone() },
                ClientFrame::CallArgument { stream: 8, parameter: text_value, value: RuntimeValue::Text(String::from("second\n")) },
                ClientFrame::CallArgumentsComplete { stream: 8 },
                ClientFrame::CallCancel { stream: 8 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 8, .. })
                    && read_active_protocol_frame(&mut client, &active).await? == ServerFrame::CallCancelled { stream: 8 },
                "reference value socket did not retain accepted-call cancellation",
            )?;

            let mut wrong_bytes = text_selector.to_bytes();
            wrong_bytes[0] ^= 1;
            let wrong = ParameterId::from_bytes(wrong_bytes);
            for frame in [
                ClientFrame::CallRawStart { stream: 9, function: update_text },
                ClientFrame::CallArgument { stream: 9, parameter: wrong, value: first.clone() },
                ClientFrame::CallArgument { stream: 9, parameter: text_value, value: RuntimeValue::Text(String::from("wrong")) },
                ClientFrame::CallArgumentsComplete { stream: 9 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 9, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 9, failure: CallFailure::TargetUnavailable },
                "reference value socket invalid pair did not stay redacted",
            )?;

            for (stream, function, arguments) in [
                (
                    10,
                    update_text,
                    vec![
                        (text_selector, RuntimeValue::Text(String::from("not-a-reference"))),
                        (text_value, first.clone()),
                    ],
                ),
                (
                    11,
                    read_stored,
                    vec![
                        (text_selector, first.clone()),
                        (text_value, RuntimeValue::Text(String::from("not-an-update"))),
                    ],
                ),
            ] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && matches!(
                            read_active_protocol_frame(&mut client, &active).await?,
                            ServerFrame::CallFailed { stream: actual, failure: CallFailure::TargetUnavailable }
                                if actual == stream
                        ),
                    "reference value socket mistyped or non-update pair disclosed a target fact",
                )?;
            }

            for (stream, function) in [(12, read_stored), (13, read_links)] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::WindowUpdate { stream, channel: Channel::ResultValues, credit: 2048 },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "reference value socket did not accept its durable reader",
                )?;
                let first_event = read_active_protocol_frame(&mut client, &active).await?;
                let second_event = read_active_protocol_frame(&mut client, &active).await?;
                let completed = read_active_protocol_frame(&mut client, &active).await?;
                let values = [first_event, second_event].into_iter().filter_map(|frame| match frame {
                    ServerFrame::EventBatch { events, .. } if events.len() == 1 => match &events[0].event {
                        Event::Value(value) => Some(value.clone()),
                        _ => None,
                    },
                    _ => None,
                }).collect::<Vec<_>>();
                if !matches!(completed, ServerFrame::CallCompleted { stream: actual } if actual == stream) {
                    return Err(failure("reference value socket durable reader did not complete"));
                }
                if stream == 12 {
                    require(
                        values.iter().filter(|value| **value == RuntimeValue::Text(String::from("second\n"))).count() == 1
                            && values.iter().filter(|value| **value == RuntimeValue::Text(String::from("second"))).count() == 1,
                        "reference value socket did not preserve a byte-distinct Text value after cancellation",
                    )?;
                } else {
                    require(
                        values.iter().filter(|value| **value == *first).count() == 1,
                        "reference value socket Reference UPDATE did not store its Reference value",
                    )?;
                }
            }
            Ok(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_operation,
            finish_session(shutdown, connection, "reference value granted socket cleanup"),
            "reference value granted socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Denied, Some(update_text)),
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_link)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_stored)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_stored)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_links)),
        ];
        require(
            audits.len() == expected.len()
                && audits.iter().zip(expected).all(|(event, (kind, outcome, function))| {
                    let decision = event.decision();
                    decision.kind() == kind
                        && decision.outcome() == outcome
                        && decision.session_principal() == Some(RAW_CLIENT_USER)
                        && decision.target().map(InvocationTarget::function) == function
                }),
            "reference value socket changed the private typed audit sequence",
        )?;
        let audit_debug = format!("{audits:?}");
        require(
            !audit_debug.contains("second") && !audit_debug.contains("second\\n"),
            "unique Text values leaked into the durable security audit",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the ADR 0052 version-4 unique Text read through the authenticated
/// public raw socket. The direct dispatcher retains the private duplicate
/// conflict source. The socket must expose only its public failure frame.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0052 dispatch"]
async fn raw_unique_text_select_socket_authorises_binds_and_redacts() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (active, person, create, create_email, create_name, by_email, email, all_people) =
            install_raw_unique_text_select_socket_fixture(&kernel, &active, &standard_upgrade)
                .await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied).await?;
        let mut wrong_email_bytes = email.to_bytes();
        wrong_email_bytes[0] ^= 1;
        let wrong_email = ParameterId::from_bytes(wrong_email_bytes);

        // A denied malformed call must not disclose the parameter or value
        // error that would otherwise make its target unavailable.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "unique Text denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 1, function: by_email },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: wrong_email,
                    value: RuntimeValue::Integer(42),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 1, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 1, failure: CallFailure::ExecuteDenied },
                "unique Text denied socket disclosed a target or value fact",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text denied socket emitted a value after ExecuteDenied",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "unique Text denied socket cleanup"),
            "unique Text denied socket operation",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create),
                ExecuteGrant::new(RAW_CLIENT_USER, by_email),
                ExecuteGrant::new(RAW_CLIENT_USER, all_people),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let direct = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 4,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: person,
                            object: ObjectId::from_bytes([0x41; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "unique Text granted socket returned the wrong acknowledgement",
            )?;

            let mut created = Vec::new();
            for (stream, value, name) in [
                (1, "caf\u{e9}", "exact bytes"),
                (2, "cafe\u{301}", "byte-distinct decomposed"),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function: create },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_email,
                        value: RuntimeValue::Text(value.into()),
                    },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_name,
                        value: RuntimeValue::Text(name.into()),
                    },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "unique Text socket did not accept row creation",
                )?;
                let ServerFrame::EventBatch { events, .. } =
                    read_active_protocol_frame(&mut client, &active).await?
                else {
                    return Err(failure("unique Text socket creator did not return an event batch"));
                };
                let [orna_protocol::EventRecord {
                    event: Event::Value(RuntimeValue::Reference { target, object }), ..
                }] = events.as_slice() else {
                    return Err(failure("unique Text socket creator did not return one Reference"));
                };
                require(
                    *target == person && *object != ObjectId::from_bytes([0; 16]),
                    "unique Text socket creator returned the wrong Reference",
                )?;
                created.push(RuntimeValue::Reference { target: *target, object: *object });
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream },
                    "unique Text socket creator did not complete",
                )?;
            }
            let [exact_reference, decomposed_reference] = created.as_slice() else {
                return Err(failure("unique Text socket did not create both byte-distinct rows"));
            };

            // The private cause remains available to the direct dispatcher.
            // The same duplicate through the socket may expose only ORF1
            // `InternalFailure`, with no following value frame.
            let duplicate = RawClientDispatch::new(
                kernel.clone(),
                direct,
                30,
                RawCall {
                    function: create,
                    arguments: vec![
                        orna_protocol::CallArgument {
                            parameter: create_email,
                            value: RuntimeValue::Text("caf\u{e9}".into()),
                        },
                        orna_protocol::CallArgument {
                            parameter: create_name,
                            value: RuntimeValue::Text("duplicate private source".into()),
                        },
                    ],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate,
                30,
                CallFailure::InternalFailure,
                matches!(
                    duplicate.source(),
                    Some(PostgresKernelError::ServerInsert(
                        ServerInsertError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "unique Text duplicate did not retain its private typed conflict",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 3, function: create },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: create_email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: create_name,
                    value: RuntimeValue::Text("duplicate socket source".into()),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 3, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 3, failure: CallFailure::InternalFailure },
                "unique Text socket disclosed its private duplicate conflict",
            )?;
            require(
                timeout(Duration::from_millis(50), read_active_protocol_frame(&mut client, &active))
                    .await
                    .is_err(),
                "unique Text socket emitted a value after InternalFailure",
            )?;

            // Give exact reference-frame credit. The version-4 selector must
            // stop at the next projection until more credit is supplied.
            for frame in [
                ClientFrame::CallRawStart { stream: 4, function: by_email },
                ClientFrame::CallArgument {
                    stream: 4,
                    parameter: email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 4,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
                ClientFrame::CallArgumentsComplete { stream: 4 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 4, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 4, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(exact_reference.clone())
                    ),
                "unique Text socket did not return its first exact ORF1 value",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(client.try_read(&mut unexpected), Err(error) if error.kind() == ErrorKind::WouldBlock),
                "unique Text socket emitted a projection before its credit boundary",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 4,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            let expected_null = Event::Value(RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?);
            for (sequence, expected) in [
                (2, Event::Value(RuntimeValue::Text("exact bytes".into()))),
                (3, expected_null),
            ] {
                let actual = read_active_protocol_frame(&mut client, &active).await?;
                if !matches!(
                    actual,
                    ServerFrame::EventBatch { stream: 4, ref events, .. }
                        if events.len() == 1 && events[0].sequence == sequence
                            && events[0].event == expected
                ) {
                    return Err(failure(format!(
                        "unique Text socket did not preserve exact ordered ORF1 values: {actual:?}"
                    )));
                }
            }
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 4 },
                "unique Text socket did not complete its exact row",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 5, function: by_email },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: email,
                    value: RuntimeValue::Text("cafe\u{301}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 5,
                    channel: Channel::ResultValues,
                    credit: 2048,
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            let expected_decomposed_null = Event::Value(RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?);
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 5, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(decomposed_reference.clone())
                    )
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 2
                                && events[0].event == Event::Value(RuntimeValue::Text("byte-distinct decomposed".into()))
                    )
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 3
                                && events[0].event == expected_decomposed_null
                    )
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 5 },
                "unique Text socket did not select the C-byte-distinct row",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 6, function: by_email },
                ClientFrame::CallArgument {
                    stream: 6,
                    parameter: email,
                    value: RuntimeValue::Text("absent@example.test".into()),
                },
                ClientFrame::CallArgumentsComplete { stream: 6 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 6, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 6 },
                "unique Text socket did not complete an absent value without output",
            )?;

            for (stream, function, parameter, value) in [
                (7, by_email, wrong_email, RuntimeValue::Text("caf\u{e9}".into())),
                (8, by_email, email, RuntimeValue::Integer(42)),
                (9, all_people, email, RuntimeValue::Text("caf\u{e9}".into())),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::CallArgument { stream, parameter, value },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && read_active_protocol_frame(&mut client, &active).await?
                            == ServerFrame::CallFailed { stream, failure: CallFailure::TargetUnavailable },
                    "unique Text socket disclosed a closed target fact",
                )?;
                require(
                    timeout(
                        Duration::from_millis(50),
                        read_active_protocol_frame(&mut client, &active),
                    )
                    .await
                    .is_err(),
                    "unique Text socket emitted a value after TargetUnavailable",
                )?;
            }

            let typed_null = RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?;
            for frame in [
                ClientFrame::CallRawStart { stream: 10, function: by_email },
                ClientFrame::CallArgument {
                    stream: 10,
                    parameter: email,
                    value: typed_null,
                },
                ClientFrame::CallArgumentsComplete { stream: 10 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 10, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 10, failure: CallFailure::TargetUnavailable },
                "unique Text socket did not reject a typed NULL selector",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text socket emitted a value after its NULL closure",
            )?;

            // PostgreSQL C equality must not fold case, trim whitespace, or
            // change line endings. None of these byte-distinct values exists.
            for (stream, value) in [
                (11, "CAF\u{c9}"),
                (12, "caf\u{e9} "),
                (13, "caf\u{e9}\n"),
                (14, "caf\u{e9}\r\n"),
            ] {
                for frame in [
                    ClientFrame::CallRawStart {
                        stream,
                        function: by_email,
                    },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: email,
                        value: RuntimeValue::Text(value.into()),
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && read_active_protocol_frame(&mut client, &active).await?
                            == ServerFrame::CallCompleted { stream },
                    "unique Text socket folded a byte-distinct selector",
                )?;
            }

            // The call must remain cancellable after its first selected value
            // has crossed the public socket. Withheld credit keeps it open.
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 15,
                    function: by_email,
                },
                ClientFrame::CallArgument {
                    stream: 15,
                    parameter: email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 15,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
                ClientFrame::CallArgumentsComplete { stream: 15 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 15, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 15, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(exact_reference.clone())
                    ),
                "unique Text socket did not begin the cancellable version-4 result",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::CallCancel { stream: 15 },
            )
            .await?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 15 },
                "unique Text socket did not cancel after its first result value",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text socket emitted a frame after cancellation",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_operation,
            finish_session(shutdown, connection, "unique Text granted socket cleanup"),
            "unique Text granted socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None, None),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(by_email),
                Some(SecurityAuditDenial::Execute(ExecuteDenial::MissingExecuteGrant)),
            ),
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(all_people), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
        ];
        let audit_matches = audits.len() == expected.len()
            && audits.iter().zip(expected).all(|(event, (kind, outcome, function, denial))| {
                let decision = event.decision();
                decision.kind() == kind
                    && decision.outcome() == outcome
                    && decision.session_principal() == Some(RAW_CLIENT_USER)
                    && decision.target()
                        == function.map(|function| InvocationTarget::new(function, active.pair()))
                    && decision.denial() == denial
            });
        if !audit_matches {
            return Err(failure(format!(
                "unique Text socket changed its ordered durable audit decisions: {audits:?}"
            )));
        }
        let audit_debug = format!("{audits:?}");
        require(
            !audit_debug.contains("caf\u{e9}")
                && !audit_debug.contains("cafe\u{301}")
                && !audit_debug.contains("exact bytes")
                && !audit_debug.contains("byte-distinct decomposed")
                && !audit_debug.contains("duplicate private source")
                && !audit_debug.contains("duplicate socket source")
                && !audit_debug.contains("absent@example.test")
                && !audit_debug.contains("CAF\u{c9}")
                && !audit_debug.contains("caf\u{e9} ")
                && !audit_debug.contains("caf\u{e9}\n")
                && !audit_debug.contains("caf\u{e9}\r\n"),
            "unique Text selector values leaked into the durable security audit",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn serves_the_actual_local_peer_through_the_raw_socket_protocol() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        insert_raw_server_flag(&database, &active, 0x7f, true).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let mut server_value_wires = Vec::new();
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_two_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00",
                "local raw socket returned the wrong catalogue acknowledgement",
            )?;

            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::CallRawStart {
                    stream: 1,
                    function: client_function,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .await?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "local raw socket did not accept the catalogue CLIENT call",
            )?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "local raw socket returned the wrong catalogue CLIENT value",
            )?;
            require(
                read_catalogue_protocol_frame(&mut client, active.catalogue()).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "local raw socket did not complete the catalogue CLIENT call",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_catalogue_protocol_frame(&mut client, active.catalogue(), &frame).await?;
            }
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-2 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    orna_protocol::decode_catalogue_server_frame(active.catalogue(), &encoded)?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-2 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV2")?);
            require(
                read_catalogue_protocol_frame(&mut client, active.catalogue()).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "protocol-2 socket did not complete the raw SERVER call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "local raw socket connection cleanup");
        finish_session(
            version_two_operation,
            cleanup,
            "local raw socket protocol-2 operation",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_one_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00",
                "local raw socket returned the wrong protocol-1 acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_legacy_protocol_frame(&mut client, &frame).await?;
            }
            require(
                matches!(
                    read_legacy_protocol_frame(&mut client).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-1 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    decode_server_frame(&encoded)?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-1 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV1")?);
            require(
                read_legacy_protocol_frame(&mut client).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-1 socket did not complete the raw SERVER call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            version_one_operation,
            finish_session(
                shutdown,
                connection,
                "protocol-1 raw socket connection cleanup",
            ),
            "local raw socket protocol-1 operation",
        )?;

        let record = raw_client_record(&active)?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let mut current_pair = active.pair();
        let version_three_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "local raw socket returned the wrong active-revision acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-3 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    decode_active_server_frame(&active, &encoded)?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV3")?);
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-3 socket did not complete the raw SERVER call",
            )?;

            insert_raw_server_flag(&database, &active, 0x80, true).await?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: BOOLEAN_EVENT_CREDIT,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-3 socket did not accept the flow-controlled multi-row SERVER call",
            )?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket did not emit the first ordered SERVER row under exact credit",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "protocol-3 socket emitted a second SERVER row without result-value credit",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: BOOLEAN_EVENT_CREDIT,
                },
            )
            .await?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 2
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket did not resume with the second ordered SERVER row",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "protocol-3 socket did not complete after every flow-controlled SERVER row",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 3,
                    function: client_function,
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: orna_core::ParameterId::from_bytes([0x74; 16]),
                    value: record.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 3, .. }
                ),
                "local raw socket did not accept the active-revision record call",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 3,
                        failure: CallFailure::TargetUnavailable,
                    },
                "local raw socket did not retain the closed record-call dispatch boundary",
            )?;

            async {
                let changed_source =
                    RAW_CLIENT_FUNCTION_SOURCE.replace("'lead', 'qualified'", "'lead', 'stale'");
                let changed_bundle =
                    SourceBundle::new([SourceUnit::new("main.orna", changed_source)])?;
                let changed_report = check_standard_application(
                    &changed_bundle,
                    &StandardApplicationCheckContext::try_new(
                        active.catalogue(),
                        standard_upgrade.checked_standard_library(),
                    )?,
                );
                require(
                    changed_report.diagnostics().is_empty(),
                    "stale record preflight fixture did not compile",
                )?;
                let changed = kernel
                    .apply(&prepare_standard_application(
                        &changed_report,
                        active.pair(),
                        &active,
                    )?)
                    .await?;
                current_pair = changed.pair();
                for frame in [
                    ClientFrame::CallRawStart {
                        stream: 4,
                        function: client_function,
                    },
                    ClientFrame::CallArgument {
                        stream: 4,
                        parameter: orna_core::ParameterId::from_bytes([0x74; 16]),
                        value: record,
                    },
                    ClientFrame::CallArgumentsComplete { stream: 4 },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: 4, .. }
                    ),
                    "local raw socket did not accept the stale record call",
                )?;
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream: 4,
                            failure: CallFailure::TargetUnavailable,
                        },
                    "local raw socket did not close stale record dispatch",
                )
            }
            .await
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "protocol-3 connection cleanup");
        finish_session(
            version_three_operation,
            cleanup,
            "local raw socket protocol-3 operation",
        )?;
        require(
            server_value_wires.len() == 3
                && server_value_wires
                    .windows(2)
                    .all(|values| values[0] == values[1]),
            "protocol-1, protocol-2, and protocol-3 raw SERVER values differ after their exact marker",
        )?;

        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[0].decision().target().is_none()
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[1].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[2].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target().is_none()
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target().is_none()
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "local raw socket changed the exact authentication and execute audit sequence",
        )?;

        let opaque_payload = [0x81; 16];
        let current = kernel.recover().await?;
        let opaque_active = install_opaque_client_fixture(
            &kernel,
            &current,
            standard_upgrade.checked_standard_library(),
            client_function,
            opaque_payload,
        )
        .await?;
        current_pair = opaque_active.pair();
        let opaque_granted = SecuritySnapshot::new_with_local_peer_credentials(
            current_pair,
            functions.clone(),
            granted.principals().collect(),
            vec![],
            granted.execute_grants().collect(),
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&opaque_granted).await?;
        let registry = registered_opaque_codecs(
            opaque_active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("opaque CLIENT active revision omitted its standard"))?,
        )?;
        let expected_opaque = RuntimeValue::Opaque(OpaqueValue::new(
            &opaque_active,
            &registry,
            OPAQUE_TOKEN_TYPE_ID,
            opaque_payload,
        )?);
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_four_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00",
                "local raw socket returned the wrong registered-codec acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: client_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 57,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_registered_protocol_frame(&mut client, &opaque_active, &registry, &frame)
                    .await?;
            }
            require(
                matches!(
                    read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-4 socket did not accept the opaque CLIENT call",
            )?;
            require(
                read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?
                    == ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events: vec![orna_protocol::EventRecord {
                            sequence: 1,
                            event: Event::Value(expected_opaque),
                        }],
                    },
                "protocol-4 socket returned the wrong registered opaque value",
            )?;
            require(
                read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-4 socket did not complete the opaque CLIENT call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "protocol-4 connection cleanup");
        finish_session(
            version_four_operation,
            cleanup,
            "local raw socket protocol-4 operation",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 10
                && events[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[8].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[8].decision().target().is_none()
                && events[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[9].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[9].decision().target()
                    == Some(InvocationTarget::new(client_function, current_pair)),
            "protocol-4 opaque CLIENT call changed protected audit evidence",
        )?;

        let revoked = SecuritySnapshot::new(
            current_pair,
            functions,
            granted.principals().collect(),
            vec![],
            granted.execute_grants().collect(),
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let rejected = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let wire = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00")
                .await?;
            let mut response = [0_u8; 1];
            require(
                client.read(&mut response).await? == 0,
                "revoked local peer received bytes instead of a silent close",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let rejection = rejected.await?;
        finish_session(wire, shutdown, "revoked local raw socket cleanup")?;
        require(
            matches!(
                rejection,
                Err(LocalRawSocketError::Authentication {
                    source: LocalAuthenticationError::Kernel {
                        source: PostgresKernelError::LocalPeerAuthentication(
                            LocalPeerAuthenticationError::UnknownUid
                        )
                    }
                })
            ),
            "revoked local peer returned the wrong typed authentication rejection",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 11
                && events[10].decision().kind() == SecurityAuditKind::Authentication
                && events[10].decision().outcome() == SecurityAuditOutcome::Denied
                && events[10].decision().session_principal().is_none()
                && events[10].decision().target().is_none()
                && events[10].decision().denial()
                    == Some(SecurityAuditDenial::Authentication(
                        LocalPeerAuthenticationError::UnknownUid,
                    )),
            "revoked local peer changed the exact denied authentication audit evidence",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn protocol_five_socket_retains_legacy_values_and_closes_constructed_arguments()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        insert_raw_server_flag(&database, &active, 0x81, true).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let registry = registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("protocol-5 fixture has no selected standard context"))?,
        )?;
        let boolean_type = TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let constructed_descriptor = TypeDescriptor::list(TypeDescriptor::named(boolean_type))
            .expect("the fixed Boolean LIST descriptor is within the specified limits");
        let constructed_value = RuntimeValue::list(
            &active,
            constructed_descriptor.clone(),
            vec![RuntimeValue::Boolean(true)],
        )?;
        let constructed_rejection = orna_protocol::FrameCodecError::ConstructedValueNotAccepted {
            descriptor: constructed_descriptor.clone(),
        };
        let mut protocol = ProtocolConnection::new();
        protocol.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 9,
                function: server_function,
            },
        )?;
        protocol.receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 9,
                channel: Channel::ResultValues,
                credit: 1024,
            },
        )?;
        let before_argument = protocol.clone();
        require(
            protocol.receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: ParameterId::from_bytes([0x82; 16]),
                    value: constructed_value.clone(),
                },
            ) == Err(ConnectionError::InvalidFrame {
                source: constructed_rejection.clone(),
            }) && protocol == before_argument,
            "constructed protocol-5 argument changed state or result credit",
        )?;
        require(
            matches!(
                protocol.receive_constructed(
                    &active,
                    &registry,
                    ClientFrame::CallArgumentsComplete { stream: 9 },
                )?,
                Some(orna_protocol::ClientAction::Dispatch { stream: 9, .. })
            ),
            "protocol-5 connection did not retain its callable state after constructed rejection",
        )?;
        protocol.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 9,
                invocation: InvocationId::from_bytes([0x83; 16]),
            },
        )?;
        let before_result = protocol.clone();
        require(
            protocol.apply_constructed(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 9,
                    events: vec![Event::Value(constructed_value)],
                },
            ) == Err(ConnectionError::InvalidFrame {
                source: constructed_rejection,
            }) && protocol == before_result,
            "constructed protocol-5 result changed state or result credit",
        )?;
        require(
            matches!(
                protocol.apply_constructed(
                    &active,
                    &registry,
                    ServerAction::Events {
                        stream: 9,
                        events: vec![Event::Value(RuntimeValue::Boolean(true))],
                    },
                )?,
                ServerFrame::EventBatch { stream: 9, .. }
            ),
            "protocol-5 result credit was not retained after constructed-result rejection",
        )?;
        for hello in [
            *b"ORNA\x01\x00\x00\x05\x00\x01\x00\x00",
            *b"ORNA\x01\x01\x00\x05\x00\x00\x00\x00",
            *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x01",
            *b"ORNA\x01\x00\x00\x06\x00\x00\x00\x00",
        ] {
            require_invalid_local_raw_hello(&kernel, hello).await?;
        }

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let legacy_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "protocol-5 local raw socket returned the wrong acknowledgement",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-5 socket did not accept the legacy SERVER call",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events: vec![orna_protocol::EventRecord {
                            sequence: 1,
                            event: Event::Value(RuntimeValue::Boolean(true)),
                        }],
                    },
                "protocol-5 socket did not retain its legacy Boolean result",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-5 socket did not complete its legacy SERVER call",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: client_function,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: ParameterId::from_bytes([0x74; 16]),
                    value: raw_client_record(&active)?,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-5 socket did not accept the legacy application argument",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallFailed {
                        stream: 2,
                        failure: CallFailure::TargetUnavailable,
                    },
                "protocol-5 socket did not retain the closed application target boundary",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            legacy_operation,
            finish_session(shutdown, connection, "protocol-5 legacy socket cleanup"),
            "protocol-5 legacy socket operation",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let rejected = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let constructed_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "constructed-value socket returned the wrong protocol-5 acknowledgement",
            )?;
            send_constructed_protocol_frame(
                &mut client,
                &active,
                &registry,
                &ClientFrame::CallRawStart {
                    stream: 3,
                    function: server_function,
                },
            )
            .await?;
            send_constructed_protocol_frame(
                &mut client,
                &active,
                &registry,
                &ClientFrame::WindowUpdate {
                    stream: 3,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            client
                .write_all(&constructed_list_argument_frame(
                    3,
                    ParameterId::from_bytes([0x82; 16]),
                    boolean_type,
                ))
                .await?;
            let mut response = [0_u8; 1];
            require(
                timeout(Duration::from_secs(1), client.read(&mut response)).await?? == 0,
                "constructed protocol-5 argument returned a partial server frame",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let rejection = rejected.await?;
        finish_session(
            constructed_operation,
            shutdown,
            "constructed protocol-5 socket cleanup",
        )?;
        require(
            matches!(
                rejection,
                Err(LocalRawSocketError::Frame {
                    source: orna_protocol::FrameCodecError::ConstructedValueNotAccepted {
                        descriptor,
                    },
                }) if descriptor == constructed_descriptor
            ),
            "constructed protocol-5 argument did not close at the public frame boundary",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

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
    Ok(read_resource_server_frame_with_encoded(stream, active, registry)
        .await?
        .1)
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
        return Err(failure("exact resource byte accounting requires a values frame"));
    };
    values.byte_count = 0;
    match encode_resource_server_frame(active, registry, &ResourceServerFrame::Values(values)) {
        Err(orna_protocol::FrameCodecError::ResourceByteCountMismatch { actual, .. }) => Ok(actual),
        Ok(_) => Err(failure("resource values encoder accepted zero byte credit")),
        Err(error) => Err(failure(format!("resource values byte accounting failed: {error:?}"))),
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
) -> TestResult<(orna_core::revision::ActiveDatabaseRevision, FunctionId, FunctionId, CallSiteId)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision, body: &str| -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new("resource_fixture.sql", body.to_owned())])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("scalar resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}", unit.content(), body)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?)
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
        return Err(failure("scalar CLIENT resource plan is not an awaited resource"));
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        return Err(failure("scalar CLIENT resource plan is not a resource operation"));
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
) -> TestResult<(orna_core::revision::ActiveDatabaseRevision, FunctionId, FunctionId, ParameterId)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision, body: &str| -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new("resource_fixture.sql", body.to_owned())])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("procedural resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}", unit.content(), body)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?)
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
        return Err(failure("procedural CLIENT LET did not retain a resource operation"));
    };
    let ClientExpressionNode::Await { expression } = plan.return_expression() else {
        return Err(failure("procedural CLIENT return did not retain AWAIT"));
    };
    let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
        return Err(failure("procedural CLIENT AWAIT did not retain the resource local read"));
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
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision, body: &str| -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new("resource_fixture.sql", body.to_owned())])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("stream resource fixture has no retained source unit"))?;
        Ok(SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}", unit.content(), body)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?)
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
            .function_by_name(&QualifiedSemanticName::new(["resource_fixture", "resource"]).unwrap())
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
    let parameter = target
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
        return Err(failure("stream CLIENT resource plan is not an awaited resource"));
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        return Err(failure("stream CLIENT resource plan is not a resource operation"));
    };
    require(
        operation.kind() == orna_artifact::client_plan::ResourceKind::Stream
            && operation.target() == target
            && operation.target_revision() == active.pair()
            && operation.arguments().len() == 1
            && operation.arguments()[0].0 == parameter,
        "stream CLIENT resource plan did not retain canonical target metadata",
    )?;

    Ok((active, client, target, parameter, operation.call_site_id()))
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
)> {
    let append_source = |active: &orna_core::revision::ActiveDatabaseRevision, body: &str| -> TestResult<SourceBundle> {
        if active.source().units().is_empty() {
            return Ok(SourceBundle::new([SourceUnit::new("action_fixture.orna", body.to_owned())])?);
        }
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("action fixture has no retained source unit"))?;
        Ok(SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}", unit.content(), body)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?)
    };
    let server_source = append_source(active, RAW_ACTION_SERVER_SOURCE)?;
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
    let client_source = append_source(&active, RAW_ACTION_CLIENT_SOURCE)?;
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
    Ok((active, client, target, client_parameter, target_parameter))
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
/// The V2-to-V3 upgrade pipeline is deliberately not supported in this build
/// (work ADR 0055 defers standard upgrades after `orna.std/2`, so
/// `prepare_standard_upgrade_v2_to_v3` fails closed), but the live proof
/// needs the V3 snapshot active so the sealed route's opaque codec registry
/// binds the `std.terminal.Document` and `std.io.ByteStream` codecs. The
/// proof therefore installs V3 exactly as the accepted V1-to-V2 pipeline
/// installs V2 from the empty base: retain and verify the V3 snapshot, seed
/// the retained V1 and V2 source records the V3 source parent chain requires,
/// build the empty-base application candidate pinned to the V3 snapshot
/// through the version-two catalogue hash context, and apply candidate plus
/// snapshot through the kernel's test-hooks persistence seam (the same path
/// [`PostgresKernel::apply_standard_upgrade`] uses for the compiler-produced
/// V2 upgrade).
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
    let plan = OpaqueClientPlan::return_opaque(OPAQUE_TOKEN_TYPE_ID, payload).encode();
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
                 JOIN _orna_kernel.invocation_audit_events AS invocation
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
            ([0x51; 16], [0x52; 16], Some(all_target), "allowed", "completed", Some(2_i64), Some(80_i64)),
            ([0x53; 16], [0x54; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
            ([0x55; 16], [0x56; 16], Some(all_target), "allowed", "completed", Some(2_i64), Some(80_i64)),
            ([0x57; 16], [0x58; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
            ([0x61; 16], [0x62; 16], None, "denied", "cancelled", None, None),
            ([0x71; 16], [0x72; 16], Some(target), "denied", "failed", None, None),
            ([0x81; 16], [0x82; 16], Some(target), "allowed", "completed", Some(1_i64), Some(39_i64)),
        ];
        for (index, row) in rows.iter().enumerate() {
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let nested_invocation_id: Vec<u8> = row.try_get("nested_invocation_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("catalogue_revision_id")?;
            let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            let invocation_outcome: String = row.try_get("invocation_outcome")?;
            let (request, parent, target, decision, terminal, count, bytes) = expected[index];
            let target = target.map(|function| function.to_bytes().to_vec());
            require(
                request_id == request
                    && parent_invocation_id == parent
                    && call_site_id == call_site.to_bytes().to_vec()
                    && nested_invocation_id.len() == 16
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
                    && invocation_outcome == decision,
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
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database() AND pid <> pg_backend_pid()",
                &[],
            )
            .await?;
        let count: i64 = row.try_get(0)?;
        require(
            count == 0,
            "raw CLIENT dispatch leaked a PostgreSQL database session",
        )
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "database session leak check",
    )
}

/// Builds one complete checked `sys.invoke` Request for a no-argument scalar CLIENT fixture.
#[cfg(feature = "test-hooks")]
fn sealed_scalar_resource_request(client: FunctionId) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(client),
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

/// Proves one accepted application SERVER function survives the user-facing
/// source/check/install/grant/invoke path and renders its typed result.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_server_function_dogfood_source_through_orna_invoke() -> TestResult<()> {
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
        let active = kernel
            .apply(&prepare_standard_application(
                &report,
                installed_standard.pair(),
                &installed_standard,
            )?)
            .await?;
        let read_id = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["dogfood", "read"])
            .ok_or_else(|| failure("the installed dogfood source is missing dogfood.read"))?
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
            vec![ExecuteGrant::new(RAW_CLIENT_USER, read_id)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let object = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["dogfood", "item"])
            .ok_or_else(|| failure("the installed dogfood source is missing dogfood.item"))?;
        let field = object
            .fields()
            .iter()
            .find(|field| field.name() == "value")
            .ok_or_else(|| failure("the installed dogfood source is missing dogfood.item.value"))?;
        let table = format!("t_{:032x}", u128::from_be_bytes(object.id().to_bytes()));
        let column = format!("f_{:032x}", u128::from_be_bytes(field.id().to_bytes()));
        let object_id = format!("{:032x}", u128::from_be_bytes([0x91; 16]));
        run_database_statement(
            &database,
            &format!(
                "INSERT INTO _orna_data.{table} (_orna_object_id, {column}) VALUES (decode('{object_id}', 'hex'), {INPUT})"
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
                "the installed SERVER dogfood read invocation did not complete: {:?}, stdout={:?}, stderr={:?}",
                read_outcome, read_stdout, read_stderr,
            )));
        }
        require(
            read_stdout == expected,
            "the installed SERVER dogfood read invocation returned the wrong value",
        )?;
        require(
            read_stderr.is_empty(),
            "the quiet SERVER dogfood read invocation wrote progress diagnostics",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}


const RAW_ORDINARY_INSPECTOR_SOURCE: &str =
    include_str!("fixtures/client_inspector_dogfood.orna");

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
                    format!("{}\n{}", unit.content(), RAW_ORDINARY_INSPECTOR_SOURCE)
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
        // Reuse one enclosing invocation identity so the two runs exercise the same client epoch.
        let deterministic_parent = InvocationId::from_bytes([0x58; 16]);
        let grants = LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
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
        let expected_row_counts = [1usize, 1, 1, 0, 0, 0, 0, 0, 2];
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
        let ui_carrier_rows = properties
            .get("carrier_rows")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI carrier_rows property was missing"))?;
        require(
            ui_carrier_rows == "1,1,1,0,0,0,0,0,2",
            "ordinary Inspector UI carrier_rows did not match the echo fixture",
        )?;

        let first_carriers = executor.completed_values.clone();
        let mut second_executor = RecordingInstalledResourceExecutor {
            inner: InstalledClientResourceExecutor::new(kernel.clone(), session, active.clone()),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        };
        let mut second_state = ClientStateStore::new();
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
            second_ui.opaque_type() == orna_standard::STD_UI_TYPE_ID
                && second_ui.canonical_payload() == payload.as_slice(),
            "ordinary Inspector ORNA-UI/1 payload bytes were not deterministic for the same carrier set",
        )?;
        require(
            first_carriers.len() == second_executor.completed_values.len()
                && first_carriers
                    .iter()
                    .zip(second_executor.completed_values.iter())
                    .all(|((first_type, first_value), (second_type, second_value))| {
                        first_type == second_type
                            && match (first_value, second_value) {
                                (RuntimeValue::Opaque(first), RuntimeValue::Opaque(second)) => {
                                    first.opaque_type() == second.opaque_type()
                                        && first.canonical_payload() == second.canonical_payload()
                                }
                                _ => false,
                            }
                    }),
            "ordinary Inspector repeat did not evaluate the same ordered carrier bytes",
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
