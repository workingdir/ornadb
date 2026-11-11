//! The `core.encoding.json` natives: `encode<T>` writes a value as JSON text, and `decode<T>` and
//! `decode_with_options<T>` read JSON text as a value of `T`. Each call charges one step, as the other codec natives do.

mod decode;
mod encode;
mod number;
mod parse;
mod scope;
#[cfg(test)]
mod tests;

use orna_hir::Ty;

use self::decode::{Options, decode as decode_node};
use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// `encode<T>(value: T): Str`.
pub(super) fn encode(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let ty = target(targs)?;
    let [value] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let mut out = String::new();
    encode::encode(machine, &value, ty, None, &mut out)?;
    if out.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Text(out))
}

/// `decode<T>(input: Str): T`. An unknown object key is a value error.
pub(super) fn decode(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [input] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    decode_text(machine, targs, input, false)
}

/// `decode_with_options<T>(input: Str, ignore_unknown_fields: Bool): T`.
pub(super) fn decode_with_options(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [input, ignore] =
        <[Value; 2]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let Value::Bool(ignore_unknown_fields) = ignore else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    decode_text(machine, targs, input, ignore_unknown_fields)
}

fn decode_text(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    input: Value,
    ignore_unknown_fields: bool,
) -> Result<Value, Stop> {
    let ty = target(targs)?;
    let Value::Text(text) = input else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if text.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let node = parse::parse(&machine.limits, &text)?;
    decode_node(
        machine,
        &node,
        ty,
        None,
        Options {
            ignore_unknown_fields,
        },
    )
}

/// The type argument `T` of a codec call.
fn target(targs: &[Ty]) -> Result<Ty, Stop> {
    targs.first().copied().ok_or(Stop::Error("ORNA-EVAL-TYPE"))
}
