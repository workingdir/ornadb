//! The typed OVB-1 codecs and the typed Error value view.

use crate::Result;
use crate::cbor::encode_raw;
use crate::error::Error;
use crate::raw::{Raw, Value, array, map, tag, text};
use crate::tags::{ERROR_TAG, OPTION_TAG, RANGE_TAG, TUPLE_TAG, UNIT_TAG, UUID_TAG};
use crate::validation::{integer_map, validate_error};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use std::collections::BTreeMap;
use std::fmt;

/// A type-directed failure from a canonical value decoder.
///
/// The underlying [`Error`] identifies the malformed OVB condition while
/// `path` identifies the expected type position, from the outer value inward.
/// No decoded payload is retained or formatted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeError {
    error: Error,
    path: Vec<String>,
}

impl DecodeError {
    pub(crate) fn new(error: Error, path: Vec<String>) -> Self {
        Self { error, path }
    }

    /// Returns the underlying canonical-value failure.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Returns the expected type path from the root to the failing value.
    pub fn path(&self) -> &[String] {
        &self.path
    }

    /// Returns the expected type path in a stable human-readable form.
    pub fn type_path(&self) -> String {
        self.path.join(".")
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "OVB-1 decode: {}", self.error)
        } else {
            write!(f, "OVB-1 decode at {}: {}", self.type_path(), self.error)
        }
    }
}

impl std::error::Error for DecodeError {}

/// Public type-directed OVB-1 codec contract.
///
/// Implementations must use the existing [`Value`] representation; the
/// blanket `Option<T>` implementation below supplies canonical tag 60013
/// handling without introducing another value model.
pub trait OvbCodec: Sized {
    /// Stable type label used in [`DecodeError`] paths.
    fn type_label() -> &'static str;

    /// Converts the typed value to a validated canonical value.
    fn encode_value(&self) -> Result<Value>;

    /// Converts a validated canonical value to the typed value.
    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError>;
}

/// Encodes a typed value as canonical OVB-1 bytes.
pub fn encode_typed<T: OvbCodec>(value: &T) -> Result<Vec<u8>> {
    value.encode_value()?.encode()
}

/// Decodes canonical OVB-1 bytes as the requested type.
pub fn decode_typed<T: OvbCodec>(bytes: &[u8]) -> std::result::Result<T, DecodeError> {
    let value = Value::decode(bytes)
        .map_err(|error| DecodeError::new(error, vec![T::type_label().to_owned()]))?;
    let mut path = vec![T::type_label().to_owned()];
    T::decode_value(&value, &mut path)
}

/// Short aliases for callers that use the generic codec operations directly.
pub fn encode<T: OvbCodec>(value: &T) -> Result<Vec<u8>> {
    encode_typed(value)
}

/// Short alias for the generic typed decoder.
pub fn decode<T: OvbCodec>(bytes: &[u8]) -> std::result::Result<T, DecodeError> {
    decode_typed(bytes)
}

fn decode_type_mismatch(path: &[String]) -> DecodeError {
    DecodeError::new(Error::InvalidValue, path.to_vec())
}

impl OvbCodec for Value {
    fn type_label() -> &'static str {
        "Value"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(self.clone())
    }

    fn decode_value(
        value: &Value,
        _path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        Ok(value.clone())
    }
}

impl OvbCodec for Raw {
    fn type_label() -> &'static str {
        "Raw"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(self.clone())
    }

    fn decode_value(
        value: &Value,
        _path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        Ok(value.raw().clone())
    }
}

