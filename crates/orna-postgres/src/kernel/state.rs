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
    security::{append_security_audit_event, recover_security_snapshot_for_active},
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

/// Rebinds a retained session to the active security snapshot before USER-state access.
///
/// The returned session is local to this operation; the caller-owned binding
/// remains opaque and is never replaced or exposed. A disabled principal or
/// revoked selected role fails before any state query, write, or audit append.
fn revalidate_authenticated_session(
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    active_pair: RevisionPair,
    function: FunctionId,
) -> Result<AuthenticatedSession, PostgresKernelError> {
    security
        .bind_authenticated_session(
            authenticated_session.principal(),
            authenticated_session.active_roles().to_vec(),
        )
        .map_err(|_| PostgresKernelError::StateExecuteDenied {
            pair: active_pair,
            function,
            reason: ExecuteDenial::InvalidSession,
        })
}

/// Rejects a caller-supplied root that is not an active CLIENT identity.
///
/// USER-state storage is keyed by the root function, so the root must be
/// resolved against the same active catalogue snapshot as its state slots.
/// This guard deliberately runs before any state query, write, or allowed
/// USER-state audit append.
fn validate_active_user_state_root(
    active: &ActiveDatabaseRevision,
    root_function: FunctionId,
) -> Result<(), PostgresKernelError> {
    let definition = validate_user_state_root_definition(
        root_function,
        active.catalogue().function_by_id(root_function),
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == root_function && revision.id() == definition.current_revision()
        })
        .ok_or_else(|| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must have its active CLIENT function revision",
        })?;
    if revision.artifact().kind() != ExecutableArtifactKind::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must carry an active CLIENT function artifact",
        });
    }
    Ok(())
}

fn validate_user_state_root_definition(
    root_function: FunctionId,
    definition: Option<&FunctionDefinition>,
) -> Result<&FunctionDefinition, PostgresKernelError> {
    let definition = definition.ok_or_else(|| PostgresKernelError::DurableInvariant {
        relation: STATE_RELATION,
        record: format!("{root_function:?}"),
        rule: "USER state root must identify an active CLIENT function",
    })?;
    if definition.id() != root_function {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root catalogue definition must match its supplied identity",
        });
    }
    if definition.domain() != FunctionDomain::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must be a CLIENT function",
        });
    }
    Ok(definition)
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
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
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

#[derive(Debug)]
struct PendingStateWrite {
    key: UserStateKey,
    value: orna_core::value::RuntimeValue,
    value_type: TypeId,
    revision: u64,
    existing: bool,
}

struct EncodedStateWrite {
    key: UserStateKey,
    value_bytes: Vec<u8>,
    value_type: TypeId,
    revision: u64,
    existing: bool,
}

fn plan_user_state_changes(
    changes: &[UserStateChange],
    principal: orna_core::PrincipalId,
    current_cells: &mut HashMap<UserStateKey, Option<UserStateCell>>,
) -> Result<(Vec<UserStateWriteResult>, Vec<PendingStateWrite>), PostgresKernelError> {
    reject_duplicate_user_state_keys(changes)?;
    let mut results = Vec::with_capacity(changes.len());
    let mut pending = Vec::new();
    let mut staged_cells = HashMap::<UserStateKey, UserStateCell>::new();
    let mut has_conflict = false;
    for change in changes {
        let key = change.key_without_principal().with_principal(principal);
        let current = if let Some(staged) = staged_cells.get(&key) {
            Some(staged)
        } else {
            current_cells
                .get(&key)
                .expect("write planner receives every requested key")
                .as_ref()
        };
        let result = match apply_change(current, change, principal) {
            Ok(result) => result,
            Err(error) if error.code() == Some("ORNA0902") => {
                has_conflict = true;
                continue;
            }
            Err(error) => return Err(PostgresKernelError::UserState(error)),
        };
        let UserStateWriteOutcome::Written { revision } = result.outcome() else {
            unreachable!("apply_change only returns Written outcomes")
        };
        let existing = current.is_some();
        let updated = UserStateCell::new(
            key.clone(),
            change.value().clone(),
            change.value_type(),
            revision,
            SystemTime::now(),
        );
        staged_cells.insert(key.clone(), updated);
        pending.push(PendingStateWrite {
            key,
            value: change.value().clone(),
            value_type: change.value_type(),
            revision,
            existing,
        });
        results.push(result);
    }
    if has_conflict {
        let conflicts = changes
            .iter()
            .map(|change| {
                let key = change.key_without_principal().with_principal(principal);
                let current_revision = current_cells
                    .get(&key)
                    .expect("write planner receives every requested key")
                    .as_ref()
                    .map_or(0, UserStateCell::revision);
                UserStateWriteResult::new(
                    change.key_without_principal(),
                    UserStateWriteOutcome::Conflict { current_revision },
                )
            })
            .collect();
        return Ok((conflicts, Vec::new()));
    }
    for (key, cell) in staged_cells {
        current_cells.insert(key, Some(cell));
    }
    Ok((results, pending))
}

