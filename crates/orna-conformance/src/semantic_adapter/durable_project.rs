use super::*;

impl DurableTransactionalEvaluator {
    /// Admits distinct project modules and executes one namespace-qualified
    /// root function through the durable activation lifecycle.  Module source
    /// is parsed and retained independently; this boundary never constructs a
    /// combined source string.
    pub async fn execute_project(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        project: &ProjectUnit,
        root_entry: &str,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        self.execute_project_with_arguments(
            RuntimeTarget {
                repository,
                identity,
                owner_id,
                initial_digest,
            },
            project,
            root_entry,
            &Environment::new(),
        )
        .await
    }

    /// Executes one admitted project activation with canonical named arguments.
    ///
    /// The argument map crosses this bounded adapter only after project
    /// admission. It is retained solely for the root invocation and does not
    /// grant a caller access to a different module, function, or transaction.
    pub async fn execute_project_with_arguments(
        &self,
        target: RuntimeTarget<'_>,
        project: &ProjectUnit,
        root_entry: &str,
        arguments: &Environment,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        if !root_entry.contains('.') {
            return Ok(StageOutcome::Skipped {
                reason: "project transaction roots must be namespace-qualified".into(),
            });
        }
        let admitted =
            match admit_transaction_project(project, self.limits, root_entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => return Ok(*outcome),
            };
        if admit_project_list_stream(project, &admitted, root_entry, target.identity).is_ok() {
            return Ok(StageOutcome::Skipped {
                reason: PROJECT_STREAM_ROOT_ADMISSION_REASON.into(),
            });
        }
        self.execute_admitted_project_with_arguments(target, root_entry, admitted, arguments)
            .await
    }
    /// Derives a fresh CLI request identity and its canonical argument
    /// fingerprint. A caller supplies a per-invocation nonce; retries of one
    /// protocol request must retain the returned pair rather than derive a new
    /// one.
    pub fn project_request_identity(
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        root_entry: &str,
        arguments: &Environment,
        nonce: u128,
    ) -> Result<(RequestIdentity, [u8; 32]), RuntimeError> {
        fn update_text(digest: &mut Sha256, value: &str) {
            digest.update(
                u64::try_from(value.len())
                    .expect("project request text length fits u64")
                    .to_be_bytes(),
            );
            digest.update(value.as_bytes());
        }

        fn update_material(
            digest: &mut Sha256,
            identity: RuntimeIdentity,
            initial_digest: [u8; 32],
            root_entry: &str,
            arguments: &Environment,
            nonce: u128,
        ) -> Result<(), RuntimeError> {
            digest.update(identity.database_id);
            digest.update(identity.repository_id);
            digest.update(initial_digest);
            digest.update(nonce.to_be_bytes());
            update_text(digest, root_entry);
            for (name, value) in arguments {
                update_text(digest, name);
                let encoded = value.encode().map_err(|_| RuntimeError::RecoveryInvalid)?;
                digest.update(
                    u64::try_from(encoded.len())
                        .expect("project request value length fits u64")
                        .to_be_bytes(),
                );
                digest.update(encoded);
            }
            Ok(())
        }

        let mut request_digest = Sha256::new();
        request_digest.update(b"orna-cli/project-request/v1\0");
        update_material(
            &mut request_digest,
            identity,
            initial_digest,
            root_entry,
            arguments,
            nonce,
        )?;
        let request_bytes: [u8; 32] = request_digest.finalize().into();
        let mut request_id = [0; 16];
        request_id.copy_from_slice(&request_bytes[..16]);
        if request_id == [0; 16] {
            request_id[0] = 1;
        }

        let mut fingerprint = Sha256::new();
        fingerprint.update(b"orna-cli/project-fingerprint/v1\0");
        update_material(
            &mut fingerprint,
            identity,
            initial_digest,
            root_entry,
            arguments,
            nonce,
        )?;
        Ok((
            RequestIdentity {
                session_id: identity.database_id,
                request_id,
            },
            fingerprint.finalize().into(),
        ))
    }

