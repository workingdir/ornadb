//! The canonical CBOR data model, its validated wrapper, raw accessors and
//! IEEE-754 float ordering.

use crate::cbor::{Reader, encode_raw};
use crate::codec::ErrorValue;
use crate::decimal::Decimal;
use crate::error::Error;
use crate::identity::uuid_raw;
use crate::primary_key::is_permitted_key;
use crate::tags::{
    DIAGNOSTIC_TAG, ERROR_TAG, OPTION_TAG, PRESENT_NODE_TAG, STORED_TABLE_REFERENCE_TAG,
    SYS_VALUE_BOX_TAG, UNIT_TAG, UUID_TAG,
};
use crate::validation::validate_raw;
use crate::{CANONICAL_NAN_BITS, MAX_DEPTH, Result};
use num_bigint::BigInt;
use num_bigint::Sign;
use std::cmp::Ordering;
use std::fmt;

/// Canonical CBOR's supported data model. `Tag` is public for schema and
/// protocol adapters, but both construction and decoding validate the closed
/// Orna tag registry before bytes may be emitted.
#[derive(Clone, Eq, PartialEq, Hash)]
pub enum Raw {
    Null,
    Bool(bool),
    Int(BigInt),
    Float(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Raw>),
    Map(Vec<(Raw, Raw)>),
    Tag(u64, Box<Raw>),
}

enum RawDebugValue {
    Null,
    Bool(bool),
    Int(BigInt),
    Float(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<RawDebugValue>),
    Map(Vec<(RawDebugValue, RawDebugValue)>),
    Tag(u64, Box<RawDebugValue>),
}

impl fmt::Debug for RawDebugValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::Bool(value) => formatter.debug_tuple("Bool").field(value).finish(),
            Self::Int(value) => formatter.debug_tuple("Int").field(value).finish(),
            Self::Float(value) => formatter.debug_tuple("Float").field(value).finish(),
            Self::Bytes(value) => formatter.debug_tuple("Bytes").field(value).finish(),
            Self::Text(value) => formatter.debug_tuple("Text").field(value).finish(),
            Self::Array(values) => formatter.debug_tuple("Array").field(values).finish(),
            Self::Map(entries) => formatter.debug_tuple("Map").field(entries).finish(),
            Self::Tag(tag, value) => formatter
                .debug_tuple("Tag")
                .field(tag)
                .field(value)
                .finish(),
        }
    }
}

impl fmt::Debug for Raw {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match raw_debug_value(self, 0, false) {
            Ok(value) => fmt::Debug::fmt(&value, formatter),
            // Raw is also used before canonical validation. If a malformed
            // intermediate cannot be safely projected, traces fail closed.
            Err(_) => formatter.write_str("\"<redacted>\""),
        }
    }
}

/// A validated OVB-1 value.  The inner raw value is never exposed mutably.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Value(pub(crate) Raw);

impl fmt::Debug for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match raw_debug_value(&self.0, 0, false) {
            Ok(raw) => formatter.debug_tuple("Value").field(&raw).finish(),
            // Invalid internal markers are never printed with their payload.
            Err(_) => formatter.debug_tuple("Value").field(&"<redacted>").finish(),
        }
    }
}

