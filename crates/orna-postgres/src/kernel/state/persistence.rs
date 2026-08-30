//! USER-state row encoding, decoding, loading, and persistence.

use super::*;

pub(super) struct EncodedStateWrite {
    pub(super) key: UserStateKey,
    pub(super) value_bytes: Vec<u8>,
    pub(super) value_type: TypeId,
    pub(super) revision: u64,
    pub(super) existing: bool,
}

pub(super) async fn load_state_cell(
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

pub(super) async fn persist_state_write(
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

pub(super) fn decode_state_cell(
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

pub(super) fn state_column<T: FromSqlOwned>(
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

pub(super) fn state_id(
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
