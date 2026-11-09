//! UUID text and byte identities, digests, and row/schema/argument identities.

use crate::Result;
use crate::cbor::encode_raw;
use crate::error::Error;
use crate::format3::{ContextValue, ValueFormat, encode_context_raw};
use crate::media::MimeRegistry;
use crate::raw::{Raw, bytes, tag};
use crate::schema::{SchemaDescriptor, validate_type, validate_value_as_type};
use crate::tags::UUID_TAG;
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use unicode_normalization::UnicodeNormalization;

pub fn semantic_digest(domain: &str, value: &ContextValue) -> Result<[u8; 32]> {
    if value.format != ValueFormat::Sov3 || domain.as_bytes().contains(&0) || !domain.is_ascii() {
        return Err(Error::InvalidProfile);
    }
    let payload = value.encode()?;
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(payload);
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest.finalize());
    Ok(output)
}

pub fn sov3_domain_digest(domain: &str, value: &ContextValue) -> Result<[u8; 32]> {
    semantic_digest(domain, value)
}

pub(crate) fn uuid_raw(bytes: [u8; 16]) -> Raw {
    tag(UUID_TAG, Raw::Bytes(bytes.to_vec()))
}

/// Returns the lowercase, hyphenated canonical text form of an OVB UUID.
///
/// UUID-backed Orna identifiers are opaque bytes at the value boundary.  This
/// helper only defines their canonical text spelling; it does not impose UUID
/// version or variant semantics.
pub fn canonical_uuid_text(bytes: [u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Parses only the canonical lowercase, hyphenated UUID text spelling.
pub fn parse_canonical_uuid_text(text: &str) -> Result<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return Err(Error::InvalidValue);
    }
    for index in [8, 13, 18, 23] {
        if bytes[index] != b'-' {
            return Err(Error::InvalidValue);
        }
    }
    let mut output = [0u8; 16];
    let mut source = 0;
    for destination in &mut output {
        while source < bytes.len() && bytes[source] == b'-' {
            source += 1;
        }
        let high = hex_digit(bytes.get(source).copied()).ok_or(Error::InvalidValue)?;
        let low = hex_digit(bytes.get(source + 1).copied()).ok_or(Error::InvalidValue)?;
        *destination = high << 4 | low;
        source += 2;
    }
    if source != bytes.len() || canonical_uuid_text(output) != text {
        return Err(Error::InvalidValue);
    }
    Ok(output)
}

/// Encodes bytes with the RFC 4648 standard Base64 alphabet and mandatory
/// padding. The output never contains whitespace.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // `len.div_ceil(3) * 4` cannot overflow: slices are at most `isize::MAX` bytes.
    let capacity = bytes.len().div_ceil(3) * 4;
    let mut output = String::with_capacity(capacity);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[((first & 0x03) << 4 | second.unwrap_or(0) >> 4) as usize] as char);
        output.push(match second {
            Some(second) => {
                ALPHABET[((second & 0x0f) << 2 | third.unwrap_or(0) >> 6) as usize] as char
            }
            None => '=',
        });
        output.push(match third {
            Some(third) => ALPHABET[(third & 0x3f) as usize] as char,
            None => '=',
        });
    }
    output
}

