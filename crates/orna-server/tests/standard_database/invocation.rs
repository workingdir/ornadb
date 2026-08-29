use super::*;

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn dispatches_raw_client_calls_through_security_audit_and_evaluation() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("raw-client-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!("raw CLIENT live runtime could not start: {error}"))
                })?;
            runtime
                .block_on(dispatches_raw_client_calls_through_security_audit_and_evaluation_inner())
        })
        .map_err(|error| failure(format!("raw CLIENT live thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("raw CLIENT live thread panicked")),
    }
}

async fn dispatches_raw_client_calls_through_security_audit_and_evaluation_inner() -> TestResult<()>
{
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
async fn raw_sys_invoke_is_denied_before_sealed_entry() -> TestResult<()> {
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
            CallFailure::ExecuteDenied,
            matches!(
                system_entry.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::UnknownFunction,
                    ..
                })
            ),
            "the raw sys.invoke entry did not retain its closed denial",
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
                && audits[0].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[0].decision().target()
                    == Some(InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()))
                && audits[0].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction))
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[1].decision().target()
                    == Some(InvocationTarget::new(ordinary_unknown, active.pair()))
                && audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction)),
            "raw system and unknown targets changed the exact denial audit sequence",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_standard_invocation_dogfooding_through_sealed_sys_invoke() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("standard-invoke-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "standard invocation live runtime could not start: {error}"
                    ))
                })?;
            runtime
                .block_on(proves_standard_invocation_dogfooding_through_sealed_sys_invoke_inner())
        })
        .map_err(|error| {
            failure(format!(
                "standard invocation live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("standard invocation live thread panicked")),
    }
}

async fn proves_standard_invocation_dogfooding_through_sealed_sys_invoke_inner() -> TestResult<()> {
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
/// - `--explain` prints the plan, including the accepted TTY runtime offer
///   (name, version, consumed sinks, preference, and trust), and neither
///   dispatches nor audits;
/// - the default runtime selection and an explicit `tty` selection both carry
///   the same offer through the sealed dispatch/decode path, while an
///   unsupported override fails closed before dispatch;
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
        let by_identity =
            installed_invoke_request_with_runtime(by_identity, Some(RuntimeFamily::Tty));
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
                && plan.contains(
                    format!("runtimes tty@{}", orna_runtime_tty::RUNTIME_VERSION).as_str()
                )
                && plan.contains("trace: Off")
                && plan.contains("output: none"),
            "the --explain plan did not carry the resolution and sealed request facts",
        )?;
        require(
            plan.contains(
                format!("selected: tty@{}", orna_runtime_tty::RUNTIME_VERSION).as_str()
            )
                && plan.contains(
                    format!(
                        "tty@{} (consumes {}, {}; preference rank 0; trusted)",
                        orna_runtime_tty::RUNTIME_VERSION,
                        STD_TERMINAL_DOCUMENT_TYPE_ID.canonical(),
                        STD_IO_BYTE_STREAM_TYPE_ID.canonical(),
                    )
                    .as_str(),
                ),
            "the --explain plan did not carry the accepted TTY runtime offer contract",
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

        // A recognised-but-not-installed runtime override fails closed while
        // the target is otherwise valid. It must not construct, dispatch, or
        // audit a sealed request.
        let unsupported_runtime = installed_invoke_request_with_runtime(
            installed_invoke_request(
                InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
                    "std", "invoke", "echo",
                ])?)?,
                vec![CliArgumentInput::Canonical {
                    parameter: "p_value".to_owned(),
                    value: "43".to_owned(),
                }],
                false,
                false,
            ),
            Some(RuntimeFamily::NotInstalled),
        );
        let (unsupported_outcome, unsupported_stdout, unsupported_stderr) =
            installed_invoke_run(&database, unsupported_runtime).await?;
        require(
            matches!(unsupported_outcome, Err(error) if error.kind() == InstalledInvokeErrorKind::Usage),
            "an unsupported runtime override did not fail closed as usage",
        )?;
        require(
            unsupported_stdout.is_empty() && unsupported_stderr.is_empty(),
            "an unsupported runtime override wrote to a command channel",
        )?;
        require(
            kernel.recover_security_audit_events().await?.len()
                == security_events_after_invocations.len()
                && invocation_audit_count(&database).await? == invocation_rows_before + 3,
            "an unsupported runtime override dispatched or appended audit evidence",
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
async fn proves_scalar_client_resource_pending_continues_through_installed_evaluator()
-> TestResult<()> {
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
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
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
            return Err(failure(
                "scalar pending resource did not complete the sealed invocation",
            ));
        };
        let records = events.records();
        require(
            records.len() == 3
                && records[0].event().kind() == InvocationEventKind::InvocationStarted
                && records[1].event().kind() == InvocationEventKind::ValueBatch
                && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
            "scalar pending resource did not retain the completed invocation event sequence",
        )?;
        let InvocationEventBody::ValueBatch {
            schema: None,
            values,
        } = records[1].event().body()
        else {
            return Err(failure(
                "scalar pending resource completion did not carry a plain typed batch",
            ));
        };
        require(
            values.len() == 1 && values[0].value() == &RuntimeValue::Integer(43),
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
async fn proves_installed_no_broker_resource_returns_typed_result_and_terminal() -> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

    with_test_database(|database| async move {
        let uid = nix::unistd::getuid().as_raw();
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
            .ok_or_else(|| failure("no-broker resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client_function, target, _call_site) =
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
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("no-broker resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let request = InvokeRequest::new(InvokeRequestInput {
            target: InvocationRequestTarget::function_id(client_function),
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
                CONNECTION_PROTOCOL_MAJOR,
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
        })?;
        let retained = encode_invoke_request(&active, &registry, &request)?;

        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client_stream)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "no-broker resource socket did not complete its constructed handshake",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: SYS_INVOKE_FUNCTION_ID,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1_024,
                },
                ClientFrame::CallInvokeRequest {
                    stream: 1,
                    request: retained.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "no-broker resource socket did not accept the sealed invocation",
            )?;
            let started =
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &started,
                    ServerFrame::EventBatch { stream: 1, channel: Channel::ResultValues, events }
                        if events.len() == 1
                            && matches!(
                                &events[0].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::InvocationStarted
                            )
                ),
                "no-broker resource socket did not publish invocation start",
            )?;
            let terminal_events =
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &terminal_events,
                    ServerFrame::EventBatch { stream: 1, channel: Channel::ResultValues, events }
                        if events.len() == 2
                            && matches!(
                                &events[0].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::ValueBatch
                                        && matches!(
                                            event.body(),
                                            InvocationEventBody::ValueBatch { schema: None, values }
                                                if values.len() == 1
                                                    && values[0].value() == &RuntimeValue::Integer(43)
                                        )
                            )
                            && matches!(
                                &events[1].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::InvocationCompleted
                            )
                ),
                "no-broker resource socket did not return its typed scalar and terminal event",
            )?;
            require(
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "no-broker resource socket did not close the sealed invocation terminally",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            operation,
            finish_session(shutdown, connection, "no-broker resource socket cleanup"),
            "no-broker resource socket operation",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_procedural_client_resource_through_installed_evaluator() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("procedural-client-resource-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "build procedural client resource runtime failed: {error}"
                    ))
                })?;
            runtime.block_on(proves_procedural_client_resource_through_installed_evaluator_inner())
        })
        .map_err(|error| {
            failure(format!(
                "spawn procedural client resource thread failed: {error}"
            ))
        })?;
    handle
        .join()
        .map_err(|_| failure("procedural client resource thread panicked"))?
}

