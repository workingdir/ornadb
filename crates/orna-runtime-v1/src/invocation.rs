//! Runtime materialization and durable invocation observations.
//!
//! The portable 1.0 system model calls the declaration row sys.Function; it
//! does not define a separate sys.Procedure row. A materialized procedure in
//! this module is therefore a runtime-owned, snapshot-pinned executable view
//! over one admitted sys.Function, never a second declaration identity.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use libsql::{TransactionBehavior, params};
use num_bigint::BigInt;
use orna_foundation_v1::{
    CwdCapture, FunctionRef, InvocationArgumentRef, InvocationRef, RunRef, SnapshotRef, TypeRef,
    function_reference, invocation_argument_reference,
    invocation_reference, object_reference, snapshot_reference, type_reference,
    validate_function_reference, validate_invocation_argument_reference,
    validate_invocation_reference, validate_type_reference,
};

use crate::{
    CatalogueFunction, RunObservationId, RuntimeError, RuntimeState, WriterLease, decode_bool,
    CatalogueError, decode_u64, encode_capture, fixed, now_ms, validate_digest, validate_id,
    validate_observation_text, validate_writer_lease,
};

const MAX_INVOCATION_ARGUMENTS: usize = 512;
const MAX_INVOCATION_TAIL_PAGE_SIZE: usize = 256;

/// One resolved callable and its exact durable catalogue pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedProcedure {
    function: FunctionRef,
    snapshot: SnapshotRef,
    capture: CwdCapture,
    qualified_name: String,
    object_id: [u8; 16],
    revision_id: [u8; 32],
    semantic_hash: [u8; 32],
    result_type_object_id: [u8; 16],
    result_type: TypeRef,
    parameters: Vec<MaterializedProcedureParameter>,
    materialized_ms: i64,
}

impl MaterializedProcedure {
    pub fn function(&self) -> &FunctionRef {
        &self.function
    }

    pub fn snapshot(&self) -> &SnapshotRef {
        &self.snapshot
    }

    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    pub fn parameters(&self) -> &[MaterializedProcedureParameter] {
        &self.parameters
    }

    pub fn result_type(&self) -> &TypeRef {
        &self.result_type
    }

    pub fn materialized_ms(&self) -> i64 {
        self.materialized_ms
    }
}

/// Exact parameter metadata copied from the admitted function signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedProcedureParameter {
    pub name: String,
    pub position: u64,
    pub type_object_id: [u8; 16],
    pub type_reference: TypeRef,
}

/// Named, already-bound argument metadata for invocation admission.
///
/// The runtime retains no argument payload. A digest is accepted only for an
/// unredacted value; callers must omit digests for secrets and other
/// low-entropy protected values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationArgumentInput {
    pub name: String,
    pub type_reference: TypeRef,
    pub value_digest: Option<[u8; 32]>,
    pub redacted: bool,
}

/// Launch ownership is exactly one parent invocation, owner session, or
/// top-level command run, as required by the portable invocation row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationLaunchOwner {
    Parent([u8; 16]),
    OwnerSession([u8; 16]),
    TopLevelRun(RunObservationId),
}

/// Complete metadata needed to durably admit one invocation before user code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationObservationRegistration {
    pub id: [u8; 16],
    pub procedure: MaterializedProcedure,
    pub owner: InvocationLaunchOwner,
    /// Run correlation for a child or session-owned invocation, or the exact
    /// same run named by TopLevelRun.
    pub run: Option<RunObservationId>,
    pub arguments: Vec<InvocationArgumentInput>,
    pub idempotency_key_hash: Option<[u8; 32]>,
}

/// Durable status vocabulary for one invocation observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationObservationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Orphaned,
}

impl InvocationObservationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Orphaned
        )
    }

    fn code(self) -> i64 {
        match self {
            Self::Queued => 1,
            Self::Running => 2,
            Self::Succeeded => 3,
            Self::Failed => 4,
            Self::Cancelled => 5,
            Self::Orphaned => 6,
        }
    }
}

/// Terminal outcome recorded after the target and its children have ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationCompletion {
    Succeeded,
    Failed { diagnostic_code: String },
    Cancelled { diagnostic_code: Option<String> },
    Orphaned { diagnostic_code: Option<String> },
}

/// Redaction-safe argument row of a durable invocation observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationArgumentObservation {
    pub reference: InvocationArgumentRef,
    pub invocation: InvocationRef,
    pub name: String,
    pub position: u64,
    pub type_reference: TypeRef,
    pub value_digest: Option<[u8; 32]>,
    pub redacted: bool,
}

/// Durable sys.Invocation projection. Its reference is descriptive and grants
/// no execution, cancellation, or await authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationObservation {
    pub reference: InvocationRef,
    pub id: [u8; 16],
    pub procedure: MaterializedProcedure,
    pub owner: InvocationLaunchOwner,
    pub parent: Option<InvocationRef>,
    pub run: Option<RunRef>,
    pub arguments: Vec<InvocationArgumentObservation>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub observed_ms: i64,
    pub status: InvocationObservationStatus,
    pub failure_code: Option<String>,
    pub idempotency_key_hash: Option<[u8; 32]>,
    pub live: bool,
}

