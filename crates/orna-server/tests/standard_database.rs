#![cfg(unix)]

use std::{
    error::Error, io::ErrorKind, os::unix::net::UnixStream as StandardUnixStream, sync::Arc,
    time::Duration,
};

use orna_artifact::client_plan::{OPAQUE_FORMAT_VERSION, OpaqueClientPlan};
use orna_compiler::{
    StandardApplicationCheckContext, check, check_standard_application, prepare,
    prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, PrincipalId,
    SourceRevisionId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context,
        function_semantic_digest_with_version,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionVolatility,
    },
    revision::{
        DefinitionIdentity, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget, DeployableRevision, DeployableRevisionContent,
        DeployableRevisionInput, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair,
    },
    security::{
        ExecuteDenial, ExecuteGrant, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecurityAuditDenial,
        SecurityAuditKind, SecurityAuditOutcome, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    types::ResolvedType,
    value::{EnumValue, OpaqueValue, RecordValue, RuntimeValue},
};
use orna_postgres::{AuthenticatedRawCallResult, PostgresKernel, PostgresKernelError};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, Event, RawCall, ServerAction, ServerFrame,
    decode_active_server_frame, decode_registered_server_frame, decode_server_frame,
    encode_active_client_frame, encode_client_frame, encode_registered_client_frame,
};
use orna_server::{
    LocalAuthenticationError, LocalRawSocketError, LocalRawSocketResources,
    OpenStandardDatabaseError, RawClientDispatch, open_standard_database, serve_local_raw_stream,
};
use orna_standard::{
    BOOLEAN_TYPE_ID, OPAQUE_TOKEN_TYPE_ID, registered_opaque_codecs,
    retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Barrier,
    time::sleep,
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
    CREATE SERVER FUNCTION app.create_flagged(p_value BOOLEAN)\n\
    RETURNS ROWS (created REF app.flag)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO app.flag AS made (value)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
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

fn raw_call(function: FunctionId) -> RawCall {
    RawCall {
        function,
        arguments: vec![],
    }
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