async fn proves_procedural_client_resource_through_installed_evaluator_inner() -> TestResult<()> {
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
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        if let Some(standard) = active.catalogue_hash_context().standard() {
            functions.extend(
                standard
                    .catalogue()
                    .functions()
                    .iter()
                    .map(FunctionDefinition::id),
            );
        }
        functions.sort_unstable();
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

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none() -> TestResult<()>
{
    let handle = std::thread::Builder::new()
        .name("orna-installed-stream-evaluator".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!("build stream evaluator runtime failed: {error}"))
                })?;
            runtime.block_on(
                installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none_inner(
                ),
            )
        })
        .map_err(|error| failure(format!("spawn stream evaluator thread failed: {error}")))?;
    handle
        .join()
        .map_err(|_| failure("stream evaluator thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = open_standard_database(kernel(&database)?).await.map_err(|error| {
            failure(format!("open standard database failed: {error:?}"))
        })?;
        let active = kernel.recover().await.map_err(|error| {
            failure(format!("recover installed standard failed: {error:?}"))
        })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("stream evaluator fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (active, _client, _target, _parameter, _call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard).await?;
        let client = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "call_all"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.call_all"))?
            .id();
        let target = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "all"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.all"))?
            .id();
        let call_site = {
            let revision = active
                .function_revisions()
                .iter()
                .find(|revision| revision.function() == client)
                .ok_or_else(|| failure("stream evaluator call_all revision is missing"))?;
            let plan = ResourceClientPlan::decode(revision.artifact().payload())?;
            let ClientExpressionNode::Await { expression } = plan.expression() else {
                return Err(failure("stream evaluator call_all is not an awaited resource"));
            };
            let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
                return Err(failure("stream evaluator call_all is not a resource operation"));
            };
            operation.call_site_id()
        };
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.create"))?
            .id();
        let root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "root"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.root"))?
            .id();
        let create_definition = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("stream evaluator create disappeared from the catalogue"))?;
        let create_marker = create_definition
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("stream evaluator create has no p_marker parameter"))?
            .id();
        let create_sequence = create_definition
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("stream evaluator create has no p_sequence parameter"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_marker,
                        RuntimeValue::Text("evaluator-terminal".to_owned()),
                    )?,
                    FunctionArgument::new(create_sequence, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| failure(format!("insert stream evaluator fixture row failed: {error:?}")))?;

        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_marker,
                        RuntimeValue::Text("evaluator-terminal-next".to_owned()),
                    )?,
                    FunctionArgument::new(create_sequence, RuntimeValue::Integer(2))?,
                ],
            )
            .await
            .map_err(|error| {
                failure(format!("insert second stream evaluator fixture row failed: {error:?}"))
            })?;
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        if let Some(standard) = active.catalogue_hash_context().standard() {
            functions.extend(
                standard
                    .catalogue()
                    .functions()
                    .iter()
                    .map(FunctionDefinition::id),
            );
        }
        functions.sort_unstable();
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
                ExecuteGrant::new(RAW_CLIENT_USER, root),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(root),
                vec![],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "stream evaluator could not create an owned root invocation",
        )?;
        let audit_session = database.open().await?;
        let root_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation_id
                     FROM _orna_kernel.invocation_audit_events
                     WHERE function_id = $1
                       AND session_principal_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&root.to_bytes().to_vec(), &RAW_CLIENT_USER.to_bytes().to_vec()],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| failure("root invocation audit identity was not 16 bytes"))?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "root invocation audit lookup",
        )?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "stream evaluator CLIENT grant was denied: {denial:?}"
                )))
            }
        };
        let grants = LocalCapabilityGrantSet::new();
        let mut executor =
            InstalledClientResourceExecutor::new(kernel.clone(), session, active.clone());
        let mut state = ClientStateStore::default();
        let operation = async {
            let pending = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )
            .expect_err("the first stream AWAIT unexpectedly completed synchronously");
            let (resource_key, resource_generation) = match pending {
                ClientExecutionError::ResourceEvaluation {
                    source: orna_client::ClientResourceExecutionError::Pending { key, generation },
                    ..
                } => (key, generation),
                error => {
                    return Err(failure(format!(
                        "the first stream AWAIT returned an unexpected result: {error:?}"
                    )))
                }
            };
            let first_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(completion);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator first batch timed out"))??;
            require(
                matches!(
                    &first_completion,
                    ClientResourceCompletion::StreamValues {
                        key,
                        generation,
                        values,
                        ..
                    } if *key == resource_key
                        && *generation == resource_generation
                        && values.as_slice() == [RuntimeValue::Text("evaluator-terminal".to_owned())]
                ),
                "installed evaluator did not receive its exact typed stream batch",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its pending resource"))?
                .apply_completion(&active, first_completion)
                .map_err(|error| failure(format!("apply first stream batch failed: {error:?}")))?;
            let after_batch = state
                .resource(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its first batch"))?;
            require(
                after_batch.status() == ClientResourceStatus::Loading
                    && !after_batch.stream_complete()
                    && after_batch.request_id().is_some(),
                "first stream batch did not retain the loading resource state",
            )?;

            let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            ))?;
            let list = RuntimeValue::list(
                &active,
                list_descriptor.clone(),
                vec![RuntimeValue::Text("evaluator-terminal".to_owned())],
            )?;
            let expected_batch = RuntimeValue::option(
                &active,
                TypeDescriptor::option(list_descriptor)?,
                Some(list),
            )?;
            let batch_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            require(
                batch_result.value() == &expected_batch,
                "next installed stream AWAIT did not return its typed OPTION<LIST<T>> batch",
            )?;
            let second_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(
                            completion,
                        );
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator second batch timed out"))??;
            require(
                matches!(
                    &second_completion,
                    ClientResourceCompletion::StreamValues {
                        key,
                        generation,
                        values,
                        ..
                    } if *key == resource_key
                        && *generation == resource_generation
                        && values.as_slice()
                            == [RuntimeValue::Text("evaluator-terminal-next".to_owned())]
                ),
                "installed evaluator did not receive its second typed stream batch",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its second batch"))?
                .apply_completion(&active, second_completion)
                .map_err(|error| failure(format!("apply second stream batch failed: {error:?}")))?;
            let second_list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            ))?;
            let second_list = RuntimeValue::list(
                &active,
                second_list_descriptor.clone(),
                vec![RuntimeValue::Text("evaluator-terminal-next".to_owned())],
            )?;
            let expected_second_batch = RuntimeValue::option(
                &active,
                TypeDescriptor::option(second_list_descriptor)?,
                Some(second_list),
            )?;
            let second_batch_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            require(
                second_batch_result.value() == &expected_second_batch,
                "installed stream AWAIT did not return its second typed batch",
            )?;

            let terminal_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(completion);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator terminal completion timed out"))??;
            require(
                matches!(
                    &terminal_completion,
                    ClientResourceCompletion::StreamCompleted {
                        key,
                        generation,
                        ..
                    } if *key == resource_key && *generation == resource_generation
                ),
                "installed evaluator did not receive successful stream terminal completion",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its terminal resource"))?
                .apply_completion(&active, terminal_completion)
                .map_err(|error| failure(format!("apply stream terminal completion failed: {error:?}")))?;
            let terminal_state = state
                .resource(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its completed resource"))?
                .clone();
            require(
                terminal_state.status() == ClientResourceStatus::Ready
                    && terminal_state.stream_complete()
                    && terminal_state.value().is_none()
                    && terminal_state.request_id().is_some(),
                "stream terminal completion did not publish the READY terminal state",
            )?;

            let request_id = terminal_state
                .request_id()
                .ok_or_else(|| failure("completed stream resource lost its request identity"))?;
            let request_id_bytes = request_id.to_bytes().to_vec();
            let audit_session = database.open().await?;
            let audit_operation = async {
                let row = audit_session
                    .client()
                    .query_one(
                        "SELECT resource.parent_invocation_id,
                                resource.call_site_id,
                                resource.nested_invocation_id,
                                resource.target_function_id,
                                resource.source_revision_id,
                                resource.catalogue_revision_id,
                                resource.session_principal_id,
                                resource.decision_outcome,
                                resource.terminal_outcome,
                                invocation.outcome AS invocation_outcome,
                                invocation.function_id AS invocation_function_id,
                                invocation.source_revision_id AS invocation_source_revision_id,
                                invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                invocation.session_principal_id AS invocation_session_principal_id,
                                invocation.effective_principal_id AS invocation_effective_principal_id
                         FROM _orna_kernel.resource_audit_events AS resource
                         LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                           ON invocation.invocation_id = resource.nested_invocation_id
                         WHERE resource.request_id = $1",
                        &[&request_id_bytes],
                    )
                    .await?;
                let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
                let recorded_call_site: Vec<u8> = row.try_get("call_site_id")?;
                let nested: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
                let recorded_target: Option<Vec<u8>> = row.try_get("target_function_id")?;
                let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
                let catalogue_revision: Option<Vec<u8>> =
                    row.try_get("catalogue_revision_id")?;
                let principal: Vec<u8> = row.try_get("session_principal_id")?;
                let decision: String = row.try_get("decision_outcome")?;
                let terminal: String = row.try_get("terminal_outcome")?;
                let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
                let invocation_function: Option<Vec<u8>> =
                    row.try_get("invocation_function_id")?;
                let invocation_source_revision: Option<Vec<u8>> =
                    row.try_get("invocation_source_revision_id")?;
                let invocation_catalogue_revision: Option<Vec<u8>> =
                    row.try_get("invocation_catalogue_revision_id")?;
                let invocation_session_principal: Option<Vec<u8>> =
                    row.try_get("invocation_session_principal_id")?;
                let invocation_effective_principal: Option<Vec<u8>> =
                    row.try_get("invocation_effective_principal_id")?;
                require(
                    parent == parent_invocation.to_bytes().to_vec()
                        && recorded_call_site == call_site.to_bytes().to_vec()
                        && nested.as_ref().is_some_and(|nested| {
                            nested.len() == 16
                                && nested.as_slice() != parent_invocation.to_bytes()
                                && nested.as_slice() != request_id.to_bytes()
                        }),
                    "stream resource audit lost parent, call-site, or nested invocation identity",
                )?;
                require(
                    recorded_target == Some(target.to_bytes().to_vec())
                        && source_revision == Some(active.pair().source().to_bytes().to_vec())
                        && catalogue_revision
                            == Some(active.pair().catalogue().to_bytes().to_vec())
                        && principal == RAW_CLIENT_USER.to_bytes().to_vec()
                        && decision == "allowed"
                        && terminal == "completed"
                        && invocation_outcome.as_deref() == Some("allowed")
                        && invocation_function == Some(target.to_bytes().to_vec())
                        && invocation_source_revision
                            == Some(active.pair().source().to_bytes().to_vec())
                        && invocation_catalogue_revision
                            == Some(active.pair().catalogue().to_bytes().to_vec())
                        && invocation_session_principal == Some(RAW_CLIENT_USER.to_bytes().to_vec())
                        && invocation_effective_principal
                            == Some(RAW_CLIENT_USER.to_bytes().to_vec()),
                    "stream resource audit lost target, revision, principal, or terminal evidence",
                )
            }
            .await;
            finish_session(
                audit_operation,
                audit_session.shutdown().await,
                "stream resource audit lookup",
            )?;
            let terminal_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            let expected_terminal = RuntimeValue::option(
                &active,
                TypeDescriptor::option(TypeDescriptor::list(TypeDescriptor::named(
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                ))?)?,
                None,
            )?;
            require(
                terminal_result.value() == &expected_terminal,
                "next installed stream AWAIT did not return typed terminal None",
            )?;
            require(
                state.resource(resource_key) == Some(&terminal_state),
                "terminal None AWAIT mutated the completed stream resource",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        drop(executor);
        finish_session(operation, Ok(()), "installed stream evaluator cleanup")?;
        require_no_database_sessions(&database).await
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
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_ref_expression_client_loads_object_field_and_redacts_missing_reference()
-> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        let (active, _literal, _composed, _external) =
            install_expression_client_fixture(&kernel).await?;
        let item = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["expr", "item"])
            .ok_or_else(|| failure("expression CLIENT fixture is missing expr.item"))?;
        let title = item
            .fields()
            .iter()
            .find(|field| field.name() == "title")
            .ok_or_else(|| failure("expression CLIENT fixture is missing expr.item.title"))?;
        let ref_composed = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["expr", "ref_composed"])
            .map(FunctionDefinition::id)
            .ok_or_else(|| failure("expression CLIENT fixture is missing expr.ref_composed"))?;
        let parameter = active
            .catalogue()
            .function_by_id(ref_composed)
            .and_then(|function| function.parameters().first())
            .ok_or_else(|| failure("expr.ref_composed is missing p_item"))?;
        require(
            parameter.resolved_type() == ResolvedType::reference(item.id()),
            "expr.ref_composed lost its typed REF expr.item parameter",
        )?;

        let object = ObjectId::from_bytes([0x4a; 16]);
        insert_expression_item_row(&database, &active, item.id(), title.id(), object, "hello")
            .await?;

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
            vec![ExecuteGrant::new(RAW_CLIENT_USER, ref_composed)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference = RuntimeValue::Reference {
            target: item.id(),
            object,
        };
        let audit_before = security_audit_count(&database).await?;
        let success = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            1,
            RawCall {
                function: ref_composed,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: parameter.id(),
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        if !(success.source().is_none()
            && success.actions()
                == [
                    ServerAction::Events {
                        stream: 1,
                        events: vec![Event::Value(RuntimeValue::Text("hello!?".into()))],
                    },
                    ServerAction::Completed { stream: 1 },
                ])
        {
            return Err(failure(format!(
                "installed raw REF expression did not load the object field and concatenate its suffixes: actions={:?}, source={:?}",
                success.actions(),
                success.source(),
            )));
        }
        let audit_after_success = security_audit_count(&database).await?;
        require(
            audit_after_success == audit_before + 1,
            "successful installed REF expression changed audit cardinality unexpectedly",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.last().is_some_and(|event| {
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().target()
                        == Some(InvocationTarget::new(ref_composed, active.pair()))
                    && event.decision().session_principal() == Some(RAW_CLIENT_USER)
            }),
            "successful installed REF expression did not retain a redacted allowed audit decision",
        )?;

        let missing_reference = RuntimeValue::Reference {
            target: item.id(),
            object: ObjectId::from_bytes([0x4b; 16]),
        };
        let missing = RawClientDispatch::new(
            kernel.clone(),
            session,
            2,
            RawCall {
                function: ref_composed,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: parameter.id(),
                    value: missing_reference,
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &missing,
            2,
            CallFailure::ClientEvaluationFailed,
            matches!(missing.source(), Some(PostgresKernelError::ClientExecution(_))),
            "missing installed REF object did not fail through the redacted CLIENT evaluation boundary",
        )?;
        let audit_after_missing = security_audit_count(&database).await?;
        require(
            audit_after_missing == audit_after_success + 1,
            "missing installed REF object changed audit cardinality unexpectedly",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            i64::try_from(events.len())? == audit_after_missing
                && events.iter().rev().take(2).all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                        && event.decision().target()
                            == Some(InvocationTarget::new(ref_composed, active.pair()))
                }),
            "missing installed REF object leaked detail or changed the execute audit boundary",
        )?;
        require_no_database_sessions(&database).await
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
        let upgrade_v6 =
            orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five).map_err(|error| {
                failure(format!(
                    "prepare V5-to-V6 standard upgrade failed: {error:?}"
                ))
            })?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| {
                failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}"))
            })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (
            active,
            client,
            target,
            client_parameter,
            target_parameter,
            local_client,
            local_target,
            local_client_parameter,
            local_target_parameter,
        ) = install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(standard.verified_snapshot().executables().iter().map(
            |executable| {
                SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.verified_snapshot().revision(),
                    executable.revision().id(),
                )
            },
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
                ExecuteGrant::new(RAW_CLIENT_USER, local_client),
                ExecuteGrant::new(RAW_CLIENT_USER, local_target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(local_target),
                vec![CliArgumentInput::Canonical {
                    parameter: local_target_parameter.canonical(),
                    value: "43".to_owned(),
                }],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "action evaluator could not create an owned root invocation",
        )?;
        let audit_session = database.open().await?;
        let root_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation_id
                     FROM _orna_kernel.invocation_audit_events
                     WHERE function_id = $1
                       AND session_principal_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[
                        &local_target.to_bytes().to_vec(),
                        &RAW_CLIENT_USER.to_bytes().to_vec(),
                    ],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| failure("action root invocation audit identity was not 16 bytes"))?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "action root invocation audit lookup",
        )?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed action grant was denied: {denial:?}"
                )));
            }
        };
        let argument = FunctionArgument::new(client_parameter, RuntimeValue::Integer(43))?;
        let evaluation_grants = LocalCapabilityGrantSet::new();
        let mut evaluation_state = ClientStateStore::default();
        let mut evaluation_executor = DeterministicStreamResourceExecutor;
        let result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                std::slice::from_ref(&argument),
                &[],
                &evaluation_grants,
                &mut evaluation_state,
                parent_invocation,
                &mut evaluation_executor,
            )?;
        require(
            result.context().parent_invocation_id() == parent_invocation,
            "action evaluator did not retain its authenticated parent invocation",
        )?;
        let local_authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(local_client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed local action grant was denied: {denial:?}"
                )));
            }
        };
        let local_argument =
            FunctionArgument::new(local_client_parameter, RuntimeValue::Integer(43))?;
        let local_result = evaluate_client_function_with_arguments(
            &active,
            &local_authorisation,
            std::slice::from_ref(&local_argument),
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure(
                "action CLIENT function did not return an opaque action value",
            ));
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
        let RuntimeValue::Opaque(local_action) = local_result.value() else {
            return Err(failure(
                "local action CLIENT function did not return an opaque action value",
            ));
        };
        let local_descriptor = decode_action_payload(&active, local_action.canonical_payload())?;
        require(
            local_descriptor.domain() == ActionTargetDomain::Client
                && local_descriptor.target() == local_target
                && local_descriptor.target_revision() == active.pair()
                && local_descriptor.arguments().len() == 1
                && local_descriptor.arguments()[0].parameter() == local_target_parameter
                && local_descriptor.arguments()[0].value() == local_argument.value(),
            "local action value lost its authenticated CLIENT target or canonical argument",
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
        let action_result: TestResult<ClientActionOutcome> = match action_result {
            Err(ClientActionError::Pending) => {
                let completion = match timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(completion) = executor.poll() {
                            return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(
                                completion,
                            );
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                {
                    Ok(Ok(completion)) => Ok(completion),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(failure("installed action resource completion timed out")),
                };
                match completion {
                    Ok(completion) => {
                        if !matches!(
                            &completion,
                            ClientResourceCompletion::Ready { value, .. }
                                if value == &RuntimeValue::Integer(43)
                        ) {
                            Err(failure(
                                "installed SERVER action did not return typed INTEGER(43)",
                            ))
                        } else {
                            complete_client_action(
                                &active,
                                &mut action_state,
                                completion,
                                &mut executor,
                            )
                            .map_err(|error| {
                                failure(format!(
                                    "installed action resource completion failed: {error:?}"
                                ))
                            })
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            result => Err(failure(format!(
                "installed SERVER action did not enter its pending executor path: {result:?}"
            ))),
        };
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
        let mut local_action_state = ClientActionState::default();
        let mut local_state = ClientStateStore::default();
        let mut local_executor = DeterministicStreamResourceExecutor;
        let local_action_result = trigger_client_action(
            &active,
            local_result.value(),
            &local_authorisation,
            local_result.context(),
            &mut local_action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut local_state,
            &mut local_executor,
        );
        require(
            local_action_result == Ok(ClientActionOutcome::Completed)
                && matches!(local_action_state.status(), ClientResourceStatus::Idle),
            "local CLIENT action did not complete through the installed executor",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let target_bytes = target.to_bytes().to_vec();
        let payload_call_site_bytes = descriptor.call_site().to_bytes().to_vec();
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
                    && call_site.len() == 16
                    && call_site != payload_call_site_bytes
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
async fn proves_server_action_denial_stays_inside_authenticated_resource_trigger() -> TestResult<()>
{
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_five = install_v5_standard(&kernel, &empty, &database).await?;
        let upgrade_v6 =
            orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five).map_err(|error| {
                failure(format!(
                    "prepare V5-to-V6 standard upgrade failed: {error:?}"
                ))
            })?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| {
                failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}"))
            })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action denial fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!(
                "action denial standard source check failed: {error:?}"
            ))
        })?;
        let (
            active,
            client,
            target,
            client_parameter,
            target_parameter,
            _local_client,
            local_target,
            _local_client_parameter,
            local_target_parameter,
        ) = install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(standard.verified_snapshot().executables().iter().map(
            |executable| {
                SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.verified_snapshot().revision(),
                    executable.revision().id(),
                )
            },
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
                ExecuteGrant::new(RAW_CLIENT_USER, local_target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(local_target),
                vec![CliArgumentInput::Canonical {
                    parameter: local_target_parameter.canonical(),
                    value: "43".to_owned(),
                }],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "action denial evaluator could not create an owned root invocation",
        )?;
        let audit_session = database.open().await?;
        let root_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation_id
                     FROM _orna_kernel.invocation_audit_events
                     WHERE function_id = $1
                       AND session_principal_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[
                        &local_target.to_bytes().to_vec(),
                        &RAW_CLIENT_USER.to_bytes().to_vec(),
                    ],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| {
                    failure("action denial root invocation audit identity was not 16 bytes")
                })?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "action denial root invocation audit lookup",
        )?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "action denial client grant was denied: {denial:?}"
                )));
            }
        };
        let argument = FunctionArgument::new(client_parameter, RuntimeValue::Integer(43))?;
        let evaluation_grants = LocalCapabilityGrantSet::new();
        let mut evaluation_state = ClientStateStore::default();
        let mut evaluation_executor = DeterministicStreamResourceExecutor;
        let result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                std::slice::from_ref(&argument),
                &[],
                &evaluation_grants,
                &mut evaluation_state,
                parent_invocation,
                &mut evaluation_executor,
            )?;
        require(
            result.context().parent_invocation_id() == parent_invocation,
            "action denial evaluator did not retain its authenticated parent invocation",
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure(
                "action denial CLIENT function did not return an opaque action value",
            ));
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
        let action_result =
            finish_pending_client_action(&active, &mut action_state, &mut executor, action_result)
                .await
                .map_err(|error| {
                    failure(format!(
                        "installed action resource completion failed: {error:?}"
                    ))
                });
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
            invocation_rows_after.len() == invocation_rows_before.len(),
            "denied SERVER action fabricated a nested invocation audit",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT parent_invocation_id, nested_invocation_id, target_function_id,
                            source_revision_id, catalogue_revision_id, decision_outcome,
                            terminal_outcome, item_count, byte_count,
                            (SELECT count(*)
                               FROM _orna_kernel.invocation_audit_events AS invocation
                              WHERE invocation.invocation_id = resource.nested_invocation_id)
                                AS nested_invocation_count
                     FROM _orna_kernel.resource_audit_events AS resource
                     WHERE parent_invocation_id = $1
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&parent_invocation_id],
                )
                .await?;
            let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
            let nested_invocation: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let nested_invocation_count: i64 = row.try_get("nested_invocation_count")?;
            let audited_target: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let decision: &str = row.try_get("decision_outcome")?;
            let terminal: &str = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            require(
                parent == parent_invocation_id
                    && nested_invocation.is_none()
                    && nested_invocation_count == 0
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

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_v5_json_value_and_encode_through_installed_sealed_invoke() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("orna-v5-json-proof".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| failure(format!("build JSON proof runtime failed: {error}")))?;
            runtime
                .block_on(proves_v5_json_value_and_encode_through_installed_sealed_invoke_inner())
        })
        .map_err(|error| failure(format!("spawn JSON proof thread failed: {error}")))?;
    handle
        .join()
        .map_err(|_| failure("JSON proof thread panicked"))?
}

