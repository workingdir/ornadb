use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_a_compiler_candidate_and_recovers_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &active)?;

        let applied = kernel.apply(&candidate).await?;
        let recovered = kernel.recover().await?;

        require_recovered_new_candidate(&candidate, &applied)?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        require(
            recovered.catalogue().schemas().len() == 1
                && recovered.catalogue().object_types().len() == 1
                && recovered.catalogue().functions().len() == 1
                && recovered.function_revisions().len() == 1,
            "basic apply did not recover one schema, object, function, and immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_source_apply_and_records_one_protected_audit_event() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;

        let applied = kernel.apply_source_apply(&candidate).await?;
        require_recovered_new_candidate(&candidate, &applied)?;
        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;

        let events = reopened.recover_security_audit_events().await?;
        let source_apply_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SourceApply)
            .collect::<Vec<_>>();
        require(
            events.len() == 1 && source_apply_events.len() == 1,
            "source apply did not record exactly one protected SourceApply event",
        )?;
        let decision = source_apply_events[0].decision();
        require(
            decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.session_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.source_apply_candidate() == Some(candidate.candidate_pair())
                && decision.target().is_none()
                && decision.denial().is_none(),
            "SourceApply audit detail did not match the committed candidate",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_rejects_removing_durable_execute_grant_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x71; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let initial_candidate = candidate(BASIC_SOURCE, &empty)?;
        let active = kernel.apply(&initial_candidate).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("execute-grant fixture omitted app.list_widgets"))?
            .id();
        let grant = ExecuteGrant::new(GRANTEE, function);
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![grant],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let omission = candidate(BASIC_SOURCE_WITHOUT_FUNCTION, &active)?;
        let error = kernel
            .apply_source_apply(&omission)
            .await
            .expect_err("source apply must reject removal of a durable EXECUTE target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "source apply returned the wrong durable EXECUTE target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected source apply changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().collect::<Vec<_>>() == [grant]
                && recovered_security.privilege_grants().next().is_none(),
            "rejected source apply changed the durable EXECUTE grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_rejects_removing_durable_privilege_grant_object_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let initial_candidate = candidate(BASIC_SOURCE, &empty)?;
        let active = kernel.apply(&initial_candidate).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("privilege-grant fixture omitted app.list_widgets"))?
            .id();
        let grant = PrivilegeGrant::new(GRANTEE, PrivilegeClass::Execute, Some(function))?;
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let omission = candidate(BASIC_SOURCE_WITHOUT_FUNCTION, &active)?;
        let error = kernel
            .apply_source_apply(&omission)
            .await
            .expect_err("source apply must reject removal of a durable privilege object target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    rule: "candidate source must retain every durable privilege grant object target",
                    ..
                }
            ),
            "source apply returned the wrong durable privilege target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected source apply changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().next().is_none()
                && recovered_security.privilege_grants().collect::<Vec<_>>() == [grant],
            "rejected source apply changed the durable privilege grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_upgrade_rejects_removing_durable_execute_grant_target() -> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("standard-upgrade fixture omitted app.list_widgets"))?
            .id();
        let grant = ExecuteGrant::new(GRANTEE, function);
        let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            vec![SecurityFunctionTarget::application(function)],
            vec![Principal::new(GRANTEE, PrincipalKind::User, PrincipalStatus::Active)],
            vec![],
            vec![grant],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let standard = verified_empty_non_golden_standard()?;
        let omission = standard_context_candidate(active.pair())?;
        let error = kernel
            .apply_test_standard_upgrade(&omission, &standard)
            .await
            .expect_err("standard upgrade must reject removal of a durable EXECUTE target");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "standard upgrade returned the wrong durable EXECUTE target rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected standard upgrade changed the active revision",
        )?;
        let recovered_security = kernel.recover_security_snapshot().await?;
        require(
            recovered_security.execute_grants().collect::<Vec<_>>() == [grant],
            "rejected standard upgrade changed the durable EXECUTE grant state",
        )?;
        require_no_candidate_residue(&database, &omission, &active).await
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_upgrade_refreshes_grants_after_waiting_for_active_revision_lock() -> TestResult<()>
{
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x74; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let active = kernel.apply(&candidate(BASIC_SOURCE, &empty)?).await?;
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "list_widgets"])
            .ok_or_else(|| failure("standard-upgrade race fixture omitted app.list_widgets"))?
            .id();
        let standard = verified_empty_non_golden_standard()?;
        let omission = standard_context_candidate(active.pair())?;

        // Hold the same singleton row lock that standard upgrades acquire, then
        // commit a durable grant while the upgrade is waiting. ReadCommitted
        // must take the grant-validation snapshot after that wait.
        let writer = database.open().await?;
        writer
            .client()
            .batch_execute("BEGIN")
            .await
            .map_err(|error| failure(format!("beginning grant writer failed: {error}")))?;
        writer
            .client()
            .query_one(
                "SELECT singleton
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true
                 FOR UPDATE",
                &[],
            )
            .await?;
        let grantee = GRANTEE.to_bytes().to_vec();
        let function_bytes = function.to_bytes().to_vec();
        writer
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES ($1, 'user', 'active')",
                &[&grantee],
            )
            .await?;
        writer
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[&grantee, &function_bytes],
            )
            .await?;

        let upgrade_task = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel
                    .apply_test_standard_upgrade(&omission, &standard)
                    .await
            }
        });

        let observer = database.open().await?;
        let mut waiting = false;
        for _ in 0..500 {
            let row = observer
                .client()
                .query_one(
                    "SELECT EXISTS (
                         SELECT 1
                         FROM pg_catalog.pg_stat_activity
                         WHERE datname = pg_catalog.current_database()
                           AND pid <> pg_catalog.pg_backend_pid()
                           AND wait_event_type = 'Lock'
                           AND query LIKE '%_orna_kernel.active_revision%'
                     )",
                    &[],
                )
                .await?;
            if row.get::<_, bool>(0) {
                waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        observer.shutdown().await?;
        if !waiting {
            upgrade_task.abort();
            writer.client().batch_execute("ROLLBACK").await?;
            writer.shutdown().await?;
            return Err(failure(
                "standard upgrade did not reach the active-revision lock wait",
            ));
        }

        writer.client().batch_execute("COMMIT").await?;
        writer.shutdown().await?;
        let error = upgrade_task
            .await
            .map_err(|error| failure(format!("standard-upgrade task failed: {error}")))?
            .expect_err("standard upgrade must reject a grant committed after its lock wait");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    rule: "candidate source must retain every durable EXECUTE grant target",
                    ..
                }
            ),
            "standard upgrade returned the wrong post-wait durable grant rejection",
        )?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&active, &recovered),
            "rejected post-wait standard upgrade changed the active revision",
        )?;
        let grants = kernel.recover_security_snapshot().await?;
        require(
            grants
                .execute_grants()
                .any(|grant| grant.function() == function),
            "committed durable EXECUTE grant disappeared after rejected upgrade",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn lists_revision_pairs_with_parent_links_and_active_candidate() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let base_pair = base.pair();
        let candidate = candidate(BASIC_SOURCE, &base)?;
        let candidate_pair = candidate.candidate_pair();
        kernel.apply(&candidate).await?;
        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;
        let entries = reopened.list_revision_pairs().await?;
        let reopened_again = PostgresKernel::from_str(&database.connection_string())?;
        reopened_again.recover().await?;
        let repeated_entries = reopened_again.list_revision_pairs().await?;
        require(
            entries == repeated_entries,
            "revision pair history changed across repeated reopen and listing",
        )?;
        require(
            entries.len() == 2,
            "revision pair history did not contain exactly the bootstrap and candidate pairs",
        )?;
        require(
            entries.windows(2).all(|window| {
                (
                    window[0].source_revision_id(),
                    window[0].catalogue_revision_id(),
                ) < (
                    window[1].source_revision_id(),
                    window[1].catalogue_revision_id(),
                )
            }),
            "revision pair history was not returned in deterministic source/catalogue order",
        )?;
        let base_entry = entries
            .iter()
            .find(|entry| {
                RevisionPair::new(entry.source_revision_id(), entry.catalogue_revision_id())
                    == base_pair
            })
            .ok_or_else(|| failure("revision pair history did not contain the bootstrap pair"))?;
        require(
            base_entry.source_parent_revision_id().is_none()
                && base_entry.catalogue_parent_revision_id().is_none(),
            "bootstrap revision pair unexpectedly carried parent links",
        )?;
        let candidate_entry = entries
            .iter()
            .find(|entry| {
                RevisionPair::new(entry.source_revision_id(), entry.catalogue_revision_id())
                    == candidate_pair
            })
            .ok_or_else(|| failure("revision pair history did not contain the candidate pair"))?;
        require(
            candidate_entry.source_parent_revision_id() == Some(base_pair.source())
                && candidate_entry.catalogue_parent_revision_id() == Some(base_pair.catalogue()),
            "candidate revision pair did not retain the bootstrap pair as both parents",
        )?;
        let active_entries = entries
            .iter()
            .filter(|entry| entry.is_active())
            .collect::<Vec<_>>();
        require(
            active_entries.len() == 1
                && RevisionPair::new(
                    active_entries[0].source_revision_id(),
                    active_entries[0].catalogue_revision_id(),
                ) == candidate_pair,
            "revision pair history did not mark exactly the candidate pair active",
        )
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_failure_rolls_back_candidate_and_audit_event() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        install_failure_point(&database, FailurePoint::PostPointerRecovery, &candidate).await?;

        let error = kernel
            .apply_source_apply(&candidate)
            .await
            .expect_err("source apply must fail when post-pointer recovery is tampered");
        assert_failure_shape(FailurePoint::PostPointerRecovery, &error)?;

        let recovered = kernel.recover().await?;
        require(
            same_recovered(&base, &recovered),
            "failed source apply changed the active revision",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events
                .iter()
                .all(|event| event.decision().kind() != SecurityAuditKind::SourceApply),
            "failed source apply left a protected SourceApply audit event",
        )?;
        require_no_candidate_residue(&database, &candidate, &base).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_append_failure_rolls_back_candidate_and_audit_event() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        let baseline = baseline(&database, &base).await?;
        let audit_count = kernel.recover_security_audit_events().await?.len();
        install_failure_point(&database, FailurePoint::AuditAppend, &candidate).await?;

        let error = kernel
            .apply_source_apply(&candidate)
            .await
            .expect_err("source apply must fail while appending its protected audit event");
        assert_failure_shape(FailurePoint::AuditAppend, &error)?;

        require_baseline(&database, &baseline, &kernel).await?;
        require_no_candidate_residue(&database, &candidate, &base).await?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == audit_count
                && events
                    .iter()
                    .all(|event| event.decision().kind() != SecurityAuditKind::SourceApply),
            "failed source-apply audit append left partial protected audit history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_rejects_a_mismatched_revision_pair() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        kernel.apply_source_apply(&candidate).await?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = $1
                 WHERE event_kind = 'source_apply'",
                &[&base.pair().source().to_bytes().to_vec()],
            )
            .await?;
        session.shutdown().await?;

        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        let error = reopened
            .recover()
            .await
            .expect_err("mismatched source apply audit pair must fail recovery");
        if !matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                rule: "source apply audit target pair must exist in protected revisions",
                ..
            }
        ) {
            return Err(failure(format!(
                "unexpected mismatched source apply audit error: {error:?}"
            )));
        }
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_apply_audit_rejects_a_wrong_principal() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let base = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &base)?;
        kernel.apply_source_apply(&candidate).await?;

        let session = database.open().await?;
        let wrong_principal = vec![0_u8; 16];
        let error = session
            .client()
            .execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET session_principal_id = $1
                 WHERE event_kind = 'source_apply'",
                &[&wrong_principal],
            )
            .await
            .expect_err("source apply audit row with a wrong principal must be rejected");
        session.shutdown().await?;

        let database_error = error.as_db_error().ok_or_else(|| {
            failure(format!(
                "wrong-principal update was not a database error: {error}"
            ))
        })?;
        require(
            database_error.code().code() == "23514"
                && database_error.constraint()
                    == Some("security_audit_events_source_apply_principal_check"),
            "wrong-principal source apply update did not fail its principal CHECK constraint",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_version_two_candidate_before_any_apply_write() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = standard_context_candidate(active.pair())?;
        require(
            candidate.catalogue_hash_context().version() == CatalogueHashVersion::Version2
                && candidate.catalogue_hash() != active.catalogue_hash(),
            "version-two transition fixture did not carry a distinct later catalogue hash",
        )?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "version-two candidate unexpectedly reached a successful normal apply",
        )?;

        require(
            error.to_string()
                == "the active and candidate catalogue hash versions require a standard context transition"
                && std::error::Error::source(&error).is_none(),
            "standard context transition error did not preserve its exact source-free contract",
        )?;
        match error {
            PostgresKernelError::StandardContextTransitionRequired {
                active: CatalogueHashVersion::Version1,
                candidate: CatalogueHashVersion::Version2,
            } => {}
            error => {
                return Err(failure(format!(
                    "expected standard context transition error, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn checks_the_expected_base_before_standard_context_transition() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let stale = RevisionPair::new(
            SourceRevisionId::from_bytes([0x91; 16]),
            active.pair().catalogue(),
        );
        let candidate = standard_context_candidate(stale)?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "stale version-two candidate unexpectedly reached a successful apply",
        )?;

        require(
            error.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&error).is_none(),
            "expected-base mismatch did not preserve its existing source-free contract",
        )?;
        match error {
            PostgresKernelError::ExpectedBaseMismatch {
                expected,
                active: actual_active,
            } => require(
                expected == stale && actual_active == active.pair(),
                "expected-base mismatch did not win before the standard context guard",
            )?,
            error => {
                return Err(failure(format!(
                    "expected stale-base mismatch before standard transition, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_the_standard_upgrade_then_reuses_normal_version_two_apply() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            version_two.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&version_two, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(upgrade.application_revision(), &version_two)?;
        let replay_baseline = baseline(&database, &version_two).await?;
        let replay = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "replaying a standard upgrade unexpectedly succeeded",
        )?;
        require(
            replay.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&replay).is_none(),
            "standard-upgrade replay changed the exact expected-base error contract",
        )?;
        match replay {
            PostgresKernelError::ExpectedBaseMismatch { expected, active } => require(
                expected == upgrade.application_revision().expected_base()
                    && active == version_two.pair(),
                "standard-upgrade replay did not fail before collision scanning",
            )?,
            error => {
                return Err(failure(format!(
                    "expected standard-upgrade replay to fail with ExpectedBaseMismatch, got {error}"
                )));
            }
        }
        require_baseline(&database, &replay_baseline, &kernel).await?;

        let repeated = orna_standard::prepare_standard_upgrade(&version_two)
            .expect_err("re-preparing an installed standard upgrade unexpectedly succeeded");
        require(
            repeated.to_string()
                == format!(
                    "standard library {} is already installed",
                    upgrade.verified_standard_snapshot().revision()
                ),
            "re-preparing an installed standard did not preserve the exact compiler error",
        )?;
        match repeated {
            orna_standard::StandardUpgradeError::Prepare {
                source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision,
                },
            } => require(
                revision == upgrade.verified_standard_snapshot().revision(),
                "re-preparation reported the wrong installed standard revision",
            )?,
            error => {
                return Err(failure(format!(
                    "expected StandardLibraryAlreadyInstalled, got {error}"
                )));
            }
        }

        let second_candidate = standard_application_candidate(
            STANDARD_APPLICATION_SOURCE_EDIT,
            &version_two,
            &upgrade,
        )?;
        let second = kernel.apply(&second_candidate).await?;
        require(
            second.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "normal same-context apply did not retain the installed standard context",
        )?;
        require_standard_context(&second, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(&second_candidate, &second)?;
        require_standard_upgrade_storage(&database, &second, &upgrade, &second_candidate).await?;

        let restarted = named_kernel(&database, "orna-standard-restart")?
            .recover()
            .await?;
        require_standard_context(&restarted, upgrade.verified_standard_snapshot())?;
        require(
            same_recovered(&restarted, &second),
            "reconnect changed current or historical function revision facts",
        )?;
        require_recovered_snapshot(&second_candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_recovers_ordered_catalogue_enum_labels() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(ENUM_APPLICATION_SOURCE, &version_two, &upgrade)?;
        let expected = candidate
            .candidate()
            .enum_types()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its declaration"))?;
        let expected_schema = candidate
            .candidate()
            .schemas()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its schema"))?;
        let expected_object = candidate
            .candidate()
            .object_types()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its object"))?;
        let expected_field = expected_object
            .fields()
            .first()
            .ok_or_else(|| failure("enum candidate object did not contain its field"))?;
        let expected_origin = candidate
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected.id()))
            .ok_or_else(|| failure("enum candidate did not contain its source origin"))?;

        let applied = kernel.apply(&candidate).await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT type_id, schema_id, name_parts, labels,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_enum_types
                 WHERE catalogue_revision_id = $1",
                &[&candidate.candidate().revision().to_bytes().to_vec()],
            )
            .await?;
        let postgres_enum_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype = 'e'
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')",
                &[],
            )
            .await?
            .try_get(0)?;
        let field_row = session
            .client()
            .query_one(
                "SELECT type_kind, scalar_type, target_type_id, value_type_id,
                        value_standard_library_revision_id, enum_type_id
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                   AND owner_type_id = $2 AND field_id = $3",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected_object.id().to_bytes().to_vec(),
                    &expected_field.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let physical_row = session
            .client()
            .query_one(
                "SELECT pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                        attribute.attnotnull
                 FROM pg_catalog.pg_attribute AS attribute
                 WHERE attribute.attrelid = pg_catalog.to_regclass($1)
                   AND attribute.attname = $2 AND NOT attribute.attisdropped",
                &[&relation(expected_object.id()), &field(expected_field.id())],
            )
            .await?;
        require(
            row.try_get::<_, Vec<u8>>(0)? == expected.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<u8>>(1)? == expected_schema.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<String>>(2)? == expected.name().parts()
                && row.try_get::<_, Vec<String>>(3)? == expected.labels()
                && row.try_get::<_, Vec<u8>>(4)?
                    == expected_origin.source().source_unit().to_bytes().to_vec()
                && row.try_get::<_, i64>(5)? == i64::from(expected_origin.source().byte_start())
                && row.try_get::<_, i64>(6)? == i64::from(expected_origin.source().byte_end())
                && expected_field.resolved_type() == ResolvedType::named(expected.id())
                && field_row.try_get::<_, String>(0)? == "enum"
                && field_row.try_get::<_, Option<String>>(1)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(2)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(3)?.is_none()
                && field_row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_row.try_get::<_, Vec<u8>>(5)? == expected.id().to_bytes().to_vec()
                && physical_row.try_get::<_, String>(0)? == "text"
                && physical_row.try_get::<_, bool>(1)?
                && postgres_enum_count == 0,
            "enum apply did not preserve its exact catalogue and text storage rows",
        )?;
        session.shutdown().await?;

        let restarted = named_kernel(&database, "orna-enum-restart")?
            .recover()
            .await?;
        require_recovered_snapshot(&candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_recovers_named_record_definitions() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(RECORD_APPLICATION_SOURCE, &version_two, &upgrade)?;
        let expected = candidate
            .candidate()
            .record_value_types()
            .first()
            .ok_or_else(|| failure("record candidate did not contain its declaration"))?;
        let expected_type_origin = candidate
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected.id()))
            .ok_or_else(|| failure("record candidate did not contain its type origin"))?;
        let expected_field_origins = expected
            .fields()
            .iter()
            .map(|field| {
                candidate
                    .origins()
                    .iter()
                    .find(|origin| {
                        origin.identity()
                            == DefinitionIdentity::Field {
                                owner: expected.id(),
                                field: field.id(),
                            }
                    })
                    .ok_or_else(|| failure("record candidate did not contain a field origin"))
            })
            .collect::<TestResult<Vec<_>>>()?;

        let applied = kernel.apply(&candidate).await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let type_row = session
            .client()
            .query_one(
                "SELECT type_id, name_parts, value_kind, mutability, persistence,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_record_value_types
                 WHERE catalogue_revision_id = $1 AND type_id = $2",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let field_rows = session
            .client()
            .query(
                "SELECT field_id, name, ordinal, type_kind, value_type_id,
                        value_standard_library_revision_id, enum_type_id,
                        source_unit_id, source_start, source_end, record_type_id
                 FROM _orna_kernel.catalogue_record_value_fields
                 WHERE catalogue_revision_id = $1 AND owner_type_id = $2
                 ORDER BY ordinal",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let postgres_composite_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype = 'c'
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND type.typname = $1",
                &[&expected
                    .name()
                    .parts()
                    .last()
                    .ok_or_else(|| failure("record name has no local part"))?],
            )
            .await?
            .try_get(0)?;
        let expected_value_type = match expected.fields()[0].descriptor().kind() {
            TypeDescriptorKind::Named(type_id) => type_id.to_bytes().to_vec(),
            _ => return Err(failure("record value field has no named descriptor")),
        };
        let expected_enum_type = match expected.fields()[1].descriptor().kind() {
            TypeDescriptorKind::Named(type_id) => type_id.to_bytes().to_vec(),
            _ => return Err(failure("record enum field has no named descriptor")),
        };
        require(
            type_row.try_get::<_, Vec<u8>>(0)? == expected.id().to_bytes().to_vec()
                && type_row.try_get::<_, Vec<String>>(1)? == expected.name().parts()
                && type_row.try_get::<_, String>(2)? == "record"
                && type_row.try_get::<_, String>(3)? == "immutable"
                && type_row.try_get::<_, String>(4)? == "persistable"
                && type_row.try_get::<_, Vec<u8>>(5)?
                    == expected_type_origin
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && type_row.try_get::<_, i64>(6)?
                    == i64::from(expected_type_origin.source().byte_start())
                && type_row.try_get::<_, i64>(7)?
                    == i64::from(expected_type_origin.source().byte_end())
                && field_rows.len() == 2
                && field_rows[0].try_get::<_, Vec<u8>>(0)?
                    == expected.fields()[0].id().to_bytes().to_vec()
                && field_rows[0].try_get::<_, String>(1)? == "enabled"
                && field_rows[0].try_get::<_, i64>(2)? == 0
                && field_rows[0].try_get::<_, String>(3)? == "value"
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(4)? == Some(expected_value_type)
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(5)?
                    == Some(
                        upgrade
                            .verified_standard_snapshot()
                            .revision()
                            .to_bytes()
                            .to_vec(),
                    )
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                && field_rows[0].try_get::<_, Vec<u8>>(7)?
                    == expected_field_origins[0]
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && field_rows[0].try_get::<_, i64>(8)?
                    == i64::from(expected_field_origins[0].source().byte_start())
                && field_rows[0].try_get::<_, i64>(9)?
                    == i64::from(expected_field_origins[0].source().byte_end())
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(10)?.is_none()
                && field_rows[1].try_get::<_, Vec<u8>>(0)?
                    == expected.fields()[1].id().to_bytes().to_vec()
                && field_rows[1].try_get::<_, String>(1)? == "stage"
                && field_rows[1].try_get::<_, i64>(2)? == 1
                && field_rows[1].try_get::<_, String>(3)? == "enum"
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(6)? == Some(expected_enum_type)
                && field_rows[1].try_get::<_, Vec<u8>>(7)?
                    == expected_field_origins[1]
                        .source()
                        .source_unit()
                        .to_bytes()
                        .to_vec()
                && field_rows[1].try_get::<_, i64>(8)?
                    == i64::from(expected_field_origins[1].source().byte_start())
                && field_rows[1].try_get::<_, i64>(9)?
                    == i64::from(expected_field_origins[1].source().byte_end())
                && field_rows[1].try_get::<_, Option<Vec<u8>>>(10)?.is_none()
                && postgres_composite_count == 0,
            "record apply did not preserve its exact protected definition rows",
        )?;
        session.shutdown().await?;

        let restarted = named_kernel(&database, "orna-record-restart")?
            .recover()
            .await?;
        require_recovered_snapshot(&candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_reconnects_a_standard_enum_record_field() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let standard = verified_standard_enum_fixture()?;
        let active = kernel_instance.recover().await?;
        let candidate = standard_enum_record_candidate(&active, &standard)?;
        let expected_record = candidate
            .candidate()
            .record_value_types()
            .first()
            .ok_or_else(|| failure("standard enum candidate has no record"))?;
        let expected_field = expected_record
            .fields()
            .first()
            .ok_or_else(|| failure("standard enum candidate has no record field"))?;
        let expected_enum = standard
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no enum"))?;
        let expected_enum_origin = standard
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected_enum.id()))
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("standard enum fixture has no enum origin"))?;
        let expected_origin = candidate
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Field {
                        owner: expected_record.id(),
                        field: expected_field.id(),
                    }
            })
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("standard enum candidate has no field origin"))?;

        let expected_binding = standard
            .catalogue()
            .type_bindings()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no binding"))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT field_id, name, ordinal, type_kind,
                        value_type_id, value_standard_library_revision_id, enum_type_id,
                        enum_standard_library_revision_id, standard_enum_type_id,
                        source_unit_id, source_start, source_end, record_type_id
                 FROM _orna_kernel.catalogue_record_value_fields
                 WHERE catalogue_revision_id = $1 AND owner_type_id = $2",
                &[
                    &candidate.candidate().revision().to_bytes().to_vec(),
                    &expected_record.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let postgres_type_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype IN ('c', 'e')
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND type.typname = ANY($1)",
                &[&vec!["status", "mode"]],
            )
            .await?
            .try_get(0)?;
        let standard_enum = session
            .client()
            .query_one(
                "SELECT name_parts, labels, source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_enum_types
                 WHERE standard_library_revision_id = $1 AND type_id = $2",
                &[
                    &standard.revision().to_bytes().to_vec(),
                    &expected_enum.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        let standard_binding = session
            .client()
            .query_one(
                "SELECT target_type_kind, target_type_id, target_enum_type_id
                 FROM _orna_kernel.standard_catalogue_type_bindings
                 WHERE standard_library_revision_id = $1 AND type_binding_id = $2",
                &[
                    &standard.revision().to_bytes().to_vec(),
                    &expected_binding.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        require(
            row.try_get::<_, Vec<u8>>(0)? == expected_field.id().to_bytes().to_vec()
                && row.try_get::<_, String>(1)? == "mode"
                && row.try_get::<_, i64>(2)? == 0
                && row.try_get::<_, String>(3)? == "enum"
                && row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                && row.try_get::<_, Option<Vec<u8>>>(7)?
                    == Some(standard.revision().to_bytes().to_vec())
                && row.try_get::<_, Option<Vec<u8>>>(8)?
                    == Some(expected_enum.id().to_bytes().to_vec())
                && row.try_get::<_, Vec<u8>>(9)?
                    == expected_origin.source_unit().to_bytes().to_vec()
                && row.try_get::<_, i64>(10)? == i64::from(expected_origin.byte_start())
                && row.try_get::<_, i64>(11)? == i64::from(expected_origin.byte_end())
                && row.try_get::<_, Option<Vec<u8>>>(12)?.is_none()
                && standard_enum.try_get::<_, Vec<String>>(0)? == expected_enum.name().parts()
                && standard_enum.try_get::<_, Vec<String>>(1)? == expected_enum.labels()
                && standard_enum.try_get::<_, Vec<u8>>(2)?
                    == expected_enum_origin.source_unit().to_bytes().to_vec()
                && standard_enum.try_get::<_, i64>(3)?
                    == i64::from(expected_enum_origin.byte_start())
                && standard_enum.try_get::<_, i64>(4)?
                    == i64::from(expected_enum_origin.byte_end())
                && standard_binding.try_get::<_, String>(0)? == "enum"
                && standard_binding.try_get::<_, Option<Vec<u8>>>(1)?.is_none()
                && standard_binding.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(expected_enum.id().to_bytes().to_vec())
                && postgres_type_count == 0,
            "standard enum upgrade did not persist its definition, binding, and record tuple",
        )?;
        session.shutdown().await?;

        let reconnected = kernel(&database)?.recover().await?;
        require_recovered_snapshot(&candidate, &reconnected)?;
        let recovered_standard = reconnected
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("reconnected record recovery returned no standard"))?;
        let recovered_field = reconnected
            .catalogue()
            .record_value_type_by_id(expected_record.id())
            .and_then(|record| record.fields().first())
            .ok_or_else(|| failure("reconnected record recovery returned no field"))?;
        require(
            recovered_standard.revision() == standard.revision()
                && recovered_standard.digest() == standard.digest()
                && recovered_standard.catalogue().enum_types() == standard.catalogue().enum_types()
                && recovered_standard.catalogue().type_bindings()
                    == standard.catalogue().type_bindings()
                && recovered_field.descriptor() == &TypeDescriptor::named(expected_enum.id()),
            "reconnected standard enum record recovery changed its pinned descriptor facts",
        )
    })
    .await
}
#[tokio::test]
#[cfg(feature = "test-hooks")]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_nested_record_field_targets_through_the_two_trigger_oracle() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let empty = kernel_instance.recover().await?;
        let version_one = kernel_instance
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
        let candidate = standard_application_candidate(
            NESTED_RECORD_APPLICATION_SOURCE,
            &version_two,
            &upgrade,
        )?;
        let records = candidate.candidate().record_value_types();
        if !(records.len() == 2
            && records[0].name().to_string() == "app.outer"
            && records[1].name().to_string() == "app.inner")
        {
            return Err(failure(format!(
                "nested candidate did not preserve source declaration order: {:?}",
                records
                    .iter()
                    .map(|record| record.name().to_string())
                    .collect::<Vec<_>>()
            )));
        }
        let outer = &records[0];
        let inner = &records[1];
        let child = outer
            .fields()
            .iter()
            .find(|field| field.name() == "child")
            .ok_or_else(|| failure("outer record has no child field"))?;
        let TypeDescriptorKind::Named(target) = child.descriptor().kind() else {
            return Err(failure(
                "child field descriptor is not a resolved Named identity",
            ));
        };
        require(
            target == inner.id(),
            "child field does not target the exact inner application record identity",
        )?;
        let child_origin = candidate
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Field {
                        owner: outer.id(),
                        field: child.id(),
                    }
            })
            .map(DefinitionOrigin::source)
            .ok_or_else(|| failure("child field has no declaration origin"))?;
        require(
            child_origin.source_unit().to_bytes().to_vec()
                == candidate
                    .source()
                    .units()
                    .first()
                    .ok_or_else(|| failure("nested candidate has no source unit"))?
                    .id()
                    .to_bytes()
                    .to_vec(),
            "child field origin does not slice the candidate source unit",
        )?;
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let outer_type = outer.id().to_bytes().to_vec();
        let inner_type = inner.id().to_bytes().to_vec();
        let child_field = child.id().to_bytes().to_vec();

        let session = database.open().await?;
        install_nested_record_field_oracle_triggers(
            session.client(),
            &catalogue_revision,
            &outer_type,
            &child_field,
            &inner_type,
            &child_origin,
        )
        .await?;
        session.shutdown().await?;

        let before = baseline(&database, &version_two).await?;
        let error = failed_apply_error(
            kernel_instance.apply(&candidate).await,
            "nested record candidate unexpectedly survived the sentinel oracle",
        )?;
        match &error {
            PostgresKernelError::Database(database_error) => {
                let database_error = database_error.as_db_error().ok_or_else(|| {
                    failure(format!(
                        "nested record apply failed without database fields: {error}"
                    ))
                })?;
                if !(database_error.code().code() == "P0001"
                    && database_error.message() == "ORNA_APPLY_NESTED_RECORD_FIELD_OK")
                {
                    return Err(failure(format!(
                        "nested record apply failed with the wrong sentinel: {error}"
                    )));
                }
            }
            _ => {
                return Err(failure(format!(
                    "nested record apply failed before the P0001 sentinel: {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel_instance).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
const NESTED_RECORD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE app.inner AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n";

#[cfg(feature = "test-hooks")]
fn bytea_hex_literal(bytes: &[u8]) -> String {
    format!(
        "'\\x{}'::bytea",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(feature = "test-hooks")]
async fn install_nested_record_field_oracle_triggers(
    client: &tokio_postgres::Client,
    catalogue_revision: &[u8],
    outer_type: &[u8],
    child_field: &[u8],
    inner_type: &[u8],
    child_origin: &SourceOrigin,
) -> TestResult<()> {
    let revision = bytea_hex_literal(catalogue_revision);
    let outer = bytea_hex_literal(outer_type);
    let child = bytea_hex_literal(child_field);
    let inner = bytea_hex_literal(inner_type);
    let source_unit = bytea_hex_literal(&child_origin.source_unit().to_bytes());
    let source_start = i64::from(child_origin.byte_start());
    let source_end = i64::from(child_origin.byte_end());
    client
        .batch_execute(&format!(
            "CREATE FUNCTION _orna_kernel.orna_nested_target_ordering_assert()
             RETURNS trigger LANGUAGE plpgsql AS $function$
             BEGIN
                 IF NEW.catalogue_revision_id = {revision} AND NEW.type_id = {inner} THEN
                     IF NOT EXISTS (
                         SELECT 1 FROM _orna_kernel.catalogue_record_value_fields
                         WHERE catalogue_revision_id = {revision}
                           AND owner_type_id = {outer}
                           AND field_id = {child}
                     ) THEN
                         RAISE EXCEPTION 'ORNA_APPLY_FIELD_BEFORE_TARGET_VIOLATED';
                     END IF;
                 END IF;
                 RETURN NEW;
             END
             $function$;
             CREATE TRIGGER orna_nested_target_ordering
             BEFORE INSERT ON _orna_kernel.catalogue_record_value_types
             FOR EACH ROW EXECUTE FUNCTION _orna_kernel.orna_nested_target_ordering_assert();
             CREATE FUNCTION _orna_kernel.orna_nested_field_tuple_assert()
             RETURNS trigger LANGUAGE plpgsql AS $function$
             BEGIN
                 IF NOT (NEW.owner_type_id = {outer} AND NEW.field_id = {child}) THEN
                     RETURN NULL;
                 END IF;
                 IF NEW.type_kind <> 'record'
                     OR NEW.record_type_id <> {inner}
                     OR NEW.value_type_id IS NOT NULL
                     OR NEW.value_standard_library_revision_id IS NOT NULL
                     OR NEW.enum_type_id IS NOT NULL
                     OR NEW.enum_standard_library_revision_id IS NOT NULL
                     OR NEW.standard_enum_type_id IS NOT NULL
                     OR NEW.name <> 'child'
                     OR NEW.ordinal <> 0
                     OR NEW.source_unit_id <> {source_unit}
                     OR NEW.source_start <> {source_start}
                     OR NEW.source_end <> {source_end}
                 THEN
                     RAISE EXCEPTION 'ORNA_APPLY_TUPLE_MISMATCH %', NEW;
                 END IF;
                 IF NEW.catalogue_revision_id <> {revision} THEN
                     RAISE EXCEPTION 'ORNA_APPLY_REVISION_MISMATCH %', NEW.catalogue_revision_id;
                 END IF;
                 IF NOT EXISTS (
                     SELECT 1 FROM _orna_kernel.catalogue_record_value_types
                     WHERE catalogue_revision_id = NEW.catalogue_revision_id
                       AND type_id = NEW.record_type_id
                 ) THEN
                     RAISE EXCEPTION 'ORNA_APPLY_TARGET_MISSING';
                 END IF;
                 RAISE EXCEPTION 'ORNA_APPLY_NESTED_RECORD_FIELD_OK';
             END
             $function$;
             CREATE CONSTRAINT TRIGGER orna_nested_field_tuple
             AFTER INSERT ON _orna_kernel.catalogue_record_value_fields
             DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW EXECUTE FUNCTION _orna_kernel.orna_nested_field_tuple_assert();"
        ))
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn prepares_standard_upgrade_from_postgres_recovered_version_one_members() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_UPGRADE_V1_SOURCE, &empty)?;
        kernel.apply(&version_one_candidate).await?;

        let recovered = named_kernel(&database, "orna-standard-preparation-recovery")?
            .recover()
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&recovered)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        require(
            upgrade.application_revision().expected_base() == recovered.pair(),
            "standard upgrade expected base did not use the PostgreSQL-recovered pair",
        )?;

        let upgraded = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            upgraded.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&upgraded, upgrade.verified_standard_snapshot())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_an_inactive_standard_revision_collision_before_standard_writes() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let reserved_revision = upgrade.verified_standard_snapshot().revision();
        let hostile_catalogue = CatalogueRevisionId::from_bytes([0xe1; 16]);
        require(
            hostile_catalogue != upgrade.verified_standard_snapshot().catalogue().revision(),
            "hostile standard revision collision accidentally reused the standard catalogue ID",
        )?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, 'orna.test/hostile', $4, 'sha256')",
                &[
                    &reserved_revision.to_bytes().to_vec(),
                    &version_one.source().id().to_bytes().to_vec(),
                    &hostile_catalogue.to_bytes().to_vec(),
                    &vec![0_u8; 32],
                ],
            )
            .await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive standard identity collision unexpectedly allowed the upgrade",
        )?;
        require(
            error.to_string()
                == "the database contains an identity reserved for the standard library"
                && std::error::Error::source(&error).is_none(),
            "inactive standard identity collision changed its exact source-free error contract",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(revision),
            } => require(
                revision == reserved_revision,
                "inactive standard identity collision returned the wrong durable identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive standard revision collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let hostile = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash, hash_algorithm
                 FROM _orna_kernel.standard_library_revisions WHERE id = $1",
                &[&reserved_revision.to_bytes().to_vec()],
            )
            .await?;
        require(
            hostile.try_get::<_, Vec<u8>>(0)? == version_one.source().id().to_bytes()
                && hostile.try_get::<_, Vec<u8>>(1)? == hostile_catalogue.to_bytes()
                && hostile.try_get::<_, i16>(2)? == 1
                && hostile.try_get::<_, String>(3)? == "orna.test/hostile"
                && hostile.try_get::<_, Vec<u8>>(4)? == vec![0_u8; 32]
                && hostile.try_get::<_, String>(5)? == "sha256",
            "inactive standard revision collision row changed after the rejected upgrade",
        )?;
        let standard_rows = session
            .client()
            .query_one(
                "SELECT
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_schemas),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_value_types),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_type_bindings)",
                &[],
            )
            .await?;
        require(
            standard_rows.try_get::<_, i64>(0)? == 0
                && standard_rows.try_get::<_, i64>(1)? == 0
                && standard_rows.try_get::<_, i64>(2)? == 0,
            "rejected collision unexpectedly wrote standard catalogue rows",
        )?;
        session.shutdown().await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_reserved_type_stored_as_an_inactive_standard_enum() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_APPLICATION_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let requested_type = orna_standard::BOOLEAN_TYPE_ID;
        let hostile_revision = StandardLibraryRevisionId::from_bytes([0xe2; 16]);
        let hostile_catalogue = CatalogueRevisionId::from_bytes([0xe3; 16]);
        let hostile_schema = orna_core::SchemaId::from_bytes([0xe4; 16]);
        let source_unit = version_one
            .source()
            .units()
            .first()
            .ok_or_else(|| failure("hostile enum collision fixture has no source unit"))?;

        let session = database.open().await?;
        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, 'orna.test/hostile-enum', $4, 'sha256')",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &version_one.source().id().to_bytes().to_vec(),
                    &hostile_catalogue.to_bytes().to_vec(),
                    &vec![0_u8; 32],
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_schemas
                    (standard_library_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, ARRAY['hostile'], $3, 0, 1)",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &hostile_schema.to_bytes().to_vec(),
                    &source_unit.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_enum_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, ARRAY['hostile', 'collision'], ARRAY['x'], $4, 0, 1)",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &requested_type.to_bytes().to_vec(),
                    &hostile_schema.to_bytes().to_vec(),
                    &source_unit.id().to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive standard enum identity collision unexpectedly allowed the upgrade",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::Type(type_id),
            } => require(
                type_id == requested_type,
                "inactive standard enum collision returned the wrong type identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive standard enum type collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let hostile_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.standard_catalogue_enum_types
                 WHERE standard_library_revision_id = $1 AND type_id = $2",
                &[
                    &hostile_revision.to_bytes().to_vec(),
                    &requested_type.to_bytes().to_vec(),
                ],
            )
            .await?
            .try_get(0)?;
        require(
            hostile_count == 1,
            "rejected standard enum collision changed its hostile durable row",
        )?;
        session.shutdown().await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_reserved_types_stored_as_inactive_application_values() -> TestResult<()> {
    reject_inactive_application_type_collision(
        InactiveApplicationTypeKind::Enum,
        orna_standard::BOOLEAN_TYPE_ID,
    )
    .await?;
    reject_inactive_application_type_collision(
        InactiveApplicationTypeKind::Record,
        orna_standard::INTEGER_TYPE_ID,
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_mutual_references_with_real_postgres_foreign_keys() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(MUTUAL_REFERENCE_SOURCE, &active)?;
        let applied = kernel.apply(&candidate).await?;

        let left = applied.catalogue().object_types()[0].id();
        let right = applied.catalogue().object_types()[1].id();
        let session = database.open().await?;
        let foreign_keys = session
            .client()
            .query(
                "SELECT conrelid::regclass::text, confrelid::regclass::text, confdeltype::text\n                 FROM pg_constraint\n                 WHERE contype = 'f'\n                   AND conrelid::regclass::text = ANY($1::text[])\n                 ORDER BY conrelid::regclass::text",
                &[&vec![relation(left), relation(right)]],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<(String, String, String)>, tokio_postgres::Error>>()?;
        session.shutdown().await?;
        require(
            same_members(
                &foreign_keys,
                &[
                    (relation(left), relation(right), "a".into()),
                    (relation(right), relation(left), "a".into()),
                ],
            ),
            "mutual REF apply did not install exact left/right NO ACTION foreign keys",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_only_edit_reuses_the_immutable_function_revision_and_artifact() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let first_revision = only_revision(&first)?.clone();
        let before = immutable_rows(&database, &first_revision).await?;
        let candidate = candidate(BASIC_SOURCE_ONLY_EDIT, &first)?;
        require(
            candidate.new_function_revisions().is_empty(),
            "source-only compiler preparation allocated an immutable function revision",
        )?;

        let applied = kernel.apply(&candidate).await?;
        let reused = only_revision(&applied)?;
        require_recovered_snapshot(&candidate, &applied)?;
        require(
            reused == &first_revision,
            "source-only apply changed the complete immutable function revision record",
        )?;
        let after = immutable_rows(&database, reused).await?;
        require(
            before == after,
            "source-only apply rewrote or added immutable function revision or artifact rows",
        )?;
        require(
            applied.historical_function_revisions().is_empty(),
            "source-only apply invented function revision history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn replay_safe_field_rename_preserves_live_storage_and_execution() -> TestResult<()> {
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            FIELD_RENAME_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        let object = original.catalogue().object_types()[0].id();
        let original_field = original.catalogue().object_types()[0].fields()[0].id();
        let function = original.catalogue().functions()[0].id();
        let original_revision = only_revision(&original)?.clone();
        let original_immutable = immutable_rows(&database, &original_revision).await?;
        let original_physical = physical_catalogue(&database, object).await?;
        let stored_object = ObjectId::from_bytes([91; 16]);
        insert_private_text(
            &database,
            object,
            original_field,
            stored_object,
            "kept@example.test",
        )
        .await?;
        let proof = RenameProof {
            object,
            field: original_field,
            function,
            revision: original_revision,
            immutable: original_immutable,
            physical: original_physical,
            stored_object,
        };

        let renamed_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &original)?;
        require(
            renamed_candidate.new_function_revisions().is_empty(),
            "field rename allocated a new immutable function revision",
        )?;
        require(
            renamed_candidate.source().bundle_hash() != original.source().bundle_hash()
                && renamed_candidate.source().revision_hash() != original.source().revision_hash()
                && renamed_candidate.catalogue_hash() != original.catalogue_hash(),
            "field rename did not change all source and catalogue hashes",
        )?;
        require_rename_semantics(
            &renamed_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let renamed = initial_kernel.apply(&renamed_candidate).await?;
        require_recovered_snapshot(&renamed_candidate, &renamed)?;
        require_rename_state(&database, &renamed, &proof).await?;

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_rename_state(&database, &recovered, &proof).await?;
        let replay_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &recovered)?;
        require(
            replay_candidate.new_function_revisions().is_empty(),
            "exact field-rename replay allocated a new immutable function revision",
        )?;
        require_rename_semantics(
            &replay_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let replayed = replay_kernel.apply(&replay_candidate).await?;
        let final_kernel = kernel(&database)?;
        let final_recovered = final_kernel.recover().await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_recovered_snapshot(&replay_candidate, &final_recovered)?;
        require_rename_state(&database, &final_recovered, &proof).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn required_unique_reference_replay_and_rename_preserve_physical_identity() -> TestResult<()>
{
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_REFERENCE_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        require_recovered_snapshot(&original_candidate, &original)?;

        let person = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "person"])
            .ok_or_else(|| failure("initial apply did not create assignments.person"))?;
        let assignment = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "assignment"])
            .ok_or_else(|| failure("initial apply did not create assignments.assignment"))?;
        let owner = assignment
            .field_by_name("owner")
            .ok_or_else(|| failure("initial apply did not create assignment.owner"))?;
        require(
            assignment.fields().len() == 1
                && owner.is_required_unique_reference()
                && owner.resolved_type() == ResolvedType::reference(person.id()),
            "initial apply changed the required unique reference semantics",
        )?;

        let assignment_physical = physical_catalogue(&database, assignment.id()).await?;
        let person_physical = physical_catalogue(&database, person.id()).await?;
        let field_hex = format!("{:032x}", u128::from_be_bytes(owner.id().to_bytes()));
        let unique_name = format!("uq_{field_hex}");
        let foreign_key_name = format!("fk_{field_hex}");
        require(
            assignment_physical
                .constraints
                .iter()
                .any(|(_, name, _)| name == &unique_name)
                && assignment_physical
                    .constraints
                    .iter()
                    .any(|(_, name, _)| name == &foreign_key_name)
                && assignment_physical
                    .indexes
                    .iter()
                    .any(|(_, name, _)| name == &unique_name),
            "initial apply did not install the stable unique and foreign-key identities",
        )?;
        let proof = UniqueReferenceProof {
            person: person.id(),
            assignment: assignment.id(),
            field: owner.id(),
            person_physical,
            assignment_physical,
        };

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_unique_reference_state(&database, &recovered, &proof, "owner").await?;
        let replay_candidate = candidate(UNIQUE_REFERENCE_ORIGINAL_SOURCE, &recovered)?;
        let replayed = replay_kernel.apply(&replay_candidate).await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_unique_reference_state(&database, &replayed, &proof, "owner").await?;

        let rename_candidate = candidate(UNIQUE_REFERENCE_RENAMED_SOURCE, &replayed)?;
        let renamed = replay_kernel.apply(&rename_candidate).await?;
        require_recovered_snapshot(&rename_candidate, &renamed)?;
        require_unique_reference_state(&database, &renamed, &proof, "assignee").await?;

        let final_recovered = kernel(&database)?.recover().await?;
        require_unique_reference_state(&database, &final_recovered, &proof, "assignee").await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unique_text_replay_and_rename_preserve_c_collation_and_physical_identity() -> TestResult<()>
{
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_TEXT_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        require_recovered_snapshot(&original_candidate, &original)?;

        let account = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["accounts", "account"])
            .ok_or_else(|| failure("initial apply did not create accounts.account"))?;
        let email = account
            .field_by_name("email")
            .ok_or_else(|| failure("initial apply did not create account.email"))?;
        let username = account
            .field_by_name("username")
            .ok_or_else(|| failure("initial apply did not create account.username"))?;
        require(
            account.fields().len() == 2
                && email.nullable()
                && email.unique()
                && email.resolved_type()
                    == ResolvedType::scalar(StandardScalar::CharacterLargeObject)
                && !username.nullable()
                && username.unique()
                && username.resolved_type()
                    == ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            "initial apply changed the v1 unique Text field semantics",
        )?;
        require_unique_text_physical_shape(&database, account.id(), email.id()).await?;
        require_unique_text_physical_shape(&database, account.id(), username.id()).await?;
        let proof = UniqueTextProof {
            object: account.id(),
            nullable_field: email.id(),
            required_field: username.id(),
            physical: physical_catalogue(&database, account.id()).await?,
        };

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_unique_text_state(&database, &recovered, &proof, "email", "username").await?;
        let replay_candidate = candidate(UNIQUE_TEXT_ORIGINAL_SOURCE, &recovered)?;
        let replayed = replay_kernel.apply(&replay_candidate).await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_unique_text_state(&database, &replayed, &proof, "email", "username").await?;

        let renamed_candidate = candidate(UNIQUE_TEXT_RENAMED_SOURCE, &replayed)?;
        let renamed = replay_kernel.apply(&renamed_candidate).await?;
        require_recovered_snapshot(&renamed_candidate, &renamed)?;
        require_unique_text_state(&database, &renamed, &proof, "contact_email", "handle").await?;

        let restarted = named_kernel(&database, "orna-unique-text-restart")?
            .recover()
            .await?;
        require_unique_text_state(&database, &restarted, &proof, "contact_email", "handle").await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unique_text_non_c_collation_tamper_fails_recovery_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_TEXT_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        let account = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["accounts", "account"])
            .ok_or_else(|| failure("initial apply did not create accounts.account"))?;
        let email = account
            .field_by_name("email")
            .ok_or_else(|| failure("initial apply did not create account.email"))?;
        tamper_unique_text_column_collation(&database, account.id(), email.id()).await?;

        let error = kernel(&database)?
            .recover()
            .await
            .expect_err("non-C unique Text column and index unexpectedly passed recovery");
        let table_name = format!(
            "t_{:032x}",
            u128::from_be_bytes(account.id().to_bytes())
        );
        require(
            error.to_string()
                == format!(
                    "durable invariant failed for _orna_data record {table_name}.2: column must have the exact private name, PostgreSQL type, shape, and PUBLIC access"
                )
                && std::error::Error::source(&error).is_none(),
            "non-C unique Text tamper changed the exact source-free recovery failure",
        )?;
        match error {
            PostgresKernelError::DurableInvariant {
                relation: "_orna_data",
                record,
                rule: "column must have the exact private name, PostgreSQL type, shape, and PUBLIC access",
            } if record == format!("{table_name}.2") => Ok(()),
            error => Err(failure(format!(
                "expected unique Text column collation invariant, got {error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn changed_function_history_is_retained_and_revert_reactivates_it() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let original = only_revision(&first)?.clone();
        let changed_candidate = candidate(BASIC_CHANGED_SOURCE, &first)?;
        let changed = kernel.apply(&changed_candidate).await?;
        let changed_revision = only_revision(&changed)?.clone();
        require(
            changed_revision.id() != original.id()
                && changed.historical_function_revisions() == [original.clone()],
            "changed function apply did not retain the previous immutable revision",
        )?;
        require_recovered_snapshot(&changed_candidate, &changed)?;
        require(
            changed.function_revisions() == changed_candidate.new_function_revisions(),
            "changed function apply did not activate its newly prepared immutable revision",
        )?;

        let revert_candidate = candidate(BASIC_SOURCE, &changed)?;
        require(
            revert_candidate.new_function_revisions().is_empty(),
            "revert preparation allocated a new immutable function revision",
        )?;
        let reverted = kernel.apply(&revert_candidate).await?;
        require_recovered_snapshot(&revert_candidate, &reverted)?;
        require(
            only_revision(&reverted)?.id() == original.id(),
            "revert did not reactivate the retained matching immutable revision",
        )?;
        require(
            reverted.historical_function_revisions() == [changed_revision],
            "revert did not retire the changed immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_apply_has_one_winner_and_no_loser_residue() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator.client().query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY]).await?;
        let left_kernel = named_kernel(&database, "orna-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move { left_kernel.apply(&left_for_task).await });
        wait_for_advisory_wait(&database, "orna-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move { right_kernel.apply(&right_for_task).await });
        wait_for_active_lock_block(&database, "orna-apply-race-a", "orna-apply-race-b").await?;
        coordinator.client().query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY]).await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left").await?;
        let right_result = wait_for_apply_task(right_task, "right").await?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate, &empty).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_source_apply_has_one_winner_one_audit_and_no_loser_residue()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator
            .client()
            .query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY])
            .await?;
        let left_kernel = named_kernel(&database, "orna-source-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move {
            left_kernel.apply_source_apply(&left_for_task).await
        });
        wait_for_advisory_wait(&database, "orna-source-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-source-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move {
            right_kernel.apply_source_apply(&right_for_task).await
        });
        wait_for_active_lock_block(
            &database,
            "orna-source-apply-race-a",
            "orna-source-apply-race-b",
        )
        .await?;
        coordinator
            .client()
            .query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY])
            .await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left source apply").await?;
        let right_result = wait_for_apply_task(right_task, "right source apply").await?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base source-apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base source-apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate, &empty).await?;

        let reopened = PostgresKernel::from_str(&database.connection_string())?;
        reopened.recover().await?;
        let events = reopened.recover_security_audit_events().await?;
        let source_apply_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SourceApply)
            .collect::<Vec<_>>();
        require(
            source_apply_events.len() == 1,
            "same-base source-apply race did not record exactly one protected SourceApply event",
        )?;
        let decision = source_apply_events[0].decision();
        require(
            decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.session_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.source_apply_candidate() == Some(winner_candidate.candidate_pair())
                && decision.target().is_none()
                && decision.denial().is_none(),
            "same-base source-apply race audit detail did not match the winning candidate",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn every_apply_failure_point_rolls_back_to_the_exact_base() -> TestResult<()> {
    for point in FailurePoint::ALL {
        with_test_database(|database| async move {
            let kernel = kernel(&database)?;
            kernel.bootstrap().await?;
            let initial = kernel.recover().await?;
            let (base, candidate) = if matches!(point, FailurePoint::StatusSweep) {
                let committed = kernel.apply(&candidate(BASIC_SOURCE, &initial)?).await?;
                let changed = candidate(BASIC_CHANGED_SOURCE, &committed)?;
                (committed, changed)
            } else {
                let candidate = candidate(BASIC_SOURCE, &initial)?;
                (initial, candidate)
            };
            if matches!(
                point,
                FailurePoint::DefinitionReference | FailurePoint::DeferredReference
            ) {
                require(
                    !candidate.references().is_empty(),
                    "reference trigger fixture must contain references",
                )?;
            }
            let baseline = baseline(&database, &base).await?;
            install_failure_point(&database, point, &candidate).await?;

            let result = if matches!(point, FailurePoint::AuditAppend) {
                kernel.apply_source_apply(&candidate).await
            } else {
                kernel.apply(&candidate).await
            };
            let error = result.expect_err("triggered apply must fail");
            assert_failure_shape(point, &error)?;
            require_baseline(&database, &baseline, &kernel).await?;
            require_no_candidate_residue(&database, &candidate, &base).await
        })
        .await?;
    }
    Ok(())
}