fn reject_duplicate_user_state_keys(
    changes: &[UserStateChange],
) -> Result<(), PostgresKernelError> {
    let mut keys = HashSet::with_capacity(changes.len());
    for change in changes {
        let key = change.key_without_principal();
        if !keys.insert(key.clone()) {
            return Err(PostgresKernelError::UserState(
                UserStateError::InvalidChange {
                    reason: format!("USER state write batch contains duplicate key {key}"),
                },
            ));
        }
    }
    Ok(())
}

async fn load_state_cell(
    transaction: &Transaction<'_>,
    key: &UserStateKey,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Option<UserStateCell>, PostgresKernelError> {
    let row = transaction
        .query_opt(
            "SELECT principal_id, root_function_id, root_state_profile,
                    function_id, function_instance_key, state_slot_id,
                    value_bytes, value_type_id, revision, updated_at
             FROM _orna_kernel.user_state_cells
             WHERE principal_id = $1
               AND root_function_id = $2
               AND root_state_profile = $3
               AND function_id = $4
               AND function_instance_key = $5
               AND state_slot_id = $6",
            &[
                &key.principal().to_bytes().to_vec(),
                &key.root_function().to_bytes().to_vec(),
                &key.state_profile(),
                &key.function().to_bytes().to_vec(),
                &key.instance_key(),
                &key.state_slot().to_bytes().to_vec(),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    row.map(|row| decode_state_cell(&row, active, registry))
        .transpose()
}

async fn persist_state_write(
    transaction: &Transaction<'_>,
    write: &EncodedStateWrite,
) -> Result<(), PostgresKernelError> {
    let principal = write.key.principal().to_bytes().to_vec();
    let root_function = write.key.root_function().to_bytes().to_vec();
    let state_profile = write.key.state_profile();
    let function = write.key.function().to_bytes().to_vec();
    let instance_key = write.key.instance_key();
    let state_slot = write.key.state_slot().to_bytes().to_vec();
    reject_sealed_inspect_state_type(write.value_type, write.key.to_string())?;
    let value_type = write.value_type.to_bytes().to_vec();
    let revision =
        i64::try_from(write.revision).map_err(|_| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: write.key.to_string(),
            rule: "USER state revision must fit PostgreSQL BIGINT",
        })?;
    if write.existing {
        transaction
            .execute(
                "UPDATE _orna_kernel.user_state_cells
                 SET value_bytes = $1, value_type_id = $2, revision = $3,
                     updated_at = transaction_timestamp()
                 WHERE principal_id = $4
                   AND root_function_id = $5
                   AND root_state_profile = $6
                   AND function_id = $7
                   AND function_instance_key = $8
                   AND state_slot_id = $9",
                &[
                    &write.value_bytes,
                    &value_type,
                    &revision,
                    &principal,
                    &root_function,
                    &state_profile,
                    &function,
                    &instance_key,
                    &state_slot,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    } else {
        transaction
            .execute(
                "INSERT INTO _orna_kernel.user_state_cells
                    (principal_id, root_function_id, root_state_profile,
                     function_id, function_instance_key, state_slot_id,
                     value_bytes, value_type_id, revision)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &principal,
                    &root_function,
                    &state_profile,
                    &function,
                    &instance_key,
                    &state_slot,
                    &write.value_bytes,
                    &value_type,
                    &revision,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

fn decode_state_cell(
    row: &Row,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<UserStateCell, PostgresKernelError> {
    let principal = PrincipalId::from_bytes(state_id(row, "principal_id", "USER state principal")?);
    let root_function = FunctionId::from_bytes(state_id(
        row,
        "root_function_id",
        "USER state root function",
    )?);
    let state_profile: String = state_column(row, "root_state_profile", "USER state profile")?;
    let function = FunctionId::from_bytes(state_id(row, "function_id", "USER state function")?);
    let instance_key: String =
        state_column(row, "function_instance_key", "USER state instance key")?;
    let state_slot = StateSlotId::from_bytes(state_id(row, "state_slot_id", "USER state slot")?);
    let value_type = TypeId::from_bytes(state_id(row, "value_type_id", "USER state value type")?);
    reject_sealed_inspect_state_type(value_type, "selected row")?;
    let value_bytes: Vec<u8> = state_column(row, "value_bytes", "USER state value")?;
    let value = decode_constructed_value(active, registry, &value_bytes)
        .map_err(PostgresKernelError::UserStateValueCodec)?;
    if is_sealed_inspect_runtime_value(&value) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: "selected row".to_owned(),
            rule: "USER state cannot expose sealed Inspector values",
        });
    }
    let revision: i64 = state_column(row, "revision", "USER state revision")?;
    let revision = u64::try_from(revision).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: STATE_RELATION,
        record: "selected row".to_owned(),
        rule: "USER state revision must be a positive unsigned integer",
    })?;
    if revision == 0 {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: "selected row".to_owned(),
            rule: "USER state revision must be positive",
        });
    }
    let updated_at: SystemTime = state_column(row, "updated_at", "USER state update timestamp")?;
    let key = UserStateKey::new(
        principal,
        root_function,
        state_profile,
        function,
        instance_key,
        state_slot,
    )
    .map_err(PostgresKernelError::UserState)?;
    Ok(UserStateCell::new(
        key, value, value_type, revision, updated_at,
    ))
}

fn require_expected_type(
    cell: &UserStateCell,
    expected_types: &BTreeMap<(FunctionId, StateSlotId), TypeId>,
) -> Result<(), PostgresKernelError> {
    let Some(expected) = expected_types.get(&(cell.key().function(), cell.key().state_slot()))
    else {
        return Ok(());
    };
    if cell_type_matches(cell, *expected) {
        return Ok(());
    }
    let change = UserStateChange::new(
        cell.key().root_function(),
        cell.key().state_profile().to_owned(),
        cell.key().function(),
        cell.key().instance_key().to_owned(),
        cell.key().state_slot(),
        Some(cell.revision()),
        cell.value().clone(),
        *expected,
    )
    .map_err(PostgresKernelError::UserState)?;
    match apply_change(Some(cell), &change, cell.key().principal()) {
        Err(error) => Err(PostgresKernelError::UserState(error)),
        Ok(_) => unreachable!("a mismatched expected type must fail closed"),
    }
}

fn require_declared_user_state_type(
    key: UserStateKeyWithoutPrincipal,
    expected_type: TypeId,
    current_type: TypeId,
) -> Result<(), PostgresKernelError> {
    if expected_type == current_type {
        return Ok(());
    }
    Err(PostgresKernelError::UserState(
        UserStateError::TypeIncompatible {
            key: Box::new(key),
            expected: expected_type,
            current: current_type,
        },
    ))
}

fn active_user_state_slot_type(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    state_slot: StateSlotId,
) -> Result<TypeId, PostgresKernelError> {
    let definition = active.catalogue().function_by_id(function).ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot must identify an active CLIENT function",
        }
    })?;
    if definition.domain() != FunctionDomain::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot owner must be a CLIENT function",
        });
    }
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function && revision.id() == definition.current_revision()
        })
        .ok_or_else(|| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot owner must have its active CLIENT function revision",
        })?;
    let plan = decode_active_client_state_plan(revision)?;
    declared_user_state_slot(revision.function(), function, state_slot, &plan)
}

