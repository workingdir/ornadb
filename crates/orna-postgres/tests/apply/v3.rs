use super::*;

// Work ADR 0059 implementation order item 4: the live production-path V3
// install proof. The tests below prove that a fresh database installs V1,
// upgrades to V2, and upgrades to V3 through the normal compiler-backed
// pipeline (`prepare_standard_upgrade_v2_to_v3` + `apply_standard_upgrade`,
// never a test-hooks fixture), that the active revision reopens pinned to
// `orna.std/3` with the exact V3 snapshot facts, that the V1 and V2 pins
// from the earlier activations remain in the historical revision records,
// that tampered V3 standard rows fail recovery closed without changing prior
// history, and that the sealed `sys.invoke` echo dogfooding proof runs
// against the V3-pinned active revision.

const V3_PROOF_CLIENT_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
const V3_PROOF_CLIENT_ROLE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
const V3_PROOF_CLIENT_ROLE_SECOND: PrincipalId = PrincipalId::from_bytes([0x73; 16]);
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

/// Installs the complete production standard chain on a fresh database:
/// the empty base, the V1 application activation, the V1-to-V2 upgrade
/// through `prepare_standard_upgrade_v1_to_v2` + `apply_standard_upgrade`,
/// and the V2-to-V3 upgrade through `prepare_standard_upgrade_v2_to_v3` +
/// `apply_standard_upgrade`.
struct V3StandardChain {
    version_one: ActiveDatabaseRevision,
    version_two: ActiveDatabaseRevision,
    version_three: ActiveDatabaseRevision,
    version_three_upgrade: orna_standard::StandardUpgrade,
}

async fn install_v3_standard_chain(database: &TestDatabase) -> TestResult<V3StandardChain> {
    let kernel = kernel(database)?;
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
    let version_one = kernel.apply(&version_one_candidate).await?;

    let version_two_upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&version_one)
        .map_err(|error| failure(format!("V1-to-V2 upgrade preparation failed: {error}")))?;
    let version_two = kernel.apply_standard_upgrade(&version_two_upgrade).await?;
    require(
        version_two.catalogue_hash_context().version() == CatalogueHashVersion::Version2
            && version_two
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision())
                == Some(STANDARD_LIBRARY_V2_REVISION_ID),
        "the V1-to-V2 upgrade did not install a version-two context pinned to orna.std/2",
    )?;
    require_standard_context(
        &version_two,
        version_two_upgrade.verified_standard_snapshot(),
    )?;

    let version_three_upgrade = orna_standard::prepare_standard_upgrade_v2_to_v3(&version_two)
        .map_err(|error| failure(format!("V2-to-V3 upgrade preparation failed: {error}")))?;
    let version_three = kernel
        .apply_standard_upgrade(&version_three_upgrade)
        .await?;
    Ok(V3StandardChain {
        version_one,
        version_two,
        version_three,
        version_three_upgrade,
    })
}

fn user_state_plan_candidate(
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let candidate =
        standard_application_candidate(STANDARD_APPLICATION_SOURCE_EDIT, active, upgrade)?;
    let function = candidate
        .candidate()
        .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
            "app", "enabled",
        ])?)
        .ok_or_else(|| failure("existing CLIENT fixture did not contain app.enabled"))?
        .id();
    let revision = candidate
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == function)
        .ok_or_else(|| failure("existing CLIENT fixture revision did not persist"))?;
    let slot = StateSlotId::from_bytes([0xa5; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: false },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let payload = plan
        .encode()
        .map_err(|error| failure(format!("USER state plan encoding failed: {error}")))?;
    let content_hash = orna_core::canonical_hash::artifact_payload_digest(&payload)?;
    let artifact = orna_core::revision::ExecutableArtifact::new(
        orna_core::revision::ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        orna_artifact::client_plan::STATE_FORMAT_VERSION,
        payload,
        content_hash,
    )?;
    let function_definition = candidate
        .candidate()
        .function_by_id(function)
        .ok_or_else(|| failure("USER state fixture function declaration disappeared"))?;
    let function_references = candidate
        .references()
        .iter()
        .filter(|reference| reference.source_function() == function)
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = orna_core::canonical_hash::function_semantic_digest_with_version(
        revision.semantic_hash_version(),
        function_definition,
        revision.language_version(),
        &artifact,
        candidate.expressions(),
        &function_references,
    )?;
    let replacement = orna_core::revision::FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        semantic_hash,
        revision.language_version(),
        artifact,
    )
    .map_err(|error| {
        failure(format!(
            "USER state function revision rebuild failed: {error}"
        ))
    })?
    .with_semantic_hash_version(revision.semantic_hash_version());
    let new_revisions = candidate
        .new_function_revisions()
        .iter()
        .map(|item| {
            if item.function() == function {
                replacement.clone()
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>();
    let current_revisions = candidate
        .current_function_revisions()
        .ok_or_else(|| failure("V3 application candidate omitted current function revisions"))?
        .iter()
        .map(|item| {
            if item.function() == function {
                replacement.clone()
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        candidate.catalogue_hash_context(),
        candidate.candidate(),
        &current_revisions,
        candidate.expressions(),
        candidate.origins(),
        candidate.references(),
    )?;
    let content = DeployableRevisionContent::new(
        candidate.origins().to_vec(),
        candidate.expressions().to_vec(),
        new_revisions,
        candidate.references().to_vec(),
    )
    .with_current_function_revisions(current_revisions);
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        orna_core::revision::DeployableRevisionInput::new(
            candidate.expected_base(),
            candidate.source().clone(),
            candidate.parent_catalogue(),
            candidate.candidate().clone(),
            catalogue_hash,
            content,
        ),
        candidate.catalogue_hash_context().clone(),
    )?)
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_public_user_state_profiles_and_atomic_conflict_batch() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("v3-user-state-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| failure(format!("V3 user-state runtime failed: {error}")))?;
            runtime.block_on(proves_public_user_state_profiles_and_atomic_conflict_batch_inner())
        })
        .map_err(|error| failure(format!("V3 user-state thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("V3 user-state thread panicked")),
    }
}

async fn proves_public_user_state_profiles_and_atomic_conflict_batch_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        let default_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let named_change = UserStateChange::new(
            function,
            "blue".to_owned(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        let default_key = default_change.key_without_principal();
        let named_key = named_change.key_without_principal();
        let seeded = kernel
            .write_user_state(&session, &[default_change, named_change])
            .await?;
        require(
            seeded.len() == 2
                && seeded[0].key() == &default_key
                && seeded[1].key() == &named_key
                && seeded[0].outcome() == UserStateWriteOutcome::Written { revision: 1 }
                && seeded[1].outcome() == UserStateWriteOutcome::Written { revision: 1 },
            "initial USER state write did not return exact ordered keys and revisions",
        )?;
        let initial_audits = kernel.recover_security_audit_events().await?;
        let write_audits = initial_audits
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(2)
            })
            .count();
        require(
            write_audits == 1,
            "successful USER state batch did not record one write audit",
        )?;

        let default_cells = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        let named_cells = kernel
            .load_user_state(&session, function, "blue", &[], &expected_types)
            .await?;
        require(
            default_cells.len() == 1
                && default_cells[0].key().without_principal() == default_key
                && default_cells[0].revision() == 1
                && default_cells[0].value() == &RuntimeValue::Boolean(true)
                && named_cells.len() == 1
                && named_cells[0].key().without_principal() == named_key
                && named_cells[0].revision() == 1
                && named_cells[0].value() == &RuntimeValue::Boolean(false),
            "default and named USER state profile loads did not return persisted cells",
        )?;
        let profile_load_audits = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Load)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            profile_load_audits == 2,
            "default and named USER state loads did not record two redacted load audits",
        )?;

        let revisioned_default = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        let revised = kernel
            .write_user_state(&session, &[revisioned_default])
            .await?;
        require(
            revised.len() == 1
                && revised[0].key() == &default_key
                && revised[0].outcome() == UserStateWriteOutcome::Written { revision: 2 },
            "USER state successor write did not advance the revision to two",
        )?;
        let revised_default_cells = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        require(
            revised_default_cells.len() == 1
                && revised_default_cells[0].key().without_principal() == default_key
                && revised_default_cells[0].revision() == 2
                && revised_default_cells[0].value() == &RuntimeValue::Boolean(false),
            "USER state successor load did not return revision two",
        )?;
        let audit_count_before_conflict = kernel.recover_security_audit_events().await?.len();
        let stale_default = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let fresh_named = UserStateChange::new(
            function,
            "blue".to_owned(),
            function,
            String::new(),
            slot,
            Some(1),
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let stale_default_key = stale_default.key_without_principal();
        let fresh_named_key = fresh_named.key_without_principal();
        let conflicts = kernel
            .write_user_state(&session, &[stale_default, fresh_named])
            .await?;
        require(
            conflicts.len() == 2
                && conflicts[0].key() == &stale_default_key
                && conflicts[1].key() == &fresh_named_key
                && conflicts[0].outcome()
                    == UserStateWriteOutcome::Conflict {
                        current_revision: 2,
                    }
                && conflicts[1].outcome()
                    == UserStateWriteOutcome::Conflict {
                        current_revision: 1,
                    },
            "mixed USER state conflict did not return exact ordered per-key results",
        )?;
        let audits_after_conflict = kernel.recover_security_audit_events().await?;
        let write_audits_after_conflict = audits_after_conflict
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
            })
            .count();
        require(
            audits_after_conflict.len() == audit_count_before_conflict + 1
                && write_audits_after_conflict == 3,
            "mixed USER state conflict did not record its redacted write audit",
        )?;
        let default_after = kernel
            .load_user_state(&session, function, "", &[], &expected_types)
            .await?;
        let named_after = kernel
            .load_user_state(&session, function, "blue", &[], &expected_types)
            .await?;
        require(
            default_after.len() == 1
                && default_after[0].key().without_principal() == default_key
                && default_after[0].revision() == 2
                && default_after[0].value() == &RuntimeValue::Boolean(false)
                && named_after.len() == 1
                && named_after[0].key().without_principal() == named_key
                && named_after[0].revision() == 1
                && named_after[0].value() == &RuntimeValue::Boolean(false),
            "mixed USER state conflict changed persisted cells",
        )?;
        Ok(())
    })
    .await
}

