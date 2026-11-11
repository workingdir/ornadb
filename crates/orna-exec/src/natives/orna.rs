//! The `core.encoding.orna` natives: `encode<T>` writes a value as canonical Orna text. Each call charges one step, as
//! the other codec natives do.

mod decode;
mod encode;
#[cfg(test)]
mod tests;

use orna_hir::Ty;
use orna_syntax::parse_expression;

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// `encode<T>(value: T): Str`. The text ends with one final line feed, as canonical output does.
pub(super) fn encode(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let ty = targs
        .first()
        .copied()
        .ok_or(Stop::Error("ORNA-EVAL-TYPE"))?;
    let [value] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let mut out = String::new();
    encode::encode(machine, &value, ty, None, 0, &mut out)?;
    out.push('\n');
    if out.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Text(out))
}

/// `decode<T>(input: Str): T`. The text must be exactly one canonical expression of type `T`; anything else is a value
/// error. Each call charges one step.
pub(super) fn decode(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let ty = targs
        .first()
        .copied()
        .ok_or(Stop::Error("ORNA-EVAL-TYPE"))?;
    let [input] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let Value::Text(text) = input else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if text.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let parsed = parse_expression(&text);
    if !parsed.is_ok() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    decode::decode(machine, &parsed.value, ty, None, 0)
}
