//! Argument and result helpers shared by the families that work on lists, integers and options.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_value::CanonicalValue;

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// The arguments as exactly `N` values, in parameter order.
pub(super) fn exactly<const N: usize>(arguments: Vec<Value>) -> Result<[Value; N], Stop> {
    <[Value; N]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))
}

/// The items of a list value.
pub(super) fn list(value: Value) -> Result<Rc<[Value]>, Stop> {
    match value {
        Value::List(items) => Ok(items),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The integer of an integer value.
pub(super) fn int(value: Value) -> Result<BigInt, Stop> {
    match value {
        Value::Int(number) => Ok(number),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The truth of a boolean value.
pub(super) fn truth(value: Value) -> Result<bool, Stop> {
    match value {
        Value::Bool(flag) => Ok(flag),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// A list holding `items`.
pub(super) fn list_of(items: Vec<Value>) -> Value {
    Value::List(Rc::from(items))
}

/// A pair, as a tuple value.
pub(super) fn pair(first: Value, second: Value) -> Value {
    Value::Tuple(Rc::from(vec![first, second]))
}

/// An option holding `value`.
pub(super) fn some(value: Value) -> Value {
    Value::Option(Some(Rc::new(value)))
}

/// The empty option.
pub(super) fn none() -> Value {
    Value::Option(None)
}

/// Charges `count` steps, one at a time, as the old evaluator's per-element step does.
pub(super) fn charge(machine: &mut Machine<'_>, count: usize) -> Result<(), Stop> {
    for _ in 0..count {
        machine.tick(1)?;
    }
    Ok(())
}

/// The canonical bytes of `value`, the identity the old evaluator compares rows and keys by.
pub(super) fn canonical_bytes(value: &Value) -> Result<Vec<u8>, Stop> {
    let raw = value.to_raw().map_err(Stop::Error)?;
    CanonicalValue::new(raw)
        .map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))?
        .encode()
        .map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))
}
