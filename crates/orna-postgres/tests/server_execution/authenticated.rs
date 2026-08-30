use super::*;

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_server_select_commits_allowed_and_denied_execute_decisions() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("authenticated-server-select-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "authenticated SERVER SELECT live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(
                authenticated_server_select_commits_allowed_and_denied_execute_decisions_inner(),
            )
        })
        .map_err(|error| {
            failure(format!(
                "authenticated SERVER SELECT live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("authenticated SERVER SELECT live thread panicked")),
    }
}

#[cfg(feature = "test-hooks")]
async fn authenticated_server_select_commits_allowed_and_denied_execute_decisions_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA enum_exec;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_execution_candidate(ENUM_EXECUTION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;
        let enum_type = applied
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no enum"))?;
        let object = applied
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no object"))?;
        let enum_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("authenticated enum object has no field"))?;
        let function = applied
            .catalogue()
            .functions()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no function"))?;
        let principal = PrincipalId::from_bytes([0xa7; 16]);
        let function_ids = applied
            .catalogue()
            .functions()
            .iter()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        let allowed = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(principal, function.id())],
        )?;
        let allowed = kernel.replace_security_snapshot(&allowed).await?;
        let allowed_session = allowed.bind_authenticated_session(principal, vec![])?;
        let object_id = ObjectId::from_bytes([0xe2; 16]);
        let session = database.open().await?;
        let insert = format!(
            "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
            relation(object.id()),
            field(enum_field.id()),
        );
        session
            .client()
            .execute(&insert, &[&object_id.to_bytes().to_vec(), &"customer"])
            .await?;
        session.shutdown().await?;

        let result = kernel
            .execute_authenticated_server_select(&allowed_session, function.id(), &[])
            .await?;
        let [RuntimeValue::Enum(value)] = result.rows().rows()[0].values() else {
            return Err(failure(
                "authenticated SERVER SELECT did not return one enum value",
            ));
        };
        require(
            result.pair() == applied.pair()
                && result.function() == function.id()
                && result.function_revision() == function.current_revision()
                && value.enum_type() == enum_type.id()
                && value.label() == "customer",
            "authenticated SERVER SELECT changed its pinned result",
        )?;

        let unexpected_parameter = ParameterId::from_bytes([0xa8; 16]);
        let invalid_arguments = vec![FunctionArgument::new(
            unexpected_parameter,
            RuntimeValue::Boolean(true),
        )?];
        let error = kernel
            .execute_authenticated_server_select(
                &allowed_session,
                function.id(),
                &invalid_arguments,
            )
            .await
            .expect_err("allowed invalid arguments must fail after audit");
        require_select_argument_error(
            &error,
            applied.pair(),
            function.id(),
            function.current_revision(),
            None,
            "this function does not accept arguments",
        )?;

        let role = PrincipalId::from_bytes([0xa9; 16]);
        let selected = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        let selected = kernel.replace_security_snapshot(&selected).await?;
        let selected_session = selected.bind_authenticated_session(principal, vec![role])?;
        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = selected_session.clone();
        let execution_function = function.id();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let mut execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_authenticated_server_select_with_test_barrier(
                    &execution_session,
                    execution_function,
                    &[],
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        if tokio::time::timeout(WAIT, reached.wait()).await.is_err() {
            execution.abort_and_wait().await;
            return Err(failure(
                "authenticated SERVER SELECT did not pin its security snapshot",
            ));
        }
        let revoked = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        if tokio::time::timeout(WAIT, resume.wait()).await.is_err() {
            execution.abort_and_wait().await;
            return Err(failure(
                "authenticated SERVER SELECT did not resume after security replacement",
            ));
        }
        execution
            .finish("authenticated SERVER SELECT snapshot proof")
            .await?;
        kernel.replace_security_snapshot(&selected).await?;

        let unselected_session = selected.bind_authenticated_session(principal, vec![])?;
        let error = kernel
            .execute_authenticated_server_select(&unselected_session, function.id(), &[])
            .await
            .expect_err("unselected role must not authorise SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::MissingExecuteGrant,
        )?;

        let unknown_function = FunctionId::from_bytes([0xaa; 16]);
        let error = kernel
            .execute_authenticated_server_select(&selected_session, unknown_function, &[])
            .await
            .expect_err("unknown function must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            unknown_function,
            ExecuteDenial::UnknownFunction,
        )?;

        let stale = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        kernel.replace_security_snapshot(&stale).await?;
        let error = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await
            .expect_err("stale active-role selection must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        let disabled = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Disabled),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        kernel.replace_security_snapshot(&disabled).await?;
        let error = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await
            .expect_err("disabled session must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        let replacement_principal = PrincipalId::from_bytes([0xab; 16]);
        let unknown = SecuritySnapshot::new(
            applied.pair(),
            function_ids,
            vec![Principal::new(
                replacement_principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&unknown).await?;
        let error = kernel
            .execute_authenticated_server_select(&allowed_session, function.id(), &[])
            .await
            .expect_err("unknown session must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        kernel.replace_security_snapshot(&selected).await?;
        let error = kernel
            .execute_authenticated_server_select_with_forced_post_commit_driver_shutdown(
                &selected_session,
                function.id(),
                &[],
            )
            .await
            .expect_err("post-commit driver shutdown must fail SERVER SELECT");
        require(
            matches!(error, PostgresKernelError::DriverTask(_)),
            "authenticated SERVER SELECT hid its post-commit driver failure",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let execute = audits
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 9,
            "authenticated SERVER audit count differs",
        )?;
        let target = InvocationTarget::new(function.id(), applied.pair());
        require_server_execute_audit(
            execute[0],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(principal),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[1],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(principal),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[2],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(role),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[3],
            SecurityAuditOutcome::Denied,
            principal,
            None,
            None,
            target,
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        require_server_execute_audit(
            execute[4],
            SecurityAuditOutcome::Denied,
            principal,
            None,
            None,
            InvocationTarget::new(unknown_function, applied.pair()),
            Some(ExecuteDenial::UnknownFunction),
        )?;
        for event in &execute[5..8] {
            require_server_execute_audit(
                event,
                SecurityAuditOutcome::Denied,
                principal,
                None,
                None,
                target,
                Some(ExecuteDenial::InvalidSession),
            )?;
        }
        require_server_execute_audit(
            execute[8],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(role),
            target,
            None,
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT reject_authenticated_server_audit
                 CHECK (false) NOT VALID",
            )
            .await?;
        session.shutdown().await?;
        let audit_failure = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await;
        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT reject_authenticated_server_audit",
            )
            .await?;
        session.shutdown().await?;
        let error = audit_failure.expect_err("rejected audit insert must fail SERVER SELECT");
        require(
            matches!(
                error,
                PostgresKernelError::Database(ref source)
                    if source.as_db_error().and_then(|error| error.constraint())
                        == Some("reject_authenticated_server_audit")
            ),
            "authenticated SERVER SELECT hid its audit insert failure",
        )?;
        let after_failure = kernel.recover_security_audit_events().await?;
        require(
            after_failure
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
                .count()
                == 9,
            "failed authenticated SERVER audit inserted a decision",
        )?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_unique_text_selected_select_requires_version_four_dispatch()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let scalar_source = unique_text_select_source("raw_unique_text_v1");
        let version_one = kernel.apply(&candidate(&scalar_source, &empty)?).await?;
        let scalar_fixture =
            UniqueTextSelectFixture::from_active(&version_one, "raw_unique_text_v1")?;
        require_unique_text_select_type_authority(
            &version_one,
            scalar_fixture,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            "version-one scalar Text",
        )?;
        insert_unique_text_select_rows(&database, scalar_fixture).await?;
        for (function, parameter, value, name) in [
            (
                scalar_fixture.by_email,
                scalar_fixture.by_email_parameter,
                "café",
                "nullable unique scalar Text selector",
            ),
            (
                scalar_fixture.by_required_email,
                scalar_fixture.by_required_email_parameter,
                "required@example.test",
                "required unique scalar Text selector",
            ),
        ] {
            let selected = kernel
                .execute_server_select_with_arguments(
                    function,
                    &unique_text_select_argument(parameter, value)?,
                )
                .await?;
            require_unique_text_select_result(&selected, scalar_fixture, name)?;
        }
        require_no_session_leaks(&database).await
    })
    .await?;

    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA raw_unique_text;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let value_source = unique_text_select_source("raw_unique_text");
        let applied = kernel
            .apply(&standard_execution_candidate(
                &value_source,
                &version_two,
                &upgrade,
            )?)
            .await?;
        let fixture = UniqueTextSelectFixture::from_active(&applied, "raw_unique_text")?;
        require_unique_text_select_type_authority(
            &applied,
            fixture,
            ResolvedType::Value(CHARACTER_LARGE_OBJECT_TYPE_ID),
            "version-two standard Text value",
        )?;
        insert_unique_text_select_rows(&database, fixture).await?;
        kernel
            .install_catalogue_health_service(RAW_UNIQUE_TEXT_SERVICE_UID)
            .await?;
        let session = kernel
            .authenticate_local_peer(RAW_UNIQUE_TEXT_SERVICE_UID)
            .await?;

        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.by_email,
                &[FunctionArgument::new(
                    ParameterId::from_bytes([0xa1; 16]),
                    RuntimeValue::Text("redacted@example.test".into()),
                )?],
            )
            .await
            .expect_err("raw unique Text SELECT must deny before target inspection");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == applied.pair() && function == fixture.by_email
            ),
            "raw unique Text SELECT did not deny before target inspection",
        )?;

        kernel
            .grant_catalogue_health_service_execute(applied.pair(), fixture.by_email)
            .await?;
        kernel
            .grant_catalogue_health_service_execute(applied.pair(), fixture.by_required_email)
            .await?;
        for (function, parameter, value, name) in [
            (
                fixture.by_email,
                fixture.by_email_parameter,
                "café",
                "nullable unique Text selector",
            ),
            (
                fixture.by_required_email,
                fixture.by_required_email_parameter,
                "required@example.test",
                "required unique Text selector",
            ),
        ] {
            let selected = kernel
                .dispatch_authenticated_raw_call_with_arguments(
                    &session,
                    function,
                    &unique_text_select_argument(parameter, value)?,
                )
                .await?;
            require_raw_unique_text_select_result(selected, fixture, name)?;
        }

        for (value, name) in [
            ("CAFÉ", "case-distinct Text"),
            ("café ", "whitespace-distinct Text"),
            ("line\r\nending@example.test", "CRLF-distinct Text"),
            ("cafe\u{301}", "C-byte-distinct Text"),
            ("absent@example.test", "absent Text"),
            ("nullable@example.test", "nullable stored Text"),
            ("", "empty Text"),
        ] {
            let result = kernel
                .dispatch_authenticated_raw_call_with_arguments(
                    &session,
                    fixture.by_email,
                    &unique_text_select_argument(fixture.by_email_parameter, value)?,
                )
                .await?;
            require(
                result == AuthenticatedRawCallResult::Server(vec![]),
                format!("{name} did not complete without projected values"),
            )?;
        }
        let required_absent = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.by_required_email,
                &unique_text_select_argument(
                    fixture.by_required_email_parameter,
                    "absent-required@example.test",
                )?,
            )
            .await?;
        require(
            required_absent == AuthenticatedRawCallResult::Server(vec![]),
            "absent required unique Text did not complete without projected values",
        )?;

        let wrong_parameter = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.by_email,
                &[FunctionArgument::new(
                    ParameterId::from_bytes([0xa2; 16]),
                    RuntimeValue::Text("café".into()),
                )?],
            )
            .await
            .expect_err("an allowed wrong ParameterId must be target-unavailable");
        require_raw_target_unavailable(&wrong_parameter, fixture.by_email, "wrong ParameterId")?;

        let mistyped = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.by_email,
                &[FunctionArgument::new(
                    fixture.by_email_parameter,
                    RuntimeValue::Integer(42),
                )?],
            )
            .await
            .expect_err("an allowed mistyped Text parameter must be target-unavailable");
        require_raw_target_unavailable(&mistyped, fixture.by_email, "mistyped Text parameter")?;

        kernel
            .grant_catalogue_health_service_execute(applied.pair(), fixture.all_people)
            .await?;
        let non_version_four = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.all_people,
                &[FunctionArgument::new(
                    ParameterId::from_bytes([0xa3; 16]),
                    RuntimeValue::Text("not accepted by the v1 target".into()),
                )?],
            )
            .await
            .expect_err("an allowed one-argument non-version-4 target must be target-unavailable");
        require_raw_target_unavailable(
            &non_version_four,
            fixture.all_people,
            "non-version-4 target",
        )?;

        let execute = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 14,
            "raw unique Text SELECT audit count differs",
        )?;
        require_server_execute_audit(
            &execute[0],
            SecurityAuditOutcome::Denied,
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            None,
            None,
            InvocationTarget::new(fixture.by_email, applied.pair()),
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        let expected_allowed_targets = [
            fixture.by_email,
            fixture.by_required_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_email,
            fixture.by_required_email,
            fixture.by_email,
            fixture.by_email,
            fixture.all_people,
        ];
        for (event, function) in execute[1..].iter().zip(expected_allowed_targets) {
            require_server_execute_audit(
                event,
                SecurityAuditOutcome::Allowed,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID),
                Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID),
                InvocationTarget::new(function, applied.pair()),
                None,
            )?;
        }
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_identity_selected_select_binds_reference_and_commits_audits()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA raw_identity;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_execution_candidate(
                EXECUTION_SOURCE,
                &version_two,
                &upgrade,
            )?)
            .await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        kernel
            .install_catalogue_health_service(RAW_IDENTITY_SERVICE_UID)
            .await?;
        let session = kernel
            .authenticate_local_peer(RAW_IDENTITY_SERVICE_UID)
            .await?;
        let root_argument = selector_argument(fixture, fixture.root)?;

        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.select_node,
                &root_argument,
            )
            .await
            .expect_err("raw identity SELECT must deny before target inspection");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == applied.pair() && function == fixture.select_node
            ),
            "raw identity SELECT did not deny before target inspection",
        )?;

        kernel
            .grant_catalogue_health_service_execute(applied.pair(), fixture.select_node)
            .await?;
        let selected = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.select_node,
                &root_argument,
            )
            .await?;
        require(
            selected
                == AuthenticatedRawCallResult::Server(vec![
                    RuntimeValue::Reference {
                        target: fixture.node,
                        object: fixture.root,
                    },
                    RuntimeValue::Integer(20),
                    RuntimeValue::Text(String::from("other")),
                    RuntimeValue::Boolean(false),
                ]),
            "raw identity SELECT did not return the exact ordered projected values",
        )?;

        let selected_null = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.select_node,
                &selector_argument(fixture, fixture.other)?,
            )
            .await?;
        require(
            matches!(
                selected_null,
                AuthenticatedRawCallResult::Server(values)
                    if matches!(
                        values.as_slice(),
                        [
                            RuntimeValue::Reference { target, object },
                            RuntimeValue::Integer(10),
                            RuntimeValue::Null(child_label),
                            RuntimeValue::Null(same_as_child),
                        ]
                            if *target == fixture.node
                                && *object == fixture.other
                                && child_label.resolved_type()
                                    == ResolvedType::scalar(StandardScalar::CharacterLargeObject)
                                && same_as_child.resolved_type()
                                    == ResolvedType::scalar(StandardScalar::Boolean)
                    )
            ),
            "raw identity SELECT did not normalise the nullable projections in order",
        )?;

        let absent = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.select_node,
                &selector_argument(fixture, ObjectId::from_bytes([0x91; 16]))?,
            )
            .await?;
        require(
            absent == AuthenticatedRawCallResult::Server(vec![]),
            "an absent same-type raw identity SELECT must complete without values",
        )?;

        let unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.select_node,
                &[FunctionArgument::new(
                    ParameterId::from_bytes([0x92; 16]),
                    RuntimeValue::Reference {
                        target: fixture.node,
                        object: fixture.root,
                    },
                )?],
            )
            .await
            .expect_err("an allowed wrong ParameterId must be target-unavailable");
        require(
            matches!(
                unavailable,
                PostgresKernelError::RawCallTargetUnavailable { function, .. }
                    if function == fixture.select_node
            ),
            "an allowed wrong ParameterId did not close as target-unavailable",
        )?;

        let execute = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 5,
            "raw identity SELECT audit count differs",
        )?;
        let target = InvocationTarget::new(fixture.select_node, applied.pair());
        require_server_execute_audit(
            &execute[0],
            SecurityAuditOutcome::Denied,
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            None,
            None,
            target,
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        for event in &execute[1..] {
            require_server_execute_audit(
                event,
                SecurityAuditOutcome::Allowed,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID),
                Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID),
                target,
                None,
            )?;
        }
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
fn require_server_execute_denial(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    reason: ExecuteDenial,
) -> TestResult<()> {
    require(
        matches!(
            error,
            PostgresKernelError::ServerExecuteDenied {
                pair: denied_pair,
                function: denied_function,
                reason: denied_reason,
            } if *denied_pair == pair && *denied_function == function && *denied_reason == reason
        ),
        "authenticated SERVER denial changed its exact context",
    )
}

#[cfg(feature = "test-hooks")]
fn require_server_execute_audit(
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
        "authenticated SERVER audit decision changed its closed evidence",
    )
}