/// Proves the active-revision lock serialises concurrent missing-cell writes
/// before persistence. One writer commits revision one and the other observes
/// the committed cell, returns the accepted ORNA0902 conflict, and appends its
/// redacted audit without leaking a database uniqueness error.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn concurrent_missing_user_state_writes_return_one_write_and_one_conflict() -> TestResult<()>
{
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let setup_kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = setup_kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("concurrent USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = setup_kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = setup_kernel.replace_security_snapshot(&security).await?;
        let first_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let second_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let first_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let second_change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(false),
            value_type,
        )?;
        require(
            first_change.expected_revision().is_none()
                && second_change.expected_revision().is_none(),
            "concurrent USER state writers did not carry expected_revision=None",
        )?;
        let expected_key = first_change.key_without_principal();
        require(
            expected_key == second_change.key_without_principal(),
            "concurrent USER state writers did not target one missing cell",
        )?;
        let first_kernel = named_kernel(&database, "orna-user-state-race-a")?;
        let second_kernel = named_kernel(&database, "orna-user-state-race-b")?;
        let first_task = tokio::spawn(async move {
            first_kernel
                .write_user_state(&first_session, std::slice::from_ref(&first_change))
                .await
        });
        let second_task = tokio::spawn(async move {
            second_kernel
                .write_user_state(&second_session, std::slice::from_ref(&second_change))
                .await
        });


        let (first_join, second_join) = tokio::time::timeout(
            APPLY_TIMEOUT,
            async { tokio::join!(first_task, second_task) },
        )
        .await
        .map_err(|_| failure("timed out waiting for concurrent USER state writers"))?;
        let first_results = first_join
            .map_err(|error| failure(format!("first USER state writer task failed: {error}")))??;
        let second_results = second_join
            .map_err(|error| failure(format!("second USER state writer task failed: {error}")))??;
        require(
            first_results.len() == 1
                && second_results.len() == 1
                && first_results[0].key() == &expected_key
                && second_results[0].key() == &expected_key,
            "concurrent USER state writes did not return one aligned result per batch",
        )?;
        let outcomes = [first_results[0].outcome(), second_results[0].outcome()];
        require(
            outcomes
                .iter()
                .filter(|outcome| **outcome == UserStateWriteOutcome::Written { revision: 1 })
                .count()
                == 1
                && outcomes
                    .iter()
                    .filter(|outcome| {
                        **outcome == UserStateWriteOutcome::Conflict {
                            current_revision: 1,
                        }
                    })
                    .count()
                    == 1,
            "concurrent USER state writes did not return one Written(1) and one ORNA0902 Conflict(1)",
        )?;

        let final_kernel = kernel(&database)?;
        let final_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let final_cells = final_kernel
            .load_user_state(
                &final_session,
                function,
                "",
                &[],
                &expected_types,
            )
            .await?;
        require(
            final_cells.len() == 1
                && final_cells[0].key().without_principal() == expected_key
                && final_cells[0].revision() == 1
                && matches!(final_cells[0].value(), RuntimeValue::Boolean(_)),
            "concurrent USER state writes left anything other than one revision-one cell",
        )?;

        let inspection = database.open().await?;
        let principal_bytes = V3_PROOF_CLIENT_USER.to_bytes().to_vec();
        let function_bytes = function.to_bytes().to_vec();
        let slot_bytes = slot.to_bytes().to_vec();
        let row = inspection
            .client()
            .query_one(
                "SELECT COUNT(*)::BIGINT, COALESCE(MAX(revision), 0)::BIGINT
                 FROM _orna_kernel.user_state_cells
                 WHERE principal_id = $1
                   AND root_function_id = $2
                   AND root_state_profile = ''
                   AND function_id = $2
                   AND function_instance_key = ''
                   AND state_slot_id = $3",
                &[&principal_bytes, &function_bytes, &slot_bytes],
            )
            .await?;
        let row_count: i64 = row.try_get(0)?;
        let max_revision: i64 = row.try_get(1)?;
        inspection.shutdown().await?;
        require(
            row_count == 1 && max_revision == 1,
            "concurrent USER state writes left partial or duplicate durable rows",
        )?;

        let write_audits = final_kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            write_audits == 2,
            "concurrent USER state writes did not leave two redacted write audits",
        )?;
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn user_state_write_linearizes_before_concurrent_security_replacement() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let setup_kernel = kernel(&database)?;
        let application =
            user_state_plan_candidate(&chain.version_three, &chain.version_three_upgrade)?;
        let active = setup_kernel.apply(&application).await?;
        let function = active
            .catalogue()
            .function_by_name(&orna_core::catalogue::QualifiedSemanticName::new([
                "app", "enabled",
            ])?)
            .ok_or_else(|| failure("linearization USER state proof function did not persist"))?
            .id();
        let slot = StateSlotId::from_bytes([0xa5; 16]);
        let value_type = orna_standard::BOOLEAN_TYPE_ID;
        let expected_types = BTreeMap::from([((function, slot), value_type)]);
        let recovered_security = setup_kernel.recover_security_snapshot().await?;
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            recovered_security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        let security = setup_kernel.replace_security_snapshot(&security).await?;
        let retained_session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;
        let change = UserStateChange::new(
            function,
            String::new(),
            function,
            String::new(),
            slot,
            None,
            RuntimeValue::Boolean(true),
            value_type,
        )?;
        let expected_key = change.key_without_principal();
        let disabled = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            security.function_targets().collect(),
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Disabled,
            )],
            vec![],
            vec![],
        )?;
        let initial_audits = setup_kernel.recover_security_audit_events().await?;
        let initial_audit_count = initial_audits.len();
        let initial_write_audits = initial_audits
            .iter()
            .filter(|event| {
                let decision = event.decision();
                decision.kind() == SecurityAuditKind::UserState
                    && decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                    && decision.user_state_root_function() == Some(function)
                    && decision.user_state_cell_count() == Some(1)
            })
            .count();
        require(
            initial_write_audits == 0,
            "linearization USER state fixture unexpectedly wrote an audit before the race",
        )?;

        install_user_state_insert_pause_trigger(&database).await?;
        let race_result: TestResult<()> = async {
            let coordinator = database.open().await?;
            coordinator
                .client()
                .query_one(
                    "SELECT pg_advisory_lock($1)",
                    &[&USER_STATE_INSERT_RACE_LOCK_KEY],
                )
                .await?;

            let writer_kernel =
                named_kernel(&database, "orna-user-state-linearization-writer")?;
            let writer_session = retained_session.clone();
            let writer_change = change.clone();
            let mut writer_task = Some(tokio::spawn(async move {
                writer_kernel
                    .write_user_state(
                        &writer_session,
                        std::slice::from_ref(&writer_change),
                    )
                    .await
            }));
            if let Err(error) =
                wait_for_advisory_wait(&database, "orna-user-state-linearization-writer").await
            {
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(error);
            }

            let replacement_kernel =
                named_kernel(&database, "orna-user-state-linearization-replacement")?;
            let replacement_snapshot = disabled.clone();
            let mut replacement_task = Some(tokio::spawn(async move {
                replacement_kernel
                    .replace_security_snapshot(&replacement_snapshot)
                    .await
            }));
            if let Err(error) = wait_for_active_lock_block(
                &database,
                "orna-user-state-linearization-writer",
                "orna-user-state-linearization-replacement",
            )
            .await
            {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(error);
            }

            if let Err(error) = coordinator
                .client()
                .query_one(
                    "SELECT pg_advisory_unlock($1)",
                    &[&USER_STATE_INSERT_RACE_LOCK_KEY],
                )
                .await
            {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                let _ = coordinator.shutdown().await;
                return Err(Box::new(error));
            }
            if let Err(error) = coordinator.shutdown().await {
                abort_kernel_task(replacement_task.take()).await;
                abort_kernel_task(writer_task.take()).await;
                return Err(error);
            }

            let writer_task = writer_task
                .take()
                .ok_or_else(|| failure("linearization USER state writer task disappeared"))?;
            let replacement_task = replacement_task.take().ok_or_else(|| {
                failure("linearization security replacement task disappeared")
            })?;
            let (writer_join, replacement_join) = tokio::join!(
                wait_for_kernel_task(writer_task, "linearization USER state writer"),
                wait_for_kernel_task(replacement_task, "linearization security replacement"),
            );
            let writer_results = writer_join?;
            let replacement_snapshot = replacement_join?;
            let writer_results = writer_results?;
            let replacement_snapshot = replacement_snapshot?;

            // The replacement was observed waiting on the writer's active
            // revision lock; joining the writer first makes the commit order
            // explicit before the replacement result is accepted.
            require(
                writer_results.len() == 1
                    && writer_results[0].key() == &expected_key
                    && writer_results[0].outcome()
                        == UserStateWriteOutcome::Written { revision: 1 },
                "linearized USER state writer did not commit exactly one revision-one result",
            )?;
            let replacement_status = replacement_snapshot
                .principals()
                .find(|principal| principal.id() == V3_PROOF_CLIENT_USER)
                .map(|principal| principal.status());
            require(
                replacement_snapshot.revision() == active.pair()
                    && replacement_status == Some(PrincipalStatus::Disabled),
                "concurrent security replacement did not commit the disabled principal",
            )?;

            let final_kernel = kernel(&database)?;
            let after_replacement_audits =
                final_kernel.recover_security_audit_events().await?;
            let after_replacement_write_audits = after_replacement_audits
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::UserState
                        && decision.outcome() == SecurityAuditOutcome::Allowed
                        && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                        && decision.user_state_root_function() == Some(function)
                        && decision.user_state_cell_count() == Some(1)
                })
                .count();
            require(
                after_replacement_audits.len() == initial_audit_count + 1
                    && after_replacement_write_audits == initial_write_audits + 1,
                "security replacement left a stale or duplicate allowed USER state audit",
            )?;

            let recovered_disabled = final_kernel.recover_security_snapshot().await?;
            let recovered_status = recovered_disabled
                .principals()
                .find(|principal| principal.id() == V3_PROOF_CLIENT_USER)
                .map(|principal| principal.status());
            require(
                recovered_disabled.revision() == active.pair()
                    && recovered_status == Some(PrincipalStatus::Disabled),
                "disabled security replacement did not persist durably",
            )?;
            let denied = final_kernel
                .load_user_state(
                    &retained_session,
                    function,
                    "",
                    &[],
                    &expected_types,
                )
                .await
                .expect_err("retained disabled session must be denied on a later USER state load");
            require(
                matches!(
                    denied,
                    PostgresKernelError::StateExecuteDenied {
                        pair,
                        function: denied_function,
                        reason: ExecuteDenial::InvalidSession,
                    } if pair == active.pair()
                        && denied_function == SYS_STATE_LOAD_USER_STATE_FUNCTION_ID
                ),
                "retained disabled session returned the wrong typed USER state denial",
            )?;
            let after_denial_audits = final_kernel.recover_security_audit_events().await?;
            let after_denial_write_audits = after_denial_audits
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::UserState
                        && decision.outcome() == SecurityAuditOutcome::Allowed
                        && decision.session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && decision.user_state_operation() == Some(UserStateAuditOperation::Write)
                        && decision.user_state_root_function() == Some(function)
                        && decision.user_state_cell_count() == Some(1)
                })
                .count();
            require(
                after_denial_audits.len() == after_replacement_audits.len()
                    && after_denial_write_audits == after_replacement_write_audits,
                "denied retained USER state session appended an unexpected allowed audit",
            )?;

            let principal_bytes = V3_PROOF_CLIENT_USER.to_bytes().to_vec();
            let function_bytes = function.to_bytes().to_vec();
            let slot_bytes = slot.to_bytes().to_vec();
            let inspection = database.open().await?;
            let operation: TestResult<(i64, i64, i64)> = async {
                let row = inspection
                    .client()
                    .query_one(
                        "SELECT COUNT(*)::BIGINT,
                                COALESCE(MIN(revision), 0)::BIGINT,
                                COALESCE(MAX(revision), 0)::BIGINT
                         FROM _orna_kernel.user_state_cells
                         WHERE principal_id = $1
                           AND root_function_id = $2
                           AND root_state_profile = ''
                           AND function_id = $2
                           AND function_instance_key = ''
                           AND state_slot_id = $3",
                        &[&principal_bytes, &function_bytes, &slot_bytes],
                    )
                    .await?;
                Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
            }
            .await;
            let (row_count, min_revision, max_revision) = finish_test_session(
                operation,
                inspection.shutdown().await,
                "linearization USER state cell inspection",
            )?;
            require(
                row_count == 1 && min_revision == 1 && max_revision == 1,
                "linearized USER state write did not leave exactly one revision-one cell",
            )
        }
        .await;
        let cleanup_result = remove_user_state_insert_pause_trigger(&database).await;
        match (race_result, cleanup_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(test_error), Err(cleanup_error)) => Err(failure(format!(
                "USER state linearization proof failed: {test_error}; trigger cleanup also failed: {cleanup_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_the_v3_standard_install_and_reopen() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();

        // The active revision pins `orna.std/3` through a version-two
        // catalogue hash context, and the recovered snapshot matches the
        // companion application revision the upgrade prepared.
        require(
            chain.version_three.catalogue_hash_context().version()
                == CatalogueHashVersion::Version2
                && chain
                    .version_three
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID),
            "the V2-to-V3 upgrade did not pin orna.std/3 through the version-two context",
        )?;
        require_standard_context(&chain.version_three, standard)?;
        require_recovered_snapshot(
            chain.version_three_upgrade.application_revision(),
            &chain.version_three,
        )?;

        // The V3 snapshot facts: the three ordered units with their exact
        // reserved identities and logical paths, the append-only V2 source
        // parent edge, and the two output value types.
        let units = standard.source().units();
        require(
            units.len() == 3
                && units[0].ordinal() == 0
                && units[0].id() == orna_compiler::STD_TYPES_SOURCE_UNIT_ID
                && units[0].logical_path() == "std/types.orna"
                && units[1].ordinal() == 1
                && units[1].id() == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID
                && units[1].logical_path() == "std/invoke.orna"
                && units[2].ordinal() == 2
                && units[2].id() == STD_OUTPUT_SOURCE_UNIT_ID
                && units[2].logical_path() == "std/output.orna",
            "the V3 snapshot did not retain the exact three-unit bundle",
        )?;
        require(
            standard.source().bundle() == STANDARD_SOURCE_V3_BUNDLE_ID
                && standard.source().id() == STANDARD_SOURCE_V3_REVISION_ID
                && standard.source().parent() == Some(STANDARD_SOURCE_V2_REVISION_ID),
            "the V3 source revision did not retain its append-only V2 parent edge",
        )?;
        let value_types = standard.catalogue().value_types();
        let document = value_types
            .iter()
            .find(|definition| definition.id() == STD_TERMINAL_DOCUMENT_TYPE_ID)
            .ok_or_else(|| failure("the V3 snapshot is missing std.terminal.Document"))?;
        let bytestream = value_types
            .iter()
            .find(|definition| definition.id() == STD_IO_BYTE_STREAM_TYPE_ID)
            .ok_or_else(|| failure("the V3 snapshot is missing std.io.ByteStream"))?;
        require(
            document.name().parts() == ["std", "terminal", "document"]
                && document.persistence() == ValueTypePersistence::Transient
                && document.representation_contract() == STD_TERMINAL_DOCUMENT_CONTRACT
                && bytestream.name().parts() == ["std", "io", "bytestream"]
                && bytestream.persistence() == ValueTypePersistence::Transient
                && bytestream.representation_contract() == STD_IO_BYTE_STREAM_CONTRACT,
            "the V3 snapshot did not retain the two output value types",
        )?;
        require(
            standard
                .catalogue()
                .function_by_id(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID)
                .is_some()
                && standard.executables().iter().any(|executable| {
                    executable.function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        && executable.revision().id()
                            == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                }),
            "the V3 snapshot did not retain the unchanged std.invoke.echo executable",
        )?;

        // The durable V3 standard rows: header, three source units, the two
        // output schemas and value types, the echo function, its immutable
        // revision, the 44-byte parameter-echo artifact, and the exact
        // ordered reference sequence.
        let session = database.open().await?;
        let client = session.client();
        let v3_revision = standard.revision().to_bytes().to_vec();
        let v3_bundle = standard.source().bundle().to_bytes().to_vec();
        let v2_standard = chain
            .version_two
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the V2 active revision omitted its standard snapshot"))?;
        let v2_bundle = v2_standard.source().bundle().to_bytes().to_vec();
        let header = client
            .query_one(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash
                 FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&v3_revision],
            )
            .await?;
        require(
            header.try_get::<_, Vec<u8>>(0)? == standard.revision().to_bytes()
                && header.try_get::<_, Vec<u8>>(1)? == standard.source().id().to_bytes()
                && header.try_get::<_, Vec<u8>>(2)? == standard.catalogue().revision().to_bytes()
                && header.try_get::<_, i16>(3)? == 2
                && header.try_get::<_, String>(4)? == standard.language_version()
                && header.try_get::<_, Vec<u8>>(5)? == standard.digest().to_bytes(),
            "the V3 standard header row did not retain the exact digest-version-two facts",
        )?;
        let stored_units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path,
                        membership.source_unit_id, source_unit.content_hash,
                        source_unit.bundle_id, source_unit.content
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&v3_bundle],
            )
            .await?;
        let parent_units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path,
                        membership.source_unit_id, source_unit.content_hash,
                        source_unit.bundle_id, source_unit.content
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&v2_bundle],
            )
            .await?;
        require(
            parent_units.len() == 2
                && parent_units[0].try_get::<_, i64>(0)? == 0
                && parent_units[0].try_get::<_, String>(1)? == "std/types.orna"
                && parent_units[0].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_TYPES_SOURCE_UNIT_ID.to_bytes()
                && parent_units[0].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && parent_units[0].try_get::<_, Vec<u8>>(3)?
                    == stored_units[0].try_get::<_, Vec<u8>>(3)?
                && parent_units[0].try_get::<_, String>(5)?
                    == stored_units[0].try_get::<_, String>(5)?
                && parent_units[1].try_get::<_, i64>(0)? == 1
                && parent_units[1].try_get::<_, String>(1)? == "std/invoke.orna"
                && parent_units[1].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes()
                && parent_units[1].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && parent_units[1].try_get::<_, Vec<u8>>(3)?
                    == stored_units[1].try_get::<_, Vec<u8>>(3)?
                && parent_units[1].try_get::<_, String>(5)?
                    == stored_units[1].try_get::<_, String>(5)?,
            "the V2 parent source bundle lost reused source-unit membership or bytes",
        )?;
        require(
            stored_units.len() == 3
                && stored_units[0].try_get::<_, i64>(0)? == 0
                && stored_units[0].try_get::<_, String>(1)? == "std/types.orna"
                && stored_units[0].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_TYPES_SOURCE_UNIT_ID.to_bytes()
                && stored_units[0].try_get::<_, Vec<u8>>(3)? == units[0].content_hash().to_bytes()
                && stored_units[0].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && stored_units[0].try_get::<_, String>(5)? == units[0].content()
                && stored_units[1].try_get::<_, i64>(0)? == 1
                && stored_units[1].try_get::<_, String>(1)? == "std/invoke.orna"
                && stored_units[1].try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes()
                && stored_units[1].try_get::<_, Vec<u8>>(3)? == units[1].content_hash().to_bytes()
                && stored_units[1].try_get::<_, Vec<u8>>(4)? == v2_bundle
                && stored_units[1].try_get::<_, String>(5)? == units[1].content()
                && stored_units[2].try_get::<_, i64>(0)? == 2
                && stored_units[2].try_get::<_, String>(1)? == "std/output.orna"
                && stored_units[2].try_get::<_, Vec<u8>>(2)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes()
                && stored_units[2].try_get::<_, Vec<u8>>(3)? == units[2].content_hash().to_bytes()
                && stored_units[2].try_get::<_, Vec<u8>>(4)? == v3_bundle
                && stored_units[2].try_get::<_, String>(5)? == units[2].content(),
            "the V3 source units did not persist complete parent/child membership or bytes",
        )?;
        let schemas = client
            .query(
                "SELECT schema_id, name_parts FROM _orna_kernel.standard_catalogue_schemas
                 WHERE standard_library_revision_id = $1 ORDER BY schema_id",
                &[&v3_revision],
            )
            .await?;
        let terminal_schema = schemas
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_TERMINAL_SCHEMA_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the std.terminal schema row"))?;
        let io_schema = schemas
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok() == Some(STD_IO_SCHEMA_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the std.io schema row"))?;
        require(
            schemas.len() == 5
                && terminal_schema.try_get::<_, Vec<String>>(1)? == vec!["std", "terminal"]
                && io_schema.try_get::<_, Vec<String>>(1)? == vec!["std", "io"],
            "the V3 snapshot did not persist the std.terminal and std.io schemas",
        )?;
        let stored_value_types = client
            .query(
                "SELECT type_id, schema_id, name_parts, value_kind, mutability,
                        persistence, representation_contract, source_unit_id
                 FROM _orna_kernel.standard_catalogue_value_types
                 WHERE standard_library_revision_id = $1 ORDER BY type_id",
                &[&v3_revision],
            )
            .await?;
        let stored_document = stored_value_types
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_TERMINAL_DOCUMENT_TYPE_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the Document value type row"))?;
        let stored_bytestream = stored_value_types
            .iter()
            .find(|row| {
                row.try_get::<_, Vec<u8>>(0).ok()
                    == Some(STD_IO_BYTE_STREAM_TYPE_ID.to_bytes().to_vec())
            })
            .ok_or_else(|| failure("the V3 snapshot is missing the ByteStream value type row"))?;
        require(
            stored_value_types.len() == 16
                && stored_document.try_get::<_, Vec<u8>>(1)? == STD_TERMINAL_SCHEMA_ID.to_bytes()
                && stored_document.try_get::<_, Vec<String>>(2)?
                    == vec!["std", "terminal", "document"]
                && stored_document.try_get::<_, String>(3)? == "opaque"
                && stored_document.try_get::<_, String>(4)? == "immutable"
                && stored_document.try_get::<_, String>(5)? == "transient"
                && stored_document.try_get::<_, String>(6)? == STD_TERMINAL_DOCUMENT_CONTRACT
                && stored_document.try_get::<_, Vec<u8>>(7)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes()
                && stored_bytestream.try_get::<_, Vec<u8>>(1)? == STD_IO_SCHEMA_ID.to_bytes()
                && stored_bytestream.try_get::<_, Vec<String>>(2)?
                    == vec!["std", "io", "bytestream"]
                && stored_bytestream.try_get::<_, String>(3)? == "opaque"
                && stored_bytestream.try_get::<_, String>(4)? == "immutable"
                && stored_bytestream.try_get::<_, String>(5)? == "transient"
                && stored_bytestream.try_get::<_, String>(6)? == STD_IO_BYTE_STREAM_CONTRACT
                && stored_bytestream.try_get::<_, Vec<u8>>(7)?
                    == STD_OUTPUT_SOURCE_UNIT_ID.to_bytes(),
            "the V3 snapshot did not persist the two output value types",
        )?;
        let function = client
            .query_one(
                "SELECT name_parts, current_function_revision_id, source_unit_id
                 FROM _orna_kernel.standard_catalogue_functions
                 WHERE standard_library_revision_id = $1 AND function_id = $2",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            function.try_get::<_, Vec<String>>(0)? == vec!["std", "invoke", "echo"]
                && function.try_get::<_, Vec<u8>>(1)?
                    == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()
                && function.try_get::<_, Vec<u8>>(2)?
                    == orna_compiler::STD_INVOKE_SOURCE_UNIT_ID.to_bytes(),
            "the V3 snapshot did not retain the exact std.invoke.echo function row",
        )?;
        let artifact = client
            .query_one(
                "SELECT artifact_kind, format, format_version, octet_length(payload)
                 FROM _orna_kernel.standard_function_artifacts
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            artifact.try_get::<_, String>(0)? == "server_plan"
                && artifact.try_get::<_, String>(1)? == "orna.server-parameter-echo"
                && artifact.try_get::<_, i32>(2)? == 1
                && artifact.try_get::<_, i32>(3)? == 44,
            "the V3 snapshot did not retain the exact 44-byte parameter-echo artifact",
        )?;
        let references = client
            .query(
                "SELECT ordinal FROM _orna_kernel.standard_definition_references
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2
                 ORDER BY ordinal",
                &[
                    &v3_revision,
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            references.len() == 3
                && (0..3).all(|ordinal| {
                    references
                        .get(ordinal as usize)
                        .and_then(|row| row.try_get::<_, i64>(0).ok())
                        == Some(ordinal)
                }),
            "the V3 snapshot did not persist the exact three ordered references",
        )?;
        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[
                    &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "standard"
                && authority.try_get::<_, Vec<u8>>(1)?
                    == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes()
                && authority.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(standard.revision().to_bytes().to_vec()),
            "the V3 companion authority row did not pin the exact standard executable",
        )?;
        session.shutdown().await?;
        let marker = database.open().await?;
        marker
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2
                 WHERE singleton = true",
                &[
                    &chain.version_two.pair().source().to_bytes().to_vec(),
                    &chain.version_two.pair().catalogue().to_bytes().to_vec(),
                ],
            )
            .await?;
        marker.shutdown().await?;
        let recovered_parent = named_kernel(&database, "orna-v2-parent-recover")?
            .recover()
            .await?;
        require(
            recovered_parent.pair() == chain.version_two.pair()
                && recovered_parent
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V2_REVISION_ID),
            "the V2 parent standard bundle was not recoverable after the V3 upgrade",
        )?;
        let marker = database.open().await?;
        marker
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2
                 WHERE singleton = true",
                &[
                    &chain.version_three.pair().source().to_bytes().to_vec(),
                    &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                ],
            )
            .await?;
        marker.shutdown().await?;

        // Historical pins intact: the V1, V2, and V3 standard headers all
        // remain installed, and the three historical application catalogue
        // revisions retain their exact pins (V1 activation without a pin,
        // V2 companion pinned to orna.std/2, V3 companion pinned to orna.std/3).
        let session = database.open().await?;
        let client = session.client();
        let headers = client
            .query(
                "SELECT id, digest_version FROM _orna_kernel.standard_library_revisions
                 ORDER BY id",
                &[],
            )
            .await?;
        require(
            headers.len() == 3
                && headers[0].try_get::<_, Vec<u8>>(0)? == STANDARD_LIBRARY_REVISION_ID.to_bytes()
                && headers[0].try_get::<_, i16>(1)? == 1
                && headers[1].try_get::<_, Vec<u8>>(0)?
                    == STANDARD_LIBRARY_V2_REVISION_ID.to_bytes()
                && headers[1].try_get::<_, i16>(1)? == 2
                && headers[2].try_get::<_, Vec<u8>>(0)?
                    == STANDARD_LIBRARY_V3_REVISION_ID.to_bytes()
                && headers[2].try_get::<_, i16>(1)? == 2,
            "the historical V1, V2, and V3 standard headers did not all remain installed",
        )?;
        let v1_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_one.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        let v2_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_two.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        let v3_pin = client
            .query_one(
                "SELECT canonical_hash_version, standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&chain.version_three.pair().catalogue().to_bytes().to_vec()],
            )
            .await?;
        require(
            v1_pin.try_get::<_, i16>(0)? == 1
                && v1_pin.try_get::<_, Option<Vec<u8>>>(1)?.is_none()
                && v2_pin.try_get::<_, i16>(0)? == 2
                && v2_pin.try_get::<_, Option<Vec<u8>>>(1)?
                    == Some(STANDARD_LIBRARY_V2_REVISION_ID.to_bytes().to_vec())
                && v3_pin.try_get::<_, i16>(0)? == 2
                && v3_pin.try_get::<_, Option<Vec<u8>>>(1)?
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID.to_bytes().to_vec()),
            "the historical application revisions did not retain the exact V1, V2, and V3 pins",
        )?;
        session.shutdown().await?;

        // Reopening the database recovers the same active pair pinned to V3.
        let reopened = named_kernel(&database, "orna-v3-reopen")?.recover().await?;
        require(
            reopened.pair() == chain.version_three.pair()
                && reopened
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(STANDARD_LIBRARY_V3_REVISION_ID),
            "reopening the installed database changed its active pair or pinned standard",
        )?;
        require_standard_context(&reopened, standard)?;
        require_recovered_snapshot(
            chain.version_three_upgrade.application_revision(),
            &reopened,
        )?;

        // Re-preparing the installed V3 upgrade fails closed with the exact
        // already-installed compiler error.
        let repeated = orna_standard::prepare_standard_upgrade_v2_to_v3(&chain.version_three)
            .expect_err("re-preparing an installed V3 standard unexpectedly succeeded");
        require(
            repeated.to_string()
                == format!(
                    "standard library {} is already installed",
                    STANDARD_LIBRARY_V3_REVISION_ID
                ),
            "re-preparing the installed V3 did not preserve the exact compiler error",
        )?;
        match repeated {
            orna_standard::StandardUpgradeError::Prepare {
                source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision },
            } => require(
                revision == STANDARD_LIBRARY_V3_REVISION_ID,
                "re-preparation reported the wrong installed standard revision",
            )?,
            error => {
                return Err(failure(format!(
                    "expected StandardLibraryAlreadyInstalled, got {error}"
                )));
            }
        }
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_sealed_echo_invocation_and_rejects_tampered_v3_rows() -> TestResult<()> {
    const ECHO_BY_NAME: i32 = 41;
    const ECHO_BY_IDENTITY: i32 = 42;

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, exactly as the V2 dogfooding proof does.
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Invoke through sys.invoke by qualified name and parameter name.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_BY_NAME,
        )?;
        let retained_name = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result_name = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_name)
            .await?;
        let invocation_name = require_echo_completion(&result_name, ECHO_BY_NAME)?;
        let events_name = match &result_name {
            SealedInvocationResult::Completed { events, .. } => events,
            _ => {
                return Err(failure(
                    "the name-addressed sealed invocation did not complete",
                ));
            }
        };

        // The completed kernel result carries the exact RESULT_VALUES Event
        // batch a server adapter delivers before CALL_COMPLETED; prove the
        // payload round-trips the sealed protocol bytes.
        let payload = encode_invocation_event_batch(&chain.version_three, &registry, events_name)?;
        let decoded = decode_invocation_event_batch(&chain.version_three, &registry, &payload)?;
        require(
            decoded == *events_name,
            "the completed Event batch did not round-trip the sealed RESULT_VALUES payload",
        )?;

        // Repeat the invocation by the fixed function and parameter
        // identities (FunctionId ...10 and ParameterId ...10).
        let by_identity = sealed_echo_request(
            InvocationRequestTarget::function_id(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID),
            InvocationParameterSelector::parameter_id(orna_compiler::STD_INVOKE_ECHO_PARAMETER_ID),
            ECHO_BY_IDENTITY,
        )?;
        let retained_identity =
            encode_invoke_request(&chain.version_three, &registry, &by_identity)?;
        let result_identity = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained_identity)
            .await?;
        let invocation_identity = require_echo_completion(&result_identity, ECHO_BY_IDENTITY)?;
        require(
            invocation_name != invocation_identity,
            "the two sealed invocations reused one invocation identity",
        )?;

        // The allowed protected security and invocation audit events both
        // link the exact historical application RevisionPair whose catalogue
        // hash context pins orna.std/3.
        let security_events = kernel.recover_security_audit_events().await?;
        let allowed = security_events
            .iter()
            .filter(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().kind() == SecurityAuditKind::Execute
            })
            .collect::<Vec<_>>();
        require(
            allowed.len() == 2
                && allowed.iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().session_principal() == Some(V3_PROOF_CLIENT_USER)
                        && event.decision().target()
                            == Some(InvocationTarget::new(
                                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                                pair,
                            ))
                }),
            "the allowed EXECUTE evidence did not link the exact V3-pinned RevisionPair",
        )?;
        let allowed_security_ids = allowed.iter().map(|event| event.id()).collect::<Vec<_>>();
        let invocation_rows = invocation_audit_rows(&database).await?;
        require(
            invocation_rows.len() == 2
                && invocation_rows.iter().all(|row| {
                    row.outcome == "allowed"
                        && row.function
                            == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                                .to_bytes()
                                .to_vec()
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
            "the invocation audit rows did not link the exact V3-pinned RevisionPair",
        )?;
        let authority = standard_authority_row(
            &database,
            pair.catalogue(),
            orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
        )
        .await?;
        require(
            authority.as_ref().is_some_and(|row| {
                row.target_class == "standard"
                    && row.function_revision
                        == orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                            .to_bytes()
                            .to_vec()
                    && row.standard_revision == Some(standard_revision.to_bytes().to_vec())
            }),
            "the durable invocation target authority did not pin the V3 standard target",
        )?;

        // Reopen with the V3 pin: a fresh kernel recovers the same pair and
        // the same pinned standard after the sealed invocations.
        let reopened = named_kernel(&database, "orna-v3-invoke-reopen")?
            .recover()
            .await?;
        require(
            reopened.pair() == pair
                && reopened
                    .catalogue_hash_context()
                    .standard()
                    .map(|snapshot| snapshot.revision())
                    == Some(standard_revision),
            "reopening the invoked database changed its active pair or pinned standard",
        )?;

        // The tamper fixtures below each fail recovery without writing or
        // changing prior history: the exact tampered fact stays tampered, the
        // active pair and every historical pin stay unchanged, and restoring
        // the row returns the database to a clean recovery.
        reject_tampered_output_unit_digest(&database, &chain).await?;
        reject_tampered_standard_revision(&database, &chain).await?;
        reject_tampered_executable_authority(&database, &chain).await?;
        Ok(())
    })
    .await
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_sealed_security_identity_invocation_and_audit() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("sealed-security-identity-live".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| failure(format!("sealed identity runtime failed: {error}")))?;
            runtime.block_on(proves_sealed_security_identity_invocation_and_audit_inner())
        })
        .map_err(|error| failure(format!("sealed identity thread failed: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("sealed identity thread panicked")),
    }
}

async fn proves_sealed_security_identity_invocation_and_audit_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let pair = chain.version_three.pair();
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let registry = registered_opaque_codecs(standard)?;
        let recovered = kernel.recover_security_snapshot().await?;
        let mut principals = recovered.principals().collect::<Vec<_>>();
        if !principals
            .iter()
            .any(|principal| principal.id() == V3_PROOF_CLIENT_USER)
        {
            principals.push(Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            ));
        }
        for role in [V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND] {
            if !principals.iter().any(|principal| principal.id() == role) {
                principals.push(Principal::new(
                    role,
                    PrincipalKind::Role,
                    PrincipalStatus::Active,
                ));
            }
        }
        let mut memberships = recovered.memberships().collect::<Vec<_>>();
        for role in [V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND] {
            if !memberships.iter().any(|membership| {
                membership.role() == role && membership.member() == V3_PROOF_CLIENT_USER
            }) {
                memberships.push(RoleMembership::new(role, V3_PROOF_CLIENT_USER));
            }
        }
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            recovered.function_targets().collect(),
            principals,
            memberships,
            recovered.execute_grants().collect(),
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(
            V3_PROOF_CLIENT_USER,
            vec![V3_PROOF_CLIENT_ROLE_SECOND, V3_PROOF_CLIENT_ROLE],
        )?;

        let requests = [
            (
                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
                "sys.security.session_principal",
            ),
            (
                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
                "sys.security.effective_principal",
            ),
        ];
        for (function, name) in requests {
            let target = if function == SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID {
                InvocationRequestTarget::qualified_name(
                    orna_core::catalogue::QualifiedSemanticName::new(name.split('.'))?,
                )?
            } else {
                InvocationRequestTarget::function_id(function)
            };
            let request = sealed_security_identity_request(target)?;
            let retained = encode_invoke_request(&chain.version_three, &registry, &request)?;
            let result = kernel
                .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
                .await?;
            require_security_identity_completion(&result, V3_PROOF_CLIENT_USER)?;
        }
        for target in [
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(
                    "sys.security.active_roles".split('.'),
                )?,
            )?,
            InvocationRequestTarget::function_id(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID),
        ] {
            let request = sealed_security_identity_request(target)?;
            let retained = encode_invoke_request(&chain.version_three, &registry, &request)?;
            let result = kernel
                .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
                .await?;
            require_active_roles_completion(
                &result,
                &[V3_PROOF_CLIENT_ROLE, V3_PROOF_CLIENT_ROLE_SECOND],
            )?;
        }

        let security_events = kernel.recover_security_audit_events().await?;

        let allowed = security_events
            .iter()
            .filter(|event| {
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().session_principal() == Some(V3_PROOF_CLIENT_USER)
                    && event
                        .decision()
                        .target()
                        .is_some_and(|target| target.revision() == pair)
            })
            .collect::<Vec<_>>();
        require(
            allowed.len() == 4
                && allowed.iter().all(|event| {
                    event
                        .decision()
                        .target()
                        .map(|target| target.function())
                        .is_some_and(|function| {
                            function == SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
                                || function == SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID
                                || function == SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID
                        })
                })
                && allowed.iter().any(|event| {
                    event.decision().target().map(|target| target.function())
                        == Some(SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID)
                })
                && allowed.iter().any(|event| {
                    event.decision().target().map(|target| target.function())
                        == Some(SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID)
                })
                && allowed
                    .iter()
                    .filter(|event| {
                        event.decision().target().map(|target| target.function())
                            == Some(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID)
                    })
                    .count()
                    == 2,
            "sealed security identity invocations did not append exact EXECUTE evidence",
        )?;
        let security_ids = allowed.iter().map(|event| event.id()).collect::<Vec<_>>();
        let invocation_rows = invocation_audit_rows(&database).await?;
        require(
            invocation_rows.len() == 4
                && invocation_rows
                    .iter()
                    .all(|row| row.outcome == "allowed" && row.security_event.is_some())
                && invocation_rows
                    .iter()
                    .map(|row| row.security_event.clone())
                    .collect::<Vec<_>>()
                    == security_ids
                        .iter()
                        .map(|id| Some(id.to_bytes().to_vec()))
                        .collect::<Vec<_>>(),
            "sealed security identity invocations did not link invocation audit evidence",
        )?;
        for (function, name) in [
            (
                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
                "session_principal",
            ),
            (
                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
                "effective_principal",
            ),
            (SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, "active_roles"),
        ] {
            let authority = standard_authority_row(&database, pair.catalogue(), function).await?;
            require(
                authority.as_ref().is_some_and(|row| {
                    row.target_class == "system"
                        && row.function_revision == function.to_bytes().to_vec()
                        && row.standard_revision.is_none()
                }),
                match name {
                    "session_principal" => {
                        "session_principal must have a sealed system audit anchor"
                    }
                    "effective_principal" => {
                        "effective_principal must have a sealed system audit anchor"
                    }
                    _ => "active_roles must have a sealed system audit anchor",
                },
            )?;
        }

        // Recovery accepts the persisted sealed system invocation targets before
        // tampering, then fails closed when an authority binding no longer
        // identifies the admitted sealed system target. Restoring the binding
        // must return the same durable history to an accepted recovery path.
        kernel.recover().await?;

        // Exercise historical audit recovery without letting the active loader
        // reject the same collision first: point the active marker at the
        // already-valid V2 pair while the V3 invocation evidence remains
        // historical audit data.
        let collision_function = SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec();
        let collision_revision = vec![0xfa_u8; 16];
        let collision_hash = vec![0xfb_u8; 32];
        let collision_catalogue = pair.catalogue().to_bytes().to_vec();
        let schema_session = database.open().await?;
        let schema_row = schema_session
            .client()
            .query_one(
                "SELECT schema_id, source_unit_id
                 FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $1 AND source_unit_id IS NOT NULL
                 LIMIT 1",
                &[&collision_catalogue],
            )
            .await?;
        let schema_id: Vec<u8> = schema_row.try_get("schema_id")?;
        let source_unit_id: Vec<u8> = schema_row.try_get("source_unit_id")?;
        let revision_number: i64 = schema_session
            .client()
            .query_one(
                "SELECT COALESCE(MAX(revision_number), 0) + 1
                 FROM _orna_kernel.function_revisions
                 WHERE function_id = $1",
                &[&collision_function],
            )
            .await?
            .try_get(0)?;
        schema_session.client().batch_execute("BEGIN").await?;
        schema_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                    (id, introduced_catalogue_revision_id, function_id, revision_number,
                     content_hash, semantic_ir_hash, hash_algorithm, language_version, status)
                 VALUES ($1, $2, $3, $4, $5, $5, 'sha256', 'orna.language/1', 'active')",
                &[
                    &collision_revision,
                    &collision_catalogue,
                    &collision_function,
                    &revision_number,
                    &collision_hash,
                ],
            )
            .await?;
        schema_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                    (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                     security_mode, transaction_mode, volatility, return_shape,
                     return_type_kind, return_scalar_type, return_target_type_id,
                     current_function_revision_id, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, ARRAY['app', 'sealed_collision'], 'server', 'invoker',
                         'read_only', 'stable', 'rows', NULL, NULL, NULL, $4, $5, 0, 1)",
                &[
                    &collision_catalogue,
                    &collision_function,
                    &schema_id,
                    &collision_revision,
                    &source_unit_id,
                ],
            )
            .await?;
        schema_session.client().batch_execute("COMMIT").await?;
        schema_session.shutdown().await?;

        let v2_source = chain.version_two.pair().source().to_bytes().to_vec();
        let v2_catalogue = chain.version_two.pair().catalogue().to_bytes().to_vec();
        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2",
                &[&v2_source, &v2_catalogue],
            )
            .await?;
        require(
            changed == 1,
            "historical collision active marker update changed the wrong row count",
        )?;
        database_session.shutdown().await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.function_revisions",
                    rule: "each function revision must have exactly one versioned executable artifact",
                    ..
                }
            ),
            "sealed system catalogue collision did not fail historical recovery with the exact durable invariant",
        )?;

        let cleanup_session = database.open().await?;
        cleanup_session.client().batch_execute("BEGIN").await?;
        let deleted_revision = cleanup_session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&collision_revision],
            )
            .await?;
        let deleted_function = cleanup_session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&collision_catalogue, &collision_function],
            )
            .await?;
        require(
            deleted_revision == 1 && deleted_function == 1,
            "sealed system catalogue collision cleanup changed the wrong row count",
        )?;
        cleanup_session.client().batch_execute("COMMIT").await?;
        cleanup_session.shutdown().await?;

        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2",
                &[
                    &pair.source().to_bytes().to_vec(),
                    &collision_catalogue,
                ],
            )
            .await?;
        require(
            changed == 1,
            "historical collision active marker restore changed the wrong row count",
        )?;
        database_session.shutdown().await?;
        kernel.recover().await?;
        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'application'
                 WHERE catalogue_revision_id = $1
                   AND function_id = $2",
                &[
                    &pair.catalogue().to_bytes().to_vec(),
                    &SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            changed == 1,
            "sealed system authority tamper changed the wrong row count",
        )?;
        database_session.shutdown().await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "sealed system authority tamper did not fail with the exact durable invariant",
        )?;

        let database_session = database.open().await?;
        let changed = database_session
            .client()
            .execute(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'system'
                 WHERE catalogue_revision_id = $1
                   AND function_id = $2",
                &[
                    &pair.catalogue().to_bytes().to_vec(),
                    &SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            changed == 1,
            "sealed system authority restore changed the wrong row count",
        )?;
        database_session.shutdown().await?;
        kernel.recover().await?;
        Ok(())
    })
    .await
}

