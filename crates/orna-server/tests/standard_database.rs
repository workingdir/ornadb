#![cfg(unix)]

use std::{
    error::Error, io::ErrorKind, os::unix::net::UnixStream as StandardUnixStream, sync::Arc,
    time::Duration,
};

use orna_artifact::client_plan::{OPAQUE_FORMAT_VERSION, OpaqueClientPlan};
use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, StandardApplicationCheckContext, check,
    check_standard_application, prepare, prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, InvocationId, ObjectId, ParameterId,
    PrincipalId, SourceRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context,
        function_semantic_digest_with_version,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionVolatility, QualifiedSemanticName,
    },
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationParameterSelector,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    },
    invocation_binding::CliArgumentInput,
    revision::{
        DefinitionIdentity, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget, DeployableRevision, DeployableRevisionContent,
        DeployableRevisionInput, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair,
    },
    security::{
        AuthenticatedSession, ExecuteDenial, ExecuteGrant, InvocationTarget,
        LocalPeerAuthenticationError, LocalPeerCredential, Principal, PrincipalKind,
        PrincipalStatus, SecurityAuditDenial, SecurityAuditKind, SecurityAuditOutcome,
        SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::SYS_INVOKE_FUNCTION_ID,
    types::{ResolvedType, TypeDescriptor},
    value::{EnumValue, FunctionArgument, OpaqueValue, RecordValue, RuntimeValue},
};
use orna_postgres::{
    AuthenticatedRawCallResult, PostgresKernel, PostgresKernelError, SealedInvocationResult,
    ServerInsertError, ServerMutationError, ServerUpdateError,
};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, ConnectionError, Event, ProtocolConnection, RawCall,
    ServerAction, ServerFrame, decode_active_server_frame, decode_constructed_server_frame,
    decode_invocation_event_batch, decode_registered_server_frame, decode_server_frame,
    encode_active_client_frame, encode_active_server_frame, encode_client_frame,
    encode_constructed_client_frame, encode_constructed_value, encode_invocation_event_batch,
    encode_invoke_request, encode_registered_client_frame,
};
use orna_server::{
    InstalledInvokeError, InstalledInvokeErrorKind, InstalledInvokeOutcome, InstalledInvokeRequest,
    LocalAuthenticationError, LocalRawSocketError, LocalRawSocketResources,
    OpenStandardDatabaseError, RawClientDispatch, open_standard_database, run_invoke_with_kernel,
    serve_local_raw_stream,
};
use orna_standard::{
    BOOLEAN_TYPE_ID, OPAQUE_TOKEN_TYPE_ID, registered_opaque_codecs,
    retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Barrier,
    time::{sleep, timeout},
};

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const RAW_CLIENT_SCHEMA_SOURCE: &str = "CREATE SCHEMA app;\n";
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
/// One Integer single-parameter INSERT and one public Integer reader.
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
        // catalogue hash context pins orna.std/2.
        let security_events = kernel.recover_security_audit_events().await?;
        let allowed = security_events
            .iter()
            .filter(|event| event.decision().outcome() == SecurityAuditOutcome::Allowed)
            .collect::<Vec<_>>();
        require(
            allowed.len() == 2
                && allowed.iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().session_principal() == Some(RAW_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(STD_INVOKE_ECHO_FUNCTION_ID, pair))
                }),
            "the allowed EXECUTE evidence did not link the exact historical application RevisionPair",
        )?;
        let allowed_security_ids = allowed.iter().map(|event| event.id()).collect::<Vec<_>>();
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

        // Three completed invocations appended three authentication-allowed
        // and three EXECUTE-allowed security events, plus three allowed
        // invocation-audit rows linking the exact historical RevisionPair.
        let security_events_after_invocations =
            kernel.recover_security_audit_events().await?;
        require(
            security_events_after_invocations.len() == security_events_before.len() + 6,
            "the three completed invocations did not append exactly six security events",
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

/// Builds one installed `orna invoke` command request the way the command
/// parser would after stripping option prefixes (ADR 0056 step 4).
fn installed_invoke_request(
    target: InvocationRequestTarget,
    arguments: Vec<CliArgumentInput>,
    no_progress: bool,
    explain: bool,
) -> InstalledInvokeRequest {
    InstalledInvokeRequest::new(target, arguments, None, None, no_progress, explain)
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
