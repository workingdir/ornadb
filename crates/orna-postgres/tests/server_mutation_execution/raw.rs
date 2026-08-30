use super::*;

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_insert_is_denied_then_granted_and_audited() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-insert-live",
        authenticated_raw_insert_is_denied_then_granted_and_audited_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_insert_is_denied_then_granted_and_audited_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_insert_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_insert_test", "create_probe"])?;
        let read_probes = raw_function_id(&applied, &["raw_insert_test", "read_probes"])?;
        let create_named = raw_function_id(&applied, &["raw_insert_test", "create_named"])?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // The parameter-free raw INSERT is denied before its explicit grant.
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, create_probe)
            .await
            .expect_err("raw INSERT before its grant must be denied");
        require(
            matches!(denied, PostgresKernelError::RawExecuteDenied { .. }),
            "pre-grant raw INSERT returned the wrong typed error",
        )?;

        // Grant the raw SELECT only and prove the denied INSERT created nothing.
        kernel
            .grant_catalogue_health_service_execute(pair, read_probes)
            .await?;
        let empty_select = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                empty_select,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the denied INSERT must not create any object",
        )?;

        // Grant the parameter-free raw INSERT only and invoke it.
        kernel
            .grant_catalogue_health_service_execute(pair, create_probe)
            .await?;
        let inserted = kernel
            .dispatch_authenticated_raw_call(&session, create_probe)
            .await?;
        let inserted = match inserted {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference { target, object } = &inserted[0] else {
            return Err(failure("raw INSERT must return an object reference"));
        };
        require(
            *target == probe && *object != ObjectId::from_bytes([0; 16]),
            "raw INSERT reference must name the probe type and a real row",
        )?;

        // The raw SELECT now proves exactly one object exists.
        let one_probe = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                one_probe,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "raw SELECT must return exactly the stored Boolean true value",
        )?;

        // An allowed-but-invalid raw mutation target closes as an unavailable
        // authorised raw call target under ADR 0040.
        kernel
            .grant_catalogue_health_service_execute(pair, create_named)
            .await?;
        let unavailable = kernel
            .dispatch_authenticated_raw_call(&session, create_named)
            .await
            .expect_err("a parameterised raw INSERT target must be unavailable");
        require(
            matches!(
                unavailable,
                PostgresKernelError::RawCallTargetUnavailable { .. }
            ),
            "an invalid raw mutation target returned the wrong typed error",
        )?;
        let unchanged = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                unchanged,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "an invalid raw mutation target must not change any row",
        )?;

        // One authentication audit, then one audit per dispatch: the pre-grant
        // call was denied, every later call allowed.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 7, "raw dispatch audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[2..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                }),
            "raw dispatch audit kinds and outcomes differ",
        )?;

        // ADR 0040: a parameterised Boolean SERVER INSERT target stores its
        // Boolean argument. Its exact ParameterId comes from the compiled
        // active catalogue, never from source text or a fixed identity.
        let create_flagged = raw_function_id(&applied, &["raw_insert_test", "create_flagged"])?;
        let create_flagged_definition = applied
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?;
        let stored_parameter = create_flagged_definition
            .parameter_by_name("p_stored")
            .ok_or_else(|| failure("create_flagged.p_stored is absent from the active catalogue"))?
            .id();
        let create_flagged_revision = create_flagged_definition.current_revision();
        let mut wrong_parameter_bytes = stored_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != stored_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let wrong_parameter_argument =
            FunctionArgument::new(wrong_parameter, RuntimeValue::Boolean(true))?;
        let client_boolean = raw_function_id(&applied, &["raw_insert_test", "client_boolean"])?;

        // Authorisation wins over argument validation: before its grant, even a
        // wrong-parameter Boolean call is denied and creates no row.
        let denied_with_argument = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                std::slice::from_ref(&wrong_parameter_argument),
            )
            .await
            .expect_err("an ungranted raw INSERT must be denied before argument validation");
        require(
            matches!(
                denied_with_argument,
                PostgresKernelError::RawExecuteDenied { .. }
            ),
            "pre-grant raw INSERT with arguments returned the wrong typed error",
        )?;
        let after_denied = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_denied,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the denied raw INSERT with arguments must not create any row",
        )?;

        // After the grant, the wrong ParameterId fails as a generic unavailable
        // raw target, rolls back its savepoint, and keeps the allowed audit.
        kernel
            .grant_catalogue_health_service_execute(pair, create_flagged)
            .await?;
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                std::slice::from_ref(&wrong_parameter_argument),
            )
            .await
            .expect_err("a wrong parameter id must make the raw INSERT target unavailable");
        require(
            matches!(
                wrong_parameter_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { .. }
            ),
            "a wrong parameter id returned the wrong typed error",
        )?;
        let after_wrong_parameter = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_wrong_parameter,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "a wrong parameter id must not create any row",
        )?;

        // A real PostgreSQL row write then fails through an AFTER INSERT
        // trigger. The raw dispatch pauses after recovery while the harness
        // installs the trigger, then resumes and fails the write. The typed
        // ServerInsert database failure must survive the raw dispatch
        // unchanged, and the tentative row must roll back.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let triggered_arguments = vec![FunctionArgument::new(
            stored_parameter,
            RuntimeValue::Boolean(true),
        )?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    create_flagged,
                    &triggered_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        // The helper waits for recovery, installs the trigger, resumes the
        // dispatch, awaits the task, and removes the trigger before it
        // returns. Cleanup runs even when the dispatch task fails, times out,
        // or unexpectedly commits.
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterRow,
            execution,
            reached,
            resume,
            "triggered raw dispatch",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted {
                context,
                source,
            }) => (context, source),
            other => {
                return Err(failure(format!(
                    "triggered raw dispatch returned {other:?}"
                )));
            }
        };
        require_context(
            context,
            applied.pair(),
            create_flagged,
            create_flagged_revision,
        )?;
        let source = match source.as_ref() {
            ServerInsertError::Database { source } => source,
            other => {
                return Err(failure(format!(
                    "triggered raw dispatch returned {other:?}"
                )));
            }
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("triggered raw dispatch has no database error code"))?;
        require(
            code == &SqlState::RAISE_EXCEPTION,
            "triggered raw dispatch error code differs",
        )?;
        let after_trigger = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_trigger,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the triggered raw INSERT must roll back its tentative row",
        )?;

        // The exact ParameterId binds TRUE then FALSE, each returning a real
        // probe reference with a distinct nonzero object identity.
        let inserted_true = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                &[FunctionArgument::new(
                    stored_parameter,
                    RuntimeValue::Boolean(true),
                )?],
            )
            .await?;
        let inserted_false = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                &[FunctionArgument::new(
                    stored_parameter,
                    RuntimeValue::Boolean(false),
                )?],
            )
            .await?;
        let true_values = match inserted_true {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "the TRUE raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: true_target,
            object: true_object,
        } = &true_values[0]
        else {
            return Err(failure("the TRUE raw INSERT must return a probe reference"));
        };
        let false_values = match inserted_false {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "the FALSE raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: false_target,
            object: false_object,
        } = &false_values[0]
        else {
            return Err(failure(
                "the FALSE raw INSERT must return a probe reference",
            ));
        };
        require(
            *true_target == probe
                && *true_object != ObjectId::from_bytes([0; 16])
                && *false_target == probe
                && *false_object != ObjectId::from_bytes([0; 16])
                && true_object != false_object,
            "argument raw INSERTs must return distinct real probe references",
        )?;

        // The stored Boolean multiset now contains the parameter-free TRUE, the
        // argument TRUE, and the argument FALSE, in no particular row order.
        let multiset = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        let multiset = match multiset {
            AuthenticatedRawCallResult::Server(values) => values,
            other => {
                return Err(failure(format!(
                    "raw SELECT must return Server values, got {other:?}"
                )));
            }
        };
        require(
            multiset.len() == 3
                && multiset
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2
                && multiset
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "raw SELECT must return the parameter-free TRUE plus the argument TRUE and FALSE",
        )?;

        // One Boolean argument rejects every non-INSERT raw target: the health
        // intrinsic, an active CLIENT function, and the granted SERVER SELECT.
        kernel
            .grant_catalogue_health_service_execute(pair, client_boolean)
            .await?;
        for (target, label) in [
            (CATALOGUE_HEALTH_FUNCTION_ID, "the health intrinsic"),
            (client_boolean, "the active CLIENT function"),
            (read_probes, "the granted SERVER SELECT"),
        ] {
            let rejected = kernel
                .dispatch_authenticated_raw_call_with_arguments(
                    &session,
                    target,
                    std::slice::from_ref(&wrong_parameter_argument),
                )
                .await
                .expect_err("a Boolean argument must reject a non-INSERT raw target");
            require(
                matches!(
                    rejected,
                    PostgresKernelError::RawCallTargetUnavailable { .. }
                ),
                format!("{label} with an argument returned the wrong typed error"),
            )?;
        }
        let after_rejected = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        let after_rejected = match after_rejected {
            AuthenticatedRawCallResult::Server(values) => values,
            other => {
                return Err(failure(format!(
                    "raw SELECT must return Server values, got {other:?}"
                )));
            }
        };
        require(
            after_rejected.len() == 3
                && after_rejected
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2
                && after_rejected
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "rejected argument calls must not change any stored row",
        )?;

        // Audit index 7 is the denied wrong-parameter call before its grant.
        // Audit index 9 is the allowed wrong-parameter call: its audit survived
        // the savepoint rollback. Audit index 11 is the triggered write
        // failure: its allowed audit survived that rollback, then every later
        // dispatch was allowed.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 20, "raw dispatch audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[2..7].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                })
                && audits[7].decision().kind() == SecurityAuditKind::Execute
                && audits[7].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[7]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[8..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                })
                && audits[9]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[11]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[16]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(CATALOGUE_HEALTH_FUNCTION_ID)
                && audits[17]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(client_boolean)
                && audits[18]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(read_probes),
            "raw dispatch audit kinds, outcomes, and targets differ",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, read_probes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_named),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_flagged),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, client_boolean),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the five fixed-service grants",
        )?;

        Ok(())
    })
    .await
}