impl Value {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    /// Builds the stored table-reference identity `[database, table, key]`
    /// (tag 60021, ORNA-REF-001). The key must be one primary-key value; no row
    /// or snapshot context is embedded.
    pub fn table_reference(database_id: [u8; 16], table_id: [u8; 16], key: Raw) -> Result<Self> {
        if !is_permitted_key(&key) {
            return Err(Error::InvalidValue);
        }
        Self::new(Raw::Tag(
            STORED_TABLE_REFERENCE_TAG,
            Box::new(Raw::Array(vec![
                uuid_raw(database_id),
                uuid_raw(table_id),
                key,
            ])),
        ))
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
    }
    /// Returns a transport/trace-safe projection with every nested Error
    /// payload redacted and generic protected wrappers hidden. Canonical values
    /// remain lossless for local recovery; callers crossing an unprivileged
    /// observability boundary must opt into this projection because OVB has no
    /// field-level secrecy classifier for those wrappers.
    pub fn redacted_for_trace(&self) -> Result<Self> {
        Self::new(redact_trace_values(&self.0, 0, true)?)
    }
    /// Redacts Error values while retaining surrounding registered wrappers.
    /// Typed protocol decoders use this only before they apply their own
    /// wrapper-aware projection; generic trace output must use
    /// [`Self::redacted_for_trace`] instead.
    pub fn redacted_error_values_for_trace(&self) -> Result<Self> {
        Self::new(redact_trace_values(&self.0, 0, false)?)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let raw = r.raw(0)?;
        if r.at != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        validate_raw(&raw, 0)?;
        if encode_raw(&raw)? != bytes {
            return Err(Error::NonCanonical);
        }
        Ok(Self(raw))
    }
    pub fn int(v: BigInt) -> Self {
        Self(Raw::Int(v))
    }
    pub fn float_bits(bits: u64) -> Self {
        Self(Raw::Float(if is_nan_bits(bits) {
            CANONICAL_NAN_BITS
        } else {
            bits
        }))
    }
    pub fn decimal(coefficient: BigInt, exponent10: BigInt) -> Result<Self> {
        let decimal = Decimal::try_new(coefficient, exponent10)?;
        Self::new(decimal.raw())
    }
    pub fn option(value: Option<Value>) -> Result<Self> {
        let mut x = vec![Raw::Int(0.into())];
        if let Some(v) = value {
            x[0] = Raw::Int(1.into());
            x.push(v.0);
        }
        Self::new(tag(OPTION_TAG, Raw::Array(x)))
    }
    pub fn unit() -> Self {
        Self(tag(UNIT_TAG, Raw::Array(vec![])))
    }
    pub fn uuid(bytes: [u8; 16]) -> Self {
        Self(tag(UUID_TAG, Raw::Bytes(bytes.to_vec())))
    }
    pub fn protected() -> Self {
        Self(Raw::Tag(0, Box::new(Raw::Null)))
    } // unencodable marker
}