impl OvbCodec for bool {
    fn type_label() -> &'static str {
        "Bool"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(Raw::Bool(*self))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Bool(value) => Ok(*value),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for BigInt {
    fn type_label() -> &'static str {
        "Int"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::int(self.clone()))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Int(value) => Ok(value.clone()),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for i64 {
    fn type_label() -> &'static str {
        "Int"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::int(BigInt::from(*self)))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Int(value) => value.to_i64().ok_or_else(|| decode_type_mismatch(path)),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for u64 {
    fn type_label() -> &'static str {
        "UInt"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::int(BigInt::from(*self)))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Int(value) => value.to_u64().ok_or_else(|| decode_type_mismatch(path)),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for f64 {
    fn type_label() -> &'static str {
        "Float"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::float_bits(self.to_bits()))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Float(bits) => Ok(f64::from_bits(*bits)),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for String {
    fn type_label() -> &'static str {
        "Str"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(Raw::Text(self.clone()))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Text(value) => Ok(value.clone()),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for Vec<u8> {
    fn type_label() -> &'static str {
        "Blob"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(Raw::Bytes(self.clone()))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Bytes(value) => Ok(value.clone()),
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

impl OvbCodec for [u8; 16] {
    fn type_label() -> &'static str {
        "Uuid"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::uuid(*self))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        match value.raw() {
            Raw::Tag(UUID_TAG, payload) => match payload.as_ref() {
                Raw::Bytes(bytes) if bytes.len() == 16 => {
                    let mut uuid = [0_u8; 16];
                    uuid.copy_from_slice(bytes);
                    Ok(uuid)
                }
                _ => Err(decode_type_mismatch(path)),
            },
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

/// A typed OVB-1 map codec backed by Rust's ordered map.
///
/// Map entries are ordered by the complete canonical bytes of their encoded
/// keys, rather than by Rust's `Ord` implementation. This preserves OVB-1's
/// deterministic ordering even when those orders differ.
impl<K, V> OvbCodec for BTreeMap<K, V>
where
    K: OvbCodec + Ord,
    V: OvbCodec,
{
    fn type_label() -> &'static str {
        "Map"
    }

    fn encode_value(&self) -> Result<Value> {
        let mut entries = Vec::with_capacity(self.len());
        for (key, value) in self {
            let key_value = key.encode_value()?;
            let key_bytes = key_value.encode()?;
            let value = value.encode_value()?;
            entries.push((key_bytes, key_value.0, value.0));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(Error::DuplicateOrUnorderedMapKey);
        }
        Value::new(Raw::Map(
            entries
                .into_iter()
                .map(|(_, key, value)| (key, value))
                .collect(),
        ))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        let Raw::Map(entries) = value.raw() else {
            return Err(decode_type_mismatch(path));
        };

        let mut decoded = BTreeMap::new();
        for (index, (key, value)) in entries.iter().enumerate() {
            path.push(index.to_string());

            path.push("Key".to_owned());
            let key = Value::new(key.clone())
                .map_err(|error| DecodeError::new(error, path.clone()))
                .and_then(|key| K::decode_value(&key, path));
            path.pop();
            let key = key?;

            path.push("Value".to_owned());
            let value = Value::new(value.clone())
                .map_err(|error| DecodeError::new(error, path.clone()))
                .and_then(|value| V::decode_value(&value, path));
            path.pop();
            let value = value?;

            if decoded.insert(key, value).is_some() {
                path.push("Key".to_owned());
                return Err(DecodeError::new(
                    Error::DuplicateOrUnorderedMapKey,
                    path.clone(),
                ));
            }
            path.pop();
        }
        Ok(decoded)
    }
}

impl<T: OvbCodec> OvbCodec for Option<T> {
    fn type_label() -> &'static str {
        "Option"
    }

    fn encode_value(&self) -> Result<Value> {
        match self {
            None => Value::option(None),
            Some(value) => Value::option(Some(value.encode_value()?)),
        }
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        let Raw::Tag(OPTION_TAG, payload) = value.raw() else {
            return Err(decode_type_mismatch(path));
        };
        let Raw::Array(payload) = payload.as_ref() else {
            return Err(decode_type_mismatch(path));
        };
        match payload.as_slice() {
            [Raw::Int(flag)] if flag.is_zero() => Ok(None),
            [Raw::Int(flag), payload] if *flag == BigInt::from(1) => {
                path.push(T::type_label().to_owned());
                let result = T::decode_value(
                    &Value::new(payload.clone())
                        .map_err(|error| DecodeError::new(error, path.clone()))?,
                    path,
                );
                path.pop();
                result.map(Some)
            }
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

/// A typed ordered range with inclusive lower and configurable upper bounds.
///
/// `None` represents an unbounded endpoint. The endpoint type parameter keeps
/// both present bounds in the same type; this codec preserves empty ranges
/// without trying to impose ordering on `T`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeValue<T> {
    lower: Option<T>,
    upper: Option<T>,
    upper_inclusive: bool,
}

impl<T> RangeValue<T> {
    /// Creates a range with an inclusive lower bound and the supplied upper
    /// bound semantics.
    pub const fn new(lower: Option<T>, upper: Option<T>, upper_inclusive: bool) -> Self {
        Self {
            lower,
            upper,
            upper_inclusive,
        }
    }

    /// Returns the inclusive lower bound, or `None` for an unbounded start.
    pub const fn lower(&self) -> Option<&T> {
        self.lower.as_ref()
    }

    /// Returns the upper bound, or `None` for an unbounded end.
    pub const fn upper(&self) -> Option<&T> {
        self.upper.as_ref()
    }

    /// Reports whether a present upper bound is inclusive.
    pub const fn upper_inclusive(&self) -> bool {
        self.upper_inclusive
    }
}

impl<T: OvbCodec> OvbCodec for RangeValue<T> {
    fn type_label() -> &'static str {
        "Range"
    }

    fn encode_value(&self) -> Result<Value> {
        let lower = self.lower.encode_value()?;
        let upper = self.upper.encode_value()?;
        Value::new(tag(
            RANGE_TAG,
            Raw::Array(vec![lower.0, upper.0, Raw::Bool(self.upper_inclusive)]),
        ))
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        let Raw::Tag(RANGE_TAG, payload) = value.raw() else {
            return Err(decode_type_mismatch(path));
        };
        let Raw::Array(fields) = payload.as_ref() else {
            return Err(decode_type_mismatch(path));
        };
        let [lower, upper, Raw::Bool(upper_inclusive)] = fields.as_slice() else {
            return Err(decode_type_mismatch(path));
        };

        Ok(Self {
            lower: decode_range_endpoint::<T>(lower, "lower", path)?,
            upper: decode_range_endpoint::<T>(upper, "upper", path)?,
            upper_inclusive: *upper_inclusive,
        })
    }
}

fn decode_range_endpoint<T: OvbCodec>(
    raw: &Raw,
    endpoint: &str,
    path: &mut Vec<String>,
) -> std::result::Result<Option<T>, DecodeError> {
    path.push(endpoint.to_owned());
    let result = Value::new(raw.clone())
        .map_err(|error| DecodeError::new(error, path.clone()))
        .and_then(|value| <Option<T> as OvbCodec>::decode_value(&value, path));
    path.pop();
    result
}

fn tuple_components<'a>(
    value: &'a Value,
    path: &[String],
) -> std::result::Result<&'a [Raw], DecodeError> {
    let Raw::Tag(TUPLE_TAG, payload) = value.raw() else {
        return Err(decode_type_mismatch(path));
    };
    let Raw::Array(components) = payload.as_ref() else {
        return Err(decode_type_mismatch(path));
    };
    Ok(components)
}

fn decode_tuple_element<T: OvbCodec>(
    raw: &Raw,
    index: usize,
    path: &mut Vec<String>,
) -> std::result::Result<T, DecodeError> {
    path.push(index.to_string());
    path.push(T::type_label().to_owned());
    let result = Value::new(raw.clone())
        .map_err(|error| DecodeError::new(error, path.clone()))
        .and_then(|value| T::decode_value(&value, path));
    path.pop();
    path.pop();
    result
}

impl OvbCodec for () {
    fn type_label() -> &'static str {
        "Unit"
    }

