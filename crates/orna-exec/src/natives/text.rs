//! The `core.text` natives: trimming, splitting and joining, searching, replacing, normalisation and Unicode case
//! mapping, copied from the old evaluator's `text` family. Only `split` and `join` charge steps, one per element, as the
//! old evaluator does; every produced string is bounded by the string limit.

use std::rc::Rc;

use unicode_normalization::UnicodeNormalization;

use super::unicode_case::{lowercase, uppercase, white_space};
use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// `trim(Str): Str`, removing leading and trailing White_Space of Unicode 16.0.0.
pub(super) fn trim(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value] = strings::<1>(arguments)?;
    bounded(machine, value.trim_matches(white_space).to_owned())
}

/// `split(Str, Str): [Str]`. Leading and trailing empty fields are kept; an empty separator splits into scalar values.
pub(super) fn split(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value, separator] = strings::<2>(arguments)?;
    let count = if separator.is_empty() {
        value.chars().count()
    } else {
        value.split(separator.as_str()).count()
    };
    items(machine, count)?;
    let mut fields = Vec::with_capacity(count);
    if separator.is_empty() {
        for scalar in value.chars() {
            machine.tick(1)?;
            fields.push(Value::Text(scalar.to_string()));
        }
    } else {
        for field in value.split(separator.as_str()) {
            machine.tick(1)?;
            fields.push(Value::Text(field.to_owned()));
        }
    }
    Ok(Value::List(Rc::from(fields)))
}

/// `join([Str], Str): Str`, the elements separated by the separator. The result is bounded as it grows.
pub(super) fn join(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [values, separator] =
        <[Value; 2]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let (Value::List(values), Value::Text(separator)) = (values, separator) else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    items(machine, values.len())?;
    let mut output = String::new();
    for (index, value) in values.iter().enumerate() {
        let Value::Text(value) = value else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        machine.tick(1)?;
        if index > 0 {
            output.push_str(&separator);
        }
        output.push_str(value);
        if output.len() > machine.limits.max_string_bytes {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
    }
    Ok(Value::Text(output))
}

/// `starts_with(Str, Str): Bool`.
pub(super) fn starts_with(
    _machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [value, prefix] = strings::<2>(arguments)?;
    Ok(Value::Bool(value.starts_with(prefix.as_str())))
}

/// `ends_with(Str, Str): Bool`.
pub(super) fn ends_with(_machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value, suffix] = strings::<2>(arguments)?;
    Ok(Value::Bool(value.ends_with(suffix.as_str())))
}

/// `contains(Str, Str): Bool`.
pub(super) fn contains(_machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value, needle] = strings::<2>(arguments)?;
    Ok(Value::Bool(value.contains(needle.as_str())))
}

/// `replace(Str, Str, Str): Str`. The replacement is non-overlapping and left to right, as `str::replace` does.
pub(super) fn replace(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value, from, to] = strings::<3>(arguments)?;
    bounded(machine, value.replace(from.as_str(), to.as_str()))
}

/// `normalise(Str, Str): Str`. The form labels `NFC` and `NFD` are the canonical forms; any other label is refused.
pub(super) fn normalise(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value, form] = strings::<2>(arguments)?;
    match form.as_str() {
        "NFC" => bounded(machine, value.nfc().collect()),
        "NFD" => bounded(machine, value.nfd().collect()),
        _ => Err(Stop::Error("ORNA-EVAL-VALUE")),
    }
}

/// `lower(Str): Str`, under the Unicode 16.0.0 lowercase mapping with the Final_Sigma rule.
pub(super) fn lower(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value] = strings::<1>(arguments)?;
    bounded(machine, lowercase(&value))
}

/// `upper(Str): Str`, under the Unicode 16.0.0 uppercase mapping.
pub(super) fn upper(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let [value] = strings::<1>(arguments)?;
    bounded(machine, uppercase(&value))
}

/// The string arguments in order. Any other arity or argument kind is a type error.
fn strings<const N: usize>(arguments: Vec<Value>) -> Result<[String; N], Stop> {
    let values = <[Value; N]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let mut output: Vec<String> = Vec::with_capacity(N);
    for value in values {
        match value {
            Value::Text(text) => output.push(text),
            _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
        }
    }
    output.try_into().map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))
}

/// Refuses a collection of more elements than the limit allows.
fn items(machine: &Machine<'_>, count: usize) -> Result<(), Stop> {
    if count > machine.limits.max_collection_items {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}

/// Wraps a produced string, refusing one longer than the string limit.
fn bounded(machine: &Machine<'_>, value: String) -> Result<Value, Stop> {
    if value.len() > machine.limits.max_string_bytes {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(Value::Text(value))
    }
}