fn decode_active_client_state_plan(
    revision: &FunctionRevisionRecord,
) -> Result<StateClientPlan, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client || artifact.format() != CLIENT_PLAN_FORMAT
    {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{:?}", revision.function()),
            rule: "active CLIENT USER state owner must carry an orna.client-plan artifact",
        });
    }
    match artifact.version() {
        STATE_FORMAT_VERSION => StateClientPlan::decode(artifact.payload()).map_err(|_| {
            PostgresKernelError::DurableInvariant {
                relation: STATE_RELATION,
                record: format!("{:?}", revision.function()),
                rule: "active CLIENT USER state plan must decode as a version-four state plan",
            }
        }),
        CAPABILITY_FORMAT_VERSION => {
            let plan = CapabilityClientPlan::decode(artifact.payload()).map_err(|_| {
                PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    record: format!("{:?}", revision.function()),
                    rule: "active CLIENT capability state plan must decode canonically",
                }
            })?;
            match plan.inner_plan() {
                InnerClientPlan::State(state) => Ok(state.clone()),
                _ => Err(PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    record: format!("{:?}", revision.function()),
                    rule: "active CLIENT USER state owner must carry a state plan",
                }),
            }
        }
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{:?}", revision.function()),
            rule: "active CLIENT USER state owner must carry a supported state plan",
        }),
    }
}

