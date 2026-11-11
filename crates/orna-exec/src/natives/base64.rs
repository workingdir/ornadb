//! The `core.encoding.base64` natives: RFC 4648 standard Base64 over a blob and its text, copied from the old evaluator's
//! codecs family. Each call charges one step, as the old evaluator does.

use std::rc::Rc;

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// `encode(Blob): Str`. The byte count is bounded by the collection limit and the text by the string limit.
pub(super) fn encode(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let input = blob(one(arguments)?)?;
    if input.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let encoded = orna_value::base64_encode(&input);
    if encoded.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Text(encoded))
}

/// `decode(Str): Blob`. The text is bounded by the string limit, and the decoded bytes by the collection limit. Text
/// that is not strict standard Base64 is a value error.
pub(super) fn decode(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let input = text(one(arguments)?)?;
    if input.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let bytes = orna_value::base64_decode(&input).map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))?;
    if bytes.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Blob(Rc::from(bytes)))
}

/// The one argument of a unary native, or a type error for any other arity.
fn one(arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    Ok(value)
}

fn blob(value: Value) -> Result<Rc<[u8]>, Stop> {
    match value {
        Value::Blob(bytes) => Ok(bytes),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

fn text(value: Value) -> Result<String, Stop> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}
