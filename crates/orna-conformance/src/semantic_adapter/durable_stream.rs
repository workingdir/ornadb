use super::*;

impl DurableTransactionalEvaluator {
    /// Runs the finite-list project stream with a caller-owned cancellation
    /// control. Existing callers should use [`Self::execute_project_stream`]
    /// unless they need to coordinate cancellation with the source boundary.
    pub async fn execute_project_stream_with_control<C>(
        &self,
        target: RuntimeTarget<'_>,
        project: &ProjectUnit,
        root_entry: &str,
        control: &C,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError>
    where
        C: StreamRunControl,
    {
        if !root_entry.contains('.') {
            return Ok(StageOutcome::Skipped {
                reason: "project stream roots must be namespace-qualified".into(),
            });
        }
        let admitted =
            match admit_transaction_project(project, self.limits, root_entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => return Ok(*outcome),
            };
        let bridge =
            match admit_project_list_stream(project, &admitted, root_entry, target.identity) {
                Ok(bridge) => bridge,
                Err(outcome) => return Ok(*outcome),
            };
        let key = bridge.checkpoint_key()?;
        let (request, fingerprint) =
            project_stream_compat_request(target.identity, target.initial_digest, root_entry, &key);
        self.execute_admitted_project_stream(target, request, fingerprint, bridge, key, control)
            .await
    }

    /// Executes an admitted finite-list project stream under a caller-owned
    /// durable request identity and fingerprint. Matching terminal requests
    /// replay without acquiring a writer or consuming the source again.
    /// Callers that do not carry a protocol request use
    /// [`Self::execute_project_stream`], whose compatibility identity is
    /// derived from the complete stream key and pinned runtime generation.
    pub async fn execute_project_stream_request(
        &self,
        target: RuntimeTarget<'_>,
        request: RequestIdentity,
        fingerprint: [u8; 32],
        project: &ProjectUnit,
        root_entry: &str,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        self.execute_project_stream_request_with_control(
            target,
            request,
            fingerprint,
            project,
            root_entry,
            &orna_runtime::NeverCancelled,
        )
        .await
    }

    /// Executes an admitted finite-list project stream under a caller-owned
    /// durable request identity, fingerprint, and cancellation control.
    pub async fn execute_project_stream_request_with_control<C>(
        &self,
        target: RuntimeTarget<'_>,
        request: RequestIdentity,
        fingerprint: [u8; 32],
        project: &ProjectUnit,
        root_entry: &str,
        control: &C,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError>
    where
        C: StreamRunControl,
    {
        // Replay and fingerprint validation belong to the request boundary,
        // before source admission. A caller retrying a terminal request must
        // not need the current project source to remain admissible.
        let state =
            RuntimeState::open(target.repository, target.identity, target.initial_digest).await?;
        if let Some(status) = state.request_status(request, fingerprint).await? {
            return replay_or_fence_request(status);
        }
        drop(state);
        if !root_entry.contains('.') {
            return Ok(StageOutcome::Skipped {
                reason: "project stream roots must be namespace-qualified".into(),
            });
        }
        let admitted =
            match admit_transaction_project(project, self.limits, root_entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => return Ok(*outcome),
            };
        let bridge =
            match admit_project_list_stream(project, &admitted, root_entry, target.identity) {
                Ok(bridge) => bridge,
                Err(outcome) => return Ok(*outcome),
            };
        let key = bridge.checkpoint_key()?;
        self.execute_admitted_project_stream(target, request, fingerprint, bridge, key, control)
            .await
    }

    /// Stream rows take delivery-addressed mutation ids (ORNA-LIST-STREAM-MUTATION),
    /// not the content-addressed source digest, so no request mutation scope applies.
    async fn execute_admitted_project_stream<C>(
        &self,
        target: RuntimeTarget<'_>,
        request: RequestIdentity,
        fingerprint: [u8; 32],
        mut bridge: ListStreamBridge,
        key: CheckpointKey,
        control: &C,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError>
    where
        C: StreamRunControl,
    {
        let RuntimeTarget {
            repository,
            identity,
            owner_id,
            initial_digest,
        } = target;
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        if let Some(status) = state.request_status(request, fingerprint).await? {
            return replay_or_fence_request(status);
        }
        let writer = state.acquire_lease(owner_id).await?;
        let (status, capability) = state
            .reserve_request_with_admission(request, fingerprint)
            .await?;
        if status.state.is_terminal() {
            return replay_or_fence_request(status);
        }
        let capability = capability.ok_or(RuntimeError::RequestStateConflict)?;
        let registration = RunObservationRegistration {
            request,
            consumer_identity: key.consumer.clone(),
            function: bridge.entry.clone(),
            source_identity: Some(key.source.as_str().to_owned()),
            invocation_id: request.request_id,
        };
        let start = state
            .begin_observed_request_with_admission(registration, fingerprint, writer, capability)
            .await?;
        if !start.admitted {
            return replay_or_fence_request(start.request);
        }
        let registration = state
            .register_stream_observation_with_owner(
                writer,
                StreamObservationRegistration {
                    run: start.run.ok_or(RuntimeError::RecoveryInvalid)?.id,
                    producer: bridge.producer_entry.clone(),
                    consumer: Some(bridge.entry.clone()),
                    checkpoint: key.clone(),
                },
            )
            .await;
        if let Err(error) = registration {
            // Registration is a second fenced boundary in the current
            // runtime API. If it rejects after Run admission, close the
            // admitted request while this owner still holds the lease. The
            // registration error remains the externally relevant failure.
            let _ = fail_observed_request_outcome(
                &state,
                request,
                fingerprint,
                writer,
                StageOutcome::Failed(request_runtime_failure_diagnostic()),
            )
            .await;
            return Err(error);
        }
        let mut source = ListStreamSource::new(key.clone(), std::mem::take(&mut bridge.payloads));
        let mut handler = ListTableHandler::new(bridge, self.limits);
        match state
            .run_stream(writer, &key, &mut source, &mut handler, control)
            .await
        {
            Ok(StreamRunOutcome::Exhausted { .. }) => {
                terminalize_observed_outcome(
                    &state,
                    request,
                    fingerprint,
                    writer,
                    StageOutcome::Passed,
                )
                .await
            }
            Ok(StreamRunOutcome::Failed { failure, .. }) => {
                fail_observed_request_outcome(
                    &state,
                    request,
                    fingerprint,
                    writer,
                    StageOutcome::Failed(stream_failure_diagnostic(&mut handler, &failure)),
                )
                .await
            }
            Ok(StreamRunOutcome::Cancelled { .. }) => {
                cancel_observed_request_outcome(
                    &state,
                    request,
                    fingerprint,
                    writer,
                    StageOutcome::Cancelled(stream_cancelled_diagnostic()),
                )
                .await
            }
            Ok(_) => {
                state
                    .fail_stream_observation_with_owner(
                        writer,
                        &key,
                        unexpected_stream_boundary_safe_diagnostic(),
                    )
                    .await?;
                fail_observed_request_outcome(
                    &state,
                    request,
                    fingerprint,
                    writer,
                    StageOutcome::Failed(unexpected_stream_boundary_diagnostic()),
                )
                .await
            }
            Err(error) => {
                state
                    .fail_stream_observation_with_owner(
                        writer,
                        &key,
                        stream_runtime_safe_diagnostic(),
                    )
                    .await?;
                fail_observed_request_outcome(
                    &state,
                    request,
                    fingerprint,
                    writer,
                    StageOutcome::Failed(stream_error_diagnostic(error)),
                )
                .await
            }
        }
    }

    /// Zero-scope: reached only through `execute_project_with_arguments`, which has no
    /// request identity. Mutation ids stay content-addressed.
    pub(super) async fn execute_admitted_project_with_arguments(
        &self,
        target: RuntimeTarget<'_>,
        entry: &str,
        admitted: AdmittedTransaction,
        arguments: &Environment,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let RuntimeTarget {
            repository,
            identity,
            owner_id,
            initial_digest,
        } = target;
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let lease = state.acquire_lease(owner_id).await?;
        let tables = admitted
            .key_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let snapshot = state.begin_table_activation(&tables).await?;
        let publication_rows = state.publication_metadata_rows().await?;
        let context = snapshot.context();
        self.record_activation_time(context.activation_time());
        let mut evaluator = TransactionalEvaluator::new(entry, self.limits)
            .with_sys_host_context(self.sys_host.clone())
            .with_activation_time(context.activation_time())
            .with_publication_rows(publication_rows);
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = Value::decode(row).map_err(|_| RuntimeError::RecoveryInvalid)?;
                evaluator.seed_committed_from_admission(
                    table.clone(),
                    key.clone(),
                    row,
                    &admitted.key_fields,
                    &BTreeMap::new(),
                    &admitted.table_fields,
                )?;
            }
        }
        let mutations = match evaluator.execute_admitted_with_arguments(&admitted, arguments) {
            Ok(mutations) => mutations,
            Err(diagnostic) => return Ok(StageOutcome::Failed(*diagnostic)),
        };
        if mutations.is_empty() {
            return Ok(StageOutcome::Passed);
        }
        let next_digest =
            durable_activation_digest(context.capture().generation_digest(), &mutations);
        let cwd_generation = activation_cwd_generation(repository, context)?;
        let mut validator = TransactionalTableCandidateValidator::new(
            Some(cwd_generation.clone()),
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
            &admitted.table_assertions,
            &admitted.module_assertions,
            Arc::new(BTreeMap::new()),
            self.limits,
        );
        match state
            .commit_validated_table_activation(ValidatedTableActivationCommit {
                writer: lease,
                context,
                cwd_generation: Some(&cwd_generation),
                expected_schema: None,
                mutations: &mutations,
                content_pins: &mut [],
                next_digest,
                validator: &mut validator,
                faults: &NoFault,
            })
            .await
        {
            Ok(_) => {}
            Err(TableActivationError::Runtime(error)) => return Err(error),
            Err(TableActivationError::ValidationFailed(_)) => {
                return Ok(StageOutcome::Failed(
                    transaction_error("ORNA-EVAL-TABLE-ASSERT")
                        .diagnostic()
                        .clone(),
                ));
            }
        }
        Ok(StageOutcome::Passed)
    }
    /// The inputs are one adapter call: the repository, the request record and
    /// the effect handler the admitted project runs through.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_admitted_project_request(
        &self,
        repository: &Repository,
        state: &RuntimeState,
        lease: WriterLease,
        request: RequestIdentity,
        fingerprint: [u8; 32],
        entry: &str,
        admitted: AdmittedTransaction,
        arguments: &Environment,
        loaded_project: &LoadedProject,
        table_object_ids: &BTreeMap<String, TableObjectId>,
        storage_orders: &BTreeMap<String, Vec<String>>,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let tables = admitted
            .key_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let identities = tables
            .iter()
            .map(|table| {
                table_object_ids.get(*table).and_then(|object_id| {
                    orna_runtime::RuntimeTableIdentity::new((*table).to_owned(), *object_id).ok()
                })
            })
            .collect::<Option<Vec<_>>>();
        let activated = match identities {
            Some(identities) => state.begin_admitted_table_activation(&identities).await,
            None => state.begin_table_activation(&tables).await,
        };
        let snapshot = match activated {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return fail_observed_request_runtime(state, request, fingerprint, lease).await;
            }
        };
        let publication_rows = match state.publication_metadata_rows().await {
            Ok(rows) => rows,
            Err(_) => {
                return fail_observed_request_runtime(state, request, fingerprint, lease).await;
            }
        };
        let context = snapshot.context();
        self.record_activation_time(context.activation_time());
        // The activation can name its admitted tables and the system relations
        // it serves from publication metadata, all at its pinned CWD snapshot.
        let publication_relations: Vec<String> = PUBLICATION_RELATIONS
            .iter()
            .map(|relation| relation.name().to_owned())
            .collect();
        let explain_catalogue = match ActivationExplainCatalogue::new(
            &context.capture().snapshot(),
            admitted
                .key_fields
                .keys()
                .cloned()
                .chain(publication_relations),
        ) {
            Ok(catalogue) => catalogue,
            Err(_) => {
                return fail_observed_request_runtime(state, request, fingerprint, lease).await;
            }
        };
        let mut evaluator = TransactionalEvaluator::new(entry, self.limits)
            .with_sys_host_context(self.sys_host.clone())
            .with_activation_time(context.activation_time())
            .with_publication_rows(publication_rows)
            .with_mutation_scope(request_mutation_scope(&request))
            .with_explain_catalogue(explain_catalogue);
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = match Value::decode(row) {
                    Ok(row) => row,
                    Err(_) => {
                        return fail_observed_request_runtime(state, request, fingerprint, lease)
                            .await;
                    }
                };
                if evaluator
                    .seed_committed_from_admission(
                        table.clone(),
                        key.clone(),
                        row,
                        &admitted.key_fields,
                        storage_orders,
                        &admitted.table_fields,
                    )
                    .is_err()
                {
                    return fail_observed_request_runtime(state, request, fingerprint, lease).await;
                }
            }
        }
        let mutations = match evaluator.execute_admitted_with_arguments(&admitted, arguments) {
            Ok(mutations) => mutations,
            Err(diagnostic) => {
                return terminalize_observed_outcome(
                    state,
                    request,
                    fingerprint,
                    lease,
                    StageOutcome::Failed(*diagnostic),
                )
                .await;
            }
        };
        let admitted_tables = admitted.key_fields.keys().cloned().collect::<Vec<String>>();
        let mutations = match admit_recorded_rows(
            repository,
            loaded_project,
            table_object_ids,
            &admitted_tables,
            mutations,
        ) {
            Ok(mutations) => mutations,
            Err(()) => {
                return fail_observed_request_runtime(state, request, fingerprint, lease).await;
            }
        };
        if mutations.is_empty() {
            return terminalize_observed_outcome(
                state,
                request,
                fingerprint,
                lease,
                StageOutcome::Passed,
            )
            .await;
        }
        let next_digest =
            durable_activation_digest(context.capture().generation_digest(), &mutations);
        let cwd_generation = activation_cwd_generation(repository, context)?;
        let mut validator = TransactionalTableCandidateValidator::new(
            Some(cwd_generation.clone()),
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
            &admitted.table_assertions,
            &admitted.module_assertions,
            Arc::new(table_object_ids.clone()),
            self.limits,
        );
        let outcome = StageOutcome::Passed;
        let terminal = request_terminal(&outcome)?;
        match state
            .commit_validated_table_request_activation(ValidatedTableRequestActivationCommit {
                writer: lease,
                identity: request,
                fingerprint,
                context,
                mutations: &mutations,
                next_digest,
                outcome: terminal,
                validator: &mut validator,
                faults: &NoFault,
                effects: declared_effect_report(self.sys_host.as_deref()),
            })
            .await
        {
            Ok(_) => Ok(outcome),
            Err(TableActivationError::Runtime(error)) => Err(error),
            Err(TableActivationError::ValidationFailed(_)) => {
                fail_observed_request_outcome(
                    state,
                    request,
                    fingerprint,
                    lease,
                    StageOutcome::Failed(
                        transaction_error("ORNA-EVAL-TABLE-ASSERT")
                            .diagnostic()
                            .clone(),
                    ),
                )
                .await
            }
        }
    }

    /// Publishes the caller-selected frozen runtime prefix into the format-3
    /// native store as one Git commit. Publication is explicit: source
    /// execution only stages durable runtime mutations and never advances the
    /// repository by itself.
    ///
    /// A format-1/2 `HEAD` keeps its readers but has no row writer
    /// (ORNA-UPGRADE-003). It fails with
    /// [`PendingPublicationError::LegacyReadOnly`] before the prefix is
    /// frozen, so the refusal writes no freeze row and no Git state.
    pub async fn publish_pending(
        &self,
        repository: &Repository,
        runtime: &RuntimeState,
        intent_id: [u8; 16],
        checkpoint: &orna_runtime::Checkpoint,
        message: &str,
    ) -> Result<orna_repository::IndexGeneration, PendingPublicationError> {
        let head = repository
            .head()
            .map_err(|_| RuntimeError::StorageUnavailable)
            .and_then(|head| head.ok_or(RuntimeError::RecoveryInvalid))
            .map_err(PendingPublicationError::Runtime)?;
        if let Err(orna_storage::Error::LegacyReadOnly { format }) =
            orna_storage::writable_snapshot_context(repository, &head)
        {
            return Err(PendingPublicationError::LegacyReadOnly { format });
        }
        Self::publish_frozen_prefix(repository, runtime, &head, intent_id, checkpoint, message)
            .await
            .map_err(PendingPublicationError::Runtime)
    }

    /// Freezes the prefix through `checkpoint` and publishes it onto the
    /// format-3 snapshot `head`. A prefix with no visible change completes
    /// its intent without a commit.
    async fn publish_frozen_prefix(
        repository: &Repository,
        runtime: &RuntimeState,
        head: &orna_repository::GitCommitRef,
        intent_id: [u8; 16],
        checkpoint: &orna_runtime::Checkpoint,
        message: &str,
    ) -> Result<orna_repository::IndexGeneration, RuntimeError> {
        let freeze = runtime
            .publication_writer(repository)?
            .freeze(intent_id, checkpoint)
            .await?;
        let index = repository
            .index_generation()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let prepared = RuntimePublicationCoordinator::prepare_allow_noop(
            repository, runtime, head, index, &freeze, message,
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        let Some(mut coordinator) = prepared else {
            // The frozen prefix folds to the bytes already committed, so the
            // publication has nothing to stage. Complete the intent so the
            // consumed range leaves the pending tail without an empty commit
            // (ORNA-PUB-006: no false success, but also no fabricated commit).
            let commit = repository
                .head()
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .ok_or(RuntimeError::RecoveryInvalid)?;
            let commit_id =
                orna_runtime::PublicationCommitId::new(commit.as_str().as_bytes().to_vec())
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
            runtime
                .publication_writer(repository)?
                .complete_publication(&freeze, &commit_id)
                .await?;
            // The no-op range advanced no ref, so the ordinary index is still
            // the generation this call captured; re-read it rather than clone.
            return repository
                .index_generation()
                .map_err(|_| RuntimeError::StorageUnavailable);
        };
        let published = coordinator
            .publish(repository)
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        coordinator
            .complete(repository, runtime, &freeze)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(published)
    }
}