fn declared_user_state_slot(
    owner_function: FunctionId,
    function: FunctionId,
    state_slot: StateSlotId,
    plan: &StateClientPlan,
) -> Result<TypeId, PostgresKernelError> {
    let record = format!("owner={owner_function:?}, function={function:?}, slot={state_slot:?}");
    if owner_function != function {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state slot must be presented with its owning CLIENT function",
        });
    }
    let Some(slot) = plan
        .slots()
        .iter()
        .find(|slot| slot.state_slot_id() == state_slot)
    else {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state slot must be declared by its owning CLIENT function",
        });
    };
    if slot.scope() != StateScope::User {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state service cannot access LOCAL or SESSION CLIENT state slots",
        });
    }
    Ok(slot.type_id())
}

#[cfg(test)]
fn validate_user_state_slot_declaration(
    owner_function: FunctionId,
    function: FunctionId,
    state_slot: StateSlotId,
    value_type: TypeId,
    plan: &StateClientPlan,
) -> Result<(), PostgresKernelError> {
    let declared_type = declared_user_state_slot(owner_function, function, state_slot, plan)?;
    let key = UserStateKeyWithoutPrincipal::new(
        owner_function,
        String::new(),
        function,
        String::new(),
        state_slot,
    )
    .map_err(PostgresKernelError::UserState)?;
    require_declared_user_state_type(key, value_type, declared_type)
}

fn reject_sealed_inspect_state_type(
    value_type: TypeId,
    record: impl Into<String>,
) -> Result<(), PostgresKernelError> {
    if is_sealed_inspect_type_id(value_type) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: record.into(),
            rule: "USER state cannot persist sealed Inspector carrier type identities",
        });
    }
    Ok(())
}

fn state_value_registry(
    active: &ActiveDatabaseRevision,
) -> Result<OpaqueCodecRegistry, PostgresKernelError> {
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: active.pair().catalogue().canonical(),
            rule: "USER state requires the accepted verified standard snapshot",
        }
    })?;
    registered_opaque_codecs(standard).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.standard_library_revisions",
        record: standard.revision().canonical(),
        rule: "the verified standard snapshot must bind its opaque codec registry",
    })
}

fn validate_state_profile(state_profile: &str) -> Result<(), PostgresKernelError> {
    UserStateKeyWithoutPrincipal::new(
        FunctionId::from_bytes([0; 16]),
        state_profile.to_owned(),
        FunctionId::from_bytes([0; 16]),
        String::new(),
        StateSlotId::from_bytes([0; 16]),
    )
    .map(|_| ())
    .map_err(PostgresKernelError::UserState)
}

fn state_column<T: FromSqlOwned>(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: STATE_RELATION,
            record: "selected row".to_owned(),
            column,
            rule,
            source,
        })
}

fn state_id(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = state_column(row, column, rule)?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: "selected row".to_owned(),
            rule,
        })
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
mod tests {
    use super::*;
    use orna_artifact::client_plan::{ClientExpressionNode, StateDefault, StateSlot};
    use orna_core::{
        CatalogueRevisionId, FunctionRevisionId, PrincipalId, SourceRevisionId, TypeId,
        catalogue::{
            FunctionDefinition, FunctionReturn, FunctionSecurity, FunctionTransaction,
            FunctionVolatility, QualifiedSemanticName,
        },
        revision::RevisionPair,
        security::{Principal, PrincipalKind, PrincipalStatus, RoleMembership},
        types::{ResolvedType, StandardScalar},
        value::RuntimeValue,
    };