/// Proves the ADR 0064 capture surface end to end: a sealed echo invocation
/// completes, an inspection epoch captures its snapshot and trace rows in
/// the same commit, the epoch round-trips through load with the canonical
/// payload, the trace stream returns the model events with
/// `p_after_sequence` and self-observation suppression, the live
/// `state_cells` projection returns the stored cell redacted or with values
/// per the requested INSPECT classifier, and the `security_decisions`
/// projection returns the linked EXECUTE decision. A fresh recovery then
/// validates the inspection relations and the appended INSPECT audit row.
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_inspect_capture_and_projections_after_sealed_echo() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("inspect-capture-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "inspect capture live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(proves_inspect_capture_and_projections_after_sealed_echo_inner())
        })
        .map_err(|error| {
            failure(format!(
                "inspect capture live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("inspect capture live thread panicked")),
    }
}

async fn proves_inspect_capture_and_projections_after_sealed_echo_inner() -> TestResult<()> {
    const ECHO_VALUE: i32 = 41;

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, exactly as the sealed-echo proof does.
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                V3_PROOF_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
            vec![],
            vec![PrivilegeGrant::new(
                V3_PROOF_CLIENT_USER,
                PrivilegeClass::Inspect(InspectPrivilege::Values),
                None,
            )?],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Store one live USER state cell for the echo root so the live
        // state_cells projection has a decodable row.
        let state_slot = StateSlotId::from_bytes([0x42; 16]);
        let cell_value = encode_constructed_value(
            &chain.version_three,
            &registry,
            &RuntimeValue::Integer(ECHO_VALUE),
        )
        .map_err(|error| failure(format!("cell value encoding failed: {error}")))?;
        let database_session = database.open().await?;
        database_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES ($1, $2, '', $3, '', $4, $5, $6, 1)",
                &[
                    &V3_PROOF_CLIENT_USER.to_bytes().to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                    &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                        .to_bytes()
                        .to_vec(),
                    &state_slot.to_bytes().to_vec(),
                    &cell_value,
                    &orna_compiler::STD_INTEGER_TYPE_ID.to_bytes().to_vec(),
                ],
            )
            .await
            .map_err(|error| failure(format!("USER state cell insert failed: {error}")))?;
        let shutdown_result = database_session.shutdown().await;
        if let Err(error) = shutdown_result {
            return Err(failure(format!(
                "USER state insert session shutdown failed: {error}"
            )));
        }

        // Invoke through sys.invoke and capture the completed invocation.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_VALUE,
        )?;
        let retained = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = require_echo_completion(&result, ECHO_VALUE)?;

        // The dispatch auto-captures one structural epoch for the completed
        // invocation (ADR 0064), so the proof consumes that epoch rather than
        // capturing a second one (which would rewrite the invocation's trace
        // rows and violate the trace primary key).
        let resolved = kernel
            .find_latest_inspect_epoch(&session, invocation)
            .await?
            .ok_or_else(|| failure("the dispatch auto-captured epoch did not resolve"))?;
        let epoch_id = resolved;

        // The epoch round-trips through the canonical ORV5 payload and
        // agrees with the invocation, the pinned pair, and the owner.
        let loaded = kernel
            .load_inspect_snapshot(&session, epoch_id)
            .await?
            .ok_or_else(|| failure("the captured epoch did not load"))?;
        require(
            loaded.id() == epoch_id
                && loaded.invocation_id() == invocation
                && loaded.source_revision_id() == pair.source()
                && loaded.catalogue_revision_id() == pair.catalogue()
                && loaded.owner() == V3_PROOF_CLIENT_USER
                && loaded.root_target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && loaded.outcome() == InspectOutcomeKind::Allowed,
            "the loaded epoch did not retain the exact capture facts",
        )?;
        require(
            loaded.summary().event_count() == 3
                && loaded.summary().result()
                    == orna_core::inspect::InspectResultSummary::ValueBatch { value_count: 1 },
            "the loaded epoch did not retain the batch summary",
        )?;

        // The projections return the epoch rows after the ladder check, and
        // a request without a granted privilege fails closed.
        let nodes = kernel.inspect_invocation_nodes(&loaded, InspectPrivilege::OwnInvocation).await?;
        require(
            nodes.len() == 1
                && nodes[0].id() == invocation
                && nodes[0].kind() == orna_core::inspect::InspectInvocationNodeKind::Root
                && nodes[0].phase() == orna_core::inspect::InspectInvocationPhase::Completed
                && nodes[0].target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            "the loaded epoch did not retain the root invocation node",
        )?;
        let calls = kernel.inspect_calls(&loaded, InspectPrivilege::OwnInvocation).await?;
        require(
            calls.len() == 1
                && calls[0].invocation_id() == invocation
                && calls[0].value_count() == 1
                && calls[0].duration_nanoseconds() == 0,
            "the loaded epoch did not retain the root call row",
        )?;

        let resources = kernel.inspect_resources(&loaded, InspectPrivilege::OwnInvocation).await?;
        require(
            resources.len() == 3
                && resources[0].kind() == orna_core::inspect::InspectResourceKind::State
                && resources[0].status() == orna_core::inspect::InspectResourceStatus::Active
                && resources[1].kind() == orna_core::inspect::InspectResourceKind::Catalog
                && resources[1].status() == orna_core::inspect::InspectResourceStatus::Active
                && resources[2].kind() == orna_core::inspect::InspectResourceKind::Standard
                && resources[2].status() == orna_core::inspect::InspectResourceStatus::Active
                && kernel
                    .inspect_ui_nodes(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty()
                && kernel
                    .inspect_presentation_candidates(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty()
                && kernel
                    .inspect_runtime_bindings(&loaded, InspectPrivilege::OwnInvocation).await?
                    .is_empty(),
            "the populated projections returned unexpected rows",
        )?;
        let denied_audits_before = inspect_denied_audit_rows(&database).await?.len();
        let denied = kernel
            .inspect_invocation_nodes(&loaded, InspectPrivilege::Source)
            .await;
        require(
            matches!(denied, Err(PostgresKernelError::InspectDenied { .. })),
            "a projection without a granted privilege did not fail closed",
        )?;

        // The trace relation retains sequences 0..3 with the four durable
        // kinds; the model stream returns the three lifecycle events and
        // honours p_after_sequence and self-observation suppression.
        let trace_rows = inspect_trace_rows(&database, invocation).await?;
        if !(trace_rows.len() == 4
            && trace_rows[0].1 == 0
            && trace_rows[0].2 == "started"
            && trace_rows[1].1 == 1
            && trace_rows[1].2 == "value_batch"
            && trace_rows[2].1 == 2
            && trace_rows[2].2 == "completed"
            && trace_rows[3].1 == 3
            && trace_rows[3].2 == "inspect_snapshot")
        {
            return Err(failure(format!(
                "trace rows 0..3 are not exact: {trace_rows:?}"
            )));
        }
        // `p_after_sequence` is a resume cursor: `after = 0` (the spec
        // default) means "from the start" and returns the full stream
        // including the Started marker at sequence 0; a positive value
        // returns only rows strictly after it.
        let stream = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                0,
                None,
                false,
            )
            .await?;
        require(
            stream.len() == 3
                && stream[0].sequence() == 0
                && stream[0].kind() == InspectTraceEventKind::InvocationStarted
                && stream[1].sequence() == 1
                && stream[1].kind() == InspectTraceEventKind::ValueBatch
                && matches!(
                    stream[1].payload(),
                    InspectTracePayload::ValueBatch {
                        schema: None,
                        values,
                    } if values.len() == 1
                        && values[0].value() == &RuntimeValue::Integer(ECHO_VALUE)
                )
                && stream[2].sequence() == 2
                && stream[2].kind() == InspectTraceEventKind::InvocationCompleted,
            "the trace stream did not return the model lifecycle events",
        )?;
        let resumed = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                None,
                false,
            )
            .await?;
        require(
            resumed.len() == 1 && resumed[0].sequence() == 2,
            "p_after_sequence did not resume after sequence 1",
        )?;

        // An observation-produced row is suppressed by default and included
        // in the explicit include-observer mode.
        let observer = InvocationId::from_bytes([0x77; 16]);
        let observed_event = InvokeEvent::new(
            invocation,
            4,
            InvocationEventBody::Started {
                visible_principal: Some(V3_PROOF_CLIENT_USER),
            },
        )
        .map_err(|error| failure(format!("observer event construction failed: {error}")))?;
        let observed_payload = encode_constructed_value(
            &chain.version_three,
            &registry,
            &RuntimeValue::InvokeEvent(observed_event),
        )
        .map_err(|error| failure(format!("observer event encoding failed: {error}")))?;
        let database_session = database.open().await?;
        database_session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES ($1, 4, 'started', $2, $3, transaction_timestamp())",
                &[
                    &invocation.to_bytes().to_vec(),
                    &observed_payload,
                    &observer.to_bytes().to_vec(),
                ],
            )
            .await
            .map_err(|error| failure(format!("observer trace row insert failed: {error}")))?;
        let shutdown_result = database_session.shutdown().await;
        if let Err(error) = shutdown_result {
            return Err(failure(format!(
                "observer row session shutdown failed: {error}"
            )));
        }
        let suppressed = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                Some(observer),
                false,
            )
            .await?;
        require(
            suppressed.len() == 1 && suppressed[0].sequence() == 2,
            "self-observation suppression did not drop the observer row",
        )?;
        let included = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Values,
                invocation,
                1,
                Some(observer),
                true,
            )
            .await?;
        require(
            included.len() == 2
                && included[0].sequence() == 2
                && included[1].sequence() == 4
                && included[1].observer_invocation() == Some(observer),
            "include-observer mode did not return the observer row",
        )?;

        let redacted = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::OwnInvocation,
                invocation,
                0,
                Some(observer),
                false,
            )
            .await?;
        require(
            redacted.len() == 3
                && matches!(
                    redacted[1].payload(),
                    InspectTracePayload::ValueBatchRedacted { value_count: 1 }
                ),
            "the unarmed trace must retain a redacted ValueBatch without decoded values",
        )?;
        let denied = kernel
            .stream_inspect_trace(
                &loaded,
                InspectPrivilege::Source,
                invocation,
                0,
                None,
                false,
            )
            .await;
        require(
            matches!(
                denied,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a trace request without a granted privilege did not fail closed",
        )?;
        let denied_audits = inspect_denied_audit_rows(&database).await?;
        require(
            denied_audits.len() == denied_audits_before + 2
                && denied_audits[denied_audits_before..].iter().all(|audit| {
                    audit.0 == V3_PROOF_CLIENT_USER.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-privilege"
                }),
            "projection and trace denials did not append exactly one protected audit each",
        )?;

        // The live state_cells projection returns the stored cell; the typed
        // value is redacted unless the Values classifier was requested and
        // granted.
        let cells = kernel
            .inspect_state_cells(
                &loaded,
                InspectPrivilege::Values,
            )
            .await?;
        require(
            cells.len() == 1
                && cells[0].key().root_function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && cells[0].key().state_profile().is_empty()
                && cells[0].key().function() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                && cells[0].key().instance_key().is_empty()
                && cells[0].key().state_slot() == state_slot
                && cells[0].value_type() == orna_compiler::STD_INTEGER_TYPE_ID
                && cells[0].revision() == 1
                && cells[0].value()
                    == Some(
                        &InvokeValue::new(RuntimeValue::Integer(ECHO_VALUE)).map_err(|error| {
                            failure(format!("invoke value construction failed: {error}"))
                        })?,
                    ),
            "the state_cells projection did not return the stored cell with values",
        )?;
        let redacted = kernel
            .inspect_state_cells(
                &loaded,
                InspectPrivilege::OwnInvocation,
            )
            .await?;
        require(
            redacted.len() == 1 && redacted[0].revision() == 1 && redacted[0].value().is_none(),
            "the state_cells projection did not redact the stored value",
        )?;

        // The security_decisions projection returns the linked EXECUTE
        // decision and the INSPECT decision that captured this epoch.
        let decisions = kernel
            .inspect_security_decisions(
                &loaded,
                InspectPrivilege::OwnInvocation,
            )
            .await?;
        require(
            decisions.len() == 2
                && decisions[0].kind() == InspectSecurityDecisionKind::Execute
                && decisions[0].outcome() == InspectSecurityDecisionOutcome::Allowed
                && decisions[0].principals().contains(&V3_PROOF_CLIENT_USER)
                && decisions[0].target() == Some(orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID)
                && decisions[0].denial_reason().is_none()
                && decisions[0].audit_refs().len() == 1
                && decisions[1].kind() == InspectSecurityDecisionKind::Inspect
                && decisions[1].outcome() == InspectSecurityDecisionOutcome::Allowed
                && decisions[1].principals().contains(&V3_PROOF_CLIENT_USER)
                && decisions[1].target().is_none()
                && decisions[1].denial_reason().is_none()
                && decisions[1].audit_refs().len() == 1,
            "the security_decisions projection did not return the linked EXECUTE and INSPECT decisions",
        )?;

        // A fresh recovery validates the inspection relations and the
        // appended INSPECT capture audit row.
        kernel.recover().await?;
        Ok(())
    })
    .await
}