    fn encode_value(&self) -> Result<Value> {
        Ok(Value::unit())
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        let Raw::Tag(UNIT_TAG, payload) = value.raw() else {
            return Err(decode_type_mismatch(path));
        };
        let Raw::Array(components) = payload.as_ref() else {
            return Err(decode_type_mismatch(path));
        };
        if components.is_empty() {
            Ok(())
        } else {
            Err(decode_type_mismatch(path))
        }
    }
}

macro_rules! impl_tuple_codec {
    ($(($arity:literal; $($index:tt: $type:ident),+)),+ $(,)?) => {
        $(
            impl<$($type: OvbCodec),+> OvbCodec for ($($type,)+) {
                fn type_label() -> &'static str {
                    "Tuple"
                }

                fn encode_value(&self) -> Result<Value> {
                    let components = vec![
                        $(self.$index.encode_value()?.0,)+
                    ];
                    Value::new(tag(TUPLE_TAG, Raw::Array(components)))
                }

                fn decode_value(
                    value: &Value,
                    path: &mut Vec<String>,
                ) -> std::result::Result<Self, DecodeError> {
                    let components = tuple_components(value, path)?;
                    if components.len() != $arity {
                        return Err(decode_type_mismatch(path));
                    }
                    Ok((
                        $(decode_tuple_element::<$type>(&components[$index], $index, path)?,)+
                    ))
                }
            }
        )+
    };
}