    const PRINCIPAL: orna_core::PrincipalId = PrincipalId::from_bytes([0x11; 16]);
    const OTHER_PRINCIPAL: orna_core::PrincipalId = PrincipalId::from_bytes([0x22; 16]);
    const ROOT: FunctionId = FunctionId::from_bytes([0x31; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0x32; 16]);
    const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([0x36; 16]);
    const SLOT: StateSlotId = StateSlotId::from_bytes([0x33; 16]);
    const OTHER_SLOT: StateSlotId = StateSlotId::from_bytes([0x37; 16]);
    const INTEGER: TypeId = TypeId::from_bytes([0x34; 16]);
    const TEXT: TypeId = TypeId::from_bytes([0x35; 16]);

    fn change(expected_revision: Option<u64>, value: i64) -> UserStateChange {
        change_for_instance(String::new(), expected_revision, value)
    }

    fn change_for_instance(
        instance_key: String,
        expected_revision: Option<u64>,
        value: i64,
    ) -> UserStateChange {
        UserStateChange::new(
            ROOT,
            String::new(),
            FUNCTION,
            instance_key,
            SLOT,
            expected_revision,
            RuntimeValue::BigInt(value),
            INTEGER,
        )
        .expect("test change is valid")
    }

    fn cell(principal: PrincipalId, revision: u64, value: i64) -> UserStateCell {
        cell_for_instance(principal, String::new(), revision, value)
    }

    fn cell_for_instance(
        principal: PrincipalId,
        instance_key: String,
        revision: u64,
        value: i64,
    ) -> UserStateCell {
        UserStateCell::new(
            UserStateKey::new(principal, ROOT, String::new(), FUNCTION, instance_key, SLOT)
                .expect("test key is valid"),
            RuntimeValue::BigInt(value),
            INTEGER,
            revision,
            SystemTime::UNIX_EPOCH,
        )
    }

