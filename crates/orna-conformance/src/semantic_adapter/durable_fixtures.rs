use super::*;

pub(super) fn assertion_checkpoint_091_project() -> ProjectUnit {
    ProjectUnit {
        fixture_id: "ASSERT-CHECKPOINT-091".into(),
        project_id: "ASSERT-CHECKPOINT-091".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "ASSERT-CHECKPOINT-091".into(),
            source_id: "main.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
            pub table Reading(id: Int) {
                value: Int,
                assert every(reading => reading.value < 2);
            }
            pub fn input() = Stream.from_list([1, 2], source_identity: "fixture:assertion");
            pub fn ingest() { input() | for_each(value => {
                Reading.insert({ id: value, value: value });
            }); }
        "#
            .into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

pub(super) fn checkpoint_atomicity_project(fixture_id: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: fixture_id.into(),
        project_id: fixture_id.into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: fixture_id.into(),
            source_id: "main.orna".into(),
            parse_as: "module_unit".into(),
            source: include_str!("../../tests/fixtures/scenario-cp-001.orna").into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

pub(super) async fn checkpoint_atomicity_prepare(
    state: &RuntimeState,
    writer: WriterLease,
    key: &CheckpointKey,
) -> Result<orna_stream::Checkpoint, RuntimeError> {
    let initial = state.stream_checkpoint(key).await?;
    if !matches!(
        state.pause_stream(writer, key.clone()).await?,
        orna_runtime::StreamAdministrationOutcome::Paused { .. }
    ) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let checkpoint = state
        .reset_checkpoint(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition::from(&initial),
                to: checkpoint_atomicity_position("41")?,
                reason: "immutable CP-001 setup".into(),
            },
        )
        .await?;
    if !matches!(
        state.resume_stream(writer, key.clone()).await?,
        orna_runtime::StreamAdministrationOutcome::Running { .. }
    ) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(checkpoint)
}

#[derive(Clone, Copy)]
pub(super) struct CheckpointAtomicityFault(FaultPoint);

impl FaultInjector for CheckpointAtomicityFault {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

pub(super) fn checkpoint_atomicity_email_row() -> Result<Vec<u8>, RuntimeError> {
    Value::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Int(42.into())),
        (OvbRaw::Text("value".into()), OvbRaw::Int(42.into())),
    ]))
    .map_err(|_| RuntimeError::RecoveryInvalid)?
    .encode()
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

/// Drives the public validated-delivery fault seam directly because the
/// ordinary stream runner deliberately supplies `NoFault`. The checkpoint
/// positions are compared only for exact identity with the fixture values.
pub(super) async fn checkpoint_atomicity_fault_rolls_back(
    state: &RuntimeState,
    writer: WriterLease,
    key: &CheckpointKey,
    checkpoint: &orna_stream::Checkpoint,
    bridge: ListStreamBridge,
    limits: EvaluatorLimits,
    email_key: &[u8],
) -> Result<bool, RuntimeError> {
    let mut source = ListStreamSource::new(key.clone(), bridge.payloads.clone());
    let item = match source
        .next(checkpoint)
        .await
        .map_err(|_| RuntimeError::RecoveryInvalid)?
    {
        StreamSourcePoll::Item(item) => item,
        StreamSourcePoll::Waiting | StreamSourcePoll::Exhausted => {
            return Err(RuntimeError::RecoveryInvalid);
        }
    };
    if item.delivery.position != checkpoint_atomicity_position("41")?
        || item.delivery.successor != checkpoint_atomicity_position("42")?
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let expected_stream = CheckpointPrecondition::from(checkpoint);
    let lease = match state
        .stream_backend(writer)
        .apply_async(CommitIntent::Acquire {
            delivery: item.delivery.clone(),
            expected: expected_stream.clone(),
            purpose: LeasePurpose::Deliver,
        })
        .await?
    {
        CommitResult::Acquired { lease } => lease,
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let capture = state.capture().await?;
    let mut handler = ListTableHandler::new(bridge, limits);
    let StreamHandlerResult::CommitValidatedTable(mut batch) = handler.handle(&item) else {
        return Err(RuntimeError::RecoveryInvalid);
    };
    let fault = CheckpointAtomicityFault(FaultPoint::AfterCheckpoint);
    let result = state
        .commit_stream_validated_table_delivery(StreamValidatedTableDeliveryCommit {
            writer,
            expected_capture: &capture,
            mutations: &batch.mutations,
            next_digest: batch.next_digest,
            delivery: lease.clone(),
            expected_stream,
            validator: batch.validator.as_mut(),
            faults: &fault,
        })
        .await;
    let rollback_observed = matches!(
        result,
        Err(StreamTableDeliveryError::Runtime(
            RuntimeError::FaultInjected(FaultPoint::AfterCheckpoint)
        ))
    ) && state
        .committed_table_row("Email", email_key)
        .await?
        .is_none()
        && state.stream_checkpoint(key).await? == *checkpoint
        && state.capture().await? == capture;
    let cancelled = matches!(
        state
            .stream_backend(writer)
            .apply_async(CommitIntent::Cancel { lease })
            .await?,
        CommitResult::Cancelled { checkpoint: cancelled, .. } if cancelled == *checkpoint
    );
    Ok(rollback_observed && cancelled)
}

pub(super) fn checkpoint_atomicity_bridge(
    project: &ProjectUnit,
    limits: EvaluatorLimits,
    identity: RuntimeIdentity,
    catalogue: &Catalogue,
) -> Result<ListStreamBridge, RuntimeError> {
    let admitted = admit_transaction_project(project, limits, "main.ingest", catalogue)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    admit_project_list_stream(project, &admitted, "main.ingest", identity)
        .map_err(|_| RuntimeError::RecoveryInvalid)
}
pub(super) fn checkpoint_atomicity_position(token: &str) -> Result<Position, RuntimeError> {
    Ok(Position {
        token: Component::new(token).map_err(|_| RuntimeError::RecoveryInvalid)?,
    })
}

pub(super) fn checkpoint_atomicity_stream_error(
    error: orna_runtime::StreamStepError,
) -> RuntimeError {
    match error {
        orna_runtime::StreamStepError::Runtime(error) => error,
        orna_runtime::StreamStepError::Provider(_) => RuntimeError::RecoveryInvalid,
    }
}