fn redact_trace_values(raw: &Raw, depth: usize, hide_wrappers: bool) -> Result<Raw> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    if hide_wrappers
        && matches!(
            raw,
            Raw::Tag(0 | DIAGNOSTIC_TAG | PRESENT_NODE_TAG | SYS_VALUE_BOX_TAG, _)
        )
    {
        // ORNA-SECRET-002 requires these values to stay opaque in traces. The
        // generic OVB layer has no per-field disclosure classifier, so replace
        // the entire wrapper with ordinary text instead of forging a typed
        // wrapper whose redacted bit or shape could imply privileged approval.
        return Ok(Raw::Text("<redacted>".into()));
    }
    Ok(match raw {
        Raw::Tag(ERROR_TAG, _) => {
            ErrorValue::from_value(Value::new(raw.clone())?)?
                .redacted_for_trace()
                .0
                .0
        }
        Raw::Array(values) => Raw::Array(
            values
                .iter()
                .map(|value| redact_trace_values(value, depth + 1, hide_wrappers))
                .collect::<Result<_>>()?,
        ),
        Raw::Map(entries) => {
            let mut redacted = entries
                .iter()
                .map(|(key, value)| {
                    Ok((
                        redact_trace_values(key, depth + 1, hide_wrappers)?,
                        redact_trace_values(value, depth + 1, hide_wrappers)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            if entries
                .iter()
                .zip(&redacted)
                .any(|((key, _), (safe_key, _))| key != safe_key)
            {
                // Hiding a typed map key can collapse distinct keys and make
                // the projected map non-canonical. Hide the containing map
                // as a whole rather than emit an invalid or ambiguous trace.
                return Ok(Raw::Text("<redacted>".into()));
            }
            redacted.sort_by(|(left, _), (right, _)| {
                encode_raw(left)
                    .expect("validated map keys remain encodable")
                    .cmp(&encode_raw(right).expect("validated map keys remain encodable"))
            });
            Raw::Map(redacted)
        }
        Raw::Tag(tag, value) => Raw::Tag(
            *tag,
            Box::new(redact_trace_values(value, depth + 1, hide_wrappers)?),
        ),
        value => value.clone(),
    })
}

fn raw_debug_value(raw: &Raw, depth: usize, errors_redacted: bool) -> Result<RawDebugValue> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    if matches!(
        raw,
        Raw::Tag(0 | DIAGNOSTIC_TAG | PRESENT_NODE_TAG | SYS_VALUE_BOX_TAG, _)
    ) {
        // Debug has no disclosure grant. Protected markers can carry plaintext
        // before wire validation, and generic Diagnostic, Present, and sys.Value
        // wrappers have no field classifier here, so hide their whole payload.
        // The closed wrappers are redacted as a pragmatic fail-closed projection.
        return Ok(RawDebugValue::Text("<redacted>".into()));
    }
    if !errors_redacted && matches!(raw, Raw::Tag(ERROR_TAG, _)) {
        let safe = redact_trace_values(raw, depth, true)?;
        return raw_debug_value(&safe, depth, true);
    }

    Ok(match raw {
        Raw::Null => RawDebugValue::Null,
        Raw::Bool(value) => RawDebugValue::Bool(*value),
        Raw::Int(value) => RawDebugValue::Int(value.clone()),
        Raw::Float(value) => RawDebugValue::Float(*value),
        Raw::Bytes(value) => RawDebugValue::Bytes(value.clone()),
        Raw::Text(value) => RawDebugValue::Text(value.clone()),
        Raw::Array(values) => RawDebugValue::Array(
            values
                .iter()
                .map(|value| raw_debug_value(value, depth + 1, errors_redacted))
                .collect::<Result<_>>()?,
        ),
        Raw::Map(entries) => RawDebugValue::Map(
            entries
                .iter()
                .map(|(key, value)| {
                    Ok((
                        raw_debug_value(key, depth + 1, errors_redacted)?,
                        raw_debug_value(value, depth + 1, errors_redacted)?,
                    ))
                })
                .collect::<Result<_>>()?,
        ),
        Raw::Tag(tag, value) => RawDebugValue::Tag(
            *tag,
            Box::new(raw_debug_value(value, depth + 1, errors_redacted)?),
        ),
    })
}

pub(crate) fn tag(n: u64, raw: Raw) -> Raw {
    Raw::Tag(n, Box::new(raw))
}

pub(crate) fn is_nan_bits(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
}

/// IEEE-754 `totalOrder` key, defined directly by FLOAT-TOTAL-1.
pub const fn float_total_key(bits: u64) -> u64 {
    if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ 0x8000_0000_0000_0000
    }
}

pub fn float_total_cmp(left: u64, right: u64) -> Ordering {
    float_total_key(left).cmp(&float_total_key(right))
}

pub fn float_ordinary_eq(left: u64, right: u64) -> bool {
    !is_nan_bits(left)
        && !is_nan_bits(right)
        && (left == right || ((left | right) & 0x7fff_ffff_ffff_ffff) == 0)
}

pub fn float_min(values: &[u64]) -> Option<u64> {
    aggregate(values, true)
}

pub fn float_max(values: &[u64]) -> Option<u64> {
    aggregate(values, false)
}

fn aggregate(values: &[u64], min: bool) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    if values.iter().any(|v| is_nan_bits(*v)) {
        return Some(CANONICAL_NAN_BITS);
    }
    let mut candidate = values[0];
    for &v in &values[1..] {
        if (float_total_cmp(v, candidate) == Ordering::Less) == min {
            candidate = v;
        }
    }
    Some(candidate)
}

pub(crate) fn array(v: &Raw) -> Result<&Vec<Raw>> {
    if let Raw::Array(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}

pub(crate) fn map(v: &Raw) -> Result<&Vec<(Raw, Raw)>> {
    if let Raw::Map(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}

pub(crate) fn bytes(v: &Raw) -> Result<&[u8]> {
    if let Raw::Bytes(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}

pub(crate) fn text(v: &Raw) -> Result<&str> {
    if let Raw::Text(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}

pub(crate) fn integer(v: &Raw) -> Result<&BigInt> {
    if let Raw::Int(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}

pub(crate) fn int_u64(v: &Raw) -> Result<u64> {
    let integer = integer(v)?;
    if integer.sign() == Sign::Minus {
        return Err(Error::InvalidValue);
    }
    let limbs = integer.to_u64_digits().1;
    match limbs.as_slice() {
        [] => Ok(0),
        [value] => Ok(*value),
        _ => Err(Error::InvalidValue),
    }
}
