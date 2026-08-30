use super::*;

#[cfg(feature = "test-hooks")]
const RAW_REFERENCE_INSERT_SOURCE: &str = "CREATE SCHEMA raw_reference_insert;\n\
    CREATE TYPE raw_reference_insert.owner AS OBJECT (\n\
      flag BOOLEAN NOT NULL\n\
    );\n\
    CREATE TYPE raw_reference_insert.assignment AS OBJECT (\n\
      owner REF raw_reference_insert.owner NOT NULL UNIQUE, marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_owner()\n\
    RETURNS ROWS (created_owner REF raw_reference_insert.owner)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.owner AS made_owner (flag)\n\
    VALUES (TRUE) RETURNING REF(made_owner);\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_assignment(\n\
      p_owner REF raw_reference_insert.owner\n\
    ) RETURNS ROWS (created_assignment REF raw_reference_insert.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.assignment AS made_assignment (owner, marker)\n\
    VALUES (p_owner, TRUE) RETURNING REF(made_assignment);\n\
    CREATE SERVER FUNCTION raw_reference_insert.read_assignments()\n\
    RETURNS ROWS (marker BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT assignment.marker FROM raw_reference_insert.assignment assignment;\n\
    CREATE TYPE raw_reference_insert.unused_assignment AS OBJECT (\n\
      marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_unused(\n\
      p_owner REF raw_reference_insert.owner\n\
    ) RETURNS ROWS (created_unused REF raw_reference_insert.unused_assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.unused_assignment AS made_unused (marker)\n\
    VALUES (TRUE) RETURNING REF(made_unused);\n\
    CREATE SERVER FUNCTION raw_reference_insert.read_unused()\n\
    RETURNS ROWS (marker BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT unused_assignment.marker FROM raw_reference_insert.unused_assignment unused_assignment;\n";

/// One raw reference-INSERT fixture with a unique reference owner field.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct RawReferenceInsertFixture {
    owner: TypeId,
    assignment: TypeId,
    assignment_owner_field: FieldId,
    create_owner: FunctionId,
    create_assignment: FunctionId,
    create_assignment_revision: FunctionRevisionId,
    create_assignment_owner_parameter: ParameterId,
    read_assignments: FunctionId,
    create_unused: FunctionId,
    create_unused_owner_parameter: ParameterId,
    read_unused: FunctionId,
}