pub(super) fn admit_recorded_rows(
    repository: &Repository,
    project: &LoadedProject,
    table_object_ids: &BTreeMap<String, TableObjectId>,
    admitted_tables: &[String],
    mutations: Vec<TableMutation>,
) -> Result<Vec<TableMutation>, ()> {
    let Ok(Some(head)) = repository.head() else {
        return Ok(mutations);
    };
    let Ok(format) = repository.open_pinned_format_context(head.as_str()) else {
        return Ok(mutations);
    };
    let loader = ProjectLoader::default();
    let mut schemas: BTreeMap<String, (ResolvedTableSchema, orna_value::Format3Context)> =
        BTreeMap::new();
    for (table, object_id) in table_object_ids {
        let Ok(schema) =
            loader.resolve_committed_source_table_schema(repository, &head, *object_id)
        else {
            continue;
        };
        let Ok(context) = format.open_value_context(object_id.bytes()) else {
            continue;
        };
        schemas.insert(table.clone(), (schema, context));
    }
    // A table declared in a named module is admitted under its qualified name,
    // while the evaluator records the declaration name, so the recorded table is
    // resolved to the admitted one before admission and scope validation
    // (bead ornadb-jm0d).
    let qualified = |table: &str| -> Option<String> {
        if admitted_tables.iter().any(|name| name == table) {
            return Some(table.to_owned());
        }
        let suffix = format!(".{table}");
        let mut matches = admitted_tables
            .iter()
            .filter(|candidate| candidate.ends_with(&suffix));
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.clone())
    };
    let mut admitted = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let mutation = match qualified(mutation.table()) {
            Some(name) if name != mutation.table() => {
                let Some(value) = mutation.value() else {
                    admitted.push(mutation);
                    continue;
                };
                let rebuilt = if mutation.is_insert() {
                    TableMutation::insert(
                        mutation.id(),
                        name,
                        mutation.key().to_vec(),
                        value.to_vec(),
                    )
                } else {
                    TableMutation::new(
                        mutation.id(),
                        name,
                        mutation.key().to_vec(),
                        Some(value.to_vec()),
                    )
                };
                let Ok(rebuilt) = rebuilt else {
                    return Err(());
                };
                rebuilt
            }
            _ => mutation,
        };
        let Some((schema, context)) = schemas.get(mutation.table()) else {
            admitted.push(mutation);
            continue;
        };
        let Some(value) = mutation.value() else {
            admitted.push(match table_object_ids.get(mutation.table()) {
                Some(identity) => mutation.with_table_object_id(*identity),
                None => mutation,
            });
            continue;
        };
        let Ok(decoded) = orna_value::Value::decode(value) else {
            return Err(());
        };
        let Ok(row) = admit_format3_row(
            project,
            mutation.key(),
            decoded.raw(),
            schema,
            context,
            Vec::new(),
            mutation.id(),
        ) else {
            admitted.push(match table_object_ids.get(mutation.table()) {
                Some(identity) => mutation.with_table_object_id(*identity),
                None => mutation,
            });
            continue;
        };
        let (admitted_row, _bindings, _schema) = row.into_mutation_and_bindings();
        // The admitted row is named by the schema's declaration name, while the
        // activation and the commit address the table by its admitted qualified
        // name, so the table is carried across (bead ornadb-jm0d).
        let rename = |row: TableMutation, table: &str| -> Result<TableMutation, ()> {
            if row.table() == table {
                return Ok(row);
            }
            let Some(identity) = row.table_object_id() else {
                return Err(());
            };
            let rebuilt = if row.is_insert() {
                match row.value() {
                    Some(value) => TableMutation::insert(
                        row.id(),
                        table.to_owned(),
                        row.key().to_vec(),
                        value.to_vec(),
                    ),
                    None => return Err(()),
                }
            } else {
                TableMutation::new(
                    row.id(),
                    table.to_owned(),
                    row.key().to_vec(),
                    row.value().map(<[u8]>::to_vec),
                )
            };
            let Ok(rebuilt) = rebuilt else {
                return Err(());
            };
            Ok(rebuilt.with_table_object_id(identity))
        };
        if mutation.is_insert() {
            // An insert keeps the admitted row: canonical key, descriptor value
            // and insert-only semantics, which publication projects and the
            // runtime accepts for an absent key.
            let Ok(row) = rename(admitted_row, mutation.table()) else {
                return Err(());
            };
            admitted.push(row);
            continue;
        }
        // A replacement keeps the admitted key and value so publication still
        // projects them, but records replacement semantics, because the runtime
        // refuses an insert over an occupied key (bead ornadb-jm0d).
        let (Some(identity), Some(value)) = (admitted_row.table_object_id(), admitted_row.value())
        else {
            admitted.push(mutation);
            continue;
        };
        let Ok(replacement) = TableMutation::new(
            admitted_row.id(),
            mutation.table().to_owned(),
            admitted_row.key().to_vec(),
            Some(value.to_vec()),
        ) else {
            return Err(());
        };
        admitted.push(replacement.with_table_object_id(identity));
    }
    Ok(admitted)
}
