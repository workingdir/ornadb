// State operations keep the accepted error layout across their public seam.
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

//! Protected durable USER state operations from ADR 0061 step 4.
//!
//! The authenticated session supplies the principal for every operation. The
//! request never carries a principal identity. Loads use a repeatable-read
//! read-only snapshot and then append their redacted audit record in a second
//! protected transaction; writes plan every change against one snapshot and
//! commit all successful writes together.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    time::SystemTime,
};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    is_sealed_inspect_type_id,
    security::{
        append_security_audit_event, lock_active_revision, recover_security_snapshot_for_active,
    },
    server_runtime::configure_and_recover,
};
use orna_artifact::client_plan::{
    CAPABILITY_FORMAT_VERSION, CapabilityClientPlan, FORMAT_IDENTITY as CLIENT_PLAN_FORMAT,
    InnerClientPlan, STATE_FORMAT_VERSION, StateClientPlan, StateScope,
};
use orna_core::{
    FunctionId, PrincipalId, StateSlotId, TypeId,
    catalogue::{FunctionDefinition, FunctionDomain},
    revision::{
        ActiveDatabaseRevision, ExecutableArtifactKind, FunctionRevisionRecord, RevisionPair,
    },
    security::{
        AuthenticatedSession, ExecuteDenial, SecurityAuditDecision, SecuritySnapshot,
        UserStateAuditOperation,
    },
    state::{
        UserStateCell, UserStateChange, UserStateError, UserStateKey, UserStateKeyWithoutPrincipal,
        UserStateWriteOutcome, UserStateWriteResult, apply_change, cell_type_matches,
        is_sealed_inspect_runtime_value,
    },
    system::{SYS_STATE_LOAD_USER_STATE_FUNCTION_ID, SYS_STATE_WRITE_USER_STATE_FUNCTION_ID},
    value::OpaqueCodecRegistry,
};
use orna_protocol::{decode_constructed_value, encode_constructed_value};
use orna_standard::registered_opaque_codecs;
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};
#[path = "state/persistence.rs"]
mod persistence;
#[path = "state/planning.rs"]
mod planning;
#[path = "state/validation.rs"]
mod validation;

use persistence::*;
use planning::*;
use validation::*;

const STATE_RELATION: &str = "_orna_kernel.user_state_cells";

/// One function instance selected by a USER state load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserStateInstanceRequest {
    function: FunctionId,
    instance_key: String,
}

impl UserStateInstanceRequest {
    /// Creates an instance request. The empty instance key is the default
    /// instance; NUL bytes are rejected by the core durable-key model.
    pub fn new(function: FunctionId, instance_key: String) -> Result<Self, UserStateError> {
        UserStateKeyWithoutPrincipal::new(
            FunctionId::from_bytes([0; 16]),
            String::new(),
            function,
            instance_key.clone(),
            StateSlotId::from_bytes([0; 16]),
        )?;
        Ok(Self {
            function,
            instance_key,
        })
    }

    /// Returns the function owning the requested instance.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the requested instance key.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }
}

fn requested_state_instances(
    instances: &[UserStateInstanceRequest],
) -> BTreeSet<(FunctionId, String)> {
    instances
        .iter()
        .map(|request| (request.function(), request.instance_key().to_owned()))
        .collect()
}

fn state_instance_is_requested(
    instances: &[UserStateInstanceRequest],
    requested_instances: &BTreeSet<(FunctionId, String)>,
    function: FunctionId,
    instance_key: &str,
) -> bool {
    if instances.is_empty() {
        instance_key.is_empty()
    } else {
        requested_instances.contains(&(function, instance_key.to_owned()))
    }
}

