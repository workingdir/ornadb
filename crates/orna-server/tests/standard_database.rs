#![cfg(unix)]

use std::{error::Error, os::unix::net::UnixStream as StandardUnixStream};

use orna_compiler::{
    StandardApplicationCheckContext, check, check_standard_application, prepare,
    prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, PrincipalId, SourceRevisionId,
    revision::RevisionPair,
    security::{
        ExecuteDenial, ExecuteGrant, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecurityAuditDenial,
        SecurityAuditKind, SecurityAuditOutcome, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    value::RuntimeValue,
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, Event, RawCall, ServerAction, ServerFrame,
    decode_catalogue_server_frame, encode_catalogue_client_frame,
};
use orna_server::{
    LocalAuthenticationError, LocalRawSocketError, LocalRawSocketResources,
    OpenStandardDatabaseError, RawClientDispatch, open_standard_database, serve_local_raw_stream,
};
use orna_standard::{
    BOOLEAN_TYPE_ID, retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const RAW_CLIENT_SCHEMA_SOURCE: &str = "CREATE SCHEMA app;\n";
const RAW_CLIENT_FUNCTION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
const RAW_CLIENT_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
const RAW_CLIENT_UNGRANTED_USER: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
const RAW_CLIENT_STALE_USER: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

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
        let (active, client_function, server_function) =
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

        let evaluator = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            2,
            raw_call(server_function),
        )
        .finish()
        .await;
        require_dispatch_failure(
            &evaluator,
            2,
            CallFailure::ClientEvaluationFailed,
            matches!(
                evaluator.source(),
                Some(PostgresKernelError::ClientExecution(_))
            ),
            "authorised SERVER target did not become a closed CLIENT evaluator failure",
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
                Some(PostgresKernelError::ClientExecuteDenied {
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
                Some(PostgresKernelError::ClientExecuteDenied {
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
                Some(PostgresKernelError::ClientExecuteDenied {
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "stale raw CLIENT session did not retain its private typed denial",
        )?;

        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 5
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
                    == Some(InvocationTarget::new(client_function, active.pair())),
            "raw CLIENT dispatch changed the exact durable audit sequence",
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
            events.len() == 6
                && events[5].decision().denial()
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
            kernel,
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
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn serves_the_actual_local_peer_through_the_raw_socket_protocol() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, client_function, _) = install_raw_client_fixture(&kernel).await?;
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
            vec![ExecuteGrant::new(RAW_CLIENT_USER, client_function)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let catalogue = active.catalogue().clone();

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let operation = async {
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
                &catalogue,
                &ClientFrame::CallRawStart {
                    stream: 1,
                    function: client_function,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                &catalogue,
                &ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                &catalogue,
                &ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .await?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, &catalogue).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "local raw socket did not accept the catalogue CLIENT call",
            )?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, &catalogue).await?,
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
                read_catalogue_protocol_frame(&mut client, &catalogue).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "local raw socket did not complete the catalogue CLIENT call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "local raw socket connection cleanup");
        finish_session(operation, cleanup, "local raw socket protocol operation")?;

        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 2
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[0].decision().target().is_none()
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[1].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair())),
            "local raw socket changed the exact authentication and execute audit sequence",
        )?;

        let revoked = SecuritySnapshot::new(
            active.pair(),
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
            events.len() == 3
                && events[2].decision().kind() == SecurityAuditKind::Authentication
                && events[2].decision().outcome() == SecurityAuditOutcome::Denied
                && events[2].decision().session_principal().is_none()
                && events[2].decision().target().is_none()
                && events[2].decision().denial()
                    == Some(SecurityAuditDenial::Authentication(
                        LocalPeerAuthenticationError::UnknownUid,
                    )),
            "revoked local peer changed the exact denied authentication audit evidence",
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

async fn send_catalogue_protocol_frame(
    stream: &mut UnixStream,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    frame: &ClientFrame,
) -> TestResult<()> {
    stream
        .write_all(&encode_catalogue_client_frame(catalogue, frame)?)
        .await?;
    Ok(())
}

async fn read_catalogue_protocol_frame(
    stream: &mut UnixStream,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
) -> TestResult<ServerFrame> {
    let encoded = read_encoded_protocol_frame(stream).await?;
    Ok(decode_catalogue_server_frame(catalogue, &encoded)?)
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
    Ok((active, client, server))
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