/// Proves the ADR 0065 security-admin surface end to end: the identity
/// facts from a bound session, the `can_execute` and `has_privilege`
/// decisions against the recovered snapshot, the SecurityAdmin privilege
/// gate denying a session without the class while still recording the
/// closed denied audit, every admin mutation persisting its durable row
/// through the validated candidate, a privilege granted to an active role
/// passing the gate, disable failing closed for an unknown principal and
/// denying session formation afterwards, revoke removing the durable rows,
/// the audit rows carrying the closed `security_admin` kind for both
/// outcomes with the sealed target identities, and a fresh kernel
/// recovering the grants and the audit rows.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_security_admin_identity_checks_mutations_and_audit() -> TestResult<()> {
    const ADMIN: PrincipalId = PrincipalId::from_bytes([0x61; 16]);
    const USER: PrincipalId = PrincipalId::from_bytes([0x62; 16]);
    const ROLE: PrincipalId = PrincipalId::from_bytes([0x63; 16]);
    const NEW_USER: PrincipalId = PrincipalId::from_bytes([0x64; 16]);
    const OTHER: PrincipalId = PrincipalId::from_bytes([0x65; 16]);
    const UNKNOWN: PrincipalId = PrincipalId::from_bytes([0x66; 16]);
    const UNKNOWN_FUNCTION: FunctionId = FunctionId::from_bytes([0x67; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = version_one
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("the security-admin fixture function was not recovered"))?
            .id();

        // Seed the snapshot with one active admin holding the class-wide
        // SecurityAdmin privilege, one user with an active role and an
        // object-scoped EXECUTE privilege, and the application target.
        let security =
            SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                version_one.pair(),
                vec![SecurityFunctionTarget::application(function)],
                vec![
                    Principal::new(ADMIN, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                ],
                vec![RoleMembership::new(ROLE, USER)],
                vec![ExecuteGrant::new(USER, function)],
                vec![],
                vec![
                    PrivilegeGrant::new(ADMIN, PrivilegeClass::SecurityAdmin, None)?,
                    PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(function))?,
                ],
            )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let admin_session = security.bind_authenticated_session(ADMIN, vec![])?;
        let user_session = security.bind_authenticated_session(USER, vec![ROLE])?;

        // The identity functions return the typed facts of the bound session;
        // effective identity equals the session principal today.
        require(
            kernel.session_principal(&user_session) == USER
                && kernel.effective_principal(&user_session) == USER
                && kernel.active_roles(&user_session) == vec![ROLE],
            "the identity facts did not match the bound session",
        )?;

        // can_execute wraps authorise_execute: the granted user is allowed,
        // an existing principal without an EXECUTE grant fails closed on the
        // missing grant (an unknown principal would instead be an invalid
        // session; NEW_USER is created later in this proof).
        let allowed_execute = kernel.can_execute(USER, function).await?;
        require(
            matches!(
                &allowed_execute,
                ExecuteDecision::Allowed(authorised)
                    if authorised.session_principal() == USER
                        && authorised.authorising_principal() == USER
            ),
            "can_execute did not allow the granted user",
        )?;
        require(
            kernel.can_execute(ADMIN, function).await?
                == ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant),
            "can_execute did not deny an ungranted principal",
        )?;

        // has_privilege honours the class and the object scope: the
        // object-scoped EXECUTE grant reaches an object request only, and
        // the user holds no SecurityAdmin class.
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::Execute, Some(function))
                .await?
                == PrivilegeDecision::Allowed {
                    requested: PrivilegeClass::Execute,
                },
            "has_privilege did not allow the object-scoped execute privilege",
        )?;
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::Execute, None)
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::Execute,
                }),
            "has_privilege did not keep the object scope closed",
        )?;
        require(
            kernel
                .has_privilege(USER, PrivilegeClass::SecurityAdmin, None)
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::SecurityAdmin,
                }),
            "has_privilege did not deny an unprivileged user",
        )?;
        require(
            kernel
                .has_privilege(ADMIN, PrivilegeClass::SecurityAdmin, Some(function))
                .await?
                == PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::SecurityAdmin,
                }),
            "has_privilege did not close SecurityAdmin object scope",
        )?;

        // The enforcement gate denies a session without the SecurityAdmin
        // class and still records the closed denied audit decision.
        let denied = kernel
            .create_principal(&user_session, NEW_USER, PrincipalKind::User)
            .await
            .expect_err("a session without SecurityAdmin must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::SecurityAdminDenied {
                    reason: PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    },
                }
            ),
            "the gate returned the wrong typed denial",
        )?;

        // The admin mutations persist their durable rows through the
        // validated candidate and return the recovered snapshot.
        let after_create = kernel
            .create_principal(&admin_session, NEW_USER, PrincipalKind::User)
            .await?;
        require(
            after_create
                .principals()
                .any(|principal| principal.id() == NEW_USER),
            "create_principal did not persist the new principal",
        )?;
        let after_role = kernel.grant_role(&admin_session, ROLE, NEW_USER).await?;
        require(
            after_role
                .memberships()
                .any(|membership| membership.role() == ROLE && membership.member() == NEW_USER),
            "grant_role did not persist the membership",
        )?;
        let scoped_security_admin = kernel
            .grant_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                Some(function),
            )
            .await
            .expect_err("object-scoped SecurityAdmin must be rejected");
        require(
            matches!(
                scoped_security_admin,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record,
                    rule: "the security_admin privilege grant must be class-wide",
                } if record == "grant_privilege"
            ),
            "grant_privilege did not reject object-scoped SecurityAdmin",
        )?;

        let after_privilege = kernel
            .grant_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                None,
            )
            .await?;
        require(
            after_privilege.privilege_grants().any(|grant| {
                grant.grantee() == NEW_USER
                    && grant.class() == PrivilegeClass::SecurityAdmin
                    && grant.is_class_wide()
            }),
            "grant_privilege did not persist the class-wide grant",
        )?;

        // A privilege granted to an active role reaches the session through
        // the gate: the user session can now create a role.
        kernel
            .grant_privilege(&admin_session, ROLE, PrivilegeClass::SecurityAdmin, None)
            .await?;
        let after_role_privilege = kernel.create_role(&user_session, OTHER).await?;
        require(
            after_role_privilege
                .principals()
                .any(|principal| principal.id() == OTHER),
            "an active role with the privilege did not pass the gate",
        )?;

        // Disabling fails closed for an unknown principal and prevents a
        // disabled principal from forming a session afterwards.
        kernel.disable_principal(&admin_session, NEW_USER).await?;
        require(
            kernel.can_execute(NEW_USER, function).await?
                == ExecuteDecision::Denied(ExecuteDenial::InvalidSession),
            "a disabled principal must not form a session",
        )?;
        let unknown_error = kernel
            .disable_principal(&admin_session, UNKNOWN)
            .await
            .expect_err("disabling an unknown principal must fail");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::DurableInvariant {
                    rule: "the principal to disable must exist",
                    ..
                }
            ),
            "disabling an unknown principal returned the wrong error",
        )?;

        // Revoke removes the durable rows.
        let after_revoke_role = kernel.revoke_role(&admin_session, ROLE, NEW_USER).await?;
        require(
            !after_revoke_role
                .memberships()
                .any(|membership| membership.role() == ROLE && membership.member() == NEW_USER),
            "revoke_role did not remove the membership",
        )?;
        let after_revoke_privilege = kernel
            .revoke_privilege(
                &admin_session,
                NEW_USER,
                PrivilegeClass::SecurityAdmin,
                None,
            )
            .await?;
        require(
            !after_revoke_privilege
                .privilege_grants()
                .any(|grant| grant.grantee() == NEW_USER),
            "revoke_privilege did not remove the grant",
        )?;

        // Unknown revoke targets fail before the durable DELETE and do not
        // append an allowed mutation audit.
        let unknown_role = kernel
            .revoke_role(&admin_session, UNKNOWN, NEW_USER)
            .await
            .expect_err("revoking an unknown role must fail");
        require(
            matches!(
                unknown_role,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the role to revoke must exist",
                } if record == "revoke_role"
            ),
            "revoke_role accepted an unknown role target",
        )?;
        let unknown_member = kernel
            .revoke_role(&admin_session, ROLE, UNKNOWN)
            .await
            .expect_err("revoking an unknown role member must fail");
        require(
            matches!(
                unknown_member,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the role member to revoke must exist",
                } if record == "revoke_role"
            ),
            "revoke_role accepted an unknown member target",
        )?;
        let unknown_grantee = kernel
            .revoke_privilege(&admin_session, UNKNOWN, PrivilegeClass::SecurityAdmin, None)
            .await
            .expect_err("revoking an unknown privilege grantee must fail");
        require(
            matches!(
                unknown_grantee,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    record,
                    rule: "the privilege grantee must exist",
                } if record == "revoke_privilege"
            ),
            "revoke_privilege accepted an unknown grantee target",
        )?;
        let unknown_object = kernel
            .revoke_privilege(
                &admin_session,
                ADMIN,
                PrivilegeClass::Execute,
                Some(UNKNOWN_FUNCTION),
            )
            .await
            .expect_err("revoking an unknown privilege object must fail");
        require(
            matches!(
                unknown_object,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record,
                    rule: "the privilege grant object must exist",
                } if record == "revoke_privilege"
            ),
            "revoke_privilege accepted an unknown object target",
        )?;

        // The audit rows carry the closed security_admin kind for both
        // outcomes with the exact sealed target identities and the session
        // principals; argument payloads never appear.
        let events = kernel.recover_security_audit_events().await?;
        let admin_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SecurityAdmin)
            .collect::<Vec<_>>();
        require(
            admin_events.iter().any(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Denied
                    && event.decision().session_principal() == Some(USER)
                    && event.decision().security_admin_operation()
                        == Some(SecurityAdminAuditOperation::CreatePrincipal)
                    && event.decision().security_admin_target()
                        == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                    && event.decision().security_admin_denial()
                        == Some(PrivilegeDenial::MissingPrivilege {
                            requested: PrivilegeClass::SecurityAdmin,
                        })
            }),
            "the denied gate did not record its closed audit decision",
        )?;
        let allowed_creates = admin_events
            .iter()
            .filter(|event| {
                event.decision().outcome() == SecurityAuditOutcome::Allowed
                    && event.decision().security_admin_operation()
                        == Some(SecurityAdminAuditOperation::CreatePrincipal)
                    && event.decision().session_principal() == Some(ADMIN)
                    && event.decision().security_admin_target()
                        == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
            })
            .count();
        require(
            allowed_creates == 1
                && admin_events.len() == 9
                && admin_events.iter().all(|event| {
                    event.decision().security_admin_target().is_some()
                        && event.decision().session_principal().is_some()
                }),
            "the allowed mutation audit rows did not record the closed shape",
        )?;

        // A fresh kernel recovers the same privilege grants and the audit
        // rows, proving the durable round-trip through the privilege loader
        // and the security_admin audit decoder.
        let reopened = named_kernel(&database, "orna-security-admin-reopen")?
            .recover_security_snapshot()
            .await?;
        require(
            reopened.privilege_grants().any(|grant| {
                grant.grantee() == ADMIN
                    && grant.class() == PrivilegeClass::SecurityAdmin
                    && grant.is_class_wide()
            }) && reopened.principals().any(|principal| {
                principal.id() == NEW_USER && principal.status() == PrincipalStatus::Disabled
            }),
            "a fresh kernel did not recover the persisted privilege grants",
        )?;
        let reopened_audit = named_kernel(&database, "orna-security-admin-audit-reopen")?
            .recover_security_audit_events()
            .await?;
        require(
            reopened_audit
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::SecurityAdmin)
                .count()
                == admin_events.len(),
            "a fresh kernel did not recover the security-admin audit rows",
        )?;
        Ok(())
    })
    .await
}