/// Proves that one authenticated raw pair binds two same-typed parameters by
/// their active identities, not by declaration or supplied argument order.
///
/// The existing singleton journeys retain their one-argument boundaries. A
/// defaulted SERVER parameter cannot become an active target because the
/// compiler rejects it before mutation preparation. This tracer owns the pair
/// shapes that can reach PostgreSQL, authorisation order, U+0000 rollback,
/// and the typed execution-failure path.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_argument_pair_binds_two_active_parameters_and_audits() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-argument-pair-live",
        authenticated_raw_argument_pair_binds_two_active_parameters_and_audits_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_argument_pair_binds_two_active_parameters_and_audits_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_ARGUMENT_PAIR_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_argument_pair_test", "probe"]))
            .ok_or_else(|| failure("raw argument-pair probe type is absent"))?
            .id();
        let create_pair = raw_function_id(&applied, &["raw_argument_pair_test", "create_pair"])?;
        let create_unused = raw_function_id(&applied, &["raw_argument_pair_test", "create_unused"])?;
        let create_indirect =
            raw_function_id(&applied, &["raw_argument_pair_test", "create_indirect"])?;
        let create_extra = raw_function_id(&applied, &["raw_argument_pair_test", "create_extra"])?;
        let create_owner = raw_function_id(&applied, &["raw_argument_pair_test", "create_owner"])?;
        let create_assignment =
            raw_function_id(&applied, &["raw_argument_pair_test", "create_assignment"])?;
        let read_first = raw_function_id(&applied, &["raw_argument_pair_test", "read_first"])?;
        let read_second = raw_function_id(&applied, &["raw_argument_pair_test", "read_second"])?;
        let read_assignment_labels = raw_function_id(
            &applied,
            &["raw_argument_pair_test", "read_assignment_labels"],
        )?;
        let read_assignment_owners = raw_function_id(
            &applied,
            &["raw_argument_pair_test", "read_assignment_owners"],
        )?;
        let create_definition = applied
            .catalogue()
            .function_by_id(create_pair)
            .ok_or_else(|| failure("raw argument-pair creator is absent"))?;
        let first_parameter = create_definition
            .parameter_by_name("p_first")
            .ok_or_else(|| failure("raw argument-pair p_first is absent"))?
            .id();
        let second_parameter = create_definition
            .parameter_by_name("p_second")
            .ok_or_else(|| failure("raw argument-pair p_second is absent"))?
            .id();
        require(
            first_parameter != second_parameter,
            "raw argument-pair parameters must have distinct identities",
        )?;
        let create_revision = create_definition.current_revision();
        let parameter = |function: FunctionId, name: &str| {
            applied
                .catalogue()
                .function_by_id(function)
                .and_then(|definition| definition.parameter_by_name(name))
                .map(|parameter| parameter.id())
                .ok_or_else(|| {
                    failure(format!(
                        "raw argument-pair {name} parameter is absent from {function}"
                    ))
                })
        };
        let unused_first = parameter(create_unused, "p_first")?;
        let unused_second = parameter(create_unused, "p_second")?;
        let indirect_first = parameter(create_indirect, "p_first")?;
        let indirect_second = parameter(create_indirect, "p_second")?;
        let extra_first = parameter(create_extra, "p_first")?;
        let extra_second = parameter(create_extra, "p_second")?;
        let owner_parameter = parameter(create_owner, "p_name")?;
        let assignment_label = parameter(create_assignment, "p_label")?;
        let assignment_owner = parameter(create_assignment, "p_owner")?;
        let owner = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_argument_pair_test", "owner"]))
            .ok_or_else(|| failure("raw argument-pair owner type is absent"))?
            .id();
        let assignment = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| {
                name_is(
                    object.name().parts(),
                    &["raw_argument_pair_test", "assignment"],
                )
            })
            .ok_or_else(|| failure("raw argument-pair assignment type is absent"))?
            .id();
        let exact_arguments = vec![
            FunctionArgument::new(
                second_parameter,
                RuntimeValue::Text(String::from("second exact value")),
            )?,
            FunctionArgument::new(
                first_parameter,
                RuntimeValue::Text(String::from("first exact value")),
            )?,
        ];
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Denial precedes the two supplied parameter identities and values.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &exact_arguments)
            .await
            .expect_err("an ungranted raw argument pair must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == create_pair
            ),
            "raw argument-pair denial did not precede target inspection",
        )?;

        for function in [
            create_pair,
            create_unused,
            create_indirect,
            create_extra,
            create_owner,
            create_assignment,
            read_first,
            read_second,
            read_assignment_labels,
            read_assignment_owners,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // All pair-specific target rejections that can reach the protected
        // INSERT branch retain one allowed audit and create no probe row.
        let mut wrong_parameter_bytes = first_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        let rejected_pairs = [
            (
                create_pair,
                vec![
                    FunctionArgument::new(wrong_parameter, RuntimeValue::Text(String::from("wrong")))?,
                    FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
                ],
                "a wrong pair parameter",
            ),
            (
                create_pair,
                vec![
                    FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
                    FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("duplicate")))?,
                ],
                "a duplicate pair parameter",
            ),
            (
                create_pair,
                vec![FunctionArgument::new(
                    first_parameter,
                    RuntimeValue::Text(String::from("missing second")),
                )?],
                "a missing pair parameter",
            ),
            (
                create_pair,
                vec![
                    FunctionArgument::new(first_parameter, RuntimeValue::Integer(7))?,
                    FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
                ],
                "a mistyped pair parameter",
            ),
            (
                create_unused,
                vec![
                    FunctionArgument::new(unused_first, RuntimeValue::Text(String::from("used")))?,
                    FunctionArgument::new(unused_second, RuntimeValue::Text(String::from("unused")))?,
                ],
                "an unused pair parameter",
            ),
            (
                create_indirect,
                vec![
                    FunctionArgument::new(indirect_first, RuntimeValue::Text(String::from("nested first")))?,
                    FunctionArgument::new(indirect_second, RuntimeValue::Text(String::from("nested second")))?,
                ],
                "an indirectly used pair parameter",
            ),
            (
                create_extra,
                vec![
                    FunctionArgument::new(extra_first, RuntimeValue::Text(String::from("first")))?,
                    FunctionArgument::new(extra_second, RuntimeValue::Text(String::from("second")))?,
                ],
                "an extra declared pair parameter",
            ),
        ];
        for (function, arguments, label) in rejected_pairs {
            let unavailable = kernel
                .dispatch_authenticated_raw_call_with_arguments(&session, function, &arguments)
                .await
                .expect_err(label);
            require(
                matches!(
                    unavailable,
                    PostgresKernelError::RawCallTargetUnavailable {
                        function: actual,
                        rule: "raw SERVER INSERT argument target is unavailable",
                    } if actual == function
                ),
                format!("{label} returned {unavailable:?}"),
            )?;
        }
        let outer_extra = vec![
            FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
            FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
            FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("extra")))?,
        ];
        let extra = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &outer_extra)
            .await
            .expect_err("a third raw argument must close before PostgreSQL");
        require_raw_scalar_target_unavailable(
            &extra,
            create_pair,
            "raw calls accept zero arguments, one supported value, or one supported argument pair",
        )?;
        let unsupported = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, read_first, &exact_arguments)
            .await
            .expect_err("a pair must reject a non-INSERT raw target");
        require_raw_scalar_target_unavailable(
            &unsupported,
            read_first,
            "raw call arguments require a supported active SERVER mutation target",
        )?;

        // A scalar and Reference pair crosses the same protected path without
        // source rendering or positional binding.
        let owner_value = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_owner,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Text(String::from("pair owner")),
                )?],
            )
            .await?;
        let owner_object = raw_scalar_insert_reference(owner_value, owner)?;
        let assignment_value = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_assignment,
                &[
                    FunctionArgument::new(
                        assignment_owner,
                        RuntimeValue::Reference {
                            target: owner,
                            object: owner_object,
                        },
                    )?,
                    FunctionArgument::new(
                        assignment_label,
                        RuntimeValue::Text(String::from("scalar-reference pair")),
                    )?,
                ],
            )
            .await?;
        raw_scalar_insert_reference(assignment_value, assignment)?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_assignment_labels).await?,
            &RuntimeValue::Text(String::from("scalar-reference pair")),
            "raw scalar-Reference pair label",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_assignment_owners).await?,
            &RuntimeValue::Reference {
                target: owner,
                object: owner_object,
            },
            "raw scalar-Reference pair owner",
        )?;

        // Reverse supplied order while keeping the two values attached to
        // their identities. Same-typed fields prove identity, not position,
        // controls the stored row.
        let inserted = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &exact_arguments)
            .await?;
        let inserted = raw_scalar_insert_reference(inserted, probe)?;
        require(
            inserted != ObjectId::from_bytes([0; 16]),
            "raw argument-pair INSERT must allocate a real object identity",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_first).await?,
            &RuntimeValue::Text(String::from("first exact value")),
            "raw argument-pair first field",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_second).await?,
            &RuntimeValue::Text(String::from("second exact value")),
            "raw argument-pair second field",
        )?;

        // A database failure after complete pair validation stays a typed
        // SERVER INSERT failure and commits no partial pair row.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let execution_arguments = exact_arguments.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    create_pair,
                    &execution_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterRow,
            execution,
            reached,
            resume,
            "triggered raw argument-pair dispatch",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted { context, source }) => {
                (context, source)
            }
            other => return Err(failure(format!("triggered raw argument pair returned {other:?}"))),
        };
        require_context(context, pair, create_pair, create_revision)?;
        let ServerInsertError::Database { source } = source.as_ref() else {
            return Err(failure("triggered raw argument pair lost its database failure"));
        };
        require(
            source.as_db_error().map(|error| error.code()) == Some(&SqlState::RAISE_EXCEPTION),
            "triggered raw argument pair changed the PostgreSQL error code",
        )?;

        // Either Text position rejects U+0000 after authorisation, retains its
        // allowed audit, and rolls back without adding a row. The last case
        // supplies both invalid values in reverse stable-identity order, so
        // the protected validator must canonicalise before its private check.
        let (lower_parameter, higher_parameter) = if first_parameter.to_bytes() < second_parameter.to_bytes() {
            (first_parameter, second_parameter)
        } else {
            (second_parameter, first_parameter)
        };
        for arguments in [
            vec![
                FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("a\u{0}b")))?,
                FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
            ],
            vec![
                FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
                FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("a\u{0}b")))?,
            ],
            vec![
                FunctionArgument::new(higher_parameter, RuntimeValue::Text(String::from("higher\u{0}")))?,
                FunctionArgument::new(lower_parameter, RuntimeValue::Text(String::from("lower\u{0}")))?,
            ],
        ] {
            let unavailable = kernel
                .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &arguments)
                .await
                .expect_err("Text U+0000 must make a raw argument pair unavailable");
            require(
                matches!(
                    unavailable,
                    PostgresKernelError::RawCallTargetUnavailable { function, .. } if function == create_pair
                ),
                "Text U+0000 raw argument-pair rejection lost its target identity",
            )?;
        }
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_first).await?,
            &RuntimeValue::Text(String::from("first exact value")),
            "Text U+0000 raw argument-pair first field",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_second).await?,
            &RuntimeValue::Text(String::from("second exact value")),
            "Text U+0000 raw argument-pair second field",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let actual: Vec<(SecurityAuditKind, SecurityAuditOutcome, Option<FunctionId>)> = audits
            .iter()
            .map(|event| {
                let decision = event.decision();
                (
                    decision.kind(),
                    decision.outcome(),
                    decision.target().map(InvocationTarget::function),
                )
            })
            .collect();
        let allowed = SecurityAuditOutcome::Allowed;
        let execute = SecurityAuditKind::Execute;
        let expected = vec![
            (SecurityAuditKind::Authentication, allowed, None),
            (execute, SecurityAuditOutcome::Denied, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_unused)),
            (execute, allowed, Some(create_indirect)),
            (execute, allowed, Some(create_extra)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(create_owner)),
            (execute, allowed, Some(create_assignment)),
            (execute, allowed, Some(read_assignment_labels)),
            (execute, allowed, Some(read_assignment_owners)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(read_second)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(read_second)),
        ];
        require(
            actual == expected,
            "raw argument-pair audit sequence differs",
        )?;

        Ok(())
    })
    .await
}