impl_tuple_codec!(
    (1; 0: A),
    (2; 0: A, 1: B),
    (3; 0: A, 1: B, 2: C),
    (4; 0: A, 1: B, 2: C, 3: D),
    (5; 0: A, 1: B, 2: C, 3: D, 4: E),
    (6; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F),
    (7; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G),
    (8; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H),
    (9; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H, 8: I),
    (10; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H, 8: I, 9: J),
    (11; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H, 8: I, 9: J, 10: K),
    (12; 0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H, 8: I, 9: J, 10: K, 11: L),
);

/// A typed view of the portable OVB Error value (tag 60016).
///
/// This wrapper carries no execution or language-level error semantics.  It
/// only provides a typed construction and inspection boundary for the
/// canonical representation already accepted by [`Value`].
#[derive(Clone, Eq, PartialEq)]
pub struct ErrorValue(pub(crate) Value);

impl fmt::Debug for ErrorValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ErrorValue")
            .field("code", &"<redacted>")
            .field("message", &"<redacted>")
            .field("causes", &"<redacted>")
            .field("safe_details", &"<redacted>")
            .finish()
    }
}

impl ErrorValue {
    /// Constructs a canonical Error value from its portable fields.
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        causes: impl IntoIterator<Item = ErrorValue>,
        safe_details: BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut details: Vec<(Raw, Raw)> = safe_details
            .into_iter()
            .map(|(key, value)| (Raw::Text(key), value.0))
            .collect();
        details.sort_by(|(left, _), (right, _)| {
            encode_raw(left)
                .expect("text map keys are always encodable")
                .cmp(&encode_raw(right).expect("text map keys are always encodable"))
        });

        let raw = tag(
            ERROR_TAG,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text(code.into())),
                (Raw::Int(1.into()), Raw::Text(message.into())),
                (
                    Raw::Int(2.into()),
                    Raw::Array(causes.into_iter().map(|cause| cause.0.0).collect()),
                ),
                (Raw::Int(3.into()), Raw::Map(details)),
            ]),
        );
        Self::from_value(Value::new(raw)?)
    }

    /// Views a validated OVB value as a typed Error value.
    pub fn from_value(value: Value) -> Result<Self> {
        match value.raw() {
            Raw::Tag(ERROR_TAG, inner) => {
                validate_error(inner, 0)?;
                Ok(Self(value))
            }
            _ => Err(Error::InvalidTag),
        }
    }

    /// Decodes and validates a canonical OVB Error value.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(Value::decode(bytes)?)
    }

    pub fn value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.0.encode()
    }

    pub fn code(&self) -> &str {
        let fields = self.fields();
        text(fields[&0]).expect("validated Error code is text")
    }

    pub fn message(&self) -> &str {
        let fields = self.fields();
        text(fields[&1]).expect("validated Error message is text")
    }

    pub fn causes(&self) -> Vec<Self> {
        array(self.fields()[&2])
            .expect("validated Error causes are an array")
            .iter()
            .map(|cause| {
                Self::from_value(Value::new(cause.clone()).expect("validated cause"))
                    .expect("validated Error cause")
            })
            .collect()
    }

    pub fn safe_details(&self) -> BTreeMap<String, Value> {
        map(self.fields()[&3])
            .expect("validated Error details are a map")
            .iter()
            .map(|(key, value)| {
                (
                    text(key)
                        .expect("validated Error detail key is text")
                        .to_owned(),
                    Value::new(value.clone()).expect("validated Error detail value"),
                )
            })
            .collect()
    }

    /// Builds the fail-closed projection used by traces and wire-visible
    /// values. Error codes and `safe_details` are caller supplied here, so
    /// neither is treated as a disclosure grant without a classifier.
    pub fn redacted_for_trace(&self) -> Self {
        Self::new(
            "<redacted>",
            "<redacted>",
            self.causes().iter().map(Self::redacted_for_trace),
            BTreeMap::new(),
        )
        .expect("redacted Error remains canonical")
    }

    /// Encodes the redacted trace projection while `encode` continues to
    /// preserve this Error's local recovery identity.
    pub fn encode_for_trace(&self) -> Result<Vec<u8>> {
        self.redacted_for_trace().encode()
    }

    fn fields(&self) -> BTreeMap<u64, &Raw> {
        integer_map(self.inner(), &[0, 1, 2, 3], &[]).expect("ErrorValue invariant was violated")
    }

    fn inner(&self) -> &Raw {
        let Raw::Tag(ERROR_TAG, inner) = self.0.raw() else {
            unreachable!("ErrorValue invariant was violated")
        };
        inner
    }
}
