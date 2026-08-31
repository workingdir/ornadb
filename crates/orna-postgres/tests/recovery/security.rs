use super::*;

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn persists_recovers_revokes_and_disables_execute_authority() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("security-recovery-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "security recovery live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(persists_recovers_revokes_and_disables_execute_authority_inner())
        })
        .map_err(|error| {
            failure(format!(
                "security recovery live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("security recovery live thread panicked")),
    }
}

async fn persists_recovers_revokes_and_disables_execute_authority_inner() -> TestResult<()> {
    const USER_UID: u32 = 1_001;
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
    const ROLE: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
    const SERVICE: PrincipalId = PrincipalId::from_bytes([0x33; 16]);
    const OTHER_ROLE: PrincipalId = PrincipalId::from_bytes([0x34; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty_security = kernel.recover_security_snapshot().await?;
        require(
            empty_security.bind_authenticated_session(USER, vec![])
                == Err(SessionBindingError::UnknownSessionPrincipal),
            "empty bootstrap invented a security principal",
        )?;
        let empty = kernel.recover().await?;
        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            "security fixture schema did not compile",
        )?;
        let version_one = kernel
            .apply(&prepare(&schema_report, empty.pair(), &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let active = kernel
            .apply(&standard_client_candidate(
                STANDARD_SECURITY_SOURCE,
                &version_two,
                &upgrade,
            )?)
            .await?;
        let function = require_standard_client_execution(&active, &upgrade, true)?.function;
        let server_function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "read"])
            .ok_or_else(|| failure("security fixture SERVER function was not recovered"))?
            .id();
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        functions.sort_unstable();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("security fixture must use a pinned standard snapshot"))?;
        require(
            standard.catalogue().functions().is_empty(),
            "the current verified standard snapshot must contribute no functions",
        )?;
        let recovered_empty = kernel.recover_security_snapshot().await?;
        require(
            recovered_empty.functions().collect::<Vec<_>>() == functions,
            "security recovery did not derive the exact application and empty-standard target union",
        )?;

        let missing_target = SecuritySnapshot::new(
            active.pair(),
            functions[..1].to_vec(),
            vec![],
            vec![],
            vec![],
        )?;
        let missing_error = kernel
            .replace_security_snapshot(&missing_target)
            .await
            .expect_err("security replacement missing an application target must fail");
        require(
            matches!(
                missing_error,
                PostgresKernelError::SecurityFunctionSetMismatch
            ),
            "missing target replacement returned the wrong typed error",
        )?;
        let extra = FunctionId::from_bytes([0x39; 16]);
        let mut extra_targets = functions.clone();
        extra_targets.push(extra);
        extra_targets.sort_unstable();
        let extra_target = SecuritySnapshot::new(
            active.pair(),
            extra_targets,
            vec![],
            vec![],
            vec![],
        )?;
        let extra_error = kernel
            .replace_security_snapshot(&extra_target)
            .await
            .expect_err("security replacement with an extra target must fail");
        require(
            matches!(
                extra_error,
                PostgresKernelError::SecurityFunctionSetMismatch
            ),
            "extra target replacement returned the wrong typed error",
        )?;
        require(
            kernel
                .recover_security_snapshot()
                .await?
                .functions()
                .collect::<Vec<_>>()
                == functions,
            "rejected target-set replacements changed recovered security targets",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![
                ExecuteGrant::new(ROLE, function),
                ExecuteGrant::new(SERVICE, function),
                ExecuteGrant::new(SERVICE, server_function),
            ],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;

        let recovered = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        let user_session = recovered.bind_authenticated_session(USER, vec![ROLE])?;
        let service_session = recovered.bind_authenticated_session(SERVICE, vec![])?;
        let target = InvocationTarget::new(function, active.pair());
        require(
            recovered.local_peer_credentials().collect::<Vec<_>>()
                == vec![LocalPeerCredential::new(USER_UID, USER)],
            "recovered local peer credential changed",
        )?;
        let local_session = PostgresKernel::new(database.config()?)
            .authenticate_local_peer(USER_UID)
            .await?;
        require(
            local_session.principal() == USER && local_session.active_roles().is_empty(),
            "local peer authentication changed the principal or selected roles",
        )?;
        let unknown_peer_error = kernel
            .authenticate_local_peer(USER_UID + 1)
            .await
            .expect_err("unmapped local peer must fail authentication");
        require(
            matches!(
                unknown_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "unmapped local peer returned the wrong typed error",
        )?;
        require(
            matches!(
                recovered.authorise_execute(&user_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == ROLE
            ) && matches!(
                recovered.authorise_execute(&service_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == SERVICE
            ),
            "recovered direct or selected-role EXECUTE authority changed",
        )?;
        let unselected_role_session = recovered.bind_authenticated_session(USER, vec![])?;
        let missing_error = kernel
            .evaluate_client_function(&unselected_role_session, function)
            .await
            .expect_err("never-granted session must not enter the evaluator");
        require(
            matches!(
                missing_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong never-granted denial",
        )?;
        let evaluated = kernel
            .evaluate_client_function(&user_session, function)
            .await?;
        require(
            evaluated.context().pair() == active.pair()
                && evaluated.context().function() == function
                && evaluated.value() == &RuntimeValue::Boolean(true),
            "kernel CLIENT gate returned the wrong authorised result",
        )?;
        let directly_evaluated = kernel
            .evaluate_client_function(&service_session, function)
            .await?;
        require(
            directly_evaluated.value() == &RuntimeValue::Boolean(true),
            "directly authorised CLIENT evaluation returned the wrong value",
        )?;
        let evaluator_error = kernel
            .evaluate_client_function(&service_session, server_function)
            .await
            .expect_err("SERVER function must be rejected by the CLIENT evaluator");
        require(
            matches!(evaluator_error, PostgresKernelError::ClientExecution(_)),
            "allowed SERVER target returned the wrong CLIENT evaluator error",
        )?;
        let unknown = FunctionId::from_bytes([0x38; 16]);
        let unknown_error = kernel
            .evaluate_client_function(&user_session, unknown)
            .await
            .expect_err("unknown function must be denied before evaluation");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::UnknownFunction,
                } if pair == active.pair() && denied == unknown
            ),
            "kernel CLIENT gate returned the wrong unknown-function denial",
        )?;

        let stale_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x35; 16]),
            CatalogueRevisionId::from_bytes([0x36; 16]),
        );
        let stale = SecuritySnapshot::new(
            stale_pair,
            functions.clone(),
            granted.principals().collect(),
            granted.memberships().collect(),
            granted.execute_grants().collect(),
        )?;
        let stale_error = kernel
            .replace_security_snapshot(&stale)
            .await
            .expect_err("stale security replacement must fail");
        require(
            matches!(
                stale_error,
                PostgresKernelError::SecurityRevisionMismatch {
                    expected,
                    active: locked,
                } if expected == stale_pair && locked == active.pair()
            ),
            "stale security replacement returned the wrong typed error",
        )?;
        let after_stale = kernel.recover_security_snapshot().await?;
        require(
            matches!(
                after_stale.authorise_execute(&service_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == SERVICE
            ),
            "stale security replacement changed durable grants",
        )?;

        let revoked = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let reconnected = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        require(
            reconnected.authorise_execute(&user_session, target)
                == ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant),
            "reconnected snapshot retained a revoked EXECUTE grant",
        )?;
        require(
            reconnected.local_peer_credentials().next().is_none(),
            "reconnected snapshot retained a revoked local peer credential",
        )?;
        let revoked_peer_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("revoked local peer credential must block authentication");
        require(
            matches!(
                revoked_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "revoked local peer credential returned the wrong authentication error",
        )?;
        let revoked_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("revoked EXECUTE grant must block the evaluator");
        require(
            matches!(
                revoked_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong revoked-grant denial",
        )?;

        let stale_session_snapshot = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![],
            vec![ExecuteGrant::new(ROLE, function)],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel
            .replace_security_snapshot(&stale_session_snapshot)
            .await?;
        let stale_session_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("stale selected role must block the evaluator");
        require(
            matches!(
                stale_session_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::InvalidSession,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong stale-session denial",
        )?;

        let disabled = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Disabled),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel.replace_security_snapshot(&disabled).await?;
        let final_snapshot = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        require(
            final_snapshot.bind_authenticated_session(USER, vec![ROLE])
                == Err(SessionBindingError::DisabledSessionPrincipal),
            "reconnected snapshot re-enabled a disabled principal",
        )?;
        let disabled_peer_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("disabled mapped principal must fail authentication");
        require(
            matches!(
                disabled_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::InvalidPrincipal(
                        SessionBindingError::DisabledSessionPrincipal
                    )
                )
            ),
            "disabled mapped principal returned the wrong authentication error",
        )?;
        let disabled_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("disabled session must block the evaluator");
        require(
            matches!(
                disabled_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::InvalidSession,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong disabled-session denial",
        )?;

        let audit = kernel.recover_security_audit_events().await?;
        let execute = audit
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 8,
            format!(
                "security lifecycle appended {} EXECUTE audit records instead of 8",
                execute.len()
            ),
        )?;
        require_execute_audit(
            execute[0],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            InvocationTarget::new(function, active.pair()),
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        require_execute_audit(
            execute[1],
            SecurityAuditOutcome::Allowed,
            USER,
            Some(USER),
            Some(ROLE),
            InvocationTarget::new(function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[2],
            SecurityAuditOutcome::Allowed,
            SERVICE,
            Some(SERVICE),
            Some(SERVICE),
            InvocationTarget::new(function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[3],
            SecurityAuditOutcome::Allowed,
            SERVICE,
            Some(SERVICE),
            Some(SERVICE),
            InvocationTarget::new(server_function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[4],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            InvocationTarget::new(unknown, active.pair()),
            Some(ExecuteDenial::UnknownFunction),
        )?;
        for (event, expected) in [
            (execute[5], ExecuteDenial::MissingExecuteGrant),
            (execute[6], ExecuteDenial::InvalidSession),
            (execute[7], ExecuteDenial::InvalidSession),
        ] {
            require_execute_audit(
                event,
                SecurityAuditOutcome::Denied,
                USER,
                None,
                None,
                InvocationTarget::new(function, active.pair()),
                Some(expected),
            )?;
        }

        kernel.replace_security_snapshot(&granted).await?;
        let session = database.open().await?;
        let constraint = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT security_audit_events_test_reject_execute
                 CHECK (false) NOT VALID;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            constraint,
            session.shutdown().await,
            "EXECUTE audit insert failure fixture",
        )?;
        for (session, target) in [(&service_session, function), (&user_session, unknown)] {
            let audit_failure = kernel
                .evaluate_client_function(session, target)
                .await
                .expect_err("EXECUTE audit insertion failure must fail the operation");
            require(
                matches!(audit_failure, PostgresKernelError::Database(_)),
                "EXECUTE audit insertion failure returned the operation result",
            )?;
        }
        require(
            kernel.recover_security_audit_events().await? == audit,
            "failed EXECUTE audit insertion changed prior history",
        )?;
        let session = database.open().await?;
        let removal = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT security_audit_events_test_reject_execute;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            removal,
            session.shutdown().await,
            "EXECUTE audit insert failure fixture cleanup",
        )?;

        let session = database.open().await?;
        let unknown_function = FunctionId::from_bytes([0x37; 16]);
        let tamper_result = session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants
                     (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[
                    &USER.to_bytes().to_vec(),
                    &unknown_function.to_bytes().to_vec(),
                ],
            )
            .await
            .map(|_| ())
            .map_err(Into::into);
        finish_session(
            tamper_result,
            session.shutdown().await,
            "unknown grant tamper",
        )?;
        let unknown_error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("unknown durable function grant must fail recovery");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownGrantFunction)
            ),
            "unknown durable function grant returned the wrong typed error",
        )?;
        let session = database.open().await?;
        let retained: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM _orna_kernel.security_execute_grants
                 WHERE function_id = $1",
                &[&unknown_function.to_bytes().to_vec()],
            )
            .await?
            .get(0);
        finish_session(
            require(retained == 1, "rejected unknown grant tamper was repaired"),
            session.shutdown().await,
            "unknown grant retention check",
        )?;

        let session = database.open().await?;
        let cycle_result = async {
            session
                .client()
                .execute(
                    "DELETE FROM _orna_kernel.security_execute_grants
                     WHERE function_id = $1",
                    &[&unknown_function.to_bytes().to_vec()],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                     VALUES ($1, 'role', 'active')",
                    &[&OTHER_ROLE.to_bytes().to_vec()],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_role_memberships
                         (role_id, member_id)
                     VALUES ($1, $2), ($2, $1)",
                    &[&ROLE.to_bytes().to_vec(), &OTHER_ROLE.to_bytes().to_vec()],
                )
                .await?;
            Ok(())
        }
        .await;
        finish_session(cycle_result, session.shutdown().await, "role cycle tamper")?;
        let cycle_error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("durable role cycle must fail recovery");
        require(
            matches!(
                cycle_error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::CyclicRoleMembership)
            ),
            "durable role cycle returned the wrong typed error",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_closed_security_audit_history_under_hostile_search_path_and_rejects_tamper()
-> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x33; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0xf1; 16]);
    const PAIR: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x51; 16]),
        CatalogueRevisionId::from_bytes([0xc1; 16]),
    );

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let active = kernel.bootstrap().await?;
        let active_pair = RevisionPair::new(active.source(), active.catalogue());
        let session = database.open().await?;
        let insertion = session
            .client()
            .batch_execute(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome, session_principal_id)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      TIMESTAMP '1969-12-31 23:59:59',
                      'authentication', 'allowed', decode(repeat('31', 16), 'hex'));

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome, denial_reason)
                 VALUES
                     (decode(repeat('a2', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:00',
                      'authentication', 'denied', 'authentication_unknown_uid');

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, effective_principal_id,
                      authorising_principal_id, function_id, source_revision_id,
                      catalogue_revision_id)
                 VALUES
                     (decode(repeat('a3', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:01',
                      'execute', 'allowed',
                      decode(repeat('31', 16), 'hex'),
                      decode(repeat('32', 16), 'hex'),
                      decode(repeat('33', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      decode(repeat('51', 16), 'hex'),
                      decode(repeat('c1', 16), 'hex'));

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a4', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:02',
                      'execute', 'denied',
                      decode(repeat('31', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      decode(repeat('51', 16), 'hex'),
                      decode(repeat('c1', 16), 'hex'),
                      'execute_missing_grant');",
            )
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            session.shutdown().await,
            "security audit fixture insertion",
        )?;

        run_batch(
            &database,
            "CREATE TABLE public.active_revision AS
                 SELECT * FROM _orna_kernel.active_revision WITH NO DATA;
             INSERT INTO public.active_revision
                 (singleton, source_revision_id, catalogue_revision_id)
             VALUES
                 (true, decode(repeat('d1', 16), 'hex'),
                        decode(repeat('d2', 16), 'hex'));

             CREATE TABLE public.security_audit_events AS
                 SELECT * FROM _orna_kernel.security_audit_events WITH NO DATA;
             INSERT INTO public.security_audit_events
                 (sequence, event_id, recorded_at, event_kind, outcome,
                  denial_reason)
             VALUES
                 (1, decode(repeat('b1', 16), 'hex'),
                  TIMESTAMP '1970-01-01 00:00:00', 'authentication', 'denied',
                  'authentication_unknown_uid');",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        let hostile_kernel = PostgresKernel::new(hostile_config);
        let recovered_active = hostile_kernel.recover().await?;
        require(
            recovered_active.pair() == active_pair,
            "hostile search_path redirected active revision recovery",
        )?;

        let target = InvocationTarget::new(FUNCTION, PAIR);
        let expected = vec![
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa1; 16]),
                1,
                UNIX_EPOCH - Duration::from_secs(1),
                SecurityAuditDecision::recover_authentication_allowed(USER),
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa2; 16]),
                2,
                UNIX_EPOCH,
                SecurityAuditDecision::authentication_denied(
                    None,
                    LocalPeerAuthenticationError::UnknownUid,
                )?,
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa3; 16]),
                3,
                UNIX_EPOCH + Duration::from_secs(1),
                SecurityAuditDecision::recover_execute_allowed(
                    USER,
                    EFFECTIVE,
                    AUTHORISING,
                    target,
                ),
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa4; 16]),
                4,
                UNIX_EPOCH + Duration::from_secs(2),
                SecurityAuditDecision::recover_execute_denied(
                    USER,
                    target,
                    ExecuteDenial::MissingExecuteGrant,
                ),
            ),
        ];
        let recovered = hostile_kernel.recover_security_audit_events().await?;
        require(
            recovered == expected,
            "security audit recovery changed order, time, identity, or decision evidence",
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                     DROP CONSTRAINT security_audit_events_shape_check,
                     DROP CONSTRAINT security_audit_events_revision_pair_check,
                     DROP CONSTRAINT security_audit_events_denial_reason_check;
                 UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode(repeat('f1', 16), 'hex')
                 WHERE sequence = 1;",
            )
            .await?;

        let error = hostile_kernel
            .recover()
            .await
            .expect_err("malformed durable security audit data must fail full recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "audit event shape is not recognised",
                } if record == "1"
            ),
            "full recovery returned the wrong malformed security audit invariant",
        )?;

        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("invalid durable security audit shape must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "audit event shape is not recognised",
                } if record == "1"
            ),
            "security audit shape tamper returned the wrong durable invariant",
        )?;

        let retained: bool = session
            .client()
            .query_one(
                "SELECT function_id = decode(repeat('f1', 16), 'hex')
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 1",
                &[],
            )
            .await?
            .get(0);
        require(
            retained,
            "rejected security audit shape tamper was repaired",
        )?;

        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = NULL
                 WHERE sequence = 1;
                 UPDATE _orna_kernel.security_audit_events
                 SET catalogue_revision_id = NULL
                 WHERE sequence = 4;",
            )
            .await?;
        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("incomplete durable security audit pair must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "EXECUTE requires a catalogue revision",
                } if record == "4"
            ),
            "security audit pair tamper returned the wrong durable invariant",
        )?;
        let retained_pair: bool = session
            .client()
            .query_one(
                "SELECT catalogue_revision_id IS NULL
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 4",
                &[],
            )
            .await?
            .get(0);
        require(
            retained_pair,
            "rejected security audit pair tamper was repaired",
        )?;

        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET catalogue_revision_id = decode(repeat('c1', 16), 'hex'),
                     denial_reason = 'execute_not_supported'
                 WHERE sequence = 4;",
            )
            .await?;
        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("unknown durable security audit denial must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "EXECUTE denial reason is unsupported",
                } if record == "4"
            ),
            "security audit denial tamper returned the wrong durable invariant",
        )?;
        let retained_reason: String = session
            .client()
            .query_one(
                "SELECT denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 4",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained_reason == "execute_not_supported",
                "rejected security audit denial tamper was repaired",
            ),
            session.shutdown().await,
            "security audit tamper retention checks",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_security_admin_audit_with_exact_redacted_shape() -> TestResult<()> {
    const ADMIN: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const USER: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const CREATED: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let security =
            SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                active.pair(),
                vec![],
                vec![
                    Principal::new(ADMIN, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                ],
                vec![],
                vec![],
                vec![],
                vec![PrivilegeGrant::new(
                    ADMIN,
                    PrivilegeClass::SecurityAdmin,
                    None,
                )?],
            )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let admin_session = security.bind_authenticated_session(ADMIN, vec![])?;
        let user_session = security.bind_authenticated_session(USER, vec![])?;

        kernel
            .create_principal(&admin_session, CREATED, PrincipalKind::User)
            .await?;
        let denied = kernel
            .create_principal(&user_session, CREATED, PrincipalKind::User)
            .await
            .expect_err("an unprivileged session must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::SecurityAdminDenied {
                    reason: PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    },
                }
            ),
            "SecurityAdmin denial returned the wrong typed error",
        )?;

        let reopened = PostgresKernel::new(database.config()?);
        let events = reopened.recover_security_audit_events().await?;
        require(
            events.len() == 2,
            format!(
                "fresh recovery returned {} security-admin events instead of 2",
                events.len()
            ),
        )?;
        let allowed = &events[0].decision();
        require(
            allowed.kind() == SecurityAuditKind::SecurityAdmin
                && allowed.outcome() == SecurityAuditOutcome::Allowed
                && allowed.session_principal() == Some(ADMIN)
                && allowed.security_admin_operation()
                    == Some(SecurityAdminAuditOperation::CreatePrincipal)
                && allowed.security_admin_target()
                    == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                && allowed.security_admin_denial().is_none()
                && allowed.effective_principal().is_none()
                && allowed.authorising_principal().is_none()
                && allowed.target().is_none(),
            "fresh recovery changed the allowed SecurityAdmin decision shape",
        )?;
        let denied = &events[1].decision();
        require(
            denied.kind() == SecurityAuditKind::SecurityAdmin
                && denied.outcome() == SecurityAuditOutcome::Denied
                && denied.session_principal() == Some(USER)
                && denied.security_admin_operation()
                    == Some(SecurityAdminAuditOperation::CreatePrincipal)
                && denied.security_admin_target()
                    == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                && denied.security_admin_denial()
                    == Some(PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    })
                && denied.effective_principal().is_none()
                && denied.authorising_principal().is_none()
                && denied.target().is_none(),
            "fresh recovery changed the denied SecurityAdmin decision shape",
        )?;

        let session = database.open().await?;
        let rows = session
            .client()
            .query(
                "SELECT event_kind, outcome, session_principal_id,
                        effective_principal_id, authorising_principal_id, function_id,
                        source_revision_id, catalogue_revision_id, denial_reason
                 FROM _orna_kernel.security_audit_events
                 ORDER BY sequence",
                &[],
            )
            .await?;
        require(
            rows.len() == 2,
            "durable SecurityAdmin audit row count changed",
        )?;
        for (row, principal, detail) in [
            (&rows[0], ADMIN, "security_admin:create_principal"),
            (
                &rows[1],
                USER,
                "security_admin:create_principal:missing-privilege",
            ),
        ] {
            let event_kind: String = row.try_get(0)?;
            let outcome: String = row.try_get(1)?;
            let session_principal: Vec<u8> = row.try_get(2)?;
            let effective_principal: Option<Vec<u8>> = row.try_get(3)?;
            let authorising_principal: Option<Vec<u8>> = row.try_get(4)?;
            let function: Vec<u8> = row.try_get(5)?;
            let source_revision: Option<Vec<u8>> = row.try_get(6)?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get(7)?;
            let denial_reason: Option<String> = row.try_get(8)?;
            require(
                event_kind == "security_admin"
                    && outcome
                        == if principal == ADMIN {
                            "allowed"
                        } else {
                            "denied"
                        }
                    && session_principal == principal.to_bytes()
                    && function == SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID.to_bytes()
                    && effective_principal.is_none()
                    && authorising_principal.is_none()
                    && source_revision.is_none()
                    && catalogue_revision.is_none()
                    && denial_reason.as_deref() == Some(detail),
                "durable SecurityAdmin audit row contains an unexpected payload",
            )?;
        }
        for (statement, description) in [
            (
                "UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'security_admin:unsupported'
                 WHERE sequence = 1",
                "forged security-admin operation detail",
            ),
            (
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode('00000000000000000000000000000044', 'hex')
                 WHERE sequence = 1",
                "mismatched security-admin target",
            ),
        ] {
            let result = session.client().execute(statement, &[]).await;
            let error = match result {
                Ok(_) => {
                    return Err(failure(format!(
                        "{description} unexpectedly bypassed the durable audit boundary"
                    )));
                }
                Err(error) => error,
            };
            let database_error = error
                .as_db_error()
                .ok_or_else(|| failure(format!("{description} returned a non-database error")))?;
            require(
                database_error.code().code() == "23514",
                format!(
                    "{description} failed with SQLSTATE {} instead of CHECK violation",
                    database_error.code().code()
                ),
            )?;
            require(
                database_error.constraint()
                    == Some("security_audit_events_security_admin_detail_check"),
                format!(
                    "{description} failed on unexpected constraint {:?}",
                    database_error.constraint()
                ),
            )?;
        }
        finish_session(
            Ok(()),
            session.shutdown().await,
            "security-admin audit redaction",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_closed_capability_audit_history_and_rejects_unredacted_tamper() -> TestResult<()>
{
    const USER: PrincipalId = PrincipalId::from_bytes([0x41; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0xf2; 16]);
    const PAIR: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x52; 16]),
        CatalogueRevisionId::from_bytes([0xc2; 16]),
    );

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let session = database.open().await?;
        let insertion = session
            .client()
            .batch_execute(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a5', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:03',
                      'capability', 'allowed',
                      decode(repeat('41', 16), 'hex'),
                      decode(repeat('f2', 16), 'hex'),
                      decode(repeat('52', 16), 'hex'),
                      decode(repeat('c2', 16), 'hex'),
                      'capability:std.fs.read');
                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a6', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:04',
                      'capability', 'denied',
                      decode(repeat('41', 16), 'hex'),
                      decode(repeat('f2', 16), 'hex'),
                      decode(repeat('52', 16), 'hex'),
                      decode(repeat('c2', 16), 'hex'),
                      'capability:std.net.connect');",
            )
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            session.shutdown().await,
            "capability audit fixture insertion",
        )?;

        let target = InvocationTarget::new(FUNCTION, PAIR);
        let expected = vec![
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa5; 16]),
                1,
                UNIX_EPOCH + Duration::from_secs(3),
                SecurityAuditDecision::recover_capability_allowed(
                    USER,
                    target,
                    "std.fs.read".to_owned(),
                )?,
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa6; 16]),
                2,
                UNIX_EPOCH + Duration::from_secs(4),
                SecurityAuditDecision::recover_capability_denied(
                    USER,
                    target,
                    "std.net.connect".to_owned(),
                )?,
            ),
        ];
        let recovered = PostgresKernel::new(database.config()?)
            .recover_security_audit_events()
            .await?;
        require(
            recovered == expected,
            "capability audit recovery changed order, time, identity, or decision evidence",
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'capability:std.fs.read(/home/bob)'
                 WHERE sequence = 1;",
            )
            .await?;
        let error = kernel
            .recover_security_audit_events()
            .await
            .expect_err("unredacted capability audit evidence must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "capability audit name must be a qualified name with no arguments",
                } if record == "1"
            ),
            "unredacted capability tamper returned the wrong durable invariant",
        )?;

        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                     DROP CONSTRAINT security_audit_events_shape_check,
                     DROP CONSTRAINT security_audit_events_denial_reason_check;
                 UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'execute_missing_grant'
                 WHERE sequence = 1;",
            )
            .await?;
        let error = kernel
            .recover_security_audit_events()
            .await
            .expect_err("unsupported capability audit evidence must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "capability denial reason is unsupported",
                } if record == "1"
            ),
            "unsupported capability tamper returned the wrong durable invariant",
        )?;

        let retained: String = session
            .client()
            .query_one(
                "SELECT denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 1",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained == "execute_missing_grant",
                "rejected capability audit tamper was repaired",
            ),
            session.shutdown().await,
            "capability audit tamper retention checks",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_resource_audit_without_its_durable_request_reservation() -> TestResult<()>
{
    const ORPHAN_REQUEST_ID: [u8; 16] = [0x90; 16];
    const REQUEST_ID: [u8; 16] = [0x91; 16];
    const NESTED_INVOCATION_ID: [u8; 16] = [0x92; 16];
    const PARENT_INVOCATION_ID: [u8; 16] = [0x93; 16];
    const CALL_SITE_ID: [u8; 16] = [0x94; 16];
    const SESSION_PRINCIPAL_ID: [u8; 16] = [0x95; 16];
    const RESOURCE_EVENT_ID: [u8; 16] = [0x96; 16];
    const INVOCATION_EVENT_ID: [u8; 16] = [0x97; 16];
    const SECURITY_EVENT_ID: [u8; 16] = [0x98; 16];

    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel_instance.recover().await?;
        let target_function = fixture
            .catalogue
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "café.volatile_single")
            .ok_or_else(|| failure("function fixture is missing café.volatile_single"))?
            .id();
        let target_revision = active.pair();

        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id)
                 VALUES ({orphan_request}), ({request});
                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, event_kind, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id)
                 VALUES ({security_event}, 'execute', 'allowed', {session_principal},
                         {session_principal}, {session_principal}, {target_function},
                         {source_revision}, {catalogue_revision});
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES ({invocation_event}, {nested_invocation}, 'allowed', {session_principal},
                         {session_principal}, {session_principal}, {target_function},
                         {source_revision}, {catalogue_revision}, {security_event});
                 INSERT INTO _orna_kernel.resource_audit_events
                     (event_id, request_id, nested_invocation_id, parent_invocation_id,
                      call_site_id, target_function_id, source_revision_id,
                      catalogue_revision_id, session_principal_id, decision_outcome,
                      terminal_outcome, item_count, byte_count)
                 VALUES ({resource_event}, {request}, {nested_invocation}, {parent_invocation},
                         {call_site}, {target_function}, {source_revision},
                         {catalogue_revision}, {session_principal}, 'allowed',
                         'completed', 1, 1);",
                orphan_request = bytea_literal(ORPHAN_REQUEST_ID),
                request = bytea_literal(REQUEST_ID),
                security_event = bytea_literal(SECURITY_EVENT_ID),
                session_principal = bytea_literal(SESSION_PRINCIPAL_ID),
                target_function = bytea_literal(target_function.to_bytes()),
                source_revision = bytea_literal(target_revision.source().to_bytes()),
                catalogue_revision = bytea_literal(target_revision.catalogue().to_bytes()),
                invocation_event = bytea_literal(INVOCATION_EVENT_ID),
                nested_invocation = bytea_literal(NESTED_INVOCATION_ID),
                resource_event = bytea_literal(RESOURCE_EVENT_ID),
                parent_invocation = bytea_literal(PARENT_INVOCATION_ID),
                call_site = bytea_literal(CALL_SITE_ID),
            ),
        )
        .await?;

        kernel_instance.recover().await?;

        run_batch(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.resource_request_history WHERE request_id = {}",
                bytea_literal(REQUEST_ID),
            ),
        )
        .await?;
        let request = InvocationId::from_bytes(REQUEST_ID);
        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_request_history",
                    record,
                    rule: "accepted resource producer must retain its reservation",
                } if record == request.canonical()
            ),
            "missing resource request reservation returned the wrong recovery invariant",
        )?;

        let session = database.open().await?;
        let retention = async {
            let row = session
                .client()
                .query_one(
                    &format!(
                        "SELECT
                             (SELECT count(*) FROM _orna_kernel.resource_audit_events
                               WHERE request_id = {request_id}) AS resource_count,
                             (SELECT count(*) FROM _orna_kernel.invocation_audit_events
                               WHERE invocation_id = {nested_invocation}) AS invocation_count,
                             (SELECT count(*) FROM _orna_kernel.resource_request_history
                               WHERE request_id = {request_id}) AS reservation_count,
                             (SELECT count(*) FROM _orna_kernel.resource_request_history
                               WHERE request_id = {orphan_request}) AS orphan_count",
                        request_id = bytea_literal(REQUEST_ID),
                        nested_invocation = bytea_literal(NESTED_INVOCATION_ID),
                        orphan_request = bytea_literal(ORPHAN_REQUEST_ID),
                    ),
                    &[],
                )
                .await?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let reservation_count: i64 = row.try_get("reservation_count")?;
            let orphan_count: i64 = row.try_get("orphan_count")?;
            require(
                resource_count == 1
                    && invocation_count == 1
                    && reservation_count == 0
                    && orphan_count == 1,
                "failed resource recovery repaired audit or history rows",
            )
        }
        .await;
        finish_session(
            retention,
            session.shutdown().await,
            "resource recovery retention",
        )?;

        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id) VALUES ({})",
                bytea_literal(REQUEST_ID),
            ),
        )
        .await?;
        kernel_instance.recover().await?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_accepts_preaccept_resource_audit_without_nested_identity() -> TestResult<()> {
    const FIRST_REQUEST_ID: [u8; 16] = [0xa8; 16];
    const SECOND_REQUEST_ID: [u8; 16] = [0xa9; 16];
    const FIRST_PARENT_INVOCATION_ID: [u8; 16] = [0xaa; 16];
    const SECOND_PARENT_INVOCATION_ID: [u8; 16] = [0xab; 16];
    const FIRST_CALL_SITE_ID: [u8; 16] = [0xac; 16];
    const SECOND_CALL_SITE_ID: [u8; 16] = [0xad; 16];
    const SESSION_PRINCIPAL_ID: [u8; 16] = [0xae; 16];
    const FIRST_EVENT_ID: [u8; 16] = [0xaf; 16];
    const SECOND_EVENT_ID: [u8; 16] = [0xb0; 16];

    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        kernel_instance.recover().await?;
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id)
                     VALUES ({first_request}), ({second_request});
                 INSERT INTO _orna_kernel.resource_audit_events
                     (event_id, request_id, nested_invocation_id, parent_invocation_id,
                      call_site_id, session_principal_id, decision_outcome, terminal_outcome)
                 VALUES ({first_event}, {first_request}, NULL, {first_parent},
                         {first_call_site}, {principal}, 'denied', 'failed'),
                        ({second_event}, {second_request}, NULL, {second_parent},
                         {second_call_site}, {principal}, 'denied', 'cancelled');",
                first_request = bytea_literal(FIRST_REQUEST_ID),
                second_request = bytea_literal(SECOND_REQUEST_ID),
                first_parent = bytea_literal(FIRST_PARENT_INVOCATION_ID),
                second_parent = bytea_literal(SECOND_PARENT_INVOCATION_ID),
                first_call_site = bytea_literal(FIRST_CALL_SITE_ID),
                second_call_site = bytea_literal(SECOND_CALL_SITE_ID),
                principal = bytea_literal(SESSION_PRINCIPAL_ID),
                first_event = bytea_literal(FIRST_EVENT_ID),
                second_event = bytea_literal(SECOND_EVENT_ID),
            ),
        )
        .await?;

        kernel_instance.recover().await?;
        let session = database.open().await?;
        let verification = async {
            let row = session
                .client()
                .query_one(
                    "SELECT
                         (SELECT count(*) FROM _orna_kernel.resource_audit_events
                           WHERE nested_invocation_id IS NULL) AS null_nested_count,
                         (SELECT count(*) FROM _orna_kernel.resource_audit_events)
                           AS resource_count,
                         (SELECT count(*) FROM _orna_kernel.invocation_audit_events)
                           AS invocation_count",
                    &[],
                )
                .await?;
            let null_nested_count: i64 = row.try_get("null_nested_count")?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            require(
                null_nested_count == 2 && resource_count == 2 && invocation_count == 0,
                "preaccept resource audits did not retain nullable nested identity without fabricated invocation rows",
            )
        }
        .await;
        finish_session(
            verification,
            session.shutdown().await,
            "nullable preaccept resource audit recovery",
        )?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.resource_audit_events
                 DROP CONSTRAINT resource_audit_events_identity_lengths,
                 DROP CONSTRAINT resource_audit_events_nested_invocation_fk;
             UPDATE _orna_kernel.resource_audit_events
                SET nested_invocation_id = decode(repeat('a1', 15), 'hex')
              WHERE event_id = decode(repeat('af', 16), 'hex')",
        )
        .await?;
        let invalid_some = recovery_error(&database).await?;
        require(
            matches!(
                invalid_some,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "resource audit identity must be exactly sixteen bytes",
                    ..
                }
            ),
            "invalid Some nested resource identity did not fail closed",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_duplicate_resource_audit_request_and_nested_identities() -> TestResult<()>
{
    const REQUEST_ID: [u8; 16] = [0xa1; 16];
    const SECOND_REQUEST_ID: [u8; 16] = [0xa2; 16];
    const NESTED_INVOCATION_ID: [u8; 16] = [0xb1; 16];
    const SECOND_NESTED_INVOCATION_ID: [u8; 16] = [0xb2; 16];
    const PARENT_INVOCATION_ID: [u8; 16] = [0xc1; 16];
    const SECOND_PARENT_INVOCATION_ID: [u8; 16] = [0xc2; 16];
    const CALL_SITE_ID: [u8; 16] = [0xd1; 16];
    const SECOND_CALL_SITE_ID: [u8; 16] = [0xd2; 16];
    const SESSION_PRINCIPAL_ID: [u8; 16] = [0xe1; 16];
    const FIRST_EVENT_ID: [u8; 16] = [0xf1; 16];
    const SECOND_EVENT_ID: [u8; 16] = [0xf2; 16];
    const FIRST_INVOCATION_EVENT_ID: [u8; 16] = [0x11; 16];
    const SECOND_INVOCATION_EVENT_ID: [u8; 16] = [0x12; 16];

    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        kernel_instance.recover().await?;
        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.resource_audit_events
                     DROP CONSTRAINT resource_audit_events_event_id_key,
                     DROP CONSTRAINT resource_audit_events_request_id_key,
                     DROP CONSTRAINT resource_audit_events_nested_invocation_id_key;
                 INSERT INTO _orna_kernel.resource_request_history (request_id)
                     VALUES ({request_id});
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id)
                 VALUES ({first_invocation_event}, {first_nested}, 'denied', {principal}),
                        ({second_invocation_event}, {second_nested}, 'denied', {principal});
                 INSERT INTO _orna_kernel.resource_audit_events
                     (event_id, request_id, nested_invocation_id, parent_invocation_id,
                      call_site_id, session_principal_id, decision_outcome, terminal_outcome)
                 VALUES ({first_event}, {request_id}, {first_nested}, {parent}, {call_site},
                         {principal}, 'denied', 'failed'),
                        ({second_event}, {request_id}, {second_nested}, {second_parent},
                         {second_call_site}, {principal}, 'denied', 'failed');",
                request_id = bytea_literal(REQUEST_ID),
                first_nested = bytea_literal(NESTED_INVOCATION_ID),
                second_nested = bytea_literal(SECOND_NESTED_INVOCATION_ID),
                parent = bytea_literal(PARENT_INVOCATION_ID),
                second_parent = bytea_literal(SECOND_PARENT_INVOCATION_ID),
                call_site = bytea_literal(CALL_SITE_ID),
                second_call_site = bytea_literal(SECOND_CALL_SITE_ID),
                principal = bytea_literal(SESSION_PRINCIPAL_ID),
                first_event = bytea_literal(FIRST_EVENT_ID),
                second_event = bytea_literal(SECOND_EVENT_ID),
                first_invocation_event = bytea_literal(FIRST_INVOCATION_EVENT_ID),
                second_invocation_event = bytea_literal(SECOND_INVOCATION_EVENT_ID),
            ),
        )
        .await?;

        let duplicate_request = recovery_error(&database).await?;
        require(
            matches!(
                duplicate_request,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "resource request identity must be unique during recovery",
                    ..
                }
            ),
            "duplicate resource request identity did not fail closed",
        )?;

        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.resource_audit_events
                    SET request_id = {second_request}
                  WHERE event_id = {second_event};
                 INSERT INTO _orna_kernel.resource_request_history (request_id)
                     VALUES ({second_request});",
                second_request = bytea_literal(SECOND_REQUEST_ID),
                second_event = bytea_literal(SECOND_EVENT_ID),
            ),
        )
        .await?;
        kernel_instance.recover().await?;

        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.resource_audit_events
                    SET nested_invocation_id = {nested}
                  WHERE event_id = {event}",
                nested = bytea_literal(NESTED_INVOCATION_ID),
                event = bytea_literal(SECOND_EVENT_ID),
            ),
        )
        .await?;
        let duplicate_nested = recovery_error(&database).await?;
        require(
            matches!(
                duplicate_nested,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "resource nested invocation identity must be unique during recovery",
                    ..
                }
            ),
            "duplicate nested resource invocation identity did not fail closed",
        )?;

        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.resource_audit_events
                    SET nested_invocation_id = {nested}
                  WHERE event_id = {event};
                 UPDATE _orna_kernel.resource_audit_events
                    SET event_id = {first_event}
                  WHERE event_id = {second_event};",
                nested = bytea_literal(SECOND_NESTED_INVOCATION_ID),
                event = bytea_literal(SECOND_EVENT_ID),
                first_event = bytea_literal(FIRST_EVENT_ID),
                second_event = bytea_literal(SECOND_EVENT_ID),
            ),
        )
        .await?;
        let duplicate_event = recovery_error(&database).await?;
        require(
            matches!(
                duplicate_event,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "resource event identity must be unique during recovery",
                    ..
                }
            ),
            "duplicate resource event identity did not fail closed",
        )?;

        run_batch(
            &database,
            "UPDATE _orna_kernel.resource_audit_events
                SET event_id = decode(repeat('00', 16), 'hex')",
        )
        .await?;
        let duplicate_zero_event = recovery_error(&database).await?;
        require(
            matches!(
                duplicate_zero_event,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "resource event identity must be unique during recovery",
                    ..
                }
            ),
            "duplicate zero resource event identity did not fail closed",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_allowed_resource_audit_without_matching_target_evidence() -> TestResult<()>
{
    const REQUEST_ID: [u8; 16] = [0x21; 16];
    const NESTED_INVOCATION_ID: [u8; 16] = [0x22; 16];
    const PARENT_INVOCATION_ID: [u8; 16] = [0x23; 16];
    const CALL_SITE_ID: [u8; 16] = [0x24; 16];
    const SESSION_PRINCIPAL_ID: [u8; 16] = [0x25; 16];
    const RESOURCE_EVENT_ID: [u8; 16] = [0x26; 16];
    const INVOCATION_EVENT_ID: [u8; 16] = [0x27; 16];
    const SECURITY_EVENT_ID: [u8; 16] = [0x28; 16];

    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel_instance.recover().await?;
        let target_function = fixture
            .catalogue
            .functions()
            .iter()
            .find(|definition| definition.domain() == FunctionDomain::Server)
            .ok_or_else(|| failure("server function fixture is missing"))?;
        let target_revision = active.pair();

        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id)
                     VALUES ({request_id});
                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, event_kind, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id)
                 VALUES ({security_event}, 'execute', 'allowed', {principal}, {principal},
                         {principal}, {function}, {source_revision}, {catalogue});
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES ({invocation_event}, {nested}, 'allowed', {principal}, {principal},
                         {principal}, {function}, {source_revision}, {catalogue},
                         {security_event});
                 INSERT INTO _orna_kernel.resource_audit_events
                     (event_id, request_id, nested_invocation_id, parent_invocation_id,
                      call_site_id, session_principal_id, decision_outcome, terminal_outcome)
                 VALUES ({resource_event}, {request_id}, {nested}, {parent}, {call_site},
                         {principal}, 'allowed', 'failed');",
                request_id = bytea_literal(REQUEST_ID),
                nested = bytea_literal(NESTED_INVOCATION_ID),
                parent = bytea_literal(PARENT_INVOCATION_ID),
                call_site = bytea_literal(CALL_SITE_ID),
                principal = bytea_literal(SESSION_PRINCIPAL_ID),
                resource_event = bytea_literal(RESOURCE_EVENT_ID),
                invocation_event = bytea_literal(INVOCATION_EVENT_ID),
                security_event = bytea_literal(SECURITY_EVENT_ID),
                function = bytea_literal(target_function.id().to_bytes()),
                source_revision = bytea_literal(target_revision.source().to_bytes()),
                catalogue = bytea_literal(target_revision.catalogue().to_bytes()),
            ),
        )
        .await?;

        let missing_target = recovery_error(&database).await?;
        require(
            matches!(
                missing_target,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "allowed nested invocation requires resource audit target evidence",
                    ..
                }
            ),
            "allowed linked invocation without a resource target did not fail closed",
        )?;

        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.resource_audit_events
                    SET target_function_id = {function},
                        source_revision_id = {source_revision},
                        catalogue_revision_id = {catalogue}
                  WHERE event_id = {event}",
                function = bytea_literal(target_function.id().to_bytes()),
                source_revision = bytea_literal(target_revision.source().to_bytes()),
                catalogue = bytea_literal(target_revision.catalogue().to_bytes()),
                event = bytea_literal(RESOURCE_EVENT_ID),
            ),
        )
        .await?;
        kernel_instance.recover().await?;

        let mismatched_function = fixture
            .catalogue
            .functions()
            .iter()
            .find(|definition| definition.id() != target_function.id())
            .ok_or_else(|| failure("second function fixture is missing"))?;
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.resource_audit_events
                    SET target_function_id = {function}
                  WHERE event_id = {event}",
                function = bytea_literal(mismatched_function.id().to_bytes()),
                event = bytea_literal(RESOURCE_EVENT_ID),
            ),
        )
        .await?;
        let mismatched_target = recovery_error(&database).await?;
        require(
            matches!(
                mismatched_target,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "nested invocation target does not match resource audit",
                    ..
                }
            ),
            "inconsistent linked resource target did not fail closed",
        )?;

        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.resource_audit_events
                     DROP CONSTRAINT resource_audit_events_revision_pair_fk;
                 UPDATE _orna_kernel.resource_audit_events
                    SET target_function_id = {function},
                        source_revision_id = {source_revision}
                  WHERE event_id = {event};",
                function = bytea_literal(target_function.id().to_bytes()),
                source_revision = bytea_literal([0x29; 16]),
                event = bytea_literal(RESOURCE_EVENT_ID),
            ),
        )
        .await?;
        let mismatched_revision = recovery_error(&database).await?;
        require(
            matches!(
                mismatched_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_audit_events",
                    rule: "nested invocation target does not match resource audit",
                    ..
                }
            ),
            "inconsistent linked resource revision did not fail closed",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_tampered_protected_invocation_audit_evidence() -> TestResult<()> {
    const SESSION: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel.recover().await?;
        let function_id = fixture.catalogue.functions()[0].id();
        let database_session = database.open().await?;
        let insertion = database_session
            .client()
            .batch_execute(&format!(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, event_kind, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id)
                 VALUES (decode(repeat('a1', 16), 'hex'), 'execute', 'allowed',
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'));
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES (decode(repeat('b1', 16), 'hex'), decode(repeat('c1', 16), 'hex'),
                         'allowed', decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode(repeat('a1', 16), 'hex'));",
                raw_id_hex(SESSION.to_bytes()),
                raw_id_hex(EFFECTIVE.to_bytes()),
                raw_id_hex(AUTHORISING.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().catalogue().to_bytes()),
                raw_id_hex(SESSION.to_bytes()),
                raw_id_hex(EFFECTIVE.to_bytes()),
                raw_id_hex(AUTHORISING.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().catalogue().to_bytes()),
            ))
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            database_session.shutdown().await,
            "protected invocation audit fixture insertion",
        )?;
        kernel.recover().await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 DROP CONSTRAINT invocation_audit_events_identity_lengths,
                 DROP CONSTRAINT invocation_audit_events_outcome_check,
                 DROP CONSTRAINT invocation_audit_events_target_evidence_pair_check,
                 DROP CONSTRAINT invocation_audit_events_target_fk,
                 DROP CONSTRAINT invocation_audit_events_revision_pair_fk,
                 DROP CONSTRAINT invocation_audit_events_security_evidence_fk;",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET event_id = decode(repeat('b1', 15), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let malformed = recovery_error(&database).await?;
        require(
            matches!(
                malformed,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "invocation audit identity must be exactly sixteen bytes",
                    ..
                }
            ),
            "malformed invocation audit identity did not fail closed",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET event_id = decode(repeat('b1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET security_audit_event_id = decode(repeat('a2', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let unlinked = recovery_error(&database).await?;
        require(
            matches!(
                unlinked,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "linked security audit evidence is missing",
                    ..
                }
            ),
            "unlinked invocation audit decision did not fail closed",
        )?;
        let database_session = database.open().await?;
        let retained: bool = database_session
            .client()
            .query_one(
                "SELECT security_audit_event_id = decode(repeat('a2', 16), 'hex')
                 FROM _orna_kernel.invocation_audit_events
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained,
                "rejected invocation audit link tamper was repaired",
            ),
            database_session.shutdown().await,
            "invocation audit tamper retention check",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET security_audit_event_id = decode(repeat('a1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET outcome = 'denied'
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let wrong_outcome = recovery_error(&database).await?;
        require(
            matches!(
                wrong_outcome,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "linked security audit evidence does not match the invocation decision",
                    ..
                }
            ),
            "wrong invocation audit outcome did not fail closed",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET outcome = 'allowed'
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_batch(
            &database,
            "UPDATE _orna_kernel.security_audit_events
             SET source_revision_id = decode(repeat('f2', 16), 'hex')
             WHERE event_id = decode(repeat('a1', 16), 'hex');
             UPDATE _orna_kernel.invocation_audit_events
             SET source_revision_id = decode(repeat('f2', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
        )
        .await?;
        let invalid_revision = recovery_error(&database).await?;
        require(
            matches!(
                invalid_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "invalid invocation revision did not fail closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = decode('{}', 'hex')
                 WHERE event_id = decode(repeat('a1', 16), 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET source_revision_id = decode('{}', 'hex')
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
            ),
        )
        .await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT security_audit_events_invocation_evidence_key;
             UPDATE _orna_kernel.security_audit_events
             SET function_id = decode(repeat('f1', 16), 'hex')
             WHERE event_id = decode(repeat('a1', 16), 'hex');
             UPDATE _orna_kernel.invocation_audit_events
             SET function_id = decode(repeat('f1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
        )
        .await?;
        let invalid_target = recovery_error(&database).await?;
        require(
            matches!(
                invalid_target,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "invalid invocation target did not fail closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode('{}', 'hex')
                 WHERE event_id = decode(repeat('a1', 16), 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET function_id = decode('{}', 'hex')
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
            ),
        )
        .await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 ADD COLUMN request_payload bytea;",
        )
        .await?;
        let disclosure = recovery_error(&database).await?;
        require(
            matches!(
                disclosure,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "invocation audit relation has unsupported disclosure-bearing columns",
                    ..
                }
            ),
            "disclosure-bearing invocation audit column did not fail closed",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn local_peer_authentication_appends_one_protected_decision() -> TestResult<()> {
    const USER_UID: u32 = 1_001;
    const DISABLED_UID: u32 = 1_002;
    const UNKNOWN_UID: u32 = 1_003;
    const USER: PrincipalId = PrincipalId::from_bytes([0x61; 16]);
    const DISABLED: PrincipalId = PrincipalId::from_bytes([0x62; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let active = kernel.bootstrap().await?;
        let active_pair = RevisionPair::new(active.source(), active.catalogue());
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active_pair,
            vec![],
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(DISABLED, PrincipalKind::User, PrincipalStatus::Disabled),
            ],
            vec![],
            vec![],
            vec![
                LocalPeerCredential::new(USER_UID, USER),
                LocalPeerCredential::new(DISABLED_UID, DISABLED),
            ],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let authenticated = kernel.authenticate_local_peer(USER_UID).await?;
        require(
            authenticated.principal() == USER && authenticated.active_roles().is_empty(),
            "allowed local authentication changed its session",
        )?;
        let unknown = kernel
            .authenticate_local_peer(UNKNOWN_UID)
            .await
            .expect_err("unknown local peer must be denied");
        require(
            matches!(
                unknown,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "unknown local peer returned the wrong denial",
        )?;
        let disabled = kernel
            .authenticate_local_peer(DISABLED_UID)
            .await
            .expect_err("disabled mapped principal must be denied");
        require(
            matches!(
                disabled,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::InvalidPrincipal(
                        SessionBindingError::DisabledSessionPrincipal
                    )
                )
            ),
            "disabled mapped principal returned the wrong denial",
        )?;

        let revoked = SecuritySnapshot::new(
            active_pair,
            vec![],
            security.principals().collect(),
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let revoked_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("revoked local credential must be denied");
        require(
            matches!(
                revoked_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "revoked local credential returned the wrong denial",
        )?;

        let events = PostgresKernel::new(database.config()?)
            .recover_security_audit_events()
            .await?;
        require(
            events.len() == 4
                && events
                    .iter()
                    .enumerate()
                    .all(|(index, event)| event.sequence() == (index + 1) as i64)
                && events.iter().enumerate().all(|(index, event)| {
                    events[..index]
                        .iter()
                        .all(|earlier| earlier.id() != event.id())
                }),
            "authentication audit history changed its exact order or unique identities",
        )?;
        require_authentication_audit(&events[0], SecurityAuditOutcome::Allowed, Some(USER), None)?;
        require_authentication_audit(
            &events[1],
            SecurityAuditOutcome::Denied,
            None,
            Some(LocalPeerAuthenticationError::UnknownUid),
        )?;
        require_authentication_audit(
            &events[2],
            SecurityAuditOutcome::Denied,
            Some(DISABLED),
            Some(LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::DisabledSessionPrincipal,
            )),
        )?;
        require_authentication_audit(
            &events[3],
            SecurityAuditOutcome::Denied,
            None,
            Some(LocalPeerAuthenticationError::UnknownUid),
        )?;

        let session = database.open().await?;
        let constraint = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT security_audit_events_test_reject_insert
                 CHECK (false) NOT VALID;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            constraint,
            session.shutdown().await,
            "security audit insert failure fixture",
        )?;
        let audit_failure = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("audit insertion failure must fail authentication");
        require(
            matches!(audit_failure, PostgresKernelError::Database(_)),
            "audit insertion failure returned a normal authentication denial",
        )?;
        require(
            kernel.recover_security_audit_events().await? == events,
            "failed authentication audit insertion changed prior history",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

fn require_authentication_audit(
    event: &SecurityAuditEvent,
    outcome: SecurityAuditOutcome,
    principal: Option<PrincipalId>,
    denial: Option<LocalPeerAuthenticationError>,
) -> TestResult<()> {
    require(
        event.decision().kind() == SecurityAuditKind::Authentication
            && event.decision().outcome() == outcome
            && event.decision().session_principal() == principal
            && event.decision().effective_principal().is_none()
            && event.decision().authorising_principal().is_none()
            && event.decision().target().is_none()
            && event.decision().denial() == denial.map(SecurityAuditDenial::Authentication),
        "authentication audit record changed its closed decision evidence",
    )
}

pub(super) fn require_execute_audit(
    event: &SecurityAuditEvent,
    outcome: SecurityAuditOutcome,
    session: PrincipalId,
    effective: Option<PrincipalId>,
    authorising: Option<PrincipalId>,
    target: InvocationTarget,
    denial: Option<ExecuteDenial>,
) -> TestResult<()> {
    require(
        event.decision().kind() == SecurityAuditKind::Execute
            && event.decision().outcome() == outcome
            && event.decision().session_principal() == Some(session)
            && event.decision().effective_principal() == effective
            && event.decision().authorising_principal() == authorising
            && event.decision().target() == Some(target)
            && event.decision().denial() == denial.map(SecurityAuditDenial::Execute),
        "EXECUTE audit record changed its closed decision evidence",
    )
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_the_two_class_security_target_union_with_standard_targets() -> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let snapshot = kernel.recover_security_snapshot().await?;
        let system_anchor_count: i64 = {
            let session = database.open().await?;
            let operation: TestResult<i64> = async {
                Ok(session
                    .client()
                    .query_one(
                        "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                         WHERE catalogue_revision_id = $1 AND target_class = 'system'",
                        &[&fixture.active.pair().catalogue().to_bytes().to_vec()],
                    )
                    .await?
                    .get(0))
            }
            .await;
            finish_session(
                operation,
                session.shutdown().await,
                "system authority anchor count",
            )?
        };
        require(
            system_anchor_count == 3,
            "the valid fixture must retain all three system authority anchors",
        )?;
        let executable = &fixture.standard.executables()[0];
        let echo = executable.function();
        let echo_target = SecurityFunctionTarget::verified_standard(
            echo,
            fixture.standard.revision(),
            executable.revision().id(),
        );
        let mut targets = snapshot.function_targets().collect::<Vec<_>>();
        require(
            targets
                .iter()
                .filter(|target| {
                    target.class() == orna_core::security::TargetClass::VerifiedStandard
                })
                .count()
                == 1
                && targets.contains(&echo_target),
            "recovered security snapshot lost the verified standard target",
        )?;
        require(
            snapshot
                .function_targets()
                .any(|target| target.function() == fixture.app_function),
            "recovered security snapshot lost the application target",
        )?;
        targets.sort_unstable();
        let mut expected = vec![
            echo_target,
            SecurityFunctionTarget::application(fixture.app_function),
        ];
        expected.sort_unstable();
        require(
            targets == expected,
            "recovered security snapshot returned the wrong two-class target union",
        )?;
        require(
            snapshot
                .functions()
                .eq(expected.iter().map(|target| target.function())),
            "recovered security snapshot changed the canonical identity order",
        )?;

        // An EXECUTE grant on the standard target authorises only through the
        // protected boundary with the exact immutable pins.
        let granted = SecuritySnapshot::new_with_function_targets(
            fixture.active.pair(),
            snapshot.function_targets().collect(),
            vec![Principal::new(
                USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(USER, echo)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let recovered = kernel.recover_security_snapshot().await?;
        let session = recovered.bind_authenticated_session(USER, vec![])?;
        let protected = InvocationTarget::verified_standard(
            echo,
            fixture.active.pair(),
            fixture.standard.revision(),
            executable.revision().id(),
        );
        require(
            matches!(
                recovered.authorise_execute(&session, protected),
                ExecuteDecision::Allowed(evidence)
                    if evidence.authorising_principal() == USER
            ),
            "the protected standard target was not authorised by its exact grant",
        )?;

        // The ordinary raw dispatcher stays closed to the standard target even
        // when its grant exists for the protected gateway.
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, echo)
            .await
            .expect_err("raw dispatch of a standard target must deny");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::UnknownFunction,
                } if pair == fixture.active.pair() && function == echo
            ),
            "raw dispatch of a standard target returned the wrong denial",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        let target = InvocationTarget::new(echo, fixture.active.pair());
        require(
            events.len() == 1,
            "raw standard target denial did not record exactly one EXECUTE decision",
        )?;
        require_execute_audit(
            &events[0],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            target,
            Some(ExecuteDenial::UnknownFunction),
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_a_missing_application_authority_target_without_changing_active_state()
-> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x35; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let active_before = active_revision_pair(&database).await?;
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES (decode('{}', 'hex'), 'user', 'active');
                 INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'));",
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(fixture.app_function.to_bytes()),
            ),
        )
        .await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the intact two-class fixture must recover its security snapshot",
        )?;

        run_single_row_statement(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.app_function.to_bytes()),
            ),
        )
        .await?;

        let error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a missing application authority target must fail recovery");
        require(
            matches!(error, PostgresKernelError::SecurityFunctionSetMismatch),
            "missing application authority target returned the wrong typed error",
        )?;
        require(
            active_revision_pair(&database).await? == active_before,
            "rejected application authority tamper changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_an_extra_foreign_application_authority_target_without_changing_active_state()
-> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let active_before = active_revision_pair(&database).await?;
        let foreign_function = FunctionId::from_bytes([0x91; 16]);
        let foreign_revision = FunctionRevisionId::from_bytes([0x92; 16]);
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'), 'application',
                         decode('{}', 'hex'), NULL);",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(foreign_function.to_bytes()),
                raw_id_hex(foreign_revision.to_bytes()),
            ),
        )
        .await?;

        let error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a foreign application authority target must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "application invocation targets must resolve in the pinned application catalogue",
                    ..
                }
            ),
            "foreign application authority target returned the wrong durable invariant",
        )?;
        require(
            active_revision_pair(&database).await? == active_before,
            "rejected foreign application authority tamper changed the active revision pair",
        )?;
        let retained: i64 = {
            let session = database.open().await?;
            let operation: TestResult<i64> = async {
                Ok(session
                    .client()
                    .query_one(
                        "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                         WHERE catalogue_revision_id = $1 AND function_id = $2",
                        &[
                            &fixture.active.pair().catalogue().to_bytes().to_vec(),
                            &foreign_function.to_bytes().to_vec(),
                        ],
                    )
                    .await?
                    .get(0))
            }
            .await;
            finish_session(
                operation,
                session.shutdown().await,
                "foreign application authority retention check",
            )?
        };
        require(
            retained == 1,
            "recovery repaired the foreign application authority target",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_an_application_authority_target_with_a_standard_pin() -> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let active_before = active_revision_pair(&database).await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the intact two-class fixture must recover its security snapshot",
        )?;

        // Remove the database shape and pin constraints so a foreign
        // standard revision can be attached to an application authority row.
        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_target_authorities
                     DROP CONSTRAINT invocation_target_authorities_class_shape_check,
                     DROP CONSTRAINT invocation_target_authorities_standard_pin_fk;
                 UPDATE _orna_kernel.invocation_target_authorities
                 SET standard_library_revision_id = decode(repeat('66', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.app_function.to_bytes()),
            ),
        )
        .await?;

        let error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("an application authority target with a standard pin must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "application invocation targets must not pin a standard library revision",
                    ..
                }
            ),
            "application authority standard-pin tamper returned the wrong durable invariant",
        )?;
        require(
            active_revision_pair(&database).await? == active_before,
            "rejected application authority standard-pin tamper changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_a_standard_authority_target_absent_from_the_pinned_snapshot()
-> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let catalogue = fixture.active.pair().catalogue().to_bytes().to_vec();
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the intact two-class fixture must recover its security snapshot",
        )?;

        // A standard authority row whose function revision is absent from the
        // exact pinned standard executable fails recovery closed.
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode(repeat('77', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(
                    fixture.active.pair().catalogue().to_bytes(),
                ),
                raw_id_hex(
                    fixture.standard.executables()[0].function().to_bytes(),
                ),
            ),
        )
        .await?;
        let wrong_revision = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a standard target with the wrong executable revision must fail");
        require(
            matches!(
                wrong_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "standard invocation target must resolve exactly once in the pinned verified standard snapshot",
                    ..
                }
            ),
            "wrong standard executable revision returned the wrong durable invariant",
        )?;
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;

        // A missing standard authority row fails recovery without repair.
        run_single_row_statement(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        let missing = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a missing standard authority target must fail recovery");
        require(
            matches!(
                missing,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "standard invocation targets must exactly match the pinned verified standard executables",
                    ..
                }
            ),
            "missing standard authority target returned the wrong durable invariant",
        )?;
        let retained: i64 = {
            let session = database.open().await?;
            let count = session
                .client()
                .query_one(
                    "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                     WHERE catalogue_revision_id = $1 AND target_class = 'standard'",
                    &[&catalogue],
                )
                .await?
                .get(0);
            finish_session(
                Ok(count),
                session.shutdown().await,
                "standard authority retention check",
            )?
        };
        require(
            retained == 0,
            "rejected standard authority tamper was repaired",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_an_application_standard_duplicate_target() -> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let catalogue = fixture.active.pair().catalogue().to_bytes().to_vec();

        // A standard authority row that is re-classified as an application row
        // no longer resolves in the pinned application catalogue: the same
        // function identity cannot belong to both classes.
        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_target_authorities
                     DROP CONSTRAINT invocation_target_authorities_target_class_check,
                     DROP CONSTRAINT invocation_target_authorities_class_shape_check;
                 UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'application', standard_library_revision_id = NULL
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        let ambiguous = kernel
            .recover_security_snapshot()
            .await
            .expect_err("an application-class authority row without a catalogue function must fail");
        require(
            matches!(
                ambiguous,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "application invocation targets must resolve in the pinned application catalogue",
                    ..
                }
            ),
            "ambiguous application authority row returned the wrong durable invariant",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'standard', standard_library_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');
                 ALTER TABLE _orna_kernel.invocation_target_authorities
                     ADD CONSTRAINT invocation_target_authorities_target_class_check
                     CHECK (target_class IN ('application', 'standard', 'system')),
                     ADD CONSTRAINT invocation_target_authorities_class_shape_check
                     CHECK (
                        (target_class = 'application' AND standard_library_revision_id IS NULL)
                        OR (target_class = 'standard' AND standard_library_revision_id IS NOT NULL)
                        OR (target_class = 'system' AND standard_library_revision_id IS NULL)
                     );",
                raw_id_hex(fixture.standard.revision().to_bytes()),
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "restored authority rows did not recover the two-class union",
        )?;

        // The same function identity present in both the application catalogue
        // and the standard authority rows is an application-and-standard
        // duplicate. The duplicate changes the catalogue itself, so full
        // recovery fails closed without writing or repairing any row.
        let session = database.open().await?;
        let schema = session
            .client()
            .query_one(
                "SELECT schema_id FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $1 LIMIT 1",
                &[&catalogue],
            )
            .await?
            .get::<_, Vec<u8>>(0);
        let duplicate: TestResult<()> = async {
            let function_id = fixture.standard.executables()[0].function().to_bytes().to_vec();
            let revision = fixture.standard.executables()[0].revision().id().to_bytes().to_vec();
            let content_hash = vec![0x77_u8; 32];
            let unit = vec![0xa1_u8; 16];
            session
                .client()
                .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.function_revisions
                        (id, introduced_catalogue_revision_id, function_id,
                         revision_number, content_hash, semantic_ir_hash,
                         language_version, status)
                     VALUES ($1, $2, $3, 1, $4, $4, 'orna.language/1', 'active')",
                    &[&revision, &catalogue, &function_id, &content_hash],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.function_artifacts
                        (function_revision_id, artifact_kind, format,
                         format_version, payload, content_hash)
                     VALUES ($1, 'server_plan', 'orna.server-parameter-echo', 1,
                             decode('4f524e4150450000000000000001000000000000000000000000000000000000000000000000000000000000000000', 'hex'), $2)",
                    &[&revision, &content_hash],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_functions
                        (catalogue_revision_id, function_id, schema_id, name_parts,
                         domain, security_mode, transaction_mode, volatility,
                         return_shape, return_type_kind, return_scalar_type,
                         current_function_revision_id, source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, ARRAY['std', 'invoke', 'echo'], 'server',
                             'invoker', 'read_only', 'stable', 'single',
                             'scalar', 'integer', $4, $5, 0, 1)",
                    &[&catalogue, &function_id, &schema, &revision, &unit],
                )
                .await?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            duplicate,
            session.shutdown().await,
            "application and standard duplicate fixture",
        )?;
        let error = kernel
            .recover()
            .await
            .expect_err("an application and standard duplicate must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant { .. }
                    | PostgresKernelError::RevisionInvariant(_)
                    | PostgresKernelError::CatalogueSnapshot(_)
            ),
            "application and standard duplicate returned the wrong fail-closed error",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_a_grant_naming_a_removed_standard_function() -> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let echo = fixture.standard.executables()[0].function();

        // The grant on the standard target is valid while the target exists.
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES (decode('{}', 'hex'), 'user', 'active');
                 INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'));",
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the granted standard target must recover before the upgrade",
        )?;

        // A later standard upgrade removes the granted function from the
        // target union. Recovery must fail closed and must not drop, translate,
        // or keep the unknown grant.
        install_later_standard_upgrade_without_echo(&database, &fixture).await?;
        let error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a grant naming a removed standard function must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownGrantFunction)
            ),
            "removed standard function grant returned the wrong fail-closed error",
        )?;
        let retained: bool = {
            let session = database.open().await?;
            let exists = session
                .client()
                .query_one(
                    "SELECT count(*) > 0 FROM _orna_kernel.security_execute_grants
                     WHERE grantee_id = $1 AND function_id = $2",
                    &[&USER.to_bytes().to_vec(), &echo.to_bytes().to_vec()],
                )
                .await?
                .get(0);
            finish_session(
                Ok(exists),
                session.shutdown().await,
                "removed standard function grant retention check",
            )?
        };
        require(
            retained,
            "recovery repaired the grant naming the removed standard function",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_invocation_audit_standard_targets_through_the_historical_pin() -> TestResult<()> {
    const SESSION: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let echo = fixture.standard.executables()[0].function();
        let pair = fixture.active.pair();
        {
            let database_session = database.open().await?;
            let insertion = database_session
                .client()
                .batch_execute(&format!(
                    "INSERT INTO _orna_kernel.security_audit_events
                         (event_id, event_kind, outcome, session_principal_id,
                          effective_principal_id, authorising_principal_id, function_id,
                          source_revision_id, catalogue_revision_id)
                     VALUES (decode(repeat('a1', 16), 'hex'), 'execute', 'allowed',
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'));
                     INSERT INTO _orna_kernel.invocation_audit_events
                         (event_id, invocation_id, outcome, session_principal_id,
                          effective_principal_id, authorising_principal_id, function_id,
                          source_revision_id, catalogue_revision_id, security_audit_event_id)
                     VALUES (decode(repeat('b1', 16), 'hex'), decode(repeat('c1', 16), 'hex'),
                             'allowed', decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode(repeat('a1', 16), 'hex'));",
                    raw_id_hex(SESSION.to_bytes()),
                    raw_id_hex(EFFECTIVE.to_bytes()),
                    raw_id_hex(AUTHORISING.to_bytes()),
                    raw_id_hex(echo.to_bytes()),
                    raw_id_hex(pair.source().to_bytes()),
                    raw_id_hex(pair.catalogue().to_bytes()),
                    raw_id_hex(SESSION.to_bytes()),
                    raw_id_hex(EFFECTIVE.to_bytes()),
                    raw_id_hex(AUTHORISING.to_bytes()),
                    raw_id_hex(echo.to_bytes()),
                    raw_id_hex(pair.source().to_bytes()),
                    raw_id_hex(pair.catalogue().to_bytes()),
                ))
                .await
                .map_err(Into::into);
            finish_session(
                insertion,
                database_session.shutdown().await,
                "standard invocation audit fixture insertion",
            )?;
        }
        kernel.recover().await?;

        // The application RevisionPair in the audit row is the durable pin:
        // the standard target must resolve through the authority relation and
        // the historical catalogue revision's exact verified standard snapshot.
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode(repeat('77', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let wrong_executable = recovery_error(&database).await?;
        require(
            matches!(
                wrong_executable,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "wrong standard executable revision did not fail audit recovery closed",
        )?;
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;

        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_audit_events
                     DROP CONSTRAINT invocation_audit_events_target_fk;
                 DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let absent = recovery_error(&database).await?;
        require(
            matches!(
                absent,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "absent standard authority target did not fail audit recovery closed",
        )?;
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'), 'standard',
                         decode('{}', 'hex'), decode('{}', 'hex'));
                 ALTER TABLE _orna_kernel.invocation_audit_events
                     ADD CONSTRAINT invocation_audit_events_target_fk
                     FOREIGN KEY (catalogue_revision_id, function_id)
                     REFERENCES _orna_kernel.invocation_target_authorities(
                         catalogue_revision_id,
                         function_id
                     );",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(fixture.standard.revision().to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;

        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_target_authorities
                     DROP CONSTRAINT invocation_target_authorities_standard_pin_fk;
                 UPDATE _orna_kernel.invocation_target_authorities
                 SET standard_library_revision_id = decode(repeat('66', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let wrong_pin = recovery_error(&database).await?;
        require(
            matches!(
                wrong_pin,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "wrong standard revision pin did not fail audit recovery closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET standard_library_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');
                 ALTER TABLE _orna_kernel.invocation_target_authorities
                     ADD CONSTRAINT invocation_target_authorities_standard_pin_fk
                     FOREIGN KEY (catalogue_revision_id, standard_library_revision_id)
                     REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id);",
                raw_id_hex(fixture.standard.revision().to_bytes()),
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;
        require_no_session_leaks(&database).await
    })
    .await
}
