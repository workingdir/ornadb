//! The `core.encoding.ovb` natives: `encode<T>` writes a value as its canonical OVB-1 bytes, and `decode<T>` reads
//! canonical OVB-1 bytes back as a value. Each call charges one step, as the other codec natives do.

use std::rc::Rc;

use orna_hir::Ty;
use orna_value::CanonicalValue;

use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::values::{canonical_bytes, exactly};
use crate::raw_value;
use crate::value::Value;

/// `encode<T>(value: T): Blob`. The canonical bytes are bounded by the collection limit.
pub(super) fn encode(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [value] = exactly::<1>(arguments)?;
    let bytes = canonical_bytes(&value)?;
    if bytes.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Blob(Rc::from(bytes)))
}

/// `decode<T>(input: Blob): T`. The bytes must be one canonical OVB-1 value; anything else is a value error.
pub(super) fn decode(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [input] = exactly::<1>(arguments)?;
    let Value::Blob(bytes) = input else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if bytes.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let stored = CanonicalValue::decode(&bytes).map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))?;
    raw_value::from_raw(stored.raw())
}
