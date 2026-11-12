//! The `sys.blob` read-side natives over in-memory Blob values: length, digest, bounded read, and the explicit bounded
//! conversions to and from bytes. Each call charges one step. Every failure is a declared `sys.blob.*` code.

use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use orna_hir::Ty;
use sha2::{Digest, Sha256};

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

use super::hash::blob;
use super::values::{exactly, int, list, list_of};

/// The largest byte count one call may return or materialise (ORNA-BLOB-011).
const MAX_CALL_BYTES: u64 = 8 * 1024 * 1024;

/// `sys.blob.length(Blob): Int`: the exact byte count.
pub(super) fn blob_length(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [value] = exactly::<1>(arguments)?;
    Ok(Value::Int(BigInt::from(blob(value)?.len())))
}

/// `sys.blob.digest(Blob): Digest`: the SHA-256 of the raw content.
pub(super) fn blob_digest(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [value] = exactly::<1>(arguments)?;
    let bytes = blob(value)?;
    Ok(Value::Digest(Sha256::digest(&bytes).into()))
}

/// `sys.blob.read(Blob, Int, Int): Blob`: the bounded raw slice starting at `offset`, clipped at the known end.
/// A negative or unrepresentable bound, a start past the end and a checked-overflow end are `sys.blob.bounds`.
pub(super) fn blob_read(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [value, offset, length] = exactly::<3>(arguments)?;
    let bytes = blob(value)?;
    let offset = int(offset)?
        .to_u64()
        .ok_or(Stop::Error("sys.blob.bounds"))?;
    let length = int(length)?
        .to_u64()
        .ok_or(Stop::Error("sys.blob.bounds"))?;
    let total = bytes.len() as u64;
    if offset > total || offset.checked_add(length).is_none() {
        return Err(Stop::Error("sys.blob.bounds"));
    }
    if length > MAX_CALL_BYTES {
        return Err(Stop::Error("sys.blob.limit"));
    }
    let end = offset.saturating_add(length).min(total);
    Ok(Value::Blob(Rc::from(&bytes[offset as usize..end as usize])))
}

/// `sys.blob.from_bytes([Int]): Blob`: the bytes of integers in `0..=255`, at most 8 MiB of them.
pub(super) fn blob_from_bytes(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [items] = exactly::<1>(arguments)?;
    let items = list(items)?;
    if items.len() as u64 > MAX_CALL_BYTES {
        return Err(Stop::Error("sys.blob.limit"));
    }
    let bytes = items
        .iter()
        .map(|item| match item {
            Value::Int(number) => number.to_u8().ok_or(Stop::Error("sys.blob.bounds")),
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        })
        .collect::<Result<Vec<u8>, Stop>>()?;
    Ok(Value::Blob(Rc::from(bytes)))
}

/// `sys.blob.to_bytes(Blob): [Int]`: the explicit bounded materialisation, at most 8 MiB.
pub(super) fn blob_to_bytes(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [value] = exactly::<1>(arguments)?;
    let bytes = blob(value)?;
    if bytes.len() as u64 > MAX_CALL_BYTES {
        return Err(Stop::Error("sys.blob.limit"));
    }
    Ok(list_of(
        bytes
            .iter()
            .map(|byte| Value::Int(BigInt::from(*byte)))
            .collect(),
    ))
}