/// ADR 0050 RED tracer for a two-argument raw UPDATE. The compiler closes
/// nullable and defaulted mutation parameters before this live boundary; this
/// test covers every remaining public pair shape and the savepoint path.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0050 dispatch"]
fn authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-value-update-live",
        authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel.apply(&standard_application_candidate(
            RAW_REFERENCE_VALUE_UPDATE_SOURCE, &standard, &upgrade,
        )?).await?;
        let pair = applied.pair();
        let object = applied.catalogue().object_types().iter().find(|object| {
            name_is(object.name().parts(), &["raw_reference_value_update", "probe"])
        }).ok_or_else(|| failure("raw reference value probe is absent"))?.id();
        let function = |name| raw_function_id(&applied, &["raw_reference_value_update", name]);
        let create = function("create_probe")?;
        let update_text = function("update_text")?;
        let update_link = function("update_link")?;
        let update_unused = function("update_unused")?;
        let update_extra = function("update_extra")?;
        let read_stored = function("read_stored")?;
        let read_links = function("read_links")?;
        let parameter = |function, name| -> TestResult<ParameterId> {
            applied.catalogue().function_by_id(function)
                .and_then(|definition| definition.parameter_by_name(name))
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("{name} parameter is absent")))
        };
        let text_value = parameter(update_text, "p_value")?;
        let text_selector = parameter(update_text, "p_probe")?;
        let link_value = parameter(update_link, "p_value")?;
        let link_selector = parameter(update_link, "p_probe")?;
        let unused_value = parameter(update_unused, "p_value")?;
        let unused_selector = parameter(update_unused, "p_probe")?;
        let extra_value = parameter(update_extra, "p_value")?;
        let extra_selector = parameter(update_extra, "p_probe")?;
        let revision = applied.catalogue().function_by_id(update_text)
            .ok_or_else(|| failure("raw text UPDATE is absent"))?.current_revision();
        let mut wrong_bytes = text_selector.to_bytes();
        wrong_bytes[0] ^= 1;
        let wrong = ParameterId::from_bytes(wrong_bytes);
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;
        for target in [create, read_stored, read_links] {
            kernel.grant_catalogue_health_service_execute(pair, target).await?;
        }
        let first = create_raw_reference_value_update_probe(&kernel, &session, create, "seed").await?;
        let second = create_raw_reference_value_update_probe(&kernel, &session, create, "seed").await?;
        require(first != second, "the two raw value UPDATE rows must differ")?;

        // Denial precedes selector and value inspection.
        let denied = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(wrong, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("denied".into()))?]
        ).await.expect_err("ungranted pair UPDATE must deny before facts");
        require(matches!(denied, PostgresKernelError::RawExecuteDenied { pair: actual, function, .. }
            if actual == pair && function == update_text), "pair UPDATE denial differs")?;
        require(read_probe_values(&kernel, &session, read_stored).await? == [RuntimeValue::Text("seed".into()), RuntimeValue::Text("seed".into())], "denied pair UPDATE changed a row")?;
        for target in [update_text, update_link, update_unused, update_extra] {
            kernel.grant_catalogue_health_service_execute(pair, target).await?;
        }
        let unavailable = |error: PostgresKernelError, target| require(matches!(error,
            PostgresKernelError::RawCallTargetUnavailable { function, .. } if function == target),
            "allowed invalid raw value UPDATE must be unavailable");
        // Wrong, duplicate, missing, extra, mistyped, and unused public shapes
        // close after the allowed audit. The compiler rejects nullable,
        // defaulted, and indirect record-constructor UPDATE values before this
        // fixture reaches PostgreSQL.
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(wrong, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("x".into()))?]).await.expect_err("wrong parameter must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_selector, second.clone())?]).await.expect_err("duplicate parameter must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?]).await.expect_err("missing value must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_extra,
            &[FunctionArgument::new(extra_selector, first.clone())?, FunctionArgument::new(extra_value, RuntimeValue::Text("x".into()))?]).await.expect_err("extra declaration must close"), update_extra)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, RuntimeValue::Text("not-a-reference".into()))?, FunctionArgument::new(text_value, first.clone())?]).await.expect_err("mistyped pair must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_unused,
            &[FunctionArgument::new(unused_selector, first.clone())?, FunctionArgument::new(unused_value, RuntimeValue::Text("unused".into()))?]).await.expect_err("unused value must close"), update_unused)?;

        // Supplied order is reverse declaration order. ParameterId selects the
        // Reference and scalar slots, returns the exact selector, and updates
        // only one row.
        let updated = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("changed".into()))?]).await?;
        require(matches!(updated, AuthenticatedRawCallResult::Server(values) if values == [first.clone()]), "scalar pair UPDATE result differs")?;
        let stored = read_probe_values(&kernel, &session, read_stored).await?;
        require(stored.len() == 2 && stored.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1 && stored.iter().filter(|value| **value == RuntimeValue::Text("seed".into())).count() == 1, "scalar pair UPDATE did not select one row")?;
        let RuntimeValue::Reference { object: first_object, .. } = &first else { return Err(failure("first raw value row is not a reference")); };
        let RuntimeValue::Reference { object: second_object, .. } = &second else { return Err(failure("second raw value row is not a reference")); };
        let absent_object = [[7; 16], [8; 16], [9; 16]].into_iter().map(ObjectId::from_bytes)
            .find(|candidate| candidate != first_object && candidate != second_object)
            .ok_or_else(|| failure("no deterministic absent object identity remains"))?;
        let absent = RuntimeValue::Reference { target: object, object: absent_object };
        let empty = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_value, RuntimeValue::Text("absent".into()))?, FunctionArgument::new(text_selector, absent)?]).await?;
        require(matches!(empty, AuthenticatedRawCallResult::Server(values) if values.is_empty()), "absent selector must complete empty")?;
        let linked = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_link,
            &[FunctionArgument::new(link_selector, second.clone())?, FunctionArgument::new(link_value, first.clone())?]).await?;
        require(matches!(linked, AuthenticatedRawCallResult::Server(values) if values == [second.clone()]), "Reference value pair UPDATE result differs")?;
        require(read_probe_values(&kernel, &session, read_links).await?.iter().filter(|value| **value == first).count() == 1, "Reference value pair UPDATE did not select one row")?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("nul\0text".into()))?]).await.expect_err("U+0000 must close"), update_text)?;
        let after_nul = read_probe_values(&kernel, &session, read_stored).await?;
        require(after_nul.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1, "U+0000 changed a row")?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone(); let execution_session = session.clone();
        let arguments = vec![FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("rollback".into()))?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move { executor.dispatch_authenticated_raw_call_with_arguments_and_test_barrier(&execution_session, update_text, &arguments, execution_reached, execution_resume).await });
        let triggered = finish_triggered_failure(&database, object, TriggerKind::AfterUpdate, execution,
            reached, resume, "raw value UPDATE").await;
        let triggered = triggered?;
        let PostgresKernelError::ServerUpdate(ServerUpdateError::NotCommitted {
            context,
            source,
        }) = triggered else {
            return Err(failure("raw value UPDATE operational failure is not internal"));
        };
        require_context(context, pair, update_text, revision)?;
        require(matches!(source.as_ref(), ServerMutationError::Database { source }
            if source.as_db_error().is_some_and(|error| error.code() == &SqlState::RAISE_EXCEPTION)),
            "raw value UPDATE operational failure lost SQLSTATE  P0001")?;
        let after_failure = read_probe_values(&kernel, &session, read_stored).await?;
        require(after_failure.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1,
            "operational raw value UPDATE failure did not roll back")?;
        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (create, SecurityAuditOutcome::Allowed), (create, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Denied), (read_stored, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_extra, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_unused, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (read_stored, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_link, SecurityAuditOutcome::Allowed),
            (read_links, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (read_stored, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (read_stored, SecurityAuditOutcome::Allowed),
        ];
        require(audits.len() == expected.len() + 1
            && audits[0].decision().kind() == SecurityAuditKind::Authentication
            && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
            && audits[1..].iter().zip(expected).all(|(event, (function, outcome))|
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().outcome() == outcome
                    && event.decision().target() == Some(InvocationTarget::new(function, pair))),
            "raw reference value UPDATE audit sequence differs")?;
        Ok(())
    }).await
}