    fn state_plan(scope: StateScope, value_type: TypeId) -> StateClientPlan {
        StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![StateSlot::new(SLOT, value_type, scope, StateDefault::Unset)],
        )
    }

    fn root_definition(domain: FunctionDomain) -> FunctionDefinition {
        FunctionDefinition::new(
            ROOT,
            QualifiedSemanticName::new(["test", "root"]).expect("test root name"),
            domain,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::from_bytes([0x38; 16]),
            FunctionSecurity::Invoker,
            (domain == FunctionDomain::Server).then_some(FunctionTransaction::ReadOnly),
            if domain == FunctionDomain::Client {
                FunctionVolatility::Immutable
            } else {
                FunctionVolatility::Stable
            },
        )
    }

    #[test]
    fn unknown_user_state_root_is_rejected() {
        let error = validate_user_state_root_definition(FUNCTION, None)
            .expect_err("an unknown USER-state root must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state root must identify an active CLIENT function",
                ..
            }
        ));
    }

    #[test]
    fn inactive_user_state_root_is_rejected() {
        let error = validate_user_state_root_definition(OTHER_FUNCTION, None)
            .expect_err("an inactive USER-state root must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state root must identify an active CLIENT function",
                ..
            }
        ));
    }

    #[test]
    fn server_user_state_root_is_rejected() {
        let definition = root_definition(FunctionDomain::Server);
        let error = validate_user_state_root_definition(ROOT, Some(&definition))
            .expect_err("a SERVER USER-state root must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state root must be a CLIENT function",
                ..
            }
        ));
    }

    #[test]
    fn active_client_user_state_root_is_accepted() {
        let definition = root_definition(FunctionDomain::Client);
        assert!(validate_user_state_root_definition(ROOT, Some(&definition)).is_ok());
    }

    #[test]
    fn mismatched_user_state_root_definition_is_rejected() {
        let definition = root_definition(FunctionDomain::Client);
        let error = validate_user_state_root_definition(FUNCTION, Some(&definition))
            .expect_err("a catalogue definition for another identity must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state root catalogue definition must match its supplied identity",
                ..
            }
        ));
    }

    #[test]
    fn retained_session_rebinds_or_denies_before_state_access() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x41; 16]),
            CatalogueRevisionId::from_bytes([0x42; 16]),
        );
        let active = SecuritySnapshot::new(
            pair,
            vec![],
            vec![Principal::new(
                PRINCIPAL,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("active state security snapshot must validate");
        let session = active
            .bind_authenticated_session(PRINCIPAL, vec![])
            .expect("active state session must bind");
        let rebound = revalidate_authenticated_session(
            &active,
            &session,
            pair,
            SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
        )
        .expect("active retained state session must rebind");
        assert_eq!(rebound.principal(), PRINCIPAL);

        let disabled = SecuritySnapshot::new(
            pair,
            vec![],
            vec![Principal::new(
                PRINCIPAL,
                PrincipalKind::User,
                PrincipalStatus::Disabled,
            )],
            vec![],
            vec![],
        )
        .expect("disabled state security snapshot must validate");
        let error = revalidate_authenticated_session(
            &disabled,
            &session,
            pair,
            SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
        )
        .expect_err("disabled retained state session must be rejected");
        assert!(matches!(
            error,
            PostgresKernelError::StateExecuteDenied {
                function: SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
                reason: ExecuteDenial::InvalidSession,
                ..
            }
        ));

        let role = PrincipalId::from_bytes([0x43; 16]);
        let role_active = SecuritySnapshot::new(
            pair,
            vec![],
            vec![
                Principal::new(PRINCIPAL, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, PRINCIPAL)],
            vec![],
        )
        .expect("role-bound state security snapshot must validate");
        let role_session = role_active
            .bind_authenticated_session(PRINCIPAL, vec![role])
            .expect("selected role state session must bind");
        let revoked = SecuritySnapshot::new(
            pair,
            vec![],
            vec![Principal::new(
                PRINCIPAL,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("revoked-role state security snapshot must validate");
        let error = revalidate_authenticated_session(
            &revoked,
            &role_session,
            pair,
            SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
        )
        .expect_err("revoked selected role must invalidate retained state session");
        assert!(matches!(
            error,
            PostgresKernelError::StateExecuteDenied {
                function: SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
                reason: ExecuteDenial::InvalidSession,
                ..
            }
        ));
    }

    #[test]
    fn undeclared_user_slot_is_rejected() {
        let plan = state_plan(StateScope::User, INTEGER);
        let error =
            validate_user_state_slot_declaration(FUNCTION, FUNCTION, OTHER_SLOT, INTEGER, &plan)
                .expect_err("unknown USER state slot must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state slot must be declared by its owning CLIENT function",
                ..
            }
        ));
    }

    #[test]
    fn user_slot_presented_with_wrong_owner_is_rejected() {
        let plan = state_plan(StateScope::User, INTEGER);
        let error =
            validate_user_state_slot_declaration(FUNCTION, OTHER_FUNCTION, SLOT, INTEGER, &plan)
                .expect_err("USER state slot must use its owning function");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state slot must be presented with its owning CLIENT function",
                ..
            }
        ));
    }

    #[test]
    fn local_and_session_slots_are_rejected_by_user_service() {
        for scope in [StateScope::Local, StateScope::Session] {
            let plan = state_plan(scope, INTEGER);
            let error =
                validate_user_state_slot_declaration(FUNCTION, FUNCTION, SLOT, INTEGER, &plan)
                    .expect_err("non-USER state scope must fail closed");
            assert!(matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    rule: "USER state service cannot access LOCAL or SESSION CLIENT state slots",
                    ..
                }
            ));
        }
    }

    #[test]
    fn user_slot_type_mismatch_is_rejected_against_active_plan() {
        let plan = state_plan(StateScope::User, INTEGER);
        let error = validate_user_state_slot_declaration(FUNCTION, FUNCTION, SLOT, TEXT, &plan)
            .expect_err("USER state type must match the active declaration");
        let message = error.to_string();
        match error {
            PostgresKernelError::UserState(UserStateError::TypeIncompatible {
                expected,
                current,
                ..
            }) => {
                assert_eq!(expected, TEXT);
                assert_eq!(current, INTEGER);
            }
            other => panic!("expected ORNA0901 type mismatch, got {other:?}"),
        }
        assert!(message.contains("ORNA0901"));
    }

    #[test]
    fn load_declared_type_is_expected_and_persisted_type_is_current() {
        let key =
            UserStateKeyWithoutPrincipal::new(ROOT, String::new(), FUNCTION, String::new(), SLOT)
                .expect("test key is valid");
        let error = require_declared_user_state_type(key, INTEGER, TEXT)
            .expect_err("declared and persisted USER state types must agree");
        let message = error.to_string();
        match error {
            PostgresKernelError::UserState(UserStateError::TypeIncompatible {
                expected,
                current,
                ..
            }) => {
                assert_eq!(expected, INTEGER);
                assert_eq!(current, TEXT);
            }
            other => panic!("expected ORNA0901 type mismatch, got {other:?}"),
        }
        assert!(message.contains("ORNA0901"));
    }

    #[test]
    fn first_write_and_matching_revision_increment() {
        let first = apply_change(None, &change(None, 1), PRINCIPAL).expect("first write succeeds");
        assert_eq!(
            first.outcome(),
            UserStateWriteOutcome::Written { revision: 1 }
        );
        let current = cell(PRINCIPAL, 1, 1);
        let second = apply_change(Some(&current), &change(Some(1), 2), PRINCIPAL)
            .expect("matching revision succeeds");
        assert_eq!(
            second.outcome(),
            UserStateWriteOutcome::Written { revision: 2 }
        );
    }

    #[test]
    fn stale_revision_is_a_per_change_conflict_with_current_revision() {
        let current = cell(PRINCIPAL, 3, 1);
        let error = apply_change(Some(&current), &change(Some(2), 2), PRINCIPAL)
            .expect_err("stale revision must fail closed");
        assert_eq!(error.code(), Some("ORNA0902"));
        let result = UserStateWriteResult::new(
            change(Some(2), 2).key_without_principal(),
            UserStateWriteOutcome::Conflict {
                current_revision: 3,
            },
        );
        assert_eq!(
            result.outcome(),
            UserStateWriteOutcome::Conflict {
                current_revision: 3
            }
        );
    }

    #[test]
    fn type_mismatch_fails_load_and_write_closed_with_orna0901() {
        let current = cell(PRINCIPAL, 1, 1);
        let different_type = TypeId::from_bytes([0x35; 16]);
        let change = UserStateChange::new(
            ROOT,
            String::new(),
            FUNCTION,
            String::new(),
            SLOT,
            Some(1),
            RuntimeValue::BigInt(2),
            different_type,
        )
        .expect("test change is valid");
        let write_error = apply_change(Some(&current), &change, PRINCIPAL)
            .expect_err("type mismatch must fail closed");
        assert_eq!(write_error.code(), Some("ORNA0901"));

        let load_error = require_expected_type(
            &current,
            &BTreeMap::from([((FUNCTION, SLOT), different_type)]),
        )
        .expect_err("load mismatch must fail closed");
        assert!(matches!(load_error, PostgresKernelError::UserState(_)));
        assert!(load_error.to_string().contains("ORNA0901"));
    }

    #[test]
    fn principal_is_derived_from_session_not_the_change() {
        let current = cell(OTHER_PRINCIPAL, 1, 1);
        let error = apply_change(Some(&current), &change(Some(1), 2), PRINCIPAL)
            .expect_err("cross-principal cell must fail closed");
        assert_eq!(error.code(), Some("ORNA0903"));
    }

    #[test]
    fn sealed_inspector_state_types_are_rejected_but_scalars_are_allowed() {
        let sealed_types = [
            orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
            orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
            orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            orna_core::system::SYS_INSPECT_CALLS_TYPE_ID,
            orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID,
            orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID,
            orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID,
            orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        ];
        for sealed_type in sealed_types {
            let error = reject_sealed_inspect_state_type(sealed_type, "forged row")
                .expect_err("sealed Inspector identities must fail closed");
            assert!(matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    ..
                }
            ));
        }
        reject_sealed_inspect_state_type(INTEGER, "ordinary scalar")
            .expect("ordinary scalar USER state remains persistable");
    }

    #[test]
    fn forged_sealed_inspector_cell_aborts_the_write_plan() {
        let current = UserStateCell::new(
            UserStateKey::new(
                PRINCIPAL,
                ROOT,
                String::new(),
                FUNCTION,
                String::new(),
                SLOT,
            )
            .expect("test key is valid"),
            RuntimeValue::BigInt(1),
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        );
        let change = change(Some(1), 2);
        let key = change.key_without_principal().with_principal(PRINCIPAL);
        let mut current_cells = HashMap::from([(key, Some(current))]);
        let error = plan_user_state_changes(&[change], PRINCIPAL, &mut current_cells)
            .expect_err("a forged persisted Inspector identity must fail closed");
        assert!(matches!(error, PostgresKernelError::UserState(_)));
    }

    #[test]
    fn expected_type_match_and_instance_requests_are_closed() {
        let current = cell(PRINCIPAL, 1, 1);
        require_expected_type(&current, &BTreeMap::from([((FUNCTION, SLOT), INTEGER)]))
            .expect("matching type loads");
        assert!(UserStateInstanceRequest::new(FUNCTION, String::new()).is_ok());
        assert!(UserStateInstanceRequest::new(FUNCTION, "bad\0key".to_owned()).is_err());
    }

    #[test]
    fn empty_instance_request_selects_only_the_default_persisted_instance() {
        let requests = requested_state_instances(&[]);
        let default_cell = cell(PRINCIPAL, 1, 1);
        let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 1, 2);

        assert!(state_instance_is_requested(
            &[],
            &requests,
            default_cell.key().function(),
            default_cell.key().instance_key()
        ));
        assert!(!state_instance_is_requested(
            &[],
            &requests,
            named_cell.key().function(),
            named_cell.key().instance_key()
        ));
    }

    #[test]
    fn explicit_instance_request_still_selects_the_named_persisted_instance() {
        let explicit = UserStateInstanceRequest::new(FUNCTION, "named".to_owned())
            .expect("explicit instance request is valid");
        let requests = requested_state_instances(std::slice::from_ref(&explicit));
        let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 1, 2);

        assert!(state_instance_is_requested(
            std::slice::from_ref(&explicit),
            &requests,
            named_cell.key().function(),
            named_cell.key().instance_key(),
        ));
    }

    #[test]
    fn mixed_batch_conflict_returns_all_conflicts_without_staging_writes() {
        let default_cell = cell(PRINCIPAL, 3, 1);
        let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 3, 2);
        let first = change(Some(3), 10);
        let second = change_for_instance("named".to_owned(), Some(2), 20);
        let default_key = first.key_without_principal().with_principal(PRINCIPAL);
        let named_key = second.key_without_principal().with_principal(PRINCIPAL);
        let original_default = default_cell.clone();
        let original_named = named_cell.clone();
        let mut current_cells = HashMap::from([
            (default_key.clone(), Some(default_cell)),
            (named_key.clone(), Some(named_cell)),
        ]);
        let (results, pending) =
            plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
                .expect("conflicts are returned as closed results");

        assert_eq!(pending.len(), 0);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| {
            result.outcome()
                == UserStateWriteOutcome::Conflict {
                    current_revision: 3,
                }
        }));
        assert_eq!(
            current_cells.get(&default_key),
            Some(&Some(original_default))
        );
        assert_eq!(current_cells.get(&named_key), Some(&Some(original_named)));
    }

    #[test]
    fn duplicate_keys_fail_preflight_without_staging_or_persistence_plan() {
        let current = cell(PRINCIPAL, 1, 1);
        let key = current.key().clone();
        let original = current.clone();
        let first = change(Some(1), 2);
        let second = change(Some(1), 3);
        let mut current_cells = HashMap::from([(key, Some(current))]);

        let error = plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
            .expect_err("duplicate USER state keys must fail before planning");

        assert!(matches!(
            error,
            PostgresKernelError::UserState(UserStateError::InvalidChange { .. })
        ));
        assert!(error.to_string().contains("duplicate key"));
        assert_eq!(current_cells.values().next(), Some(&Some(original)));
    }

    #[test]
    fn hard_failure_returns_no_commit_plan_for_prior_success() {
        let current = cell(PRINCIPAL, 1, 1);
        let second_current = cell_for_instance(PRINCIPAL, "other".to_owned(), 1, 1);
        let first = change(Some(1), 2);
        let second = UserStateChange::new(
            ROOT,
            String::new(),
            FUNCTION,
            "other".to_owned(),
            SLOT,
            Some(1),
            RuntimeValue::BigInt(3),
            TEXT,
        )
        .expect("test change is valid");
        let first_key = first.key_without_principal().with_principal(PRINCIPAL);
        let second_key = second.key_without_principal().with_principal(PRINCIPAL);
        let mut current_cells = HashMap::from([
            (first_key, Some(current)),
            (second_key, Some(second_current)),
        ]);
        let error = plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
            .expect_err("type mismatch aborts the batch");
        assert!(
            matches!(error, PostgresKernelError::UserState(_))
                && error.to_string().contains("ORNA0901")
        );
    }
}