/// `find_latest_inspect_epoch` resolves the dispatch-auto-captured epoch for
/// a completed invocation, fails closed with `InspectDenial::MissingEpoch` when
/// no epoch exists, and fails closed with `InspectDenied` for a caller whose
/// granted ladder does not reach the epoch's owner scope.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_find_latest_inspect_epoch_resolves_the_dispatch_epoch() -> TestResult<()> {
    const ECHO_VALUE: i32 = 41;
    const FOREIGN_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0xdd; 16]);

    with_test_database(|database| async move {
        let chain = install_v3_standard_chain(&database).await?;
        let kernel = kernel(&database)?;
        let standard = chain.version_three_upgrade.verified_standard_snapshot();
        let pair = chain.version_three.pair();
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // Grant EXECUTE on std.invoke.echo to the proof principal and bind a
        // session, mirroring the sealed-echo proof.
        let security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![
                Principal::new(
                    V3_PROOF_CLIENT_USER,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
                Principal::new(
                    FOREIGN_PRINCIPAL,
                    PrincipalKind::User,
                    PrincipalStatus::Active,
                ),
            ],
            vec![],
            vec![ExecuteGrant::new(
                V3_PROOF_CLIENT_USER,
                orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            )],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(V3_PROOF_CLIENT_USER, vec![])?;

        // Invoke through sys.invoke; the sealed dispatch auto-captures one
        // structural epoch for the completed invocation.
        let by_name = sealed_echo_request(
            InvocationRequestTarget::qualified_name(
                orna_core::catalogue::QualifiedSemanticName::new(["std", "invoke", "echo"])?,
            )?,
            InvocationParameterSelector::name("p_value")?,
            ECHO_VALUE,
        )?;
        let retained = encode_invoke_request(&chain.version_three, &registry, &by_name)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = require_echo_completion(&result, ECHO_VALUE)?;

        let found = kernel
            .find_latest_inspect_epoch(&session, invocation)
            .await?;
        let epoch_id = found.ok_or_else(|| failure("the dispatched invocation has no epoch"))?;
        let loaded = kernel
            .load_inspect_snapshot(&session, epoch_id)
            .await?
            .ok_or_else(|| failure("the resolved epoch did not load"))?;
        require(
            loaded.invocation_id() == invocation
                && loaded.owner() == V3_PROOF_CLIENT_USER
                && loaded.root_target() == orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID,
            "find_latest_inspect_epoch resolved the wrong epoch",
        )?;

        // An explicit epoch uses the same authenticated ownership gate as the
        // latest lookup, then can be loaded by the already-authorised caller.
        let exact = kernel.find_inspect_epoch(&session, epoch_id).await?;
        require(
            exact == Some(epoch_id),
            "the owning principal must resolve its explicit inspect epoch",
        )?;

        let missing_before = inspect_denied_audit_rows(&database).await?.len();
        let unknown_epoch = kernel
            .find_inspect_epoch(&session, InspectEpochId::from_bytes([0xef; 16]))
            .await;
        require(
            matches!(
                unknown_epoch,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingEpoch
                })
            ),
            "an unknown explicit inspect epoch must fail closed as MissingEpoch",
        )?;

        // An invocation with no captured epoch also fails closed without
        // disclosing whether the invocation or epoch exists.
        let absent = kernel
            .find_latest_inspect_epoch(&session, InvocationId::from_bytes([0xee; 16]))
            .await;
        require(
            matches!(
                absent,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingEpoch
                })
            ),
            "an invocation without an epoch must fail closed as MissingEpoch",
        )?;
        let missing_audits = inspect_denied_audit_rows(&database).await?;
        require(
            missing_audits.len() == missing_before + 2
                && missing_audits[missing_before..].iter().all(|audit| {
                    audit.0 == V3_PROOF_CLIENT_USER.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-epoch"
                }),
            "missing epoch lookups did not append exactly one protected denial each",
        )?;

        // A foreign principal whose granted ladder is only OwnInvocation
        // cannot resolve the proof principal's epoch (required rung is
        // AnyInvocation) and fails closed with the closed denial reason.
        let foreign_session = security.bind_authenticated_session(FOREIGN_PRINCIPAL, vec![])?;
        let denial = kernel
            .find_latest_inspect_epoch(&foreign_session, invocation)
            .await;
        require(
            matches!(
                denial,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a foreign principal must fail closed on the ladder",
        )?;

        let exact_denial = kernel.find_inspect_epoch(&foreign_session, epoch_id).await;
        require(
            matches!(
                exact_denial,
                Err(PostgresKernelError::InspectDenied {
                    reason: orna_core::security::InspectDenial::MissingPrivilege
                })
            ),
            "a foreign principal must fail closed for an explicit epoch",
        )?;
        let denial_audits = inspect_denied_audit_rows(&database).await?;
        require(
            denial_audits.len() == missing_before + 4
                && denial_audits[missing_before + 2..].iter().all(|audit| {
                    audit.0 == FOREIGN_PRINCIPAL.to_bytes().to_vec()
                        && audit.1.is_none()
                        && audit.2.is_none()
                        && audit.3 == "inspect:missing-privilege"
                }),
            "foreign inspect denials did not append exactly one protected audit each",
        )?;

        Ok(())
    })
    .await
}