#[cfg(feature = "test-hooks")]
async fn create_raw_reference_value_update_probe(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    create: FunctionId,
    stored: &str,
) -> TestResult<RuntimeValue> {
    let parameter = kernel
        .recover()
        .await?
        .catalogue()
        .function_by_id(create)
        .and_then(|definition| definition.parameter_by_name("p_stored"))
        .map(|parameter| parameter.id())
        .ok_or_else(|| failure("raw reference value create parameter is absent"))?;
    let created = kernel
        .dispatch_authenticated_raw_call_with_arguments(
            session,
            create,
            &[FunctionArgument::new(
                parameter,
                RuntimeValue::Text(stored.into()),
            )?],
        )
        .await?;
    match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => match &values[0] {
            RuntimeValue::Reference { target, object }
                if *object != ObjectId::from_bytes([0; 16]) =>
            {
                Ok(RuntimeValue::Reference {
                    target: *target,
                    object: *object,
                })
            }
            _ => Err(failure(
                "raw reference value create did not return a real reference",
            )),
        },
        other => Err(failure(format!(
            "raw reference value create must return one Server value, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
async fn create_probe_reference(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    create_probe: FunctionId,
) -> TestResult<RuntimeValue> {
    let created = kernel
        .dispatch_authenticated_raw_call(session, create_probe)
        .await?;
    let created = match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference { target, object } = &created[0] else {
        return Err(failure("raw INSERT must return an object reference"));
    };
    require(
        *object != ObjectId::from_bytes([0; 16]),
        "the created reference must name a real row",
    )?;
    Ok(RuntimeValue::Reference {
        target: *target,
        object: *object,
    })
}

#[cfg(feature = "test-hooks")]
async fn read_probe_values(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    read_probes: FunctionId,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, read_probes)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_mutation_authority_and_selection() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-authority-live",
        authenticated_raw_reference_mutation_authority_and_selection_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_mutation_authority_and_selection_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_false = raw_function_id(&applied, &["raw_reference_test", "update_false"])?;
        let delete_probe = raw_function_id(&applied, &["raw_reference_test", "delete_probe"])?;
        let read_probes = raw_function_id(&applied, &["raw_reference_test", "read_probes"])?;
        let update_parameter = applied
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_false.p_probe is absent from the active catalogue"))?
            .id();
        let delete_parameter = applied
            .catalogue()
            .function_by_id(delete_probe)
            .ok_or_else(|| failure("delete_probe is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("delete_probe.p_probe is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = update_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the writer and the reader, so the reference mutations
        // stay unauthorised for the denial proof.
        for function in [create_probe, read_probes] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create two distinct rows and retain both exact references.
        let first = create_probe_reference(&kernel, &session, create_probe).await?;
        let second = create_probe_reference(&kernel, &session, create_probe).await?;
        let RuntimeValue::Reference {
            target: first_target,
            object: first_object,
        } = &first
        else {
            return Err(failure("first created value is not a reference"));
        };
        let RuntimeValue::Reference {
            target: second_target,
            object: second_object,
        } = &second
        else {
            return Err(failure("second created value is not a reference"));
        };
        require(
            first_target == second_target && *first_target != TypeId::from_bytes([0; 16]),
            "both created references must share one nonzero target type",
        )?;
        require(
            *first_object != *second_object
                && *first_object != ObjectId::from_bytes([0; 16])
                && *second_object != ObjectId::from_bytes([0; 16]),
            "the two created references must name distinct nonzero rows",
        )?;
        let mut wrong_target_bytes = first_target.to_bytes();
        wrong_target_bytes[0] ^= 0x01;
        let wrong_target_id = TypeId::from_bytes(wrong_target_bytes);
        require(
            wrong_target_id != *first_target,
            "the deliberately wrong target must differ from the created target",
        )?;

        // A wrong-binding reference UPDATE before its grant is denied before
        // binding validation, and both rows stay unchanged.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(wrong_parameter, first.clone())?],
            )
            .await
            .expect_err("reference UPDATE before its grant must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == update_false
            ),
            "pre-grant wrong-binding UPDATE returned the wrong typed denial",
        )?;
        let two_true = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            two_true.len() == 2
                && two_true
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the denied reference UPDATE must leave both rows TRUE",
        )?;

        // Grant the two reference mutations.
        for function in [update_false, delete_probe] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // The same wrong-binding argument is rejected after the grant as an
        // unavailable raw target, retaining an allowed audit.
        let wrong_binding = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(wrong_parameter, first.clone())?],
            )
            .await
            .expect_err("the same wrong-binding argument must reject the reference UPDATE");
        require(
            matches!(
                wrong_binding,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_false
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "the wrong-binding argument returned the wrong typed error",
        )?;
        let wrong_target = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(
                    update_parameter,
                    RuntimeValue::Reference {
                        target: wrong_target_id,
                        object: *first_object,
                    },
                )?],
            )
            .await
            .expect_err("a wrong target TypeId must reject the reference UPDATE");
        require(
            matches!(
                wrong_target,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_false
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "a wrong target TypeId returned the wrong typed error",
        )?;
        let unchanged = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            unchanged.len() == 2
                && unchanged
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the rejected reference UPDATEs must leave both rows TRUE",
        )?;

        // The UPDATE selects exactly the first row: the reader returns one
        // FALSE and one TRUE in no particular order.
        let updated = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(update_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                updated,
                AuthenticatedRawCallResult::Server(values)
                    if values == [first.clone()]
            ),
            "the reference UPDATE must return the identical input reference",
        )?;
        let mixed = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            mixed.len() == 2
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "the UPDATE must select exactly one row: one FALSE and one TRUE value",
        )?;

        // DELETE selects exactly the first row, then repeats as an empty
        // success, leaving the second row in place.
        let deleted = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                delete_probe,
                &[FunctionArgument::new(delete_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                deleted,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the reference DELETE must return exactly one TRUE value",
        )?;
        let one_true = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            one_true == [RuntimeValue::Boolean(true)],
            "the reference DELETE must leave exactly the second row TRUE",
        )?;

        // An exact UPDATE using the deleted reference matches no row and
        // completes empty, leaving the surviving row unchanged.
        let updated_deleted = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(update_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                updated_deleted,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the UPDATE of a deleted reference must complete with an empty value list",
        )?;
        let still_one_after_update = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            still_one_after_update == [RuntimeValue::Boolean(true)],
            "the UPDATE of a deleted reference must leave the surviving row unchanged",
        )?;

        let repeated = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                delete_probe,
                &[FunctionArgument::new(delete_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                repeated,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the repeated reference DELETE must complete with an empty value list",
        )?;
        let still_one = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            still_one == [RuntimeValue::Boolean(true)],
            "the repeated reference DELETE must leave the second row unchanged",
        )?;

        // The allowed rejections retained allowed audits before the savepoint
        // rollback; every dispatch decision is exact.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 16,
            "raw reference mutation audit count differs",
        )?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[3].decision().kind() == SecurityAuditKind::Execute
                && audits[3].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[3].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[7].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[8].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[9].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[10].decision().target()
                    == Some(InvocationTarget::new(delete_probe, pair))
                && audits[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[11].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[12].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[12].decision().target()
                    == Some(InvocationTarget::new(update_false, pair))
                && audits[13].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[13].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[14].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[14].decision().target()
                    == Some(InvocationTarget::new(delete_probe, pair))
                && audits[15].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[15].decision().target() == Some(InvocationTarget::new(read_probes, pair)),
            "raw reference mutation audit kinds, outcomes, and targets differ",
        )?;

        // The recovered grant set is exactly the four fixed-service targets.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, update_false),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, delete_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, read_probes),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the four fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw reference mutations must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit() -> TestResult<()>
{
    run_large_stack_live_test(
        "authenticated-raw-reference-non-literal-live",
        authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_reference_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_link = raw_function_id(&applied, &["raw_reference_test", "update_link"])?;
        let read_links = raw_function_id(&applied, &["raw_reference_test", "read_links"])?;
        let p_probe = applied
            .catalogue()
            .function_by_id(update_link)
            .ok_or_else(|| failure("update_link is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_link.p_probe is absent from the active catalogue"))?
            .id();
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant the three fixed-service targets.
        for function in [create_probe, update_link, read_links] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create one row and retain its exact reference.
        let reference = create_probe_reference(&kernel, &session, create_probe).await?;

        // A Reference argument against the parameter-free reader closes as an
        // unavailable raw target, retaining an allowed audit.
        let rejected_read = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                read_links,
                &[FunctionArgument::new(p_probe, reference.clone())?],
            )
            .await
            .expect_err("a Reference argument must reject the parameter-free reader");
        require(
            matches!(
                rejected_read,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == read_links
                        && rule
                            == "raw call arguments require a supported active SERVER mutation target"
            ),
            "the Reference-bearing read_links call returned the wrong typed error",
        )?;

        // The public reader exposes the exact typed NULL reference for the
        // one unlinked row.
        let initial = read_probe_values(&kernel, &session, read_links).await?;
        require(
            matches!(
                initial.as_slice(),
                [RuntimeValue::Null(null)]
                    if null.resolved_type() == ResolvedType::reference(probe)
            ),
            "read_links must initially return the exact typed NULL reference",
        )?;

        // update_link assigns a non-literal parameter expression, so it must
        // close as an unavailable raw target before any assignment runs.
        let rejected = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_link,
                &[FunctionArgument::new(p_probe, reference.clone())?],
            )
            .await
            .expect_err("a non-literal assignment UPDATE must be rejected");
        require(
            matches!(
                rejected,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_link
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "the non-literal assignment UPDATE returned the wrong typed error",
        )?;

        // The reader still exposes the exact typed NULL reference.
        let after = read_probe_values(&kernel, &session, read_links).await?;
        require(
            matches!(
                after.as_slice(),
                [RuntimeValue::Null(null)]
                    if null.resolved_type() == ResolvedType::reference(probe)
            ),
            "the rejected UPDATE must not assign the linked reference",
        )?;

        // Authentication, then allowed create, rejected Reference-bearing
        // read, ordinary read, rejected update, and final read, with the two
        // rejected audits retained at their exact targets.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 6, "raw reference audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(read_links, pair))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target() == Some(InvocationTarget::new(read_links, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(update_link, pair))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target() == Some(InvocationTarget::new(read_links, pair)),
            "raw reference audit kinds, outcomes, and targets differ",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "the rejected raw reference UPDATE must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit()
-> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-database-failure-live",
        authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_reference_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_false = raw_function_id(&applied, &["raw_reference_test", "update_false"])?;
        let read_probes = raw_function_id(&applied, &["raw_reference_test", "read_probes"])?;
        let update_false_definition = applied
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?;
        let p_probe = update_false_definition
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_false.p_probe is absent from the active catalogue"))?
            .id();
        let update_false_revision = update_false_definition.current_revision();
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant the three fixed-service targets.
        for function in [create_probe, update_false, read_probes] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Two rows: the first is the UPDATE target, the second is unrelated.
        let first = create_probe_reference(&kernel, &session, create_probe).await?;
        let second = create_probe_reference(&kernel, &session, create_probe).await?;
        require(
            first != second,
            "the two created references must be distinct",
        )?;

        // A real PostgreSQL UPDATE then fails through an AFTER UPDATE trigger.
        // The dispatch pauses after recovery while the harness installs the
        // trigger, then resumes and fails the write. The typed ServerUpdate
        // database failure must survive the raw dispatch unchanged, the
        // savepoint must roll back the tentative row, and the allowed audit
        // must commit.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let triggered_arguments = vec![FunctionArgument::new(p_probe, first.clone())?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    update_false,
                    &triggered_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterUpdate,
            execution,
            reached,
            resume,
            "triggered raw UPDATE",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerUpdate(ServerUpdateError::NotCommitted {
                context,
                source,
            }) => (context, source),
            other => {
                return Err(failure(format!("triggered raw UPDATE returned {other:?}")));
            }
        };
        require_context(context, pair, update_false, update_false_revision)?;
        let source = match source.as_ref() {
            ServerMutationError::Database { source } => source,
            other => {
                return Err(failure(format!("triggered raw UPDATE returned {other:?}")));
            }
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("triggered raw UPDATE has no database error code"))?;
        require(
            code == &SqlState::RAISE_EXCEPTION,
            "triggered raw UPDATE error code differs",
        )?;

        // The savepoint rolled back: the target row stays TRUE and the
        // unrelated second row stays TRUE.
        let values = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            values.len() == 2
                && values
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the failed UPDATE must leave both rows TRUE",
        )?;

        // The allowed UPDATE audit was retained across the rollback.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 5, "raw reference audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(read_probes, pair)),
            "raw reference audit kinds, outcomes, and targets differ",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "the failed raw reference UPDATE must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
}