#[cfg(feature = "test-hooks")]
impl RawReferenceInsertFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = |name| {
            active
                .catalogue()
                .object_types()
                .iter()
                .find(|object| name_is(object.name().parts(), &["raw_reference_insert", name]))
                .ok_or_else(|| failure(format!("raw_reference_insert.{name} type is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["raw_reference_insert", name]))
                .ok_or_else(|| failure(format!("raw_reference_insert.{name} function is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("parameter {name} is absent")))
        };
        let owner = object("owner")?;
        let assignment = object("assignment")?;
        let create_owner = function("create_owner")?;
        let create_assignment = function("create_assignment")?;
        let read_assignments = function("read_assignments")?;
        let create_unused = function("create_unused")?;
        let read_unused = function("read_unused")?;
        Ok(Self {
            owner: owner.id(),
            assignment: assignment.id(),
            assignment_owner_field: assignment
                .field_by_name("owner")
                .map(|field| field.id())
                .ok_or_else(|| failure("raw_reference_insert.assignment.owner field is absent"))?,
            create_owner: create_owner.id(),
            create_assignment: create_assignment.id(),
            create_assignment_revision: create_assignment.current_revision(),
            create_assignment_owner_parameter: parameter(create_assignment, "p_owner")?,
            read_assignments: read_assignments.id(),
            create_unused: create_unused.id(),
            create_unused_owner_parameter: parameter(create_unused, "p_owner")?,
            read_unused: read_unused.id(),
        })
    }
}

#[cfg(feature = "test-hooks")]
fn raw_reference_insert_arguments(
    fixture: RawReferenceInsertFixture,
    owner: RuntimeValue,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![FunctionArgument::new(
        fixture.create_assignment_owner_parameter,
        owner,
    )?])
}

#[cfg(feature = "test-hooks")]
async fn create_raw_reference_insert_owner(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    fixture: RawReferenceInsertFixture,
) -> TestResult<RuntimeValue> {
    let created = kernel
        .dispatch_authenticated_raw_call(session, fixture.create_owner)
        .await?;
    let created = match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw owner INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference { target, object } = &created[0] else {
        return Err(failure("raw owner INSERT must return an object reference"));
    };
    require(
        *target == fixture.owner && *object != ObjectId::from_bytes([0; 16]),
        "the created owner reference must name the owner type and a real row",
    )?;
    Ok(RuntimeValue::Reference {
        target: *target,
        object: *object,
    })
}

#[cfg(feature = "test-hooks")]
async fn read_raw_reference_insert_markers(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    fixture: RawReferenceInsertFixture,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, fixture.read_assignments)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw assignment SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
fn require_raw_reference_insert_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: RawReferenceInsertFixture,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure(
            "raw reference INSERT conflict is not a SERVER INSERT error",
        ));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "raw reference INSERT conflict has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure(
            "raw reference INSERT conflict lacks pinned execution context",
        ));
    };
    require_context(*context, pair, function, revision)?;
    let unique @ ServerMutationError::UniqueReferenceConflict {
        owner,
        field,
        referenced_type,
        source: database_source,
    } = source.as_ref()
    else {
        return Err(failure(
            "raw reference INSERT was not classified as a typed reference conflict",
        ));
    };
    require(
        *owner == fixture.assignment,
        "raw reference INSERT conflict owner differs",
    )?;
    require(
        *field == fixture.assignment_owner_field,
        "raw reference INSERT conflict field differs",
    )?;
    require(
        *referenced_type == fixture.owner,
        "raw reference INSERT conflict referenced type differs",
    )?;
    require(
        database_source
            .as_db_error()
            .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION),
        "raw reference INSERT conflict lost SQLSTATE 23505",
    )?;
    require(
        database_source
            .as_db_error()
            .and_then(|database| database.constraint())
            == Some(unique_constraint_name(fixture.assignment_owner_field).as_str()),
        "raw reference INSERT conflict constraint differs",
    )?;
    require(
        unique.to_string() == "this reference is already used by another object",
        "raw reference INSERT inner display differs",
    )
}