/// Decodes strict RFC 4648 standard Base64.
///
/// Whitespace, URL-safe aliases, malformed padding and nonzero unused
/// trailing bits are rejected before any bytes are returned.
pub fn base64_decode(input: &str) -> Result<Vec<u8>> {
    if !input.len().is_multiple_of(4) {
        return Err(Error::InvalidValue);
    }
    let capacity = (input.len() / 4).checked_mul(3).ok_or(Error::Limit)?;
    let mut output = Vec::with_capacity(capacity);
    for (index, chunk) in input.as_bytes().chunks_exact(4).enumerate() {
        let last = index + 1 == input.len() / 4;
        let first = base64_value(chunk[0]).ok_or(Error::InvalidValue)?;
        let second = base64_value(chunk[1]).ok_or(Error::InvalidValue)?;
        let third = if chunk[2] == b'=' {
            if chunk[3] != b'=' || !last || second & 0x0f != 0 {
                return Err(Error::InvalidValue);
            }
            None
        } else {
            let value = base64_value(chunk[2]).ok_or(Error::InvalidValue)?;
            if chunk[3] == b'=' {
                if !last || value & 0x03 != 0 {
                    return Err(Error::InvalidValue);
                }
                output.push(first << 2 | second >> 4);
                output.push(second << 4 | value >> 2);
                continue;
            }
            Some(value)
        };
        output.push(first << 2 | second >> 4);
        if let Some(third) = third {
            let fourth = base64_value(chunk[3]).ok_or(Error::InvalidValue)?;
            output.push(second << 4 | third >> 2);
            output.push(third << 6 | fourth);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn hex_digit(byte: Option<u8>) -> Option<u8> {
    match byte? {
        b'0'..=b'9' => Some(byte? - b'0'),
        b'a'..=b'f' => Some(byte? - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn uuid_array(r: &Raw) -> Result<[u8; 16]> {
    let Raw::Tag(UUID_TAG, v) = r else {
        return Err(Error::InvalidValue);
    };
    let b = bytes(v)?;
    if b.len() != 16 {
        return Err(Error::InvalidValue);
    };
    let mut o = [0; 16];
    o.copy_from_slice(b);
    Ok(o)
}

pub(crate) fn bytes32(r: &Raw) -> Result<[u8; 32]> {
    let b = bytes(r)?;
    if b.len() != 32 {
        return Err(Error::InvalidValue);
    };
    let mut o = [0; 32];
    o.copy_from_slice(b);
    Ok(o)
}

/// SHA-256 of ASCII domain, NUL and exact OVB-1 bytes.
pub fn domain_digest(domain: &str, payload: &Raw) -> Result<[u8; 32]> {
    if !domain.is_ascii() || domain.as_bytes().contains(&0) {
        return Err(Error::InvalidValue);
    }
    let bytes = encode_raw(payload)?;
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(hash.finalize().into())
}

/// SHA-256 of ASCII domain, NUL and the canonical SOV-3 bytes of a payload
/// that needs no format-3 context. Tagged values are rejected at any depth:
/// their encoding depends on context this function does not carry.
pub fn sov3_context_free_domain_digest(domain: &str, payload: &Raw) -> Result<[u8; 32]> {
    if !domain.is_ascii() || domain.as_bytes().contains(&0) || raw_contains_tag(payload) {
        return Err(Error::InvalidValue);
    }
    let bytes = encode_context_raw(payload, ValueFormat::Sov3, None, MimeRegistry::mime1())?;
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(hash.finalize().into())
}

fn raw_contains_tag(raw: &Raw) -> bool {
    match raw {
        Raw::Tag(..) => true,
        Raw::Array(items) => items.iter().any(raw_contains_tag),
        Raw::Map(entries) => entries
            .iter()
            .any(|(key, value)| raw_contains_tag(key) || raw_contains_tag(value)),
        Raw::Null | Raw::Bool(_) | Raw::Int(_) | Raw::Float(_) | Raw::Bytes(_) | Raw::Text(_) => {
            false
        }
    }
}

pub fn row_identity(
    database: [u8; 16],
    table: [u8; 16],
    key: Raw,
    stored: Raw,
) -> Result<[u8; 32]> {
    domain_digest(
        "orna.row.v1",
        &Raw::Array(vec![uuid_raw(database), uuid_raw(table), key, stored]),
    )
}

pub fn schema_identity(schema: &SchemaDescriptor) -> Result<[u8; 32]> {
    domain_digest("orna.schema.v1", schema.raw())
}

pub fn argument_identity(arguments: Vec<(String, Raw, Raw)>) -> Result<[u8; 32]> {
    let mut a = arguments
        .into_iter()
        .map(|(name, ty, value)| (name.nfc().collect::<String>(), ty, value))
        .collect::<Vec<_>>();
    a.sort_by(|x, y| x.0.as_bytes().cmp(y.0.as_bytes()));
    if a.windows(2).any(|x| x[0].0 == x[1].0) {
        return Err(Error::InvalidValue);
    };
    for (_, ty, value) in &a {
        validate_type(ty)?;
        validate_value_as_type(value, ty)?;
    }
    domain_digest(
        "orna.arguments.v1",
        &Raw::Array(
            a.into_iter()
                .map(|(n, t, v)| Raw::Array(vec![Raw::Text(n), t, v]))
                .collect(),
        ),
    )
}