async fn proves_v5_json_value_and_encode_through_installed_sealed_invoke_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = install_v5_standard(&kernel, &empty, &database).await?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the V5 install did not pin a verified standard snapshot"))?;
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
        let json_value =
            OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &json_payload)?;
        require(
            json_value.canonical_payload() == json_payload.as_slice(),
            "the V5 JSON codec did not retain the canonical value payload",
        )?;
        let typed_null = RuntimeValue::null(ResolvedType::named(STD_JSON_VALUE_TYPE_ID))?;
        require(
            matches!(
                FunctionArgument::new(STD_JSON_ENCODE_PARAMETER_ID, typed_null),
                Err(orna_core::value::FunctionArgumentError::NullValue { parameter, .. })
                    if parameter == STD_JSON_ENCODE_PARAMETER_ID
            ),
            "the ordinary JSON presenter argument boundary accepted a typed NULL",
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
            vec![ExecuteGrant::new(
                RAW_CLIENT_USER,
                STD_JSON_ENCODE_FUNCTION_ID,
            )],
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

        // The ordinary FunctionArgument boundary intentionally rejects typed
        // NULL. Exercise the presenter boundary instead: install one nullable
        // SERVER result, then request the pinned JSON presenter for that result.
        let checked_standard = check_standard_library_source(standard)?;
        let source = SourceBundle::new([SourceUnit::new(
            "json_null_fixture.orna",
            r#"CREATE SCHEMA json_null_fixture;
CREATE TYPE json_null_fixture.probe AS OBJECT (
  marker TEXT UNIQUE NOT NULL, linked REF json_null_fixture.probe
);
CREATE SERVER FUNCTION json_null_fixture.create(p_marker TEXT)
RETURNS ROWS (created REF json_null_fixture.probe)
SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE
AS INSERT INTO json_null_fixture.probe AS made (marker)
VALUES (p_marker) RETURNING REF(made);
CREATE SERVER FUNCTION json_null_fixture.read_links()
RETURNS ROWS (linked REF json_null_fixture.probe)
SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
AS SELECT probe.linked FROM json_null_fixture.probe probe;
"#
            .to_owned(),
        )])?;
        let report = check_standard_application(
            &source,
            &StandardApplicationCheckContext::try_new(active.catalogue(), &checked_standard)?,
        );
        require(
            report.diagnostics().is_empty(),
            "the nullable JSON presenter fixture did not compile",
        )?;
        let installed = kernel
            .apply(&prepare_standard_application(
                &report,
                active.pair(),
                &active,
            )?)
            .await?;
        let create_function = installed
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["json_null_fixture", "create"])
            .ok_or_else(|| failure("the nullable JSON fixture creator is missing"))?
            .id();
        let create_parameter = installed
            .catalogue()
            .function_by_id(create_function)
            .and_then(|function| function.parameter_by_name("p_marker"))
            .ok_or_else(|| failure("the nullable JSON fixture marker parameter is missing"))?
            .id();
        let read_function = installed
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["json_null_fixture", "read_links"])
            .ok_or_else(|| failure("the nullable JSON fixture reader is missing"))?
            .id();
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            installed.pair(),
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
                SecurityFunctionTarget::application(create_function),
                SecurityFunctionTarget::application(read_function),
            ],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, STD_JSON_ENCODE_FUNCTION_ID),
                ExecuteGrant::new(RAW_CLIENT_USER, create_function),
                ExecuteGrant::new(RAW_CLIENT_USER, read_function),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let create_result = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_function,
                &[FunctionArgument::new(
                    create_parameter,
                    RuntimeValue::Text("null-row".to_owned()),
                )?],
            )
            .await?;
        if !matches!(
            &create_result,
            AuthenticatedRawCallResult::Server(values) if values.len() == 1
        ) {
            return Err(failure(format!(
                "the nullable JSON fixture creator did not return one row: {create_result:?}"
            )));
        }
        let json_output = InvocationOutputRequirement::new(
            Some("json".to_owned()),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )?;
        let sink = InvocationSinkOffer::new(
            TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID),
            ["application/json"],
            false,
            0,
            None,
        )?;
        let read_request = InvokeRequest::new(InvokeRequestInput {
            target: InvocationRequestTarget::function_id(read_function),
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
                vec![sink],
                Vec::new(),
                1_024,
                0,
                None,
                None,
            )?,
            output_requirement: Some(json_output),
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: None,
            parent_invocation_id: None,
            observer_context: None,
        })?;
        let retained = encode_invoke_request(&installed, &registry, &read_request)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained)
            .await?;
        let mut expected_null_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected_null_payload.extend_from_slice(&16_u32.to_be_bytes());
        expected_null_payload.extend_from_slice(b"application/json");
        expected_null_payload.extend_from_slice(&4_u32.to_be_bytes());
        expected_null_payload.extend_from_slice(b"null");
        require_json_encode_completion(&result, &expected_null_payload)?;
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
        // presenter) fails closed as the accepted redacted internal Event
        // over the ORF5 transport. No presenter artifact executes, no value
        // reaches stdout, and no diagnostic reaches stderr.
        let (xml_outcome, xml_stdout, xml_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_JSON, Some("application/xml".to_owned()))?,
        )
        .await?;
        require(
            matches!(
                xml_outcome,
                Err(error)
                    if error.kind() == InstalledInvokeErrorKind::Internal
                        && error.message() == "sealed dispatch failed"
            ),
            "the unmatchable output requirement did not return the closed internal class",
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
pub(super) fn installed_invoke_request(
    target: InvocationRequestTarget,
    arguments: Vec<CliArgumentInput>,
    no_progress: bool,
    explain: bool,
) -> InstalledInvokeRequest {
    InstalledInvokeRequest::new(target, arguments, None, None, no_progress, explain, None)
}

/// Applies an optional installed runtime override to a parser-shaped invoke
/// request. Keeping the default helper unchanged makes the no-override path
/// explicit while allowing this proof to exercise `--runtime tty` and the
/// closed unsupported-family arm with the same request shape.
fn installed_invoke_request_with_runtime(
    mut request: InstalledInvokeRequest,
    runtime: Option<RuntimeFamily>,
) -> InstalledInvokeRequest {
    request.runtime = runtime;
    request
}

/// Runs one installed `orna invoke` command through the exact host flow
/// against the Compose PostgreSQL test kernel, returning the outcome or
/// failure class plus the exact bytes each channel received.
pub(super) async fn installed_invoke_run(
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
        result = complete_client_action(active, action_state, completion, executor);
    }
}
