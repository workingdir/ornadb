//! The `core.hash` natives: SHA-256 digests and their hexadecimal text, copied from the old evaluator's hash family.
//! Each call charges one step, as the old evaluator does.

use std::rc::Rc;

use sha2::{Digest, Sha256};

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// `sha256(Blob): Digest`. The input is bounded by the string limit.
pub(super) fn sha256(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let input = blob(one(arguments)?)?;
    if input.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(digest(&input))
}

/// `sha256_text(Str): Digest`, over the UTF-8 bytes of the text.
pub(super) fn sha256_text(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let input = text(one(arguments)?)?;
    Ok(digest(input.as_bytes()))
}

/// `domain_sha256(Str, Blob): Digest`: SHA-256 over the domain, a zero byte, then the payload. The domain is non-empty
/// ASCII without NUL, and the payload is bounded by the string limit.
pub(super) fn domain_sha256(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [domain, payload] = <[Value; 2]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-TYPE"))?;
    let domain = text(domain)?;
    let payload = blob(payload)?;
    if domain.is_empty()
        || !domain.is_ascii()
        || domain.as_bytes().contains(&0)
        || payload.len() > machine.limits.max_string_bytes
    {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(&payload[..]);
    Ok(Value::Blob(Rc::from(hasher.finalize().as_slice())))
}

/// `to_hex(Digest): Str`: the 32 bytes as 64 lowercase hexadecimal digits.
pub(super) fn to_hex(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let digest = blob(one(arguments)?)?;
    if digest.len() != 32 {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    }
    let mut output = String::with_capacity(64);
    for &byte in digest.iter() {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    if output.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Text(output))
}

/// `from_hex(Str): Digest?`: `Some` of the digest when the text is exactly 64 lowercase hexadecimal digits.
pub(super) fn from_hex(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let encoded = text(one(arguments)?)?;
    let valid = encoded.len() == 64
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let decoded = valid.then(|| Rc::new(Value::Blob(decode(encoded.as_bytes()))));
    Ok(Value::Option(decoded))
}

/// The bytes of validated lowercase hexadecimal text, two digits to a byte.
fn decode(hex: &[u8]) -> Rc<[u8]> {
    hex.chunks_exact(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect()
}

fn nibble(digit: u8) -> u8 {
    if digit.is_ascii_digit() {
        digit - b'0'
    } else {
        digit - b'a' + 10
    }
}

fn digest(input: &[u8]) -> Value {
    Value::Blob(Rc::from(Sha256::digest(input).as_slice()))
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