/// Returns the protected columns for every denied INSPECT audit row.
#[allow(clippy::type_complexity)]
async fn inspect_denied_audit_rows(
    database: &TestDatabase,
) -> TestResult<Vec<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, String)>> {
    let session = database.open().await?;
    let result: TestResult<Vec<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, String)>> = async {
        let rows = session
            .client()
            .query(
                "SELECT session_principal_id, effective_principal_id,
                        authorising_principal_id, denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE event_kind = 'inspect' AND outcome = 'denied'
                 ORDER BY sequence",
                &[],
            )
            .await?;
        let mut audits = Vec::with_capacity(rows.len());
        for row in &rows {
            audits.push((
                row.try_get(0)?,
                row.try_get(1)?,
                row.try_get(2)?,
                row.try_get(3)?,
            ));
        }
        Ok(audits)
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (result, shutdown_result) {
        (Ok(audits), Ok(())) => Ok(audits),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Returns `(invocation_id, sequence, kind)` for every trace row of one
/// invocation in sequence order.
async fn inspect_trace_rows(
    database: &TestDatabase,
    invocation: InvocationId,
) -> TestResult<Vec<(Vec<u8>, i64, String)>> {
    let session = database.open().await?;
    let result = async {
        let rows = session
            .client()
            .query(
                "SELECT invocation_id, sequence, kind
                 FROM _orna_kernel.inspect_trace_events
                 WHERE invocation_id = $1
                 ORDER BY sequence",
                &[&invocation.to_bytes().to_vec()],
            )
            .await?;
        let mut trace = Vec::with_capacity(rows.len());
        for row in &rows {
            trace.push((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?));
        }
        Ok(trace)
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (result, shutdown_result) {
        (Ok(trace), Ok(())) => Ok(trace),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Tamper fixture 1: the V3 `std/output.orna` unit's stored content digest is
/// replaced. Recovery reconstructs the three-unit bundle and must fail closed
/// with the exact content-hash mismatch without writing or repairing rows.
async fn reject_tampered_output_unit_digest(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let standard = chain.version_three_upgrade.verified_standard_snapshot();
    let original_content_hash: Vec<u8> = {
        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT content_hash FROM _orna_kernel.source_units WHERE id = $1",
                &[&STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec()],
            )
            .await?;
        let hash = row.try_get(0)?;
        session.shutdown().await?;
        hash
    };
    require(
        original_content_hash == standard.source().units()[2].content_hash().to_bytes(),
        "the stored output unit digest did not match the verified V3 snapshot",
    )?;
    let before = v3_durable_state(database).await?;

    let session = database.open().await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.source_units SET content_hash = $1 WHERE id = $2",
            &[
                &vec![0x77u8; 32],
                &STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "output unit tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.source_units"
                && rule == "source unit digest must match its exact UTF-8 content",
            "the wrong output unit digest did not fail with the exact source-unit invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the wrong output unit digest produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected output unit tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.source_units SET content_hash = $1 WHERE id = $2",
            &[
                &original_content_hash,
                &STD_OUTPUT_SOURCE_UNIT_ID.to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "output unit restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the output unit did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// Tamper fixture 2: the V3 standard header's source link is replaced with a
/// hostile source revision. Recovery joins the hostile source and its empty
/// bundle, cannot reconstruct the three-unit V3 source, and must fail closed
/// with the source bundle invariant without writing or repairing rows.
async fn reject_tampered_standard_revision(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let standard = chain.version_three_upgrade.verified_standard_snapshot();
    let hostile_source = SourceRevisionId::from_bytes([0xe4; 16]);
    let hostile_bundle = SourceBundleId::from_bytes([0xe5; 16]);
    require(
        hostile_source != standard.source().id() && hostile_bundle != standard.source().bundle(),
        "the hostile source revision collided with the V3 source",
    )?;
    let before = v3_durable_state(database).await?;

    let session = database.open().await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', 1)",
            &[&hostile_bundle.to_bytes().to_vec(), &vec![0xe6u8; 32]],
        )
        .await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, NULL, $2, $3, 'sha256', 1)",
            &[
                &hostile_source.to_bytes().to_vec(),
                &hostile_bundle.to_bytes().to_vec(),
                &vec![0xe7u8; 32],
            ],
        )
        .await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.standard_library_revisions
             SET source_revision_id = $1 WHERE id = $2",
            &[
                &hostile_source.to_bytes().to_vec(),
                &standard.revision().to_bytes().to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "standard revision tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.source_bundles"
                && rule
                    == "standard source bundle digest must match the ordered source unit records",
            "the wrong standard revision did not fail with the exact source bundle invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the wrong standard revision produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected standard revision tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.standard_library_revisions
             SET source_revision_id = $1 WHERE id = $2",
            &[
                &standard.source().id().to_bytes().to_vec(),
                &standard.revision().to_bytes().to_vec(),
            ],
        )
        .await?;
    session
        .client()
        .execute(
            "DELETE FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&hostile_source.to_bytes().to_vec()],
        )
        .await?;
    session
        .client()
        .execute(
            "DELETE FROM _orna_kernel.source_bundles WHERE id = $1",
            &[&hostile_bundle.to_bytes().to_vec()],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "standard revision restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the standard revision did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// Tamper fixture 3: the V3 companion authority row pins an executable
/// revision the verified standard does not contain. Recovery must reject the
/// audited standard target with the exact durable invariant without writing
/// or repairing rows.
async fn reject_tampered_executable_authority(
    database: &TestDatabase,
    chain: &V3StandardChain,
) -> TestResult<()> {
    let before = v3_durable_state(database).await?;
    let session = database.open().await?;
    let changed = session
        .client()
        .execute(
            "UPDATE _orna_kernel.invocation_target_authorities
             SET function_revision_id = $1
             WHERE catalogue_revision_id = $2 AND function_id = $3",
            &[
                &vec![0xaau8; 16],
                &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                    .to_bytes()
                    .to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        changed == 1,
        "executable authority tamper changed the wrong row count",
    )?;

    let tampered = v3_durable_state(database).await?;
    let error = recovery_error(database).await?;
    match error {
        PostgresKernelError::DurableInvariant { relation, rule, .. } => require(
            relation == "_orna_kernel.invocation_audit_events"
                && rule == "target function and pinned revision must exist together",
            "the wrong executable authority did not fail with the exact durable invariant",
        )?,
        other => {
            return Err(failure(format!(
                "the mismatched executable produced the wrong recovery error: {other}"
            )));
        }
    }
    require(
        v3_durable_state(database).await? == tampered,
        "the rejected executable tamper repaired or changed durable state",
    )?;

    let session = database.open().await?;
    let restored = session
        .client()
        .execute(
            "UPDATE _orna_kernel.invocation_target_authorities
             SET function_revision_id = $1
             WHERE catalogue_revision_id = $2 AND function_id = $3",
            &[
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
                    .to_bytes()
                    .to_vec(),
                &chain.version_three.pair().catalogue().to_bytes().to_vec(),
                &orna_compiler::STD_INVOKE_ECHO_FUNCTION_ID
                    .to_bytes()
                    .to_vec(),
            ],
        )
        .await?;
    session.shutdown().await?;
    require(
        restored == 1,
        "executable authority restore changed the wrong row count",
    )?;
    require(
        v3_durable_state(database).await? == before,
        "restoring the executable authority did not return the exact prior durable state",
    )?;
    kernel(database)?.recover().await?;
    Ok(())
}

/// The exact durable kernel facts a failed recovery must never change: the
/// active revision pointer, every standard header, every application
/// catalogue pin, and the protected audit row counts.
#[derive(Debug, Eq, PartialEq)]
struct V3DurableState {
    active_pair: (Vec<u8>, Vec<u8>),
    standard_headers: Vec<StandardHeaderRow>,
    catalogue_pins: Vec<CataloguePinRow>,
    invocation_audit_rows: i64,
    security_audit_rows: i64,
}

#[derive(Debug, Eq, PartialEq)]
struct StandardHeaderRow {
    id: Vec<u8>,
    source_revision: Vec<u8>,
    catalogue_revision: Vec<u8>,
    digest_version: i16,
}

#[derive(Debug, Eq, PartialEq)]
struct CataloguePinRow {
    id: Vec<u8>,
    standard_library_revision: Option<Vec<u8>>,
    canonical_hash_version: i16,
}

async fn v3_durable_state(database: &TestDatabase) -> TestResult<V3DurableState> {
    let session = database.open().await?;
    let operation = async {
        let active = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision",
                &[],
            )
            .await?;
        let active_pair = (active.try_get(0)?, active.try_get(1)?);
        let headers = session
            .client()
            .query(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version
                 FROM _orna_kernel.standard_library_revisions ORDER BY id",
                &[],
            )
            .await?;
        let mut standard_headers = Vec::with_capacity(headers.len());
        for row in headers {
            standard_headers.push(StandardHeaderRow {
                id: row.try_get(0)?,
                source_revision: row.try_get(1)?,
                catalogue_revision: row.try_get(2)?,
                digest_version: row.try_get(3)?,
            });
        }
        let pins = session
            .client()
            .query(
                "SELECT id, standard_library_revision_id, canonical_hash_version
                 FROM _orna_kernel.catalogue_revisions ORDER BY id",
                &[],
            )
            .await?;
        let mut catalogue_pins = Vec::with_capacity(pins.len());
        for row in pins {
            catalogue_pins.push(CataloguePinRow {
                id: row.try_get(0)?,
                standard_library_revision: row.try_get(1)?,
                canonical_hash_version: row.try_get(2)?,
            });
        }
        let invocation_audit_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        let security_audit_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.security_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        Ok(V3DurableState {
            active_pair,
            standard_headers,
            catalogue_pins,
            invocation_audit_rows,
            security_audit_rows,
        })
    }
    .await;
    finish_test_session(operation, session.shutdown().await, "V3 durable state read")
}

async fn recovery_error(database: &TestDatabase) -> TestResult<PostgresKernelError> {
    match kernel(database)?.recover().await {
        Ok(_) => Err(failure("tampered durable state recovered successfully")),
        Err(error) => Ok(error),
    }
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
fn sealed_security_identity_request(target: InvocationRequestTarget) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target,
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

fn require_active_roles_completion(
    result: &SealedInvocationResult,
    roles: &[PrincipalId],
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed active-roles invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    let values = records
        .get(1)
        .and_then(|record| match record.event().body() {
            InvocationEventBody::ValueBatch {
                schema: None,
                values,
            } => Some(values),
            _ => None,
        })
        .ok_or_else(|| failure("the sealed active-roles result lacked a plain ValueBatch"))?;
    let RuntimeValue::Constructed(value) = values
        .first()
        .ok_or_else(|| failure("the sealed active-roles result had no value"))?
        .value()
    else {
        return Err(failure(
            "the sealed active-roles result was not a constructed SET",
        ));
    };
    let ConstructedValueKind::Set(elements) = value.kind() else {
        return Err(failure(
            "the sealed active-roles result did not contain a SET",
        ));
    };
    let expected = roles
        .iter()
        .copied()
        .map(|role| RuntimeValue::Reference {
            target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
            object: ObjectId::from_bytes(role.to_bytes()),
        })
        .collect::<Vec<_>>();
    require(
        records.len() == 3
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted
            && values.len() == 1
            && matches!(
                value.descriptor().kind(),
                TypeDescriptorKind::Set(child)
                    if matches!(
                        child.kind(),
                        TypeDescriptorKind::Reference(target)
                            if target == SYS_SECURITY_PRINCIPAL_TYPE_ID
                    )
            )
            && elements == expected.as_slice(),
        "the sealed active-roles result did not return the exact typed canonical SET",
    )?;
    require(
        records
            .iter()
            .all(|record| record.event().invocation_id() == *invocation),
        "the sealed active-roles events did not retain one invocation",
    )?;
    Ok(*invocation)
}

fn require_security_identity_completion(
    result: &SealedInvocationResult,
    principal: PrincipalId,
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed security identity invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    let values = records
        .get(1)
        .and_then(|record| match record.event().body() {
            InvocationEventBody::ValueBatch {
                schema: None,
                values,
            } => Some(values),
            _ => None,
        })
        .ok_or_else(|| failure("the sealed security identity result lacked a plain ValueBatch"))?;
    require(
        records.len() == 3
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted
            && values.len() == 1
            && values[0].value()
                == &RuntimeValue::Reference {
                    target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
                    object: ObjectId::from_bytes(principal.to_bytes()),
                },
        "the sealed security identity result did not return the exact principal reference",
    )?;
    require(
        records
            .iter()
            .all(|record| record.event().invocation_id() == *invocation),
        "the sealed security identity events did not retain one invocation",
    )?;
    Ok(*invocation)
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
    finish_test_session(
        operation,
        session.shutdown().await,
        "invocation audit row read",
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
    finish_test_session(
        operation,
        session.shutdown().await,
        "standard authority row read",
    )
}