/// One authenticated raw reference-INSERT journey across denial, grant,
/// argument-target rejection, a database failure, a typed unique conflict,
/// and public recovery.
///
/// The test installs the exact checked-in reference-INSERT source, grants
/// only the owner create and the reader, creates two distinct owners, and
/// proves a wrong-parameter reference call is denied before its grant with
/// zero assignments. After granting the assignment create, a wrong parameter
/// id and a wrong reference target type each close as the exact
/// `raw SERVER INSERT argument target is unavailable` rule. A correct-type
/// reference to a definitely missing object returns the typed internal
/// SERVER INSERT database failure, leaves zero rows, and retains its allowed
/// audit. A correct first owner succeeds with a nonzero assignment reference
/// whose target differs from the owner type; repeating the same owner returns
/// the exact typed unique-reference conflict with a `NotCommitted` context,
/// leaves exactly one assignment, and retains its allowed audit. The second
/// owner succeeds with a distinct object id and the same assignment target
/// type. The public raw reader returns exactly two TRUE marker values.
/// A granted SERVER INSERT whose plan never reads its sole Reference
/// parameter (`create_unused`) closes as the exact unavailable raw target
/// rule after classification and savepoint creation, rolls back, retains its
/// allowed audit, and leaves `read_unused` empty. Recovery proves the active
/// pair is unchanged and the grant set is exactly the five fixed-service
/// grants. Rows are asserted only through the public raw reader.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique()
-> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-insert-live",
        authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let fixture = RawReferenceInsertFixture::from_active(&applied)?;
        let owner_parameter = fixture.create_assignment_owner_parameter;
        let mut wrong_parameter_bytes = owner_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != owner_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the owner create and the reader; the assignment create
        // stays unauthorised for the denial proof.
        for function in [fixture.create_owner, fixture.read_assignments] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create two distinct owners through the public raw path.
        let first_owner = create_raw_reference_insert_owner(&kernel, &session, fixture).await?;
        let second_owner = create_raw_reference_insert_owner(&kernel, &session, fixture).await?;
        let RuntimeValue::Reference {
            target: first_owner_target,
            object: first_owner_object,
        } = &first_owner
        else {
            return Err(failure("first owner value is not a reference"));
        };
        let RuntimeValue::Reference {
            target: second_owner_target,
            object: second_owner_object,
        } = &second_owner
        else {
            return Err(failure("second owner value is not a reference"));
        };
        require(
            first_owner_target == second_owner_target && *first_owner_target == fixture.owner,
            "both owners must share the exact owner target type",
        )?;
        require(
            *first_owner_object != *second_owner_object
                && *first_owner_object != ObjectId::from_bytes([0; 16])
                && *second_owner_object != ObjectId::from_bytes([0; 16]),
            "the two owners must name distinct nonzero rows",
        )?;
        let mut wrong_target_bytes = fixture.owner.to_bytes();
        wrong_target_bytes[0] ^= 0x01;
        let wrong_target = TypeId::from_bytes(wrong_target_bytes);
        require(
            wrong_target != fixture.owner,
            "the deliberately wrong target must differ from the owner target",
        )?;
        let missing_object = ObjectId::from_bytes([0xaa; 16]);
        require(
            missing_object != *first_owner_object && missing_object != *second_owner_object,
            "the missing object must not name either created owner",
        )?;

        // The reader proves zero assignments before any assignment create.
        let zero_before = read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(zero_before.is_empty(), "no assignment may exist before any grant")?;

        // Authorisation wins over argument validation: before its grant, even
        // a wrong-parameter reference call is denied and creates nothing.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(wrong_parameter, first_owner.clone())?],
            )
            .await
            .expect_err("an ungranted raw reference INSERT must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_assignment
            ),
            "pre-grant raw reference INSERT returned the wrong typed denial",
        )?;
        let zero_after_denied =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            zero_after_denied.is_empty(),
            "the denied raw reference INSERT must not create any assignment",
        )?;

        // Grant the assignment create and the unused-parameter proof pair.
        for function in [
            fixture.create_assignment,
            fixture.create_unused,
            fixture.read_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // A wrong parameter id closes as the exact unavailable raw target and
        // retains its allowed audit.
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(wrong_parameter, first_owner.clone())?],
            )
            .await
            .expect_err("a wrong parameter id must make the raw reference INSERT unavailable");
        require(
            matches!(
                wrong_parameter_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_assignment
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "a wrong parameter id returned the wrong typed error",
        )?;

        // A wrong reference target type closes with the same exact rule.
        let wrong_target_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Reference {
                        target: wrong_target,
                        object: *first_owner_object,
                    },
                )?],
            )
            .await
            .expect_err("a wrong reference target type must make the raw reference INSERT unavailable");
        require(
            matches!(
                wrong_target_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_assignment
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "a wrong reference target type returned the wrong typed error",
        )?;

        // The SOL proof pair: a granted SERVER INSERT whose plan never reads
        // its sole Reference parameter passes classification and the normal
        // active validator, then closes inside its savepoint as the exact
        // unavailable raw target rule, rolls back, retains its allowed audit,
        // and creates no row.
        let read_unused_before = kernel
            .dispatch_authenticated_raw_call(&session, fixture.read_unused)
            .await?;
        require(
            matches!(
                read_unused_before,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "read_unused must be empty before any create_unused call",
        )?;
        let unused = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_unused,
                &[FunctionArgument::new(
                    fixture.create_unused_owner_parameter,
                    first_owner.clone(),
                )?],
            )
            .await
            .expect_err("create_unused must reject a Reference argument it never reads");
        require(
            matches!(
                unused,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_unused
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "create_unused returned the wrong typed error",
        )?;
        let read_unused_after = kernel
            .dispatch_authenticated_raw_call(&session, fixture.read_unused)
            .await?;
        require(
            matches!(
                read_unused_after,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the rejected create_unused must leave read_unused empty",
        )?;

        // A correct-type reference to a definitely missing object reaches the
        // database, fails as the typed internal SERVER INSERT database
        // failure, rolls back its savepoint, and retains its allowed audit.
        let missing = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Reference {
                        target: fixture.owner,
                        object: missing_object,
                    },
                )?],
            )
            .await
            .expect_err("a missing owner object must fail the raw reference INSERT");
        let PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted {
            context,
            source,
        }) = missing
        else {
            return Err(failure(format!(
                "missing-object raw INSERT returned {missing:?}"
            )));
        };
        require_context(context, pair, fixture.create_assignment, fixture.create_assignment_revision)?;
        let ServerInsertError::Database { source } = source.as_ref() else {
            return Err(failure("missing-object raw INSERT lost its database failure"));
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("missing-object raw INSERT has no database error code"))?;
        require(
            code == &SqlState::FOREIGN_KEY_VIOLATION,
            "missing-object raw INSERT error code differs",
        )?;
        let zero_after_missing =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            zero_after_missing.is_empty(),
            "the failed raw reference INSERT must leave zero assignments",
        )?;

        // The first owner succeeds and returns one nonzero assignment
        // reference whose target differs from the owner type.
        let first_created = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, first_owner.clone())?,
            )
            .await?;
        let first_created = match first_created {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "first owner raw reference INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: first_assignment_target,
            object: first_assignment_object,
        } = &first_created[0]
        else {
            return Err(failure("first owner raw reference INSERT must return an assignment reference"));
        };
        require(
            *first_assignment_target == fixture.assignment
                && *first_assignment_target != fixture.owner
                && *first_assignment_object != ObjectId::from_bytes([0; 16]),
            "the first assignment reference must name the assignment type and a real row",
        )?;

        // Repeating the same owner returns the exact typed unique-reference
        // conflict with a NotCommitted context, and exactly one assignment
        // survives.
        let conflict = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, first_owner.clone())?,
            )
            .await
            .expect_err("a repeated owner must conflict on the unique reference");
        require_raw_reference_insert_conflict(
            &conflict,
            pair,
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        let one_after_conflict =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            one_after_conflict == [RuntimeValue::Boolean(true)],
            "the unique conflict must leave exactly one TRUE assignment",
        )?;

        // The second owner succeeds with a distinct object id and the same
        // assignment target type.
        let second_created = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, second_owner.clone())?,
            )
            .await?;
        let second_created = match second_created {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "second owner raw reference INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: second_assignment_target,
            object: second_assignment_object,
        } = &second_created[0]
        else {
            return Err(failure("second owner raw reference INSERT must return an assignment reference"));
        };
        require(
            *second_assignment_target == fixture.assignment
                && *second_assignment_target == *first_assignment_target
                && *second_assignment_object != ObjectId::from_bytes([0; 16])
                && second_assignment_object != first_assignment_object,
            "the second assignment must share the target type and use a distinct nonzero object",
        )?;

        // The public raw reader returns exactly two TRUE marker values.
        let two = read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            two.len() == 2
                && two
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the raw reader must return exactly two TRUE assignment markers",
        )?;

        // One authentication audit, then one audit per dispatch: the pre-grant
        // call at index 4 was denied, every allowed rejection and success
        // retained an allowed target audit.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 18
                && audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                })
                && audits[4].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[4].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[7].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[8].decision().target()
                    == Some(InvocationTarget::new(fixture.read_unused, pair))
                && audits[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[9].decision().target()
                    == Some(InvocationTarget::new(fixture.create_unused, pair))
                && audits[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[10].decision().target()
                    == Some(InvocationTarget::new(fixture.read_unused, pair))
                && audits[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[11].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[13].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[13].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[14].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[14].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[16].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[16].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[1..]
                    .iter()
                    .enumerate()
                    .all(|(index, event)| index == 3 || {
                        event.decision().outcome() == SecurityAuditOutcome::Allowed
                    }),
            "raw reference INSERT audit kinds, outcomes, and targets differ",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_owner),
            ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                fixture.create_assignment,
            ),
            ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                fixture.read_assignments,
            ),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_unused),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_unused),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the five fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw reference INSERTs must not change the active revision pair",
        )?;

        require_no_session_leaks(&database).await
    })
    .await
}