/// Opaque continuation for one append-only invocation tail page context.
/// Snapshot pinning prevents callers from splicing pages across generations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationObservationTailCursor {
    capture: CwdCapture,
    sequence: u64,
}

impl InvocationObservationTailCursor {
    pub fn database_id(&self) -> [u8; 16] {
        self.capture.database_id()
    }

    pub fn runtime_id(&self) -> [u8; 16] {
        self.capture.runtime_id()
    }

    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// One durable lifecycle notification plus the latest retained observation.
/// `status` and `observed_ms` describe this sequenced event; `observation` may
/// already reflect a later event if the row advanced before this page was read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationObservationTailEntry {
    pub sequence: u64,
    pub invocation_id: [u8; 16],
    pub observed_ms: i64,
    pub status: InvocationObservationStatus,
    pub observation: InvocationObservation,
}

/// Bounded keyset page from the append-only invocation lifecycle tail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationObservationTailPage {
    pub entries: Vec<InvocationObservationTailEntry>,
    pub next_cursor: Option<InvocationObservationTailCursor>,
    pub has_more: bool,
}

impl RuntimeState {
    /// Materializes one admitted sys.Function at an exact retained capture.
    /// The operation is a deterministic runtime cache write; it creates no
    /// declaration identity and never resolves against a newer snapshot.
    pub async fn materialize_procedure_at(
        &self,
        writer: WriterLease,
        qualified_name: &str,
        capture: &CwdCapture,
    ) -> Result<Option<MaterializedProcedure>, RuntimeError> {
        validate_observation_text(qualified_name)?;
        let Some(function) = self
            .catalogue_function_at(qualified_name, capture)
            .await
            .map_err(catalogue_read_error)?
        else {
            return Ok(None);
        };
        let function_reference = function
            .object
            .function_reference
            .clone()
            .ok_or(RuntimeError::InvalidObservationReference)?;
        validate_function_reference(function_reference.clone().into_row_ref(), capture)
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let object_id = function.object.object_id;
        let result_type_object_id = function.result_type.object_id();
        let snapshot = encode_capture(capture)?;
        let materialized_ms = now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        tx.execute(
            "INSERT OR IGNORE INTO sys_procedure_materialization
             (function_object_id, snapshot, qualified_name, revision_id, semantic_hash,
              result_type_object_id, materialized_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                object_id.to_vec(),
                snapshot.clone(),
                qualified_name,
                function.object.revision_id.to_vec(),
                function.object.semantic_hash.to_vec(),
                result_type_object_id.to_vec(),
                materialized_ms,
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        for parameter in &function.parameters {
            tx.execute(
                "INSERT OR IGNORE INTO sys_procedure_parameter_materialization
                 (function_object_id, snapshot, position, name, type_object_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    object_id.to_vec(),
                    snapshot.clone(),
                    i64::try_from(parameter.position).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    parameter.name.clone(),
                    parameter.type_object.object_id().to_vec(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        let mut rows = tx
            .query(
                "SELECT qualified_name, revision_id, semantic_hash, result_type_object_id,
                        materialized_ms
                 FROM sys_procedure_materialization
                 WHERE function_object_id = ?1 AND snapshot = ?2",
                params![object_id.to_vec(), snapshot.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let stored_name: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let stored_revision: [u8; 32] = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let stored_semantic: [u8; 32] = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let stored_result: [u8; 16] = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let stored_ms: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if stored_name != qualified_name
            || stored_revision != function.object.revision_id
            || stored_semantic != function.object.semantic_hash
            || stored_result != result_type_object_id
            || stored_ms < 0
        {
            return Err(RuntimeError::ProcedureMaterializationConflict);
        }
        let mut parameter_rows = tx
            .query(
                "SELECT position, name, type_object_id
                 FROM sys_procedure_parameter_materialization
                 WHERE function_object_id = ?1 AND snapshot = ?2 ORDER BY position",
                params![object_id.to_vec(), snapshot],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut persisted_parameters = Vec::new();
        while let Some(row) = parameter_rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            persisted_parameters.push((
                decode_u64(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                row.get::<String>(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
                fixed::<16>(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            ));
        }
        let expected_parameters: Vec<_> = function
            .parameters
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.position,
                    parameter.name.clone(),
                    parameter.type_object.object_id(),
                ))
            })
            .collect::<Result<_, RuntimeError>>()?;
        if persisted_parameters != expected_parameters {
            return Err(RuntimeError::ProcedureMaterializationConflict);
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Some(materialized_procedure_from_catalogue(
            function,
            capture.clone(),
            stored_ms,
        )?))
    }

    /// Admits a durable running invocation before target code begins.
    pub async fn begin_invocation_observation(
        &self,
        writer: WriterLease,
        registration: InvocationObservationRegistration,
    ) -> Result<InvocationObservation, RuntimeError> {
        validate_id(registration.id)?;
        validate_writer_lease(writer)?;
        let procedure = &registration.procedure;
        validate_id(procedure.object_id)?;
        if procedure.materialized_ms < 0
            || procedure.function.as_row_ref().snapshot != *procedure.capture.snapshot()
        {
            return Err(RuntimeError::InvalidObservationReference);
        }
        let parameters = bind_invocation_arguments(procedure, registration.arguments)?;
        if let Some(key_hash) = registration.idempotency_key_hash {
            validate_digest(key_hash)?;
        }
        let snapshot = encode_capture(&procedure.capture)?;
        let (owner_kind, parent, owner_session, owner_run) = match registration.owner {
            InvocationLaunchOwner::Parent(parent) => {
                validate_id(parent)?;
                if parent == registration.id {
                    return Err(RuntimeError::InvocationOwnerInvalid);
                }
                (1_i64, Some(parent), None, None)
            }
            InvocationLaunchOwner::OwnerSession(session) => {
                validate_id(session)?;
                (2_i64, None, Some(session), None)
            }
            InvocationLaunchOwner::TopLevelRun(run) => {
                if registration.run != Some(run) {
                    return Err(RuntimeError::InvocationOwnerInvalid);
                }
                (3_i64, None, None, Some(run.0))
            }
        };
        let run = registration.run.map(|run| run.0).or(owner_run);
        if owner_run.is_some() && run != owner_run {
            return Err(RuntimeError::InvocationOwnerInvalid);
        }
        let started_ms = now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let mut procedure_rows = tx
            .query(
                "SELECT revision_id, semantic_hash, result_type_object_id
                 FROM sys_procedure_materialization
                 WHERE function_object_id = ?1 AND snapshot = ?2 AND qualified_name = ?3",
                params![
                    procedure.object_id.to_vec(),
                    snapshot.clone(),
                    procedure.qualified_name.clone(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let procedure_row = procedure_rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::ProcedureMaterializationConflict)?;
        let revision: [u8; 32] = fixed(procedure_row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let semantic: [u8; 32] = fixed(procedure_row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let result_type: [u8; 16] = fixed(procedure_row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        if revision != procedure.revision_id
            || semantic != procedure.semantic_hash
            || result_type != procedure.result_type_object_id
        {
            return Err(RuntimeError::ProcedureMaterializationConflict);
        }
        validate_launch_owner(
            &tx,
            registration.id,
            &procedure.capture,
            &registration.owner,
            run,
            writer,
        )
        .await?;
        tx.execute(
            "INSERT INTO sys_invocation_observation
             (invocation_id, function_object_id, snapshot, generation_digest, runtime_id,
              runtime_generation, owner_kind, parent_invocation_id, run_id, owner_session_id,
              owner_id, owner_epoch, started_ms, ended_ms, observed_ms, status,
              result_type_object_id, failure_code, idempotency_key_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?13,
                     ?14, ?15, NULL, ?16)",
            params![
                registration.id.to_vec(),
                procedure.object_id.to_vec(),
                snapshot.clone(),
                procedure.capture.generation_digest().to_vec(),
                procedure.capture.runtime_id().to_vec(),
                bigint_to_i64(procedure.capture.generation())?,
                owner_kind,
                parent.map(|parent| parent.to_vec()),
                run.map(|run| run.to_vec()),
                owner_session.map(|session| session.to_vec()),
                writer.owner_id.to_vec(),
                i64::try_from(writer.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                started_ms,
                InvocationObservationStatus::Running.code(),
                procedure.result_type_object_id.to_vec(),
                registration.idempotency_key_hash.map(|hash| hash.to_vec()),
            ],
        )
        .await
        .map_err(|_| RuntimeError::InvocationObservationConflict)?;
        for (position, input, type_object_id) in parameters {
            tx.execute(
                "INSERT INTO sys_invocation_argument
                 (invocation_id, position, name, type_object_id, value_digest, redacted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    registration.id.to_vec(),
                    i64::try_from(position).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    input.name,
                    type_object_id.to_vec(),
                    input.value_digest.map(|digest| digest.to_vec()),
                    if input.redacted { 1_i64 } else { 0_i64 },
                ],
            )
            .await
            .map_err(|_| RuntimeError::InvocationObservationConflict)?;
        }
        append_invocation_tail_event(
            &tx,
            registration.id,
            started_ms,
            InvocationObservationStatus::Running,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.invocation_observation(registration.id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)
    }

    /// Records one terminal result. A parent remains active until every child
    /// has a terminal observation, and terminal observations never reopen.
    pub async fn finish_invocation_observation(
        &self,
        writer: WriterLease,
        id: [u8; 16],
        completion: InvocationCompletion,
    ) -> Result<InvocationObservation, RuntimeError> {
        validate_id(id)?;
        let (status, failure_code) = completion_fields(completion)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let mut rows = tx
            .query(
                "SELECT owner_id, owner_epoch, parent_invocation_id, status
                 FROM sys_invocation_observation WHERE invocation_id = ?1",
                params![id.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::InvocationStateConflict)?;
        let owner = WriterLease {
            owner_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            epoch: decode_u64(row.get::<i64>(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        };
        if owner != writer {
            return Err(RuntimeError::OwnerLost);
        }
        let parent: Option<Vec<u8>> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let current = decode_invocation_status(
            row.get::<i64>(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        if current.is_terminal() || current != InvocationObservationStatus::Running {
            return Err(RuntimeError::InvocationStateConflict);
        }
        let mut children = tx
            .query(
                "SELECT 1 FROM sys_invocation_observation
                 WHERE parent_invocation_id = ?1 AND status IN (?2, ?3) LIMIT 1",
                params![id.to_vec(), InvocationObservationStatus::Queued.code(), InvocationObservationStatus::Running.code()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if children
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some()
        {
            return Err(RuntimeError::InvocationChildrenActive);
        }
        let ended_ms = now_ms()?;
        let changed = tx
            .execute(
                "UPDATE sys_invocation_observation
                 SET status = ?1, failure_code = ?2, ended_ms = ?3, observed_ms = ?3
                 WHERE invocation_id = ?4 AND owner_id = ?5 AND owner_epoch = ?6
                   AND status IN (?7, ?8)",
                params![
                    status.code(),
                    failure_code,
                    ended_ms,
                    id.to_vec(),
                    writer.owner_id.to_vec(),
                    i64::try_from(writer.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    InvocationObservationStatus::Queued.code(),
                    InvocationObservationStatus::Running.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::InvocationStateConflict);
        }
        append_invocation_tail_event(&tx, id, ended_ms, status).await?;
        if parent.is_some() {
            // Parent ownership is recorded for readers; it does not transfer
            // child effects or permit detaching a child from its owner.
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.invocation_observation(id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)
    }

    /// Transfers unfinished observations owned by an abandoned writer into
    /// the explicit orphaned terminal state under the replacement fence.
    pub async fn orphan_abandoned_invocations(
        &self,
        replacement: WriterLease,
    ) -> Result<u64, RuntimeError> {
        validate_writer_lease(replacement)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, replacement).await?;
        let ended_ms = now_ms()?;
        let mut rows = tx
            .query(
                "SELECT invocation_id, parent_invocation_id FROM sys_invocation_observation
                 WHERE status IN (?1, ?2)
                   AND (owner_id <> ?3 OR owner_epoch <> ?4)
                 ORDER BY invocation_id",
                params![
                    InvocationObservationStatus::Queued.code(),
                    InvocationObservationStatus::Running.code(),
                    replacement.owner_id.to_vec(),
                    i64::try_from(replacement.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut parents = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let parent = row
                .get::<Option<Vec<u8>>>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                .map(fixed::<16>)
                .transpose()?;
            parents.insert(id, parent);
        }
        drop(rows);

        // Task termination publishes children before their parent. Sort the
        // orphan tail deepest-first so takeover history preserves that order.
        let mut remaining_children = parents
            .keys()
            .map(|id| (*id, 0_usize))
            .collect::<BTreeMap<_, _>>();
        for parent in parents.values().flatten() {
            if let Some(children) = remaining_children.get_mut(parent) {
                *children += 1;
            }
        }
        let mut ready = remaining_children
            .iter()
            .filter_map(|(id, children)| (*children == 0).then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut ids = Vec::with_capacity(parents.len());
        while let Some(id) = ready.iter().next().copied() {
            ready.remove(&id);
            ids.push(id);
            if let Some(Some(parent_id)) = parents.get(&id) {
                if let Some(children) = remaining_children.get_mut(parent_id) {
                    *children -= 1;
                    if *children == 0 {
                        ready.insert(*parent_id);
                    }
                }
            }
        }
        if ids.len() != parents.len() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let mut changed = 0_u64;
        for id in ids {
            let updated = tx
                .execute(
                    "UPDATE sys_invocation_observation
                     SET status = ?1, failure_code = 'sys.invoke.orphaned',
                         ended_ms = ?2, observed_ms = ?2
                     WHERE invocation_id = ?3 AND status IN (?4, ?5)
                       AND (owner_id <> ?6 OR owner_epoch <> ?7)",
                    params![
                        InvocationObservationStatus::Orphaned.code(),
                        ended_ms,
                        id.to_vec(),
                        InvocationObservationStatus::Queued.code(),
                        InvocationObservationStatus::Running.code(),
                        replacement.owner_id.to_vec(),
                        i64::try_from(replacement.epoch)
                            .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            if updated == 1 {
                append_invocation_tail_event(
                    &tx,
                    id,
                    ended_ms,
                    InvocationObservationStatus::Orphaned,
                )
                .await?;
                changed += 1;
            }
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(changed)
    }

    /// Reads one retained invocation and its redaction-safe argument rows.
    pub async fn invocation_observation(
        &self,
        id: [u8; 16],
    ) -> Result<Option<InvocationObservation>, RuntimeError> {
        validate_id(id)?;
        load_invocation_observation(&self.connection, id).await
    }

    /// Reads durable invocation observations in deterministic start/id order.
    pub async fn invocation_observations(
        &self,
    ) -> Result<Vec<InvocationObservation>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT invocation_id FROM sys_invocation_observation
                 ORDER BY started_ms, invocation_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            ids.push(fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?);
        }
        let mut observations = Vec::with_capacity(ids.len());
        for id in ids {
            observations.push(
                load_invocation_observation(&self.connection, id)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            );
        }
        Ok(observations)
    }

    /// Reads lifecycle changes in strict durable sequence order. The token is
    /// pinned to this exact capture, order and unfiltered projection; callers
    /// must start a fresh page sequence after a CWD generation change.
    pub async fn invocation_observation_tail(
        &self,
        after: Option<InvocationObservationTailCursor>,
        limit: usize,
    ) -> Result<InvocationObservationTailPage, RuntimeError> {
        if limit == 0 || limit > MAX_INVOCATION_TAIL_PAGE_SIZE {
            return Err(RuntimeError::InvocationTailLimit);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let capture = crate::capture_tx(&tx).await?;
        // CWD captures include generation as well as digest, so repeating an
        // old digest cannot revive its cursors. Receipt replay or a rolled-back
        // write leaves the capture unchanged; committed generations invalidate.
        if after
            .as_ref()
            .is_some_and(|cursor| cursor.capture != capture)
        {
            return Err(RuntimeError::InvocationTailInvalid);
        }
        let after_sequence = after
            .as_ref()
            .map(|cursor| cursor.sequence)
            .unwrap_or(0);
        let row_limit = i64::try_from(limit + 1).map_err(|_| RuntimeError::InvocationTailLimit)?;
        let mut rows = tx
            .query(
                "SELECT sequence, invocation_id, observed_ms, status
                 FROM sys_invocation_observation_tail
                 WHERE sequence > ?1 ORDER BY sequence LIMIT ?2",
                params![
                    i64::try_from(after_sequence).map_err(|_| RuntimeError::InvocationTailInvalid)?,
                    row_limit,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut events = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            events.push((
                decode_u64(row.get::<i64>(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                row.get::<i64>(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
                decode_invocation_status(
                    row.get::<i64>(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?,
            ));
        }
        let has_more = events.len() > limit;
        events.truncate(limit);
        let mut entries = Vec::with_capacity(events.len());
        for (sequence, invocation_id, observed_ms, status) in events {
            let observation = load_invocation_observation(&tx, invocation_id)
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?;
            entries.push(InvocationObservationTailEntry {
                sequence,
                invocation_id,
                observed_ms,
                status,
                observation,
            });
        }
        // The reference specifies durable invocation observations, not a tail
        // transport protocol. Keep an empty poll at its existing sequence so
        // a caller can poll again and observe later append-only transitions.
        let next_cursor = entries
            .last()
            .map(|entry| InvocationObservationTailCursor {
                capture: capture.clone(),
                sequence: entry.sequence,
            })
            .or(after);
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(InvocationObservationTailPage {
            entries,
            next_cursor,
            has_more,
        })
    }
}

fn materialized_procedure_from_catalogue(
    function: CatalogueFunction,
    capture: CwdCapture,
    materialized_ms: i64,
) -> Result<MaterializedProcedure, RuntimeError> {
    let function_reference = function
        .object
        .function_reference
        .ok_or(RuntimeError::InvalidObservationReference)?;
    let snapshot = snapshot_reference(capture.database_id(), capture.snapshot().clone())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let parameters = function
        .parameters
        .into_iter()
        .map(|parameter| MaterializedProcedureParameter {
            name: parameter.name,
            position: parameter.position,
            type_object_id: parameter.type_object.object_id(),
            type_reference: parameter.type_object.reference().clone(),
        })
        .collect();
    Ok(MaterializedProcedure {
        function: function_reference,
        snapshot,
        capture,
        qualified_name: function.object.qualified_name,
        object_id: function.object.object_id,
        revision_id: function.object.revision_id,
        semantic_hash: function.object.semantic_hash,
        result_type_object_id: function.result_type.object_id(),
        result_type: function.result_type.reference().clone(),
        parameters,
        materialized_ms,
    })
}

fn catalogue_read_error(error: CatalogueError) -> RuntimeError {
    match error {
        CatalogueError::StorageUnavailable => RuntimeError::StorageUnavailable,
        CatalogueError::StaleCapture { current } => RuntimeError::StaleCapture { current },
        _ => RuntimeError::RecoveryInvalid,
    }
}

async fn append_invocation_tail_event(
    connection: &libsql::Connection,
    id: [u8; 16],
    observed_ms: i64,
    status: InvocationObservationStatus,
) -> Result<(), RuntimeError> {
    // Administrative effects use their own durable receipt journal; only
    // sys.Invocation lifecycle transitions consume this tail sequence.
    connection
        .execute(
            "INSERT INTO sys_invocation_observation_tail
             (invocation_id, observed_ms, status) VALUES (?1, ?2, ?3)",
            params![id.to_vec(), observed_ms, status.code()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

fn bind_invocation_arguments(
    procedure: &MaterializedProcedure,
    mut arguments: Vec<InvocationArgumentInput>,
) -> Result<Vec<(u64, InvocationArgumentInput, [u8; 16])>, RuntimeError> {
    if arguments.len() > MAX_INVOCATION_ARGUMENTS
        || arguments.len() != procedure.parameters.len()
    {
        return Err(RuntimeError::InvocationOwnerInvalid);
    }
    arguments.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    let mut bound = Vec::with_capacity(arguments.len());
    let mut names = BTreeMap::new();
    for input in arguments {
        validate_observation_text(&input.name)?;
        if input.redacted && input.value_digest.is_some() {
            return Err(RuntimeError::InvalidDigest);
        }
        if let Some(value_digest) = input.value_digest {
            validate_digest(value_digest)?;
        }
        if names.insert(input.name.clone(), ()).is_some() {
            return Err(RuntimeError::InvocationOwnerInvalid);
        }
        let parameter = procedure
            .parameters
            .iter()
            .find(|parameter| parameter.name == input.name)
            .ok_or(RuntimeError::InvocationOwnerInvalid)?;
        if input.type_reference != parameter.type_reference {
            return Err(RuntimeError::InvocationOwnerInvalid);
        }
        bound.push((
            parameter.position,
            input,
            parameter.type_object_id,
        ));
    }
    bound.sort_by_key(|(position, _, _)| *position);
    Ok(bound)
}

fn completion_fields(
    completion: InvocationCompletion,
) -> Result<(InvocationObservationStatus, Option<String>), RuntimeError> {
    let result = match completion {
        InvocationCompletion::Succeeded => (InvocationObservationStatus::Succeeded, None),
        InvocationCompletion::Failed { diagnostic_code } => {
            validate_diagnostic_code(&diagnostic_code)?;
            (InvocationObservationStatus::Failed, Some(diagnostic_code))
        }
        InvocationCompletion::Cancelled { diagnostic_code } => {
            if let Some(code) = &diagnostic_code {
                validate_diagnostic_code(code)?;
            }
            (InvocationObservationStatus::Cancelled, diagnostic_code)
        }
        InvocationCompletion::Orphaned { diagnostic_code } => {
            if let Some(code) = &diagnostic_code {
                validate_diagnostic_code(code)?;
            }
            (InvocationObservationStatus::Orphaned, diagnostic_code)
        }
    };
    Ok(result)
}

fn validate_diagnostic_code(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RuntimeError::InvalidIdentity);
    }
    Ok(())
}

async fn validate_launch_owner(
    tx: &libsql::Transaction,
    invocation_id: [u8; 16],
    capture: &CwdCapture,
    owner: &InvocationLaunchOwner,
    run: Option<[u8; 16]>,
    writer: WriterLease,
) -> Result<(), RuntimeError> {
    match owner {
        InvocationLaunchOwner::Parent(parent_id) => {
            let mut rows = tx
                .query(
                    "SELECT snapshot, generation_digest, status, owner_id, owner_epoch, run_id
                     FROM sys_invocation_observation WHERE invocation_id = ?1",
                    params![parent_id.to_vec()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let row = rows
                .next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .ok_or(RuntimeError::InvocationOwnerInvalid)?;
            let parent_snapshot: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let parent_digest: [u8; 32] = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let parent_status = decode_invocation_status(
                row.get::<i64>(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let parent_owner = WriterLease {
                owner_id: fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                epoch: decode_u64(row.get::<i64>(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            };
            let parent_run: Option<Vec<u8>> = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let parent_run = parent_run.map(fixed::<16>).transpose()?;
            if parent_snapshot != encode_capture(capture)?
                || parent_digest != capture.generation_digest()
                || parent_status.is_terminal()
                || parent_owner != writer
                || parent_run != run
            {
                return Err(RuntimeError::InvocationOwnerInvalid);
            }
        }
        InvocationLaunchOwner::OwnerSession(session_id) => validate_id(*session_id)?,
        InvocationLaunchOwner::TopLevelRun(run_id) => {
            if run != Some(run_id.0) {
                return Err(RuntimeError::InvocationOwnerInvalid);
            }
            let mut rows = tx
                .query(
                    "SELECT 1 FROM sys_run_observation
                     WHERE run_id = ?1 AND invocation_id = ?2 AND snapshot = ?3
                       AND generation_digest = ?4 AND status IN (1, 2)",
                    params![
                        run_id.0.to_vec(),
                        invocation_id.to_vec(),
                        encode_capture(capture)?,
                        capture.generation_digest().to_vec(),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            if rows
                .next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .is_none()
            {
                return Err(RuntimeError::InvocationOwnerInvalid);
            }
        }
    }
    if let Some(run_id) = run {
        let mut rows = tx
            .query(
                "SELECT 1 FROM sys_run_observation
                 WHERE run_id = ?1 AND snapshot = ?2 AND generation_digest = ?3
                   AND status IN (1, 2)",
                params![
                    run_id.to_vec(),
                    encode_capture(capture)?,
                    capture.generation_digest().to_vec(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_none()
        {
            return Err(RuntimeError::InvocationOwnerInvalid);
        }
    }
    Ok(())
}

async fn load_invocation_observation(
    connection: &libsql::Connection,
    id: [u8; 16],
) -> Result<Option<InvocationObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT function_object_id, snapshot, generation_digest, runtime_id,
                    runtime_generation, owner_kind, parent_invocation_id, run_id,
                    owner_session_id, owner_id, owner_epoch, started_ms, ended_ms,
                    observed_ms, status, result_type_object_id, failure_code,
                    idempotency_key_hash
             FROM sys_invocation_observation WHERE invocation_id = ?1",
            params![id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let function_object_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let snapshot_bytes: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let generation_digest: [u8; 32] = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id: [u8; 16] = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_generation: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let capture = crate::decode_capture(snapshot_bytes.clone(), generation_digest)?;
    if runtime_id != capture.runtime_id()
        || runtime_generation != bigint_to_i64(capture.generation())?
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let procedure = load_materialized_procedure(connection, function_object_id, &capture).await?;
    let result_type_object_id = fixed(row.get(15).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    if result_type_object_id != procedure.result_type_object_id {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let owner_kind: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let parent: Option<Vec<u8>> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let run: Option<Vec<u8>> = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let owner_session: Option<Vec<u8>> = row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let run_id = run.map(fixed::<16>).transpose()?;
    let owner = match (owner_kind, parent, owner_session, run_id) {
        (1, Some(parent), None, _run) => InvocationLaunchOwner::Parent(fixed(parent)?),
        (2, None, Some(session), _) => InvocationLaunchOwner::OwnerSession(fixed(session)?),
        (3, None, None, Some(run)) => InvocationLaunchOwner::TopLevelRun(RunObservationId(run)),
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let writer = WriterLease {
        owner_id: fixed(row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        epoch: decode_u64(row.get::<i64>(10).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
    };
    if writer.epoch == 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let started_ms: i64 = row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let ended_ms: Option<i64> = row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let observed_ms: i64 = row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let status = decode_invocation_status(
        row.get::<i64>(14).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let failure_code: Option<String> = row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let idempotency_key_hash: Option<Vec<u8>> =
        row.get(17).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let idempotency_key_hash = idempotency_key_hash.map(fixed::<32>).transpose()?;
    if let Some(key_hash) = idempotency_key_hash {
        validate_digest(key_hash)?;
    }
    if started_ms < 0
        || observed_ms < started_ms
        || status.is_terminal() != ended_ms.is_some()
        || ended_ms.is_some_and(|ended| ended < started_ms || ended > observed_ms)
        || match status {
            InvocationObservationStatus::Queued
            | InvocationObservationStatus::Running
            | InvocationObservationStatus::Succeeded => failure_code.is_some(),
            InvocationObservationStatus::Failed => failure_code.is_none(),
            InvocationObservationStatus::Cancelled | InvocationObservationStatus::Orphaned => false,
        }
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if let Some(code) = &failure_code {
        validate_diagnostic_code(code)?;
    }
    let mut current_writer_rows = connection
        .query("SELECT owner_id, epoch FROM writer_lease WHERE singleton = 1", ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let live = match current_writer_rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        Some(row) => {
            let current = WriterLease {
                owner_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                epoch: decode_u64(row.get::<i64>(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            };
            !status.is_terminal() && current == writer
        }
        None => false,
    };
    let invocation = invocation_reference(
        capture.database_id(),
        capture.snapshot().clone(),
        id,
    )
    .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_invocation_reference(invocation.clone().into_row_ref(), &capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let parent = match owner {
        InvocationLaunchOwner::Parent(parent_id) => {
            let reference = invocation_reference(
                capture.database_id(),
                capture.snapshot().clone(),
                parent_id,
            )
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
            Some(
                validate_invocation_reference(reference.into_row_ref(), &capture)
                    .map_err(|_| RuntimeError::InvalidObservationReference)?,
            )
        }
        _ => None,
    };
    let run = if let Some(run_id) = run_id {
        let run_observation = super::load_run_observation_tx(connection, RunObservationId(run_id), &capture)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        Some(
            run_observation
                .reference()
                .map_err(|_| RuntimeError::InvalidObservationReference)?,
        )
    } else {
        None
    };
    let arguments = load_invocation_arguments(connection, id, &invocation, &procedure).await?;
    Ok(Some(InvocationObservation {
        reference: invocation,
        id,
        procedure,
        owner,
        parent,
        run,
        arguments,
        started_ms,
        ended_ms,
        observed_ms,
        status,
        failure_code,
        idempotency_key_hash,
        live,
    }))
}

async fn load_materialized_procedure(
    connection: &libsql::Connection,
    function_object_id: [u8; 16],
    capture: &CwdCapture,
) -> Result<MaterializedProcedure, RuntimeError> {
    let snapshot = encode_capture(capture)?;
    let mut rows = connection
        .query(
            "SELECT qualified_name, revision_id, semantic_hash, result_type_object_id,
                    materialized_ms
             FROM sys_procedure_materialization
             WHERE function_object_id = ?1 AND snapshot = ?2",
            params![function_object_id.to_vec(), snapshot.clone()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let qualified_name: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    validate_observation_text(&qualified_name)?;
    let revision_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let semantic_hash = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let result_type_object_id = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let materialized_ms: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if materialized_ms < 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let object = object_reference(
        capture.database_id(),
        function_object_id,
        capture.snapshot().clone(),
    )
    .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let function = function_reference(object)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_function_reference(function.clone().into_row_ref(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let result_type = make_type_reference(capture, result_type_object_id)?;
    let mut parameter_rows = connection
        .query(
            "SELECT position, name, type_object_id
             FROM sys_procedure_parameter_materialization
             WHERE function_object_id = ?1 AND snapshot = ?2 ORDER BY position",
            params![function_object_id.to_vec(), snapshot],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut parameters = Vec::new();
    while let Some(row) = parameter_rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let position = decode_u64(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&name)?;
        let type_object_id = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        parameters.push(MaterializedProcedureParameter {
            name,
            position,
            type_object_id,
            type_reference: make_type_reference(capture, type_object_id)?,
        });
    }
    for (position, parameter) in parameters.iter().enumerate() {
        if parameter.position != position as u64
            || parameters[..position]
                .iter()
                .any(|previous| previous.name == parameter.name)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    let snapshot_reference = snapshot_reference(capture.database_id(), capture.snapshot().clone())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    Ok(MaterializedProcedure {
        function,
        snapshot: snapshot_reference,
        capture: capture.clone(),
        qualified_name,
        object_id: function_object_id,
        revision_id,
        semantic_hash,
        result_type_object_id,
        result_type,
        parameters,
        materialized_ms,
    })
}

fn make_type_reference(
    capture: &CwdCapture,
    type_object_id: [u8; 16],
) -> Result<TypeRef, RuntimeError> {
    let object = object_reference(
        capture.database_id(),
        type_object_id,
        capture.snapshot().clone(),
    )
    .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let type_reference = type_reference(object)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_type_reference(type_reference.clone().into_row_ref(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    Ok(type_reference)
}

async fn load_invocation_arguments(
    connection: &libsql::Connection,
    invocation_id: [u8; 16],
    invocation: &InvocationRef,
    procedure: &MaterializedProcedure,
) -> Result<Vec<InvocationArgumentObservation>, RuntimeError> {
    let capture = procedure.capture();
    let mut rows = connection
        .query(
            "SELECT position, name, type_object_id, value_digest, redacted
             FROM sys_invocation_argument WHERE invocation_id = ?1 ORDER BY position",
            params![invocation_id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut arguments = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let position = decode_u64(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&name)?;
        let type_object_id = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let value_digest: Option<Vec<u8>> =
            row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let value_digest = value_digest.map(fixed::<32>).transpose()?;
        if let Some(digest) = value_digest {
            validate_digest(digest)?;
        }
        let redacted = decode_bool(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        if redacted && value_digest.is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let parameter = procedure
            .parameters
            .iter()
            .find(|parameter| parameter.position == position && parameter.name == name)
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if parameter.type_object_id != type_object_id {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let reference = invocation_argument_reference(
            capture.database_id(),
            capture.snapshot().clone(),
            invocation_id,
            BigInt::from(position),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        validate_invocation_argument_reference(reference.clone().into_row_ref(), capture)
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        arguments.push(InvocationArgumentObservation {
            reference,
            invocation: invocation.clone(),
            name,
            position,
            type_reference: parameter.type_reference.clone(),
            value_digest,
            redacted,
        });
    }
    if arguments.len() != procedure.parameters.len() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(arguments)
}

fn decode_invocation_status(value: i64) -> Result<InvocationObservationStatus, RuntimeError> {
    match value {
        1 => Ok(InvocationObservationStatus::Queued),
        2 => Ok(InvocationObservationStatus::Running),
        3 => Ok(InvocationObservationStatus::Succeeded),
        4 => Ok(InvocationObservationStatus::Failed),
        5 => Ok(InvocationObservationStatus::Cancelled),
        6 => Ok(InvocationObservationStatus::Orphaned),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn bigint_to_i64(value: &BigInt) -> Result<i64, RuntimeError> {
    value.to_string().parse().map_err(|_| RuntimeError::RecoveryInvalid)
}

impl fmt::Display for InvocationObservationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Orphaned => "orphaned",
        })
    }
}
