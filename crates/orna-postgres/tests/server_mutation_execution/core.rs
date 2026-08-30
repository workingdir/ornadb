use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn commits_exact_typed_rows_uses_private_ids_and_allocates_unique_ids() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let owner = kernel
            .execute_server_insert(
                fixture.create_owner,
                &[FunctionArgument::new(
                    fixture.owner_name_parameter,
                    RuntimeValue::Text(String::from("Ada")),
                )?],
            )
            .await?;
        require_insert_result(
            &owner,
            applied.pair(),
            fixture.create_owner,
            fixture.create_owner_revision,
            fixture.owner,
            "created_owner",
        )?;
        require_owner_row(&database, fixture, owner.object(), "Ada").await?;

        let exact = ExactTask {
            active: true,
            count: -17,
            amount: 9_000_000_001,
            score: 3.25,
            title: String::from("exact task"),
            payload: vec![0, 1, 255],
            owner: owner.object(),
        };
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &exact)?)
            .await?;
        require_insert_result(
            &task,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let mut identities = BTreeSet::from([task.object()]);
        for index in 1..100_i32 {
            let value = ExactTask {
                active: index % 2 == 0,
                count: index,
                amount: i64::from(index) * 10_000,
                score: f64::from(index) / 4.0,
                title: format!("task {index}"),
                payload: index.to_be_bytes().to_vec(),
                owner: owner.object(),
            };
            let inserted = kernel
                .execute_server_insert(fixture.create_task, &task_arguments(fixture, &value)?)
                .await?;
            require_insert_result(
                &inserted,
                applied.pair(),
                fixture.create_task,
                fixture.create_task_revision,
                fixture.task,
                "created_task",
            )?;
            require(
                identities.insert(inserted.object()),
                "SERVER INSERT allocated a duplicate object identity",
            )?;
        }
        require(
            identities.len() == 100,
            "the 100 committed inserts did not return 100 unique identities",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 100,
            "the private task relation does not contain all 100 committed rows",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private INSERT",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "row execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_value_mutations_preserve_legacy_bind_and_result_behaviour() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let version_one = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let version_two_candidate =
            standard_application_candidate(MUTATION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&version_two_candidate).await?;

        let fixture = Fixture::from_active(&applied)?;
        require_standard_mutation_catalogue(
            &applied,
            fixture,
            upgrade.verified_standard_snapshot(),
        )?;
        let owner = insert_owner(&kernel, fixture, "Ada").await?;
        let original = ExactTask::new(owner.object());
        let inserted = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &original)?)
            .await?;
        require_insert_result(
            &inserted,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, inserted.object(), &original).await?;

        let changed = ExactTask {
            active: true,
            count: -73,
            title: String::from("updated task"),
            ..original.clone()
        };
        let updated = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, inserted.object(), &changed)?,
            )
            .await?;
        require_update_result(&updated, applied.pair(), fixture, inserted.object(), true)?;
        require_task_row(&database, fixture, inserted.object(), &changed).await?;

        let deleted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    inserted.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &deleted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            inserted.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "standard-backed DELETE left the inserted task row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn constructs_stores_and_reads_one_canonical_named_record() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let version_one = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(RECORD_MUTATION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;

        let enum_type = applied
            .catalogue()
            .enum_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.stage")
            .ok_or_else(|| failure("record mutation enum is absent"))?;
        let record = applied
            .catalogue()
            .record_value_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.status")
            .ok_or_else(|| failure("record mutation type is absent"))?;
        let object = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.case")
            .ok_or_else(|| failure("record mutation object is absent"))?;
        let object_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("record mutation object field is absent"))?;
        let create = applied
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.create")
            .ok_or_else(|| failure("record mutation INSERT function is absent"))?;
        let read = applied
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.read")
            .ok_or_else(|| failure("record mutation SELECT function is absent"))?;
        let enabled_parameter = create
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == "p_enabled")
            .ok_or_else(|| failure("record mutation Boolean parameter is absent"))?;
        let stage_parameter = create
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == "p_stage")
            .ok_or_else(|| failure("record mutation enum parameter is absent"))?;
        let stage = EnumValue::new(applied.catalogue(), enum_type.id(), "qualified")?;
        let expected = RuntimeValue::Record(RecordValue::new(
            &applied,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("stage"), RuntimeValue::Enum(stage.clone())),
            ],
        )?);
        let expected_bytes = encode_active_value(&applied, &expected)?;

        let inserted = kernel
            .execute_server_insert(
                create.id(),
                &[
                    FunctionArgument::new(stage_parameter.id(), RuntimeValue::Enum(stage))?,
                    FunctionArgument::new(enabled_parameter.id(), RuntimeValue::Boolean(true))?,
                ],
            )
            .await?;
        require_insert_result(
            &inserted,
            applied.pair(),
            create.id(),
            create.current_revision(),
            object.id(),
            "created",
        )?;

        let session = database.open().await?;
        let stored = session
            .client()
            .query_one(
                &format!(
                    "SELECT {} FROM {} WHERE _orna_object_id = $1",
                    field(object_field.id()),
                    relation(object.id()),
                ),
                &[&inserted.object().to_bytes().to_vec()],
            )
            .await?
            .try_get::<_, Vec<u8>>(0)?;
        session.shutdown().await?;
        require(
            stored == expected_bytes,
            "record INSERT did not store the exact canonical ORV3 bytes",
        )?;

        let selected = kernel.execute_server_select(read.id()).await?;
        let [row] = selected.rows().rows() else {
            return Err(failure("record SELECT did not return exactly one row"));
        };
        let [actual] = row.values() else {
            return Err(failure("record SELECT did not return exactly one value"));
        };
        require(
            selected.pair() == applied.pair()
                && selected.function() == read.id()
                && selected.function_revision() == read.current_revision()
                && actual == &expected,
            "record INSERT and SELECT did not preserve the active nominal value",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn update_returns_zero_or_one_row_and_rolls_back_reference_failures() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let first_owner = insert_owner(&kernel, fixture, "Ada").await?;
        let second_owner = insert_owner(&kernel, fixture, "Grace").await?;
        let original = ExactTask::new(first_owner.object());
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &original)?)
            .await?;

        let changed = ExactTask {
            active: true,
            count: -73,
            amount: original.amount,
            score: original.score,
            title: String::from("updated task"),
            payload: original.payload.clone(),
            owner: second_owner.object(),
        };
        let updated = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, task.object(), &changed)?,
            )
            .await?;
        require_update_result(&updated, applied.pair(), fixture, task.object(), true)?;
        require_task_row(&database, fixture, task.object(), &changed).await?;

        let absent = ObjectId::from_bytes([0xb1; 16]);
        let missing = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, absent, &changed)?,
            )
            .await?;
        require_update_result(&missing, applied.pair(), fixture, absent, false)?;
        require(
            count_rows(&database, fixture.task).await? == 1,
            "updating an absent object changed the target relation",
        )?;

        let invalid_reference = ExactTask {
            owner: ObjectId::from_bytes([0xb2; 16]),
            ..changed.clone()
        };
        let error = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, task.object(), &invalid_reference)?,
            )
            .await
            .expect_err("a missing referenced owner must reject the update");
        require_update_database_failure(&error, applied.pair(), fixture)?;
        require_task_row(&database, fixture, task.object(), &changed).await?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private UPDATE",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "SERVER UPDATE execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn delete_returns_zero_or_one_boolean_and_hides_reference_timing() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let unknown_function = FunctionId::from_bytes([0xdd; 16]);
        let unknown = kernel.execute_server_delete(unknown_function, &[]).await;
        let unknown = match unknown {
            Ok(_) => return Err(failure("unknown DELETE function unexpectedly executed")),
            Err(error) => error,
        };
        require(
            matches!(
                &unknown,
                PostgresKernelError::ServerDelete(ServerDeleteError::FunctionNotActive {
                    pair,
                    function,
                }) if *pair == applied.pair() && *function == unknown_function
            ),
            "unknown DELETE function lost its active pair or typed identity",
        )?;

        let owner = insert_owner(&kernel, fixture, "Ada").await?;
        let exact = ExactTask::new(owner.object());
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &exact)?)
            .await?;

        let wrong_target = [FunctionArgument::new(
            fixture.delete_task_selector_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: owner.object(),
            },
        )?];
        let wrong_target = kernel
            .execute_server_delete(fixture.delete_task, &wrong_target)
            .await;
        let wrong_target = match wrong_target {
            Ok(_) => {
                return Err(failure(
                    "wrong-target DELETE argument unexpectedly executed",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(ServerDeleteError::NotCommitted { context, source }) =
            &wrong_target
        else {
            return Err(failure(
                "wrong-target DELETE argument did not fail before execution",
            ));
        };
        require_context(
            *context,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
        )?;
        require(
            matches!(source.as_ref(), ServerMutationError::Argument { .. }),
            "wrong-target DELETE argument did not fail typed argument validation",
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let restricted = kernel
            .execute_server_delete(
                fixture.delete_owner,
                &delete_argument(
                    fixture.delete_owner_selector_parameter,
                    fixture.owner,
                    owner.object(),
                )?,
            )
            .await;
        let restricted = match restricted {
            Ok(_) => return Err(failure("a referenced owner was unexpectedly deleted")),
            Err(error) => error,
        };
        require_delete_restricted(
            &restricted,
            applied.pair(),
            fixture.delete_owner,
            fixture.delete_owner_revision,
            fixture.owner,
            owner.object(),
            &SqlState::FOREIGN_KEY_VIOLATION,
        )?;
        require_owner_row(&database, fixture, owner.object(), "Ada").await?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let restrict_object = ObjectId::from_bytes([0xc1; 16]);
        let set_null_object = ObjectId::from_bytes([0xc2; 16]);
        let cascade_object = ObjectId::from_bytes([0xc3; 16]);
        insert_reference_fixture_row(
            &database,
            fixture.task_restrict,
            fixture.task_restrict_field,
            restrict_object,
            task.object(),
        )
        .await?;
        insert_reference_fixture_row(
            &database,
            fixture.task_set_null,
            fixture.task_set_null_field,
            set_null_object,
            task.object(),
        )
        .await?;
        insert_reference_fixture_row(
            &database,
            fixture.task_cascade,
            fixture.task_cascade_field,
            cascade_object,
            task.object(),
        )
        .await?;

        let task_restricted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await;
        let task_restricted = match task_restricted {
            Ok(_) => return Err(failure("RESTRICT unexpectedly allowed task deletion")),
            Err(error) => error,
        };
        require_delete_restricted(
            &task_restricted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            &SqlState::RESTRICT_VIOLATION,
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;
        require(
            reference_fixture_value(
                &database,
                fixture.task_set_null,
                fixture.task_set_null_field,
                set_null_object,
            )
            .await?
                == Some(task.object().to_bytes().to_vec()),
            "failed restricted DELETE changed the SET NULL row",
        )?;
        require(
            count_rows(&database, fixture.task_cascade).await? == 1,
            "failed restricted DELETE changed the CASCADE row",
        )?;
        delete_fixture_row(&database, fixture.task_restrict, restrict_object).await?;

        let deleted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &deleted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "matched DELETE left the selected task row",
        )?;
        require(
            reference_fixture_value(
                &database,
                fixture.task_set_null,
                fixture.task_set_null_field,
                set_null_object,
            )
            .await?
            .is_none(),
            "SET NULL did not clear the dependent reference",
        )?;
        require(
            count_rows(&database, fixture.task_cascade).await? == 0,
            "CASCADE did not remove the dependent object",
        )?;

        let absent = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &absent,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            false,
        )?;

        let owner_deleted = kernel
            .execute_server_delete(
                fixture.delete_owner,
                &delete_argument(
                    fixture.delete_owner_selector_parameter,
                    fixture.owner,
                    owner.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &owner_deleted,
            applied.pair(),
            fixture.delete_owner,
            fixture.delete_owner_revision,
            fixture.owner,
            owner.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.owner).await? == 0,
            "owner remained after its dependent task was deleted",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private DELETE",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "SERVER DELETE execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn reference_failures_are_preflight_or_database_integrity_rejections() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let base = ExactTask::new(owner.object());

        let mut wrong_target = task_arguments(fixture, &base)?;
        replace_owner_argument(
            &mut wrong_target,
            fixture,
            RuntimeValue::Reference {
                target: fixture.task,
                object: ObjectId::from_bytes([0x91; 16]),
            },
        )?;
        let wrong_error = kernel
            .execute_server_insert(fixture.create_task, &wrong_target)
            .await
            .expect_err("wrong-target REF must fail before the INSERT");
        require_not_committed_argument_error(
            &wrong_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "wrong-target REF left a task row",
        )?;

        let missing_owner = ObjectId::from_bytes([0x92; 16]);
        let mut nonexistent = task_arguments(fixture, &base)?;
        replace_owner_argument(
            &mut nonexistent,
            fixture,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: missing_owner,
            },
        )?;
        let missing_error = kernel
            .execute_server_insert(fixture.create_task, &nonexistent)
            .await
            .expect_err("missing same-target REF must fail the physical foreign key");
        require_wrapped_database_failure(
            &missing_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            &SqlState::FOREIGN_KEY_VIOLATION,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "foreign-key rejection left a task row",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "reference failures changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unknown_and_tampered_functions_fail_before_the_target_insert() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let unknown = FunctionId::from_bytes([0xa1; 16]);
        let error = kernel
            .execute_server_insert(unknown, &[])
            .await
            .expect_err("unknown function must fail before target INSERT");
        require(
            matches!(
                error,
                PostgresKernelError::ServerInsert(ServerInsertError::FunctionNotActive {
                    pair,
                    function,
                }) if pair == applied.pair() && function == unknown
            ),
            "unknown function did not retain the recovered pair and function identity",
        )?;
        require_unchanged_state(&database, fixture.task, applied.pair(), 0).await?;
        require_no_session_leaks(&database).await
    })
    .await?;

    assert_tamper_rejected_before_insert(Tamper::Artifact).await?;
    assert_tamper_rejected_before_insert(Tamper::Reference).await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn row_and_deferred_trigger_failures_roll_back_with_not_committed_state() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let arguments = task_arguments(fixture, &ExactTask::new(owner.object()))?;

        let after_error = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_task,
            fixture.task,
            &arguments,
            TriggerKind::AfterRow,
            "triggered insert",
        )
        .await?;
        require_wrapped_database_failure(
            &after_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            &SqlState::RAISE_EXCEPTION,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "AFTER INSERT failure left a task row",
        )?;

        let deferred_error = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_task,
            fixture.task,
            &arguments,
            TriggerKind::DeferredConstraint,
            "triggered insert",
        )
        .await?;
        require_commit_rejected(
            &deferred_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "deferred constraint-trigger failure left a task row",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "trigger failures changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn insert_pins_snapshot_while_source_only_apply_advances() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let arguments = task_arguments(fixture, &ExactTask::new(owner.object()))?;
        let source_only = candidate(MUTATION_SOURCE_EDIT, &first)?;
        require(
            source_only.new_function_revisions().is_empty(),
            "source-only edit unexpectedly created a function revision",
        )?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let mut execution = tokio::spawn(async move {
            executor
                .execute_server_insert_with_test_barrier(
                    fixture.create_task,
                    &arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        wait_for_barrier(&mut execution, reached, "snapshot insert", "recovery").await?;

        let advancement = kernel.apply(&source_only).await;
        resume.wait().await;
        let second = match advancement {
            Ok(active) => active,
            Err(error) => {
                abort_and_wait(execution).await;
                return Err(error.into());
            }
        };
        let running = wait_for_success(execution, "snapshot insert").await?;

        require(
            first.pair() != second.pair(),
            "source-only apply did not advance the pair",
        )?;
        require(
            fixture.create_task_revision == function_revision(&second, fixture.create_task)?,
            "source-only apply did not reuse the immutable INSERT function revision",
        )?;
        require_insert_result(
            &running,
            first.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;

        let later = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        require_insert_result(
            &later,
            second.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 2,
            "snapshot test did not commit both task rows",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn delete_pins_snapshot_and_preserves_uncertain_and_committed_outcomes() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let first_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let source_only = candidate(MUTATION_SOURCE_EDIT, &first)?;
        require(
            source_only.new_function_revisions().is_empty(),
            "source-only edit unexpectedly created a function revision",
        )?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let arguments = delete_argument(
            fixture.delete_task_selector_parameter,
            fixture.task,
            first_task.object(),
        )?;
        let mut execution = tokio::spawn(async move {
            executor
                .execute_server_delete_with_test_barrier(
                    fixture.delete_task,
                    &arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        wait_for_barrier(&mut execution, reached, "snapshot delete", "recovery").await?;

        let advancement = kernel.apply(&source_only).await;
        resume.wait().await;
        let second = match advancement {
            Ok(active) => active,
            Err(error) => {
                abort_and_wait(execution).await;
                return Err(error.into());
            }
        };
        let running = wait_for_success(execution, "snapshot delete").await?;
        require(
            first.pair() != second.pair(),
            "source-only apply did not advance the pair",
        )?;
        require(
            fixture.delete_task_revision == function_revision(&second, fixture.delete_task)?,
            "source-only apply did not reuse the immutable DELETE function revision",
        )?;
        require_delete_result(
            &running,
            first.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            first_task.object(),
            true,
        )?;

        let second_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let committed_shutdown = kernel
            .execute_server_delete_with_forced_post_commit_driver_shutdown(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    second_task.object(),
                )?,
            )
            .await;
        let committed_shutdown = match committed_shutdown {
            Ok(_) => {
                return Err(failure(
                    "forced post-commit shutdown unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(
            ServerDeleteError::CommittedButShutdownFailed { result, .. },
        ) = committed_shutdown
        else {
            return Err(failure(
                "post-commit shutdown did not retain the confirmed DELETE result",
            ));
        };
        require_delete_result(
            &result,
            second.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            second_task.object(),
            true,
        )?;

        let rejected_task_value = ExactTask::new(owner.object());
        let rejected_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &rejected_task_value)?,
            )
            .await?;
        let rejected = execute_delete_with_installed_trigger(
            &database,
            &kernel,
            fixture,
            rejected_task.object(),
        )
        .await?;
        require_delete_commit_rejected(
            &rejected,
            second.pair(),
            fixture,
            rejected_task.object(),
        )?;
        require_task_row(
            &database,
            fixture,
            rejected_task.object(),
            &rejected_task_value,
        )
        .await?;
        kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    rejected_task.object(),
                )?,
            )
            .await?;

        let third_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let arguments = delete_argument(
            fixture.delete_task_selector_parameter,
            fixture.task,
            third_task.object(),
        )?;
        let (proxy_config, proxy) = start_commit_drop_proxy(&database).await?;
        let proxy_kernel = PostgresKernel::new(proxy_config);
        let uncertain = proxy_kernel
            .execute_server_delete(fixture.delete_task, &arguments)
            .await;
        wait_for_proxy(proxy).await?;
        let uncertain = match uncertain {
            Ok(_) => {
                return Err(failure(
                    "withheld DELETE commit confirmation unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(ServerDeleteError::CommitOutcomeUnknown {
            context,
            target,
            selector,
            matched,
            ..
        }) = &uncertain
        else {
            return Err(failure(
                "withheld DELETE confirmation did not retain its uncertain outcome",
            ));
        };
        require_context(
            *context,
            second.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
        )?;
        require(*target == fixture.task, "uncertain delete target differs")?;
        require(
            *selector == third_task.object(),
            "uncertain delete selector differs",
        )?;
        require(*matched, "uncertain delete lost its match state")?;
        require(
            uncertain.to_string()
                == format!(
                    "object deletion failed: the connection failed while deleting object {}; it is not known whether the delete committed; do not retry automatically",
                    third_task.object().canonical(),
                ),
            "uncertain delete lost its no-retry warning",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "DELETE outcome tests left an unexpected task row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn confirmed_commit_retains_full_result_when_driver_shutdown_fails() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let exact = ExactTask::new(owner.object());

        let error = kernel
            .execute_server_insert_with_forced_post_commit_driver_shutdown(
                fixture.create_task,
                &task_arguments(fixture, &exact)?,
            )
            .await
            .expect_err("forced post-commit shutdown must retain committed outcome");
        require(
            matches!(
                &error,
                PostgresKernelError::ServerInsert(insert)
                    if insert.commit_state() == ServerInsertCommitState::Committed
            ),
            "post-confirmed-commit shutdown failure has the wrong commit state",
        )?;
        let PostgresKernelError::ServerInsert(ServerInsertError::CommittedButShutdownFailed {
            result,
            ..
        }) = error
        else {
            return Err(failure(
                "post-confirmed-commit error did not retain the committed result",
            ));
        };
        require_insert_result(
            &result,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, result.object(), &exact).await?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn withheld_commit_confirmation_is_unknown_but_the_row_exists_once() -> TestResult<()> {
    with_test_database(|database| async move {
        let direct_kernel = kernel(&database)?;
        direct_kernel.bootstrap().await?;
        let empty = direct_kernel.recover().await?;
        let applied = direct_kernel
            .apply(&candidate(MUTATION_SOURCE, &empty)?)
            .await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&direct_kernel, fixture, "owner").await?;
        let exact = ExactTask::new(owner.object());
        let arguments = task_arguments(fixture, &exact)?;
        let (proxy_config, proxy) = start_commit_drop_proxy(&database).await?;
        let proxy_kernel = PostgresKernel::new(proxy_config);

        let outcome = proxy_kernel
            .execute_server_insert(fixture.create_task, &arguments)
            .await;
        wait_for_proxy(proxy).await?;
        let error = match outcome {
            Ok(_) => {
                return Err(failure(
                    "withheld COMMIT confirmation unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        require(
            matches!(
                &error,
                PostgresKernelError::ServerInsert(insert)
                    if insert.commit_state() == ServerInsertCommitState::Unknown
            ),
            "withheld COMMIT confirmation has the wrong commit state",
        )?;
        let PostgresKernelError::ServerInsert(ServerInsertError::CommitOutcomeUnknown {
            context,
            target,
            candidate,
            ..
        }) = &error
        else {
            return Err(failure(
                "withheld COMMIT confirmation did not retain the unknown candidate",
            ));
        };
        require_context(
            *context,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
        )?;
        require(*target == fixture.task, "unknown commit target differs")?;
        require(
            error.to_string()
                == format!(
                    "row creation failed: the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                    candidate.canonical(),
                ),
            "unknown commit error lost its no-retry warning",
        )?;
        require_task_row(&database, fixture, *candidate, &exact).await?;
        require(
            count_rows(&database, fixture.task).await? == 1,
            "unknown commit outcome did not leave exactly one durable row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn required_unique_reference_conflicts_are_typed_and_transactional() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel
            .apply(&candidate(UNIQUE_REFERENCE_SOURCE, &empty)?)
            .await?;
        let fixture = UniqueReferenceFixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.assignment).await?;

        let claimed_owner = insert_unique_owner(&kernel, fixture, "claimed").await?;
        let other_owner = insert_unique_owner(&kernel, fixture, "other").await?;
        let concurrent_owner = insert_unique_owner(&kernel, fixture, "concurrent").await?;
        let claimed =
            insert_assignment(&kernel, fixture, claimed_owner.object(), "claimed").await?;
        require_unique_insert_result(
            &claimed,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
            "created_assignment",
        )?;

        let duplicate_insert = match kernel
            .execute_server_insert(
                fixture.create_assignment,
                &assignment_arguments(fixture, claimed_owner.object(), "duplicate")?,
            )
            .await
        {
            Ok(_) => {
                return Err(failure(
                    "a second required unique reference INSERT unexpectedly committed",
                ));
            }
            Err(error) => error,
        };
        require_unique_insert_conflict(
            &duplicate_insert,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed",
        )
        .await?;

        let other = insert_assignment(&kernel, fixture, other_owner.object(), "other").await?;
        require_unique_insert_result(
            &other,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
            "created_assignment",
        )?;
        let duplicate_update = match kernel
            .execute_server_update(
                fixture.update_assignment,
                &assignment_update_arguments(
                    fixture,
                    other.object(),
                    claimed_owner.object(),
                    "duplicate update",
                )?,
            )
            .await
        {
            Ok(_) => {
                return Err(failure(
                    "an UPDATE assigning an already used reference unexpectedly committed",
                ));
            }
            Err(error) => error,
        };
        require_unique_update_conflict(&duplicate_update, applied.pair(), fixture)?;
        require_assignment_row(
            &database,
            fixture,
            other.object(),
            other_owner.object(),
            "other",
        )
        .await?;

        let self_update = kernel
            .execute_server_update(
                fixture.update_assignment,
                &assignment_update_arguments(
                    fixture,
                    claimed.object(),
                    claimed_owner.object(),
                    "claimed again",
                )?,
            )
            .await?;
        require_unique_update_result(
            &self_update,
            applied.pair(),
            fixture,
            claimed.object(),
            true,
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed again",
        )
        .await?;

        let unrelated = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_assignment,
            fixture.assignment,
            &assignment_arguments(fixture, concurrent_owner.object(), "unrelated")?,
            TriggerKind::UnrelatedUniqueViolation,
            "unrelated unique INSERT",
        )
        .await?;
        require_unrelated_unique_insert_failure(
            &unrelated,
            applied.pair(),
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        require(
            count_rows(&database, fixture.assignment).await? == 2,
            "the unrelated unique violation changed the persisted assignment set",
        )?;

        let first_reached = Arc::new(tokio::sync::Barrier::new(2));
        let first_resume = Arc::new(tokio::sync::Barrier::new(2));
        let second_reached = Arc::new(tokio::sync::Barrier::new(2));
        let second_resume = Arc::new(tokio::sync::Barrier::new(2));
        let first_kernel = kernel.clone();
        let first_arguments = assignment_arguments(fixture, concurrent_owner.object(), "first")?;
        let first_execution_reached = first_reached.clone();
        let first_execution_resume = first_resume.clone();
        let mut first = tokio::spawn(async move {
            first_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create_assignment,
                    &first_arguments,
                    first_execution_reached,
                    first_execution_resume,
                )
                .await
        });
        let second_kernel = kernel.clone();
        let second_arguments = assignment_arguments(fixture, concurrent_owner.object(), "second")?;
        let second_execution_reached = second_reached.clone();
        let second_execution_resume = second_resume.clone();
        let mut second = tokio::spawn(async move {
            second_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create_assignment,
                    &second_arguments,
                    second_execution_reached,
                    second_execution_resume,
                )
                .await
        });
        if let Err(error) =
            wait_for_barrier(&mut first, first_reached, "first unique claim", "recovery").await
        {
            abort_and_wait(second).await;
            return Err(error);
        }
        if let Err(error) = wait_for_barrier(
            &mut second,
            second_reached,
            "second unique claim",
            "recovery",
        )
        .await
        {
            abort_and_wait(first).await;
            return Err(error);
        }
        let (first_release, second_release) = tokio::join!(
            wait_for_barrier(&mut first, first_resume, "first unique claim", "resume"),
            wait_for_barrier(&mut second, second_resume, "second unique claim", "resume",),
        );
        match (first_release, second_release) {
            (Ok(()), Ok(())) => {}
            (Err(first_error), Ok(())) => {
                abort_and_wait(second).await;
                return Err(first_error);
            }
            (Ok(()), Err(second_error)) => {
                abort_and_wait(first).await;
                return Err(second_error);
            }
            (Err(first_error), Err(second_error)) => {
                return Err(failure(format!(
                    "both unique claim releases failed: {first_error}; {second_error}"
                )));
            }
        }
        let (first_outcome, second_outcome) = tokio::join!(
            wait_for_outcome(first, "first unique claim"),
            wait_for_outcome(second, "second unique claim"),
        );
        let first_outcome = first_outcome?;
        let second_outcome = second_outcome?;
        let outcomes = [first_outcome, second_outcome];
        let successes = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        require(
            successes == 1,
            "concurrent claims did not yield exactly one success",
        )?;
        for error in outcomes.iter().filter_map(|outcome| outcome.as_ref().err()) {
            require_unique_insert_conflict(
                error,
                applied.pair(),
                fixture,
                fixture.create_assignment,
                fixture.create_assignment_revision,
            )?;
        }
        let concurrent_label =
            assignment_label_for_owner(&database, fixture, concurrent_owner.object()).await?;
        require(
            matches!(concurrent_label.as_str(), "first" | "second"),
            "the concurrent winner stored an unexpected assignment value",
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed again",
        )
        .await?;
        require_assignment_row(
            &database,
            fixture,
            other.object(),
            other_owner.object(),
            "other",
        )
        .await?;

        require(
            count_rows(&database, fixture.assignment).await? == 3,
            "unique conflicts changed the persisted assignment set",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.assignment).await? == 0,
            "hostile public search_path redirected a unique-reference mutation",
        )?;
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == applied.pair(),
            "unique conflicts changed the active pair",
        )?;
        require(
            function_revision(&recovered, fixture.create_assignment)?
                == fixture.create_assignment_revision
                && function_revision(&recovered, fixture.update_assignment)?
                    == fixture.update_assignment_revision
                && function_revision(&recovered, fixture.create_owner)?
                    == fixture.create_owner_revision,
            "unique conflicts changed immutable function revisions",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unique_text_conflicts_are_typed_and_transactional() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel
            .apply(&candidate(UNIQUE_TEXT_SOURCE, &empty)?)
            .await?;
        let fixture = UniqueTextFixture::from_active(&applied)?;

        let first_null = insert_unique_text_null(&kernel, fixture, "required-null-a").await?;
        let second_null = insert_unique_text_null(&kernel, fixture, "required-null-b").await?;
        require(
            first_null.object() != second_null.object(),
            "nullable unique Text did not permit two NULL values",
        )?;

        let claimed = insert_unique_text(
            &kernel,
            fixture,
            RuntimeValue::Text("nullable".into()),
            "exact",
        )
        .await?;
        let duplicate_insert = kernel
            .execute_server_insert(
                fixture.create,
                &unique_text_arguments(fixture, RuntimeValue::Text("other".into()), "exact")?,
            )
            .await
            .expect_err("an exact required unique Text INSERT must conflict");
        require_unique_text_insert_conflict(
            &duplicate_insert,
            applied.pair(),
            fixture,
            fixture.required_field,
        )?;
        require_unique_text_row(
            &database,
            fixture,
            claimed.object(),
            Some("nullable"),
            "exact",
        )
        .await?;

        let duplicate_nullable = kernel
            .execute_server_insert(
                fixture.create,
                &unique_text_arguments(
                    fixture,
                    RuntimeValue::Text("nullable".into()),
                    "nullable-conflict",
                )?,
            )
            .await
            .expect_err("an exact nullable unique Text INSERT must conflict");
        require_unique_text_insert_conflict(
            &duplicate_nullable,
            applied.pair(),
            fixture,
            fixture.nullable_field,
        )?;

        for value in ["", "Exact", "exact ", "exact\n", "e\u{301}", "\u{00e9}"] {
            insert_unique_text(&kernel, fixture, RuntimeValue::Text(value.into()), value).await?;
        }

        let update_target = insert_unique_text(
            &kernel,
            fixture,
            RuntimeValue::Text("update".into()),
            "update-before",
        )
        .await?;
        let duplicate_update = kernel
            .execute_server_update(
                fixture.update,
                &unique_text_update_arguments(fixture, update_target.object(), "exact")?,
            )
            .await
            .expect_err("a selector/value UPDATE to an exact Text value must conflict");
        require_unique_text_update_conflict(&duplicate_update, applied.pair(), fixture)?;
        require_unique_text_row(
            &database,
            fixture,
            update_target.object(),
            Some("update"),
            "update-before",
        )
        .await?;

        kernel
            .execute_server_update(
                fixture.update,
                &unique_text_update_arguments(fixture, claimed.object(), "exact")?,
            )
            .await?;
        require_unique_text_row(
            &database,
            fixture,
            claimed.object(),
            Some("nullable"),
            "exact",
        )
        .await?;

        let unrelated = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create,
            fixture.claim,
            &unique_text_arguments(fixture, RuntimeValue::Text("unrelated".into()), "unrelated")?,
            TriggerKind::UnrelatedUniqueViolation,
            "unrelated unique Text INSERT",
        )
        .await?;
        require_unrelated_unique_insert_failure(
            &unrelated,
            applied.pair(),
            fixture.create,
            fixture.create_revision,
        )?;

        let first_reached = Arc::new(tokio::sync::Barrier::new(2));
        let first_resume = Arc::new(tokio::sync::Barrier::new(2));
        let second_reached = Arc::new(tokio::sync::Barrier::new(2));
        let second_resume = Arc::new(tokio::sync::Barrier::new(2));
        let first_kernel = kernel.clone();
        let first_arguments = unique_text_arguments(
            fixture,
            RuntimeValue::Text("concurrent-a".into()),
            "concurrent",
        )?;
        let first_execution_reached = first_reached.clone();
        let first_execution_resume = first_resume.clone();
        let mut first = tokio::spawn(async move {
            first_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create,
                    &first_arguments,
                    first_execution_reached,
                    first_execution_resume,
                )
                .await
        });
        let second_kernel = kernel.clone();
        let second_arguments = unique_text_arguments(
            fixture,
            RuntimeValue::Text("concurrent-b".into()),
            "concurrent",
        )?;
        let second_execution_reached = second_reached.clone();
        let second_execution_resume = second_resume.clone();
        let mut second = tokio::spawn(async move {
            second_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create,
                    &second_arguments,
                    second_execution_reached,
                    second_execution_resume,
                )
                .await
        });
        wait_for_barrier(
            &mut first,
            first_reached,
            "first unique Text claim",
            "recovery",
        )
        .await?;
        wait_for_barrier(
            &mut second,
            second_reached,
            "second unique Text claim",
            "recovery",
        )
        .await?;
        let (first_release, second_release) = tokio::join!(
            wait_for_barrier(
                &mut first,
                first_resume,
                "first unique Text claim",
                "resume"
            ),
            wait_for_barrier(
                &mut second,
                second_resume,
                "second unique Text claim",
                "resume"
            ),
        );
        first_release?;
        second_release?;
        let (first_outcome, second_outcome) = tokio::join!(
            wait_for_outcome(first, "first unique Text claim"),
            wait_for_outcome(second, "second unique Text claim"),
        );
        let outcomes = [first_outcome?, second_outcome?];
        require(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count() == 1,
            "concurrent Text claims did not yield exactly one success",
        )?;
        for error in outcomes.iter().filter_map(|outcome| outcome.as_ref().err()) {
            require_unique_text_insert_conflict(
                error,
                applied.pair(),
                fixture,
                fixture.required_field,
            )?;
        }
        require(
            count_rows(&database, fixture.claim).await? == 11,
            "unique Text conflict changed the persisted set",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}