/// Loads authenticated USER state cells from an already-pinned snapshot.
///
/// The caller owns the transaction's active revision and codec registry. This
/// keeps sealed invocation loading on the same repeatable-read snapshot as
/// target authorisation and evaluation, rather than opening a second kernel
/// session that could observe a different active revision.
pub(crate) async fn load_user_state_in_transaction(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    root_function: FunctionId,
    state_profile: &str,
    instances: &[UserStateInstanceRequest],
    expected_types: &BTreeMap<(FunctionId, StateSlotId), TypeId>,
) -> Result<Vec<UserStateCell>, PostgresKernelError> {
    validate_state_profile(state_profile)?;
    validate_active_user_state_root(active, root_function)?;
    let requested_instances = requested_state_instances(instances);
    let principal = authenticated_session.principal();
    for ((function, state_slot), expected_type) in expected_types {
        let declared_type = active_user_state_slot_type(active, *function, *state_slot)?;
        let key = UserStateKeyWithoutPrincipal::new(
            root_function,
            state_profile.to_owned(),
            *function,
            String::new(),
            *state_slot,
        )
        .map_err(PostgresKernelError::UserState)?;
        require_declared_user_state_type(key, *expected_type, declared_type)?;
    }
    let rows = transaction
        .query(
            "SELECT principal_id, root_function_id, root_state_profile,
                    function_id, function_instance_key, state_slot_id,
                    value_bytes, value_type_id, revision, updated_at
             FROM _orna_kernel.user_state_cells
             WHERE principal_id = $1
               AND root_function_id = $2
               AND root_state_profile = $3
             ORDER BY function_id, function_instance_key, state_slot_id",
            &[
                &principal.to_bytes().to_vec(),
                &root_function.to_bytes().to_vec(),
                &state_profile,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut cells = Vec::with_capacity(rows.len());
    for row in &rows {
        let function = state_id(row, "function_id", "USER state function identity")
            .map(FunctionId::from_bytes)?;
        let instance_key: String =
            state_column(row, "function_instance_key", "USER state instance identity")?;
        if !state_instance_is_requested(instances, &requested_instances, function, &instance_key) {
            continue;
        }
        let cell = decode_state_cell(row, active, registry)?;
        let declared_type =
            active_user_state_slot_type(active, cell.key().function(), cell.key().state_slot())?;
        require_declared_user_state_type(
            cell.key().without_principal(),
            declared_type,
            cell.value_type(),
        )?;
        require_expected_type(&cell, expected_types)?;
        cells.push(cell);
    }
    Ok(cells)
}

impl PostgresKernel {
    /// Loads the authenticated principal's USER state cells.
    ///
    /// `instances` is an optional `(function, instance_key)` filter. An empty
    /// filter selects the default instance for each USER state slot.
    /// Because state-slot declarations are deferred, `expected_types` supplies
    /// the load-time declared type by `(function, state_slot)` and a mismatch
    /// fails closed with ORNA0901.
    pub async fn load_user_state(
        &self,
        authenticated_session: &AuthenticatedSession,
        root_function: FunctionId,
        state_profile: &str,
        instances: &[UserStateInstanceRequest],
        expected_types: &BTreeMap<(FunctionId, StateSlotId), TypeId>,
    ) -> Result<Vec<UserStateCell>, PostgresKernelError> {
        validate_state_profile(state_profile)?;
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session = revalidate_authenticated_session(
                &security,
                authenticated_session,
                active.pair(),
                SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
            )?;
            let loaded_pair = active.pair();
            let registry = state_value_registry(&active)?;
            let cells = load_user_state_in_transaction(
                &transaction,
                &bound_session,
                &active,
                &registry,
                root_function,
                state_profile,
                instances,
                expected_types,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;

            // PostgreSQL forbids INSERT in the read-only snapshot. Keep the
            // load snapshot read-only, then append only the redacted audit
            // decision in its own protected transaction. Revalidate the
            // retained session and revision again because security state can
            // change between these two transactions.
            let audit_transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&audit_transaction).await?;
            lock_active_revision(&audit_transaction, loaded_pair).await?;
            let audit_active = configure_and_recover(&audit_transaction).await?;
            if audit_active.pair() != loaded_pair {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    record: format!("{root_function:?}"),
                    rule: "USER state load revision changed before audit",
                });
            }
            validate_active_user_state_root(&audit_active, root_function)?;
            let audit_security =
                recover_security_snapshot_for_active(&audit_transaction, &audit_active).await?;
            let audit_bound_session = revalidate_authenticated_session(
                &audit_security,
                authenticated_session,
                audit_active.pair(),
                SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
            )?;
            append_security_audit_event(
                &audit_transaction,
                SecurityAuditDecision::user_state_allowed(
                    &audit_bound_session,
                    UserStateAuditOperation::Load,
                    root_function,
                    cells.len() as u64,
                ),
            )
            .await?;
            audit_transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(cells)
        }
        .await;
        finish_state_session(operation, database_session.shutdown().await)
    }

    /// Writes authenticated USER state changes with optimistic revisions.
    ///
    /// Revision conflicts return an aligned all-`Conflict` result batch and
    /// persist no changes. ORNA0901/0903, codec errors,
    /// invalid batches, and database failures abort the whole transaction.
    /// Every change in one batch must identify the same root function so the
    /// protected audit event can record one unambiguous root.
    pub async fn write_user_state(
        &self,
        authenticated_session: &AuthenticatedSession,
        changes: &[UserStateChange],
    ) -> Result<Vec<UserStateWriteResult>, PostgresKernelError> {
        let Some(first_change) = changes.first() else {
            return Err(PostgresKernelError::DurableInvariant {
                relation: STATE_RELATION,
                record: "empty write batch".to_owned(),
                rule: "USER state write batches must contain at least one change",
            });
        };
        let root_function = first_change.root_function();
        if changes
            .iter()
            .any(|change| change.root_function() != root_function)
        {
            return Err(PostgresKernelError::DurableInvariant {
                relation: STATE_RELATION,
                record: "mixed-root write batch".to_owned(),
                rule: "all USER state changes in one batch must share a root function",
            });
        }
        reject_duplicate_user_state_keys(changes)?;
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let lock_pair = configure_and_recover(&transaction).await?.pair();
            lock_active_revision(&transaction, lock_pair).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session = revalidate_authenticated_session(
                &security,
                authenticated_session,
                active.pair(),
                SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
            )?;
            validate_active_user_state_root(&active, root_function)?;
            let principal = bound_session.principal();
            let registry = state_value_registry(&active)?;
            for change in changes {
                let declared_type =
                    active_user_state_slot_type(&active, change.function(), change.state_slot())?;
                require_declared_user_state_type(
                    change.key_without_principal(),
                    change.value_type(),
                    declared_type,
                )?;
            }
            let mut current_cells = HashMap::<UserStateKey, Option<UserStateCell>>::new();
            let mut fetched_keys = HashSet::new();
            for change in changes {
                let key = change.key_without_principal().with_principal(principal);
                if fetched_keys.insert(key.clone()) {
                    let cell = load_state_cell(&transaction, &key, &active, &registry).await?;
                    current_cells.insert(key, cell);
                }
            }
            let (results, pending) =
                plan_user_state_changes(changes, principal, &mut current_cells)?;
            let mut encoded_writes = Vec::with_capacity(pending.len());
            for write in pending {
                let value_bytes = encode_constructed_value(&active, &registry, &write.value)
                    .map_err(PostgresKernelError::UserStateValueCodec)?;
                encoded_writes.push(EncodedStateWrite {
                    key: write.key,
                    value_bytes,
                    value_type: write.value_type,
                    revision: write.revision,
                    existing: write.existing,
                });
            }
            for write in encoded_writes {
                persist_state_write(&transaction, &write).await?;
            }
            append_security_audit_event(
                &transaction,
                SecurityAuditDecision::user_state_allowed(
                    &bound_session,
                    UserStateAuditOperation::Write,
                    root_function,
                    changes.len() as u64,
                ),
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(results)
        }
        .await;
        finish_state_session(operation, database_session.shutdown().await)
    }
}

fn finish_state_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