    /// Executes a project activation through the durable request boundary.
    ///
    /// Unlike the request-free compatibility route, this entry point records
    /// the run observation before admission and retains the terminal result
    /// with table mutations in one owner-fenced commit.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_project_request_with_arguments(
        &self,
        target: RuntimeTarget<'_>,
        request: RequestIdentity,
        fingerprint: [u8; 32],
        project: &ProjectUnit,
        root_entry: &str,
        arguments: &Environment,
        loaded_project: &LoadedProject,
        table_object_ids: &BTreeMap<String, TableObjectId>,
        storage_orders: &BTreeMap<String, Vec<String>>,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let state =
            RuntimeState::open(target.repository, target.identity, target.initial_digest).await?;
        if let Some(status) = state.request_status(request, fingerprint).await? {
            return replay_or_fence_request(status);
        }
        let lease = state.acquire_lease(target.owner_id).await?;
        let start = state
            .begin_observed_request(
                RunObservationRegistration {
                    request,
                    consumer_identity: request_consumer_identity(),
                    function: root_entry.into(),
                    source_identity: Some(project.project_id.clone()),
                    invocation_id: request.request_id,
                },
                fingerprint,
                lease,
            )
            .await?;
        if !start.admitted {
            return replay_or_fence_request(start.request);
        }
        if !root_entry.contains('.') {
            return terminalize_observed_outcome(
                &state,
                request,
                fingerprint,
                lease,
                StageOutcome::Skipped {
                    reason: "project transaction roots must be namespace-qualified".into(),
                },
            )
            .await;
        }
        let admitted =
            match admit_transaction_project(project, self.limits, root_entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => {
                    return terminalize_observed_outcome(
                        &state,
                        request,
                        fingerprint,
                        lease,
                        *outcome,
                    )
                    .await;
                }
            };
        if admit_project_list_stream(project, &admitted, root_entry, target.identity).is_ok() {
            return terminalize_observed_outcome(
                &state,
                request,
                fingerprint,
                lease,
                StageOutcome::Skipped {
                    reason: PROJECT_STREAM_ROOT_ADMISSION_REASON.into(),
                },
            )
            .await;
        }
        self.execute_admitted_project_request(
            target.repository,
            &state,
            lease,
            request,
            fingerprint,
            root_entry,
            admitted,
            arguments,
            loaded_project,
            table_object_ids,
            storage_orders,
        )
        .await
    }

    /// Runs the deliberately narrow finite-list stream shape from independently
    /// admitted project modules. The root and its zero-argument list producer
    /// remain qualified module functions; arbitrary stream expressions stay
    /// outside this execution seam.
    pub async fn execute_project_stream(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        project: &ProjectUnit,
        root_entry: &str,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        self.execute_project_stream_with_control(
            RuntimeTarget {
                repository,
                identity,
                owner_id,
                initial_digest,
            },
            project,
            root_entry,
            &orna_runtime::NeverCancelled,
        )
        .await
    }

    /// Executes the immutable ASSERT-CHECKPOINT-091 contract through the
    /// durable project-stream adapter and returns only runtime-adapter
    /// observations. This is not compiler-produced engine evidence and does
    /// not claim full Orna conformance.
    pub async fn execute_assert_checkpoint_091(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
    ) -> Result<(StageOutcome<Diagnostic>, bool, Option<String>, u32, bool), RuntimeError> {
        let project = assertion_checkpoint_091_project();
        let outcome = self
            .execute_project_stream(
                repository,
                identity,
                owner_id,
                initial_digest,
                &project,
                "main.ingest",
            )
            .await?;
        let admitted = match admit_transaction_project(
            &project,
            self.limits,
            "main.ingest",
            &self.catalogue,
        ) {
            Ok(admitted) => admitted,
            Err(_) => return Ok((outcome, false, None, 0, false)),
        };
        let bridge = match admit_project_list_stream(&project, &admitted, "main.ingest", identity) {
            Ok(bridge) => bridge,
            Err(_) => return Ok((outcome, false, None, 0, false)),
        };
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let one = Value::int(1.into())
            .encode()
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let two = Value::int(2.into())
            .encode()
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let rows_rolled_back = state.committed_table_row("Reading", &one).await?.is_some()
            && state.committed_table_row("Reading", &two).await?.is_none();
        let writer = state.acquire_lease(owner_id).await?;
        let checkpoint_key = bridge.checkpoint_key()?;
        let checkpoint = state
            .stream_backend(writer)
            .checkpoint_async(&checkpoint_key)
            .await?;
        let checkpoint_is_expected = checkpoint.key == checkpoint_key
            && checkpoint.version == 1
            && checkpoint
                .committed
                .as_ref()
                .is_some_and(|position| position.token.as_str() == "1");
        let checkpoint_position = checkpoint_is_expected.then(|| {
            checkpoint
                .committed
                .as_ref()
                .expect("expected checkpoint has a committed position")
                .token
                .as_str()
                .to_owned()
        });
        let delivery_identity = |position: &str, successor: &str| {
            Ok::<_, RuntimeError>(FailureIdentity(DeliveryIdentity {
                consumer: checkpoint_key.consumer.clone(),
                source_format: checkpoint_key.source_format.clone(),
                source: checkpoint_key.source.clone(),
                partition_format: checkpoint_key.partition_format.clone(),
                partition: checkpoint_key.partition.clone(),
                position_format: checkpoint_key.position_format.clone(),
                position: Position {
                    token: Component::new(position).map_err(|_| RuntimeError::RecoveryInvalid)?,
                },
                successor: Position {
                    token: Component::new(successor).map_err(|_| RuntimeError::RecoveryInvalid)?,
                },
            }))
        };
        let first_delivery_identity = delivery_identity("0", "1")?;
        let failure_identity = delivery_identity("1", "2")?;
        let stream = state.stream_backend(writer);
        let first_failure = stream.failure_async(&first_delivery_identity).await?;
        let failure = stream.failure_async(&failure_identity).await?;
        let exact_one_failure = first_failure.is_none()
            && failure
                .as_ref()
                .is_some_and(|failure| failure.attempts == 1);
        let (attempts, replayable_payload) = match failure {
            Some(failure) => {
                let payload = stream
                    .failure_payload_metadata_async(&failure.identity)
                    .await?;
                let replayable_source = if checkpoint_is_expected {
                    let expected_payload = bridge.payloads.get(1);
                    let mut source =
                        ListStreamSource::new(checkpoint.key.clone(), bridge.payloads.clone());
                    source.descriptor().replayable
                        && matches!(
                            source.next(&checkpoint).await,
                            Ok(StreamSourcePoll::Item(item))
                                if item.delivery.checkpoint_key() == checkpoint_key
                                    && item.delivery.position.token.as_str() == "1"
                                    && item.delivery.successor.token.as_str() == "2"
                                    && expected_payload
                                        .is_some_and(|expected| item.payload == *expected)
                        )
                } else {
                    false
                };
                (
                    failure.attempts,
                    replayable_source
                        && payload.is_some_and(|payload| {
                            payload.redacted && payload.plaintext_bytes.is_some()
                        }),
                )
            }
            None => (0, false),
        };
        let attempts = if exact_one_failure { attempts } else { 0 };
        Ok((
            outcome,
            rows_rolled_back,
            checkpoint_position,
            attempts,
            replayable_payload,
        ))
    }

    /// Executes the immutable FAIL-001 contract through the durable stream
    /// adapter: one delivery is failed 10,000 times, retry transitions are
    /// fenced by the unchanged checkpoint, and the stable natural identity is
    /// read back after reopening. This is runtime-adapter implementation
    /// evidence, not compiler-produced or full Orna-engine execution.
    pub async fn execute_fail_001(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
    ) -> Result<Fail001Witness, RuntimeError> {
        const ATTEMPTS: u32 = 10_000;
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let writer = state.acquire_lease(owner_id).await?;
        let delivery = DeliveryIdentity {
            consumer: ConsumerIdentity {
                principal: Component::new("fixture").map_err(|_| RuntimeError::RecoveryInvalid)?,
                root: Component::new("main").map_err(|_| RuntimeError::RecoveryInvalid)?,
                function: Component::new("fail").map_err(|_| RuntimeError::RecoveryInvalid)?,
                binding: Component::new("item").map_err(|_| RuntimeError::RecoveryInvalid)?,
            },
            source_format: Component::new("list").map_err(|_| RuntimeError::RecoveryInvalid)?,
            source: Component::new("fixture:fail-001")
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition_format: Component::new("none").map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition: None,
            position_format: Component::new("index").map_err(|_| RuntimeError::RecoveryInvalid)?,
            position: Position {
                token: Component::new("0").map_err(|_| RuntimeError::RecoveryInvalid)?,
            },
            successor: Position {
                token: Component::new("1").map_err(|_| RuntimeError::RecoveryInvalid)?,
            },
        };
        let failure_identity = FailureIdentity(delivery.clone());
        let checkpoint_key = delivery.checkpoint_key();
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let initial_checkpoint = state
            .stream_backend(writer)
            .checkpoint_async(&checkpoint_key)
            .await?;
        if initial_checkpoint.version != expected.version
            || initial_checkpoint.committed != expected.committed
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::ExecutionRejected,
            class: DiagnosticClass::Permanent,
        };
        let payload_bytes = b"fail-001-item".to_vec();
        let payload = StreamFailurePayload::Plaintext(payload_bytes.clone());

        let mut previous = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await?
            {
                CommitResult::Acquired { lease } => lease,
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            match stream
                .fail_async(lease, diagnostic, payload.clone())
                .await?
            {
                CommitResult::Failed { failure } => failure,
                _ => return Err(RuntimeError::RecoveryInvalid),
            }
        };
        if previous.identity != failure_identity
            || previous.version != 1
            || previous.attempts != 1
            || previous.status != FailureStatus::Failed
            || previous.diagnostic != diagnostic
        {
            return Err(RuntimeError::RecoveryInvalid);
        }

        for attempts in 2..=ATTEMPTS {
            let mut stream = state.stream_backend(writer);
            let retrying = match stream
                .apply_async(CommitIntent::Retry {
                    failure: failure_identity.clone(),
                    expected_version: previous.version,
                    expected: expected.clone(),
                })
                .await?
            {
                CommitResult::RetryScheduled { failure } => failure,
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            if retrying.identity != failure_identity
                || retrying.version
                    != previous
                        .version
                        .checked_add(1)
                        .ok_or(RuntimeError::RecoveryInvalid)?
                || retrying.attempts != attempts
                || retrying.status != FailureStatus::Retrying
                || retrying.diagnostic != diagnostic
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            previous = match stream
                .fail_retry_async(delivery.clone(), diagnostic, payload.clone())
                .await?
            {
                CommitResult::Failed { failure } => failure,
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            if previous.identity != failure_identity
                || previous.version
                    != retrying
                        .version
                        .checked_add(1)
                        .ok_or(RuntimeError::RecoveryInvalid)?
                || previous.attempts != attempts
                || previous.status != FailureStatus::Failed
                || previous.diagnostic != diagnostic
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }

        let checkpoint_unchanged = state
            .stream_backend(writer)
            .checkpoint_async(&checkpoint_key)
            .await?
            == initial_checkpoint;
        let durable_failure = state
            .stream_backend(writer)
            .failure_async(&failure_identity)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let durable_failure_rows = state.stream_failure_row_count_for_evidence().await?;
        let payload_metadata = state
            .stream_backend(writer)
            .failure_payload_metadata_async(&failure_identity)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let single_failure_identity = durable_failure.identity == failure_identity
            && durable_failure == previous
            && durable_failure.attempts == ATTEMPTS
            && durable_failure.status == FailureStatus::Failed
            && durable_failure.diagnostic == diagnostic
            && payload_metadata.plaintext_bytes == Some(payload_bytes.len() as u64)
            && !payload_metadata.protected_reference
            && payload_metadata.redacted;
        if !single_failure_identity || durable_failure_rows != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        drop(state);

        let reopened_state = RuntimeState::open(repository, identity, initial_digest).await?;
        let reopened_writer = reopened_state.acquire_lease(owner_id).await?;
        let reopened_stream = reopened_state.stream_backend(reopened_writer);
        let reopened_failure_rows = reopened_state
            .stream_failure_row_count_for_evidence()
            .await?;
        let reopened = reopened_stream.checkpoint_async(&checkpoint_key).await?
            == initial_checkpoint
            && reopened_stream
                .failure_async(&failure_identity)
                .await?
                .is_some_and(|failure| {
                    failure == previous
                        && failure.attempts == ATTEMPTS
                        && failure.status == FailureStatus::Failed
                        && failure.diagnostic == diagnostic
                })
            && reopened_stream
                .failure_payload_metadata_async(&failure_identity)
                .await?
                .is_some_and(|metadata| {
                    metadata.plaintext_bytes == Some(payload_bytes.len() as u64)
                        && !metadata.protected_reference
                        && metadata.redacted
                });
        if !checkpoint_unchanged
            || durable_failure_rows != 1
            || reopened_failure_rows != 1
            || !reopened
        {
            return Err(RuntimeError::RecoveryInvalid);
        }

        Ok(Fail001Witness {
            attempts: ATTEMPTS,
            durable_failure_rows,
            single_failure_identity,
            checkpoint_unchanged,
            reopened,
        })
    }

    /// Executes the immutable CP-001 finite-list contract through the
    /// validated table-delivery boundary. The setup preserves the scenario's
    /// opaque checkpoint `41`; the finite source supplies item `42` and its
    /// provider-defined successor without interpreting either position as an
    /// ordered integer.
    ///
    /// CP-002 is deliberately not claimed here: the immutable scenario needs
    /// a handler that inserts and then errors, which the existing handler
    /// result API cannot express as one activation. Assertion-validation
    /// rollback is likewise separate evidence, not CP-002 handler-failure
    /// evidence.
    pub async fn execute_checkpoint_atomicity_cp_001(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
    ) -> Result<CheckpointAtomicityWitness, RuntimeError> {
        let successful_project = checkpoint_atomicity_project("CP-001");
        let successful_bridge = checkpoint_atomicity_bridge(
            &successful_project,
            self.limits,
            identity,
            &self.catalogue,
        )?;
        let fault_bridge = checkpoint_atomicity_bridge(
            &successful_project,
            self.limits,
            identity,
            &self.catalogue,
        )?;
        let successful_key = successful_bridge.checkpoint_key()?;
        let successful_state = RuntimeState::open(repository, identity, initial_digest).await?;
        let successful_writer = successful_state.acquire_lease(owner_id).await?;
        let successful_checkpoint =
            checkpoint_atomicity_prepare(&successful_state, successful_writer, &successful_key)
                .await?;
        let email_key = Value::int(42.into())
            .encode()
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let expected_email = checkpoint_atomicity_email_row()?;
        let faulted_commit_rolls_back_rows_and_checkpoint = checkpoint_atomicity_fault_rolls_back(
            &successful_state,
            successful_writer,
            &successful_key,
            &successful_checkpoint,
            fault_bridge,
            self.limits,
            &email_key,
        )
        .await?;
        if !faulted_commit_rolls_back_rows_and_checkpoint {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let mut successful_source =
            ListStreamSource::new(successful_key.clone(), successful_bridge.payloads.clone());
        let mut successful_handler = ListTableHandler::new(successful_bridge, self.limits);
        let successful_step = successful_state
            .run_stream_once(
                successful_writer,
                &successful_key,
                &mut successful_source,
                &mut successful_handler,
            )
            .await
            .map_err(checkpoint_atomicity_stream_error)?;
        let successful_after = successful_state.stream_checkpoint(&successful_key).await?;
        let complete_committed_email_row = successful_state
            .committed_table_row("Email", &email_key)
            .await?
            == Some(expected_email);
        let rows_and_checkpoint_commit_together =
            matches!(successful_step, StreamStep::Committed { .. })
                && complete_committed_email_row
                && successful_checkpoint
                    .committed
                    .as_ref()
                    .is_some_and(|position| position.token.as_str() == "41")
                && successful_after.version
                    == successful_checkpoint
                        .version
                        .checked_add(1)
                        .ok_or(RuntimeError::RecoveryInvalid)?
                && successful_after
                    .committed
                    .as_ref()
                    .is_some_and(|position| position.token.as_str() == "42");
        if !rows_and_checkpoint_commit_together || !complete_committed_email_row {
            return Err(RuntimeError::RecoveryInvalid);
        }
        drop(successful_state);

        Ok(CheckpointAtomicityWitness {
            faulted_commit_rolls_back_rows_and_checkpoint,
            rows_and_checkpoint_commit_together,
            complete_committed_email_row,
        })
    }
}
