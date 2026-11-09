//! Row and value primitives shared by every runtime table reader and writer.

use crate::RuntimeIdentity;
use crate::error::RuntimeError;
use libsql::Connection;
use num_bigint::{BigInt, Sign};
use orna_stream::{AssertionOwnerKind, Component, ConsumerIdentity, Position};
use orna_value::{CwdCapture, OvbRaw, RowRef, Snapshot, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn validate_observation_text(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty() || value.len() > 16 * 1024 || value.bytes().any(|byte| byte == 0) {
        return Err(RuntimeError::InvalidIdentity);
    }
    Ok(())
}

pub(crate) fn now_ms() -> Result<i64, RuntimeError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
            .as_millis(),
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn bigint_to_i64(value: &BigInt) -> Result<i64, RuntimeError> {
    value
        .to_string()
        .parse()
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn encode_capture(capture: &CwdCapture) -> Result<Vec<u8>, RuntimeError> {
    capture
        .snapshot()
        .encode()
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_capture(bytes: Vec<u8>, digest: [u8; 32]) -> Result<CwdCapture, RuntimeError> {
    let snapshot = Snapshot::decode_bytes(&bytes).map_err(|_| RuntimeError::RecoveryInvalid)?;
    CwdCapture::new(snapshot, digest).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_consumer_identity(value: &str) -> Result<ConsumerIdentity, RuntimeError> {
    let parts = value.split('|').collect::<Vec<_>>();
    if parts.len() != 5 || parts[0] != "consumer/v1" {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(ConsumerIdentity {
        principal: decode_component(parts[1].to_owned())?,
        root: decode_component(parts[2].to_owned())?,
        function: decode_component(parts[3].to_owned())?,
        binding: decode_component(parts[4].to_owned())?,
    })
}

pub(crate) fn row_text(row: &libsql::Row, index: i32) -> Result<String, RuntimeError> {
    row.get(index).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_optional_component(
    value: Option<String>,
) -> Result<Option<Component>, RuntimeError> {
    value.map(decode_component).transpose()
}

pub(crate) fn decode_component(value: String) -> Result<Component, RuntimeError> {
    Component::new(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_position(value: String) -> Result<Position, RuntimeError> {
    Ok(Position {
        token: decode_component(value)?,
    })
}

pub(crate) fn decode_u64(value: i64) -> Result<u64, RuntimeError> {
    u64::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_bool(value: i64) -> Result<bool, RuntimeError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

pub(crate) fn decode_u32(value: i64) -> Result<u32, RuntimeError> {
    u32::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn encode_row_ref(reference: &RowRef) -> Result<Vec<u8>, RuntimeError> {
    reference
        .encode()
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_row_ref(bytes: Vec<u8>) -> Result<RowRef, RuntimeError> {
    RowRef::decode(&bytes).map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn encode_nonnegative_bigint(value: &BigInt) -> Result<Vec<u8>, RuntimeError> {
    if value.sign() == Sign::Minus {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Value::int(value.clone())
        .encode()
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn decode_nonnegative_bigint(bytes: Vec<u8>) -> Result<BigInt, RuntimeError> {
    let value = Value::decode(&bytes).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let OvbRaw::Int(value) = value.raw() else {
        return Err(RuntimeError::RecoveryInvalid);
    };
    if value.sign() == Sign::Minus {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(value.clone())
}

pub(crate) fn decode_positive_bigint(bytes: Vec<u8>) -> Result<BigInt, RuntimeError> {
    let value = decode_nonnegative_bigint(bytes)?;
    if value == BigInt::from(0) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(value)
}

pub(crate) fn encode_assertion_owner_kind(kind: AssertionOwnerKind) -> i64 {
    match kind {
        AssertionOwnerKind::Executable => 1,
        AssertionOwnerKind::RefinedType => 2,
        AssertionOwnerKind::Table => 3,
        AssertionOwnerKind::Module => 4,
    }
}

pub(crate) fn decode_assertion_owner_kind(value: i64) -> Result<AssertionOwnerKind, RuntimeError> {
    match value {
        1 => Ok(AssertionOwnerKind::Executable),
        2 => Ok(AssertionOwnerKind::RefinedType),
        3 => Ok(AssertionOwnerKind::Table),
        4 => Ok(AssertionOwnerKind::Module),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

pub(crate) async fn capture_tx(connection: &Connection) -> Result<CwdCapture, RuntimeError> {
    let mut rows = connection.query("SELECT database_id, runtime_id, generation, generation_digest FROM runtime_meta WHERE singleton = 1", ()).await?;
    let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
    let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let digest = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    cwd_capture_from_parts(database_id, runtime_id, generation, digest)
}

pub(crate) fn cwd_capture_from_parts(
    database_id: [u8; 16],
    runtime_id: [u8; 16],
    generation: i64,
    digest: [u8; 32],
) -> Result<CwdCapture, RuntimeError> {
    if generation < 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    CwdCapture::new(
        Snapshot::cwd(database_id, runtime_id, BigInt::from(generation))
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
        digest,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn fixed<const N: usize>(value: Vec<u8>) -> Result<[u8; N], RuntimeError> {
    value.try_into().map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn validate_id(value: [u8; 16]) -> Result<(), RuntimeError> {
    if value == [0; 16] {
        Err(RuntimeError::InvalidIdentity)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_identity(value: RuntimeIdentity) -> Result<(), RuntimeError> {
    validate_id(value.database_id)?;
    validate_id(value.repository_id)
}

pub(crate) fn validate_digest(value: [u8; 32]) -> Result<(), RuntimeError> {
    if value == [0; 32] {
        Err(RuntimeError::InvalidDigest)
    } else {
        Ok(())
    }
}
