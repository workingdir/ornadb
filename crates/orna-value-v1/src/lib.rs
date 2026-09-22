//! The closed Orna 1.0 canonical-value boundary (OVB-1).
//!
//! This crate deliberately has no storage or runtime dependencies.  Values are
//! validated before they become digest input, and errors describe malformed
//! structure without echoing decoded payloads.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const OVB_VERSION: &str = "OVB-1";
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const MAX_DEPTH: usize = 128;
const MAX_ITEMS: usize = 1_000_000;

/// A deliberately payload-free failure.  Do not use `Debug` output of input
/// values in diagnostics or logs at this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    TrailingBytes,
    Limit,
    Unsupported,
    NonCanonical,
    InvalidUtf8,
    InvalidTag,
    InvalidValue,
    DuplicateOrUnorderedMapKey,
    ProtectedValue,
    InvalidPath,
    InvalidSchema,
    DecimalLimit,
    DivisionByZero,
    NonFiniteDecimal,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OVB-1 {self:?}")
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Canonical CBOR's supported data model. `Tag` is public for schema and
/// protocol adapters, but both construction and decoding validate the closed
/// Orna tag registry before bytes may be emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// A validated OVB-1 value.  The inner raw value is never exposed mutably.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Value(Raw);
impl Value {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
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
        Self::new(tag(60013, Raw::Array(x)))
    }
    pub fn unit() -> Self {
        Self(tag(60014, Raw::Array(vec![])))
    }
    pub fn uuid(bytes: [u8; 16]) -> Self {
        Self(tag(37, Raw::Bytes(bytes.to_vec())))
    }
    pub fn protected() -> Self {
        Self(Raw::Tag(0, Box::new(Raw::Null)))
    } // unencodable marker
}

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
    fn new(error: Error, path: Vec<String>) -> Self {
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
    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError>;
}

/// Encodes a typed value as canonical OVB-1 bytes.
pub fn encode_typed<T: OvbCodec>(value: &T) -> Result<Vec<u8>> {
    value.encode_value()?.encode()
}

/// Decodes canonical OVB-1 bytes as the requested type.
pub fn decode_typed<T: OvbCodec>(bytes: &[u8]) -> std::result::Result<T, DecodeError> {
    let value = Value::decode(bytes).map_err(|error| {
        DecodeError::new(error, vec![T::type_label().to_owned()])
    })?;
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

    fn decode_value(value: &Value, _path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, _path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
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
            Raw::Tag(37, payload) => match payload.as_ref() {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
        let Raw::Tag(60013, payload) = value.raw() else {
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
                    &Value::new(payload.clone()).map_err(|error| DecodeError::new(error, path.clone()))?,
                    path,
                );
                path.pop();
                result.map(Some)
            }
            _ => Err(decode_type_mismatch(path)),
        }
    }
}

fn tuple_components<'a>(value: &'a Value, path: &[String]) -> std::result::Result<&'a [Raw], DecodeError> {
    let Raw::Tag(60015, payload) = value.raw() else {
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

    fn decode_value(value: &Value, path: &mut Vec<String>) -> std::result::Result<Self, DecodeError> {
        let Raw::Tag(60014, payload) = value.raw() else {
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
                    Value::new(tag(60015, Raw::Array(components)))
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorValue(Value);

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
            60016,
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
            Raw::Tag(60016, inner) => {
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

    fn fields(&self) -> BTreeMap<u64, &Raw> {
        integer_map(self.inner(), &[0, 1, 2, 3], &[]).expect("ErrorValue invariant was violated")
    }

    fn inner(&self) -> &Raw {
        let Raw::Tag(60016, inner) = self.0.raw() else {
            unreachable!("ErrorValue invariant was violated")
        };
        inner
    }
}

fn tag(n: u64, raw: Raw) -> Raw {
    Raw::Tag(n, Box::new(raw))
}
fn is_nan_bits(bits: u64) -> bool {
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

/// Exact normalised decimal coefficient times 10 to its exponent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decimal {
    coefficient: BigInt,
    exponent10: BigInt,
}
impl Decimal {
    pub const MAX_ABS_EXPONENT: u64 = 1_000_000;
    fn new(coefficient: BigInt, exponent10: BigInt) -> Self {
        let (coefficient, exponent10) = normal_decimal(coefficient, exponent10);
        Self {
            coefficient,
            exponent10,
        }
    }
    pub fn try_new(coefficient: BigInt, exponent10: BigInt) -> Result<Self> {
        let decimal = Self::new(coefficient, exponent10);
        if decimal.exponent10.magnitude() > BigInt::from(Self::MAX_ABS_EXPONENT).magnitude() {
            return Err(Error::DecimalLimit);
        }
        Ok(decimal)
    }
    pub fn coefficient(&self) -> &BigInt {
        &self.coefficient
    }
    pub fn exponent10(&self) -> &BigInt {
        &self.exponent10
    }
    /// Renders this already-normalized Decimal in the canonical Orna text
    /// spelling. The representation stays exact and never passes through a
    /// binary floating-point value.
    pub fn canonical_text(&self) -> String {
        format!("{}e{}.decimal", self.coefficient, self.exponent10)
    }

    /// Parses only the canonical Orna Decimal text spelling.
    ///
    /// The parser intentionally rejects alternate spellings (including
    /// trailing coefficient zeroes, leading zeroes and a `+` exponent) so a
    /// textual value cannot acquire a second identity from the same exact
    /// decimal.
    pub fn from_canonical_text(text: &str) -> Result<Self> {
        let body = text.strip_suffix(".decimal").ok_or(Error::InvalidValue)?;
        let (coefficient_text, exponent_text) =
            body.split_once('e').ok_or(Error::InvalidValue)?;
        if coefficient_text.is_empty() || exponent_text.is_empty() {
            return Err(Error::InvalidValue);
        }

        let (negative, digits) = coefficient_text
            .strip_prefix('-')
            .map_or((false, coefficient_text), |digits| (true, digits));
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidValue);
        }
        if (digits.len() > 1 && digits.starts_with('0')) || (negative && digits == "0") {
            return Err(Error::NonCanonical);
        }
        let mut coefficient =
            BigInt::parse_bytes(digits.as_bytes(), 10).ok_or(Error::InvalidValue)?;
        if negative {
            coefficient = -coefficient;
        }

        let (negative_exponent, exponent_digits) = exponent_text
            .strip_prefix('-')
            .map_or((false, exponent_text), |digits| (true, digits));
        if exponent_digits.is_empty()
            || !exponent_digits
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(Error::InvalidValue);
        }
        if exponent_digits.len() > 1 && exponent_digits.starts_with('0') {
            return Err(Error::NonCanonical);
        }
        if exponent_digits.len() > 7 {
            return Err(Error::DecimalLimit);
        }
        let mut exponent =
            BigInt::parse_bytes(exponent_digits.as_bytes(), 10).ok_or(Error::InvalidValue)?;
        if negative_exponent {
            if exponent.is_zero() {
                return Err(Error::NonCanonical);
            }
            exponent = -exponent;
        }

        let decimal = Self::try_new(coefficient, exponent)?;
        if decimal.canonical_text() != text {
            return Err(Error::NonCanonical);
        }
        Ok(decimal)
    }
    fn raw(&self) -> Raw {
        tag(
            60000,
            Raw::Array(vec![
                Raw::Int(self.coefficient.clone()),
                Raw::Int(self.exponent10.clone()),
            ]),
        )
    }
    fn from_raw(raw: &Raw) -> Result<Self> {
        let Raw::Tag(60000, inner) = raw else {
            return Err(Error::InvalidTag);
        };
        let fields = array(inner)?;
        if fields.len() != 2 {
            return Err(Error::InvalidTag);
        }
        let coefficient = integer(&fields[0])?.clone();
        let exponent10 = integer(&fields[1])?.clone();
        let decimal = Self::try_new(coefficient, exponent10)?;
        if decimal.raw() != *raw {
            return Err(Error::NonCanonical);
        }
        Ok(decimal)
    }
    pub fn try_multiply(&self, other: &Self) -> Result<Self> {
        Self::try_new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }
    fn try_add_parts(&self, other_coefficient: &BigInt, other_exponent: &BigInt) -> Result<Self> {
        let e = if self.exponent10 < *other_exponent {
            self.exponent10.clone()
        } else {
            other_exponent.clone()
        };
        let shift_a = &self.exponent10 - &e;
        let shift_b = other_exponent - &e;
        if shift_a > BigInt::from(Self::MAX_ABS_EXPONENT)
            || shift_b > BigInt::from(Self::MAX_ABS_EXPONENT)
        {
            return Err(Error::DecimalLimit);
        }
        let shift_a = bounded_exponent(&shift_a)?;
        let shift_b = bounded_exponent(&shift_b)?;
        Self::try_new(
            &self.coefficient * pow10(shift_a) + other_coefficient * pow10(shift_b),
            e,
        )
    }
    pub fn try_add(&self, other: &Self) -> Result<Self> {
        self.try_add_parts(&other.coefficient, &other.exponent10)
    }
    /// Exact finite decimal subtraction. Resource limits fail closed without
    /// rounding or converting through binary floating point.
    pub fn try_subtract(&self, other: &Self) -> Result<Self> {
        let negated_coefficient = -&other.coefficient;
        self.try_add_parts(&negated_coefficient, &other.exponent10)
    }
    /// Exact finite decimal division.  A denominator with primes other than 2
    /// and 5 has no finite base-10 result and is rejected without rounding.
    pub fn divide_exact(&self, other: &Self) -> Result<Self> {
        if other.coefficient.is_zero() {
            return Err(Error::DivisionByZero);
        }
        if self.coefficient.is_zero() {
            return Ok(Self::new(BigInt::zero(), BigInt::zero()));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let mut twos = 0u64;
        let mut fives = 0u64;
        while (&denominator % BigInt::from(2u8)).is_zero() {
            denominator /= 2u8;
            twos += 1;
        }
        while (&denominator % BigInt::from(5u8)).is_zero() {
            denominator /= 5u8;
            fives += 1;
        }
        if denominator != BigInt::from(1) {
            return Err(Error::NonFiniteDecimal);
        }
        let scale = twos.max(fives);
        if scale > Self::MAX_ABS_EXPONENT {
            return Err(Error::DecimalLimit);
        }
        numerator *= BigInt::from(2u8).pow((scale - twos) as u32);
        numerator *= BigInt::from(5u8).pow((scale - fives) as u32);
        Self::try_new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }
}
impl OvbCodec for Decimal {
    fn type_label() -> &'static str {
        "Decimal"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(self.raw())
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        Decimal::from_raw(value.raw())
            .map_err(|error| DecodeError::new(error, path.clone()))
    }
}


/// An exact amount paired with its opaque nominal currency object identity.
///
/// The currency ID is a witness at the value boundary: it is carried in the
/// canonical tag-60007 payload and is never reduced to a display code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Money {
    amount: Decimal,
    currency: [u8; 16],
    value: Value,
}

impl Money {
    pub fn new(amount: Decimal, currency: [u8; 16]) -> Result<Self> {
        let value = Value::new(tag(
            60007,
            Raw::Array(vec![amount.raw(), uuid_raw(currency)]),
        ))?;
        Ok(Self {
            amount,
            currency,
            value,
        })
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let Raw::Tag(60007, inner) = value.raw() else {
            return Err(Error::InvalidTag);
        };
        let fields = array(inner)?;
        if fields.len() != 2 {
            return Err(Error::InvalidTag);
        }
        let amount = Decimal::from_raw(&fields[0])?;
        let currency = uuid_array(&fields[1])?;
        Ok(Self {
            amount,
            currency,
            value,
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(Value::decode(bytes)?)
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn amount(&self) -> &Decimal {
        &self.amount
    }

    pub fn currency(&self) -> [u8; 16] {
        self.currency
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.value.encode()
    }
}
fn bounded_exponent(n: &BigInt) -> Result<u64> {
    if n.sign() == Sign::Minus {
        return Err(Error::DecimalLimit);
    }
    let limbs = n.to_u64_digits().1;
    match limbs.as_slice() {
        [] => Ok(0),
        [value] if *value <= Decimal::MAX_ABS_EXPONENT => Ok(*value),
        _ => Err(Error::DecimalLimit),
    }
}
fn pow10(n: u64) -> BigInt {
    BigInt::from(10u8).pow(n as u32)
}
fn normal_decimal(mut c: BigInt, mut e: BigInt) -> (BigInt, BigInt) {
    if c.is_zero() {
        return (BigInt::zero(), BigInt::zero());
    }
    let ten = BigInt::from(10);
    while (&c % &ten).is_zero() {
        c /= &ten;
        e += 1;
    }
    (c, e)
}

/// A snapshot pin, never an implicit reference to current CWD.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Snapshot {
    Cwd {
        database: [u8; 16],
        runtime: [u8; 16],
        generation: BigInt,
        id: [u8; 32],
    },
    Commit {
        database: [u8; 16],
        algorithm: GitHash,
        oid: Vec<u8>,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHash {
    Sha1,
    Sha256,
}
impl Snapshot {
    pub fn cwd(database: [u8; 16], runtime: [u8; 16], generation: BigInt) -> Result<Self> {
        if generation.sign() == Sign::Minus {
            return Err(Error::InvalidValue);
        }
        let core = Raw::Array(vec![
            Raw::Int(0.into()),
            uuid_raw(database),
            uuid_raw(runtime),
            Raw::Int(generation.clone()),
        ]);
        let id = domain_digest("orna.snapshot.v1", &core)?;
        Ok(Self::Cwd {
            database,
            runtime,
            generation,
            id,
        })
    }
    pub fn raw(&self) -> Raw {
        match self {
            Self::Cwd {
                database,
                runtime,
                generation,
                id,
            } => Raw::Array(vec![
                Raw::Int(0.into()),
                uuid_raw(*database),
                uuid_raw(*runtime),
                Raw::Int(generation.clone()),
                Raw::Bytes(id.to_vec()),
            ]),
            Self::Commit {
                database,
                algorithm,
                oid,
            } => Raw::Array(vec![
                Raw::Int(1.into()),
                uuid_raw(*database),
                Raw::Text(
                    match algorithm {
                        GitHash::Sha1 => "sha1",
                        GitHash::Sha256 => "sha256",
                    }
                    .into(),
                ),
                Raw::Bytes(oid.clone()),
            ]),
        }
    }
    pub fn decode(raw: &Raw) -> Result<Self> {
        let a = array(raw)?;
        match int_u64(a.first().ok_or(Error::InvalidValue)?)? {
            0 => {
                if a.len() != 5 {
                    return Err(Error::InvalidValue);
                }
                let db = uuid_array(&a[1])?;
                let rt = uuid_array(&a[2])?;
                let generation = integer(&a[3])?.clone();
                if generation.sign() == Sign::Minus {
                    return Err(Error::InvalidValue);
                };
                let _id = bytes32(&a[4])?;
                let expected = Self::cwd(db, rt, generation.clone())?;
                if expected.raw() != *raw {
                    return Err(Error::NonCanonical);
                };
                Ok(expected)
            }
            1 => {
                if a.len() != 4 {
                    return Err(Error::InvalidValue);
                }
                let database = uuid_array(&a[1])?;
                let algorithm = match text(&a[2])? {
                    "sha1" => GitHash::Sha1,
                    "sha256" => GitHash::Sha256,
                    _ => return Err(Error::InvalidValue),
                };
                let oid = bytes(&a[3])?.to_vec();
                if oid.len()
                    != match algorithm {
                        GitHash::Sha1 => 20,
                        GitHash::Sha256 => 32,
                    }
                {
                    return Err(Error::InvalidValue);
                };
                Ok(Self::Commit {
                    database,
                    algorithm,
                    oid,
                })
            }
            _ => Err(Error::InvalidValue),
        }
    }
}
fn uuid_raw(bytes: [u8; 16]) -> Raw {
    tag(37, Raw::Bytes(bytes.to_vec()))
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
pub fn base64_encode(bytes: &[u8]) -> Result<String> {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let groups = bytes
        .len()
        .checked_add(2)
        .ok_or(Error::Limit)?
        / 3;
    let capacity = groups.checked_mul(4).ok_or(Error::Limit)?;
    let mut output = String::with_capacity(capacity);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[((first & 0x03) << 4 | second.unwrap_or(0) >> 4) as usize] as char);
        output.push(match second {
            Some(second) => ALPHABET[((second & 0x0f) << 2 | third.unwrap_or(0) >> 6) as usize]
                as char,
            None => '=',
        });
        output.push(match third {
            Some(third) => ALPHABET[(third & 0x3f) as usize] as char,
            None => '=',
        });
    }
    Ok(output)
}

/// Decodes strict RFC 4648 standard Base64.
///
/// Whitespace, URL-safe aliases, malformed padding and nonzero unused
/// trailing bits are rejected before any bytes are returned.
pub fn base64_decode(input: &str) -> Result<Vec<u8>> {
    if input.len() % 4 != 0 {
        return Err(Error::InvalidValue);
    }
    let capacity = (input.len() / 4)
        .checked_mul(3)
        .ok_or(Error::Limit)?;
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

fn uuid_array(r: &Raw) -> Result<[u8; 16]> {
    let Raw::Tag(37, v) = r else {
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
fn bytes32(r: &Raw) -> Result<[u8; 32]> {
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
    domain_digest(
        "orna.arguments.v1",
        &Raw::Array(
            a.into_iter()
                .map(|(n, t, v)| Raw::Array(vec![Raw::Text(n), t, v]))
                .collect(),
        ),
    )
}

/// The closed descriptor map defined in Format §29.  It is retained as a
/// checked raw representation to avoid accidentally dropping unknown details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaDescriptor(Raw);
impl SchemaDescriptor {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        validate_schema(&raw)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v = Value::decode(bytes)?;
        Self::new(v.0)
    }
}

/// Repository key path component codec (ORNA-PATH-001 through 012).
pub fn path_encode_component(text: &str) -> Result<String> {
    if text.is_empty() {
        return Ok("~ff".into());
    }
    let raw = text.as_bytes();
    let reserved = reserved_name(text);
    let trailing = raw.iter().rposition(|b| *b != b'.').map_or(0, |x| x + 1);
    let mut out = String::new();
    for (i, b) in raw.iter().enumerate() {
        let force = (reserved && i == 0) || (*b == b'.' && i >= trailing);
        if !force && matches!(*b,b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
            out.push(*b as char)
        } else {
            out.push_str(&format!("~{b:02x}"))
        }
    }
    if out.len() > 200 {
        return Err(Error::InvalidPath);
    }
    Ok(out)
}
pub fn path_decode_component(encoded: &str) -> Result<String> {
    if !encoded.is_ascii() || encoded.len() > 200 {
        return Err(Error::InvalidPath);
    }
    if encoded == "~ff" {
        return Ok(String::new());
    }
    if encoded.contains("~ff") {
        return Err(Error::InvalidPath);
    }
    let mut o = Vec::new();
    let b = encoded.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'~' {
            if i + 2 >= b.len() {
                return Err(Error::InvalidPath);
            }
            let h = std::str::from_utf8(&b[i + 1..i + 3]).map_err(|_| Error::InvalidPath)?;
            if !h
                .bytes()
                .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
            {
                return Err(Error::InvalidPath);
            };
            o.push(u8::from_str_radix(h, 16).map_err(|_| Error::InvalidPath)?);
            i += 3
        } else {
            if !matches!(b[i],b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
                return Err(Error::InvalidPath);
            };
            o.push(b[i]);
            i += 1
        }
    }
    let s = String::from_utf8(o).map_err(|_| Error::InvalidPath)?;
    if path_encode_component(&s)? != encoded {
        return Err(Error::NonCanonical);
    }
    Ok(s)
}
pub fn path_collision_key(encoded: &str) -> String {
    encoded
        .bytes()
        .map(|x| {
            if x.is_ascii_uppercase() {
                (x + 32) as char
            } else {
                x as char
            }
        })
        .collect()
}
/// Encodes a composite editable-row key.  Only the final component has the
/// `.orna` extension; the resulting repository-relative path is bounded.
pub fn path_encode_key_components(components: &[String]) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::InvalidPath);
    }
    let mut output: Vec<String> = components
        .iter()
        .map(|component| path_encode_component(component))
        .collect::<Result<_>>()?;
    let last = output.last_mut().ok_or(Error::InvalidPath)?;
    last.push_str(".orna");
    if output.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(output)
}
/// Decodes table-relative editable-row path components into canonical key text
/// in declared component order. The owning schema must still interpret that
/// text as typed keys. Filesystem containment and sibling collisions remain
/// the responsibility of the repository adapter.
pub fn path_decode_key_components(components: &[String]) -> Result<Vec<String>> {
    let (last, parents) = components.split_last().ok_or(Error::InvalidPath)?;
    let mut length = 0usize;
    for (index, component) in components.iter().enumerate() {
        length = length
            .checked_add(component.len())
            .and_then(|sum| sum.checked_add(usize::from(index != 0)))
            .filter(|sum| *sum <= 1024)
            .ok_or(Error::InvalidPath)?;
    }
    path_validate_relative_components(components)?;
    let mut decoded = parents
        .iter()
        .map(|component| path_decode_component(component))
        .collect::<Result<Vec<_>>>()?;
    decoded.push(path_decode_component(
        last.strip_suffix(".orna").ok_or(Error::InvalidPath)?,
    )?);
    Ok(decoded)
}
/// Rejects path traversal syntax before a repository layer performs a
/// symlink/reparse-point-aware containment check at materialisation time.
pub fn path_validate_relative_components(components: &[String]) -> Result<()> {
    if components.is_empty()
        || components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('/')
                || component.contains('\\')
        })
    {
        return Err(Error::InvalidPath);
    }
    if components.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(())
}
fn reserved_name(s: &str) -> bool {
    if s == "." || s == ".." {
        return true;
    }
    let t = s.trim_end_matches([' ', '.']);
    let lower = t.to_ascii_lowercase();
    if lower == ".git" {
        return true;
    }
    let first = lower.split('.').next().unwrap_or("");
    matches!(
        first,
        "con" | "prn" | "aux" | "nul" | "clock$" | "conin$" | "conout$"
    ) || (first.len() == 4
        && ((first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9')))
}

fn encode_raw(v: &Raw) -> Result<Vec<u8>> {
    validate_raw(v, 0)?;
    let mut out = Vec::new();
    write_raw(v, &mut out)?;
    Ok(out)
}
fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    if n < 24 {
        out.push(major << 5 | n as u8)
    } else if n <= u8::MAX as u64 {
        out.extend([major << 5 | 24, n as u8])
    } else if n <= u16::MAX as u64 {
        out.push(major << 5 | 25);
        out.extend((n as u16).to_be_bytes())
    } else if n <= u32::MAX as u64 {
        out.push(major << 5 | 26);
        out.extend((n as u32).to_be_bytes())
    } else {
        out.push(major << 5 | 27);
        out.extend(n.to_be_bytes())
    }
}
fn write_raw(v: &Raw, out: &mut Vec<u8>) -> Result<()> {
    match v {
        Raw::Null => {
            out.push(0xf6);
        }
        Raw::Bool(false) => {
            out.push(0xf4);
        }
        Raw::Bool(true) => {
            out.push(0xf5);
        }
        Raw::Int(i) => {
            let neg = i.sign() == Sign::Minus;
            let n = if neg { -i - BigInt::from(1) } else { i.clone() };
            if n.is_zero() {
                head(out, if neg { 1 } else { 0 }, 0);
            } else if n.to_u64_digits().1.len() == 1 {
                let limb = n.to_u64_digits().1[0];
                head(out, if neg { 1 } else { 0 }, limb);
            } else {
                let (_, b) = n.to_bytes_be();
                head(out, 6, if neg { 3 } else { 2 });
                head(out, 2, b.len() as u64);
                out.extend(b)
            }
        }
        Raw::Float(bits) => {
            out.push(0xfb);
            out.extend(
                (if is_nan_bits(*bits) {
                    CANONICAL_NAN_BITS
                } else {
                    *bits
                })
                .to_be_bytes(),
            )
        }
        Raw::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend(b)
        }
        Raw::Text(s) => {
            head(out, 3, s.len() as u64);
            out.extend(s.as_bytes())
        }
        Raw::Array(a) => {
            head(out, 4, a.len() as u64);
            for x in a {
                write_raw(x, out)?
            }
        }
        Raw::Map(m) => {
            let mut items = Vec::with_capacity(m.len());
            for (k, v) in m {
                items.push((encode_raw(k)?, v))
            }
            items.sort_by(|a, b| a.0.cmp(&b.0));
            if items.windows(2).any(|x| x[0].0 == x[1].0) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
            head(out, 5, items.len() as u64);
            for (k, v) in items {
                out.extend(k);
                write_raw(v, out)?
            }
        }
        Raw::Tag(n, v) => {
            head(out, 6, *n);
            write_raw(v, out)?
        }
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            items: 0,
        }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let e = self.at.checked_add(n).ok_or(Error::Limit)?;
        let x = self.bytes.get(self.at..e).ok_or(Error::Truncated)?;
        self.at = e;
        Ok(x)
    }
    fn arg(&mut self, ai: u8) -> Result<u64> {
        let (n, w) = match ai {
            0..=23 => return Ok(ai as u64),
            24 => (24, 1),
            25 => (256, 2),
            26 => (65536, 4),
            27 => (4294967296, 8),
            _ => return Err(Error::Unsupported),
        };
        let mut x = 0;
        for b in self.take(w)? {
            x = x << 8 | *b as u64
        }
        if x < n {
            return Err(Error::NonCanonical);
        }
        Ok(x)
    }
    fn raw(&mut self, depth: usize) -> Result<Raw> {
        if depth > MAX_DEPTH || self.items >= MAX_ITEMS {
            return Err(Error::Limit);
        }
        self.items += 1;
        let first = self.take(1)?[0];
        let major = first >> 5;
        let ai = first & 31;
        if major == 7 {
            return match ai {
                20 => Ok(Raw::Bool(false)),
                21 => Ok(Raw::Bool(true)),
                22 => Ok(Raw::Null),
                27 => {
                    let mut a = [0; 8];
                    a.copy_from_slice(self.take(8)?);
                    let bits = u64::from_be_bytes(a);
                    if is_nan_bits(bits) && bits != CANONICAL_NAN_BITS {
                        return Err(Error::NonCanonical);
                    }
                    Ok(Raw::Float(bits))
                }
                _ => Err(Error::Unsupported),
            };
        }
        let n = self.arg(ai)?;
        match major {
            0 => Ok(Raw::Int(BigInt::from(n))),
            1 => Ok(Raw::Int(-BigInt::from(n) - 1)),
            2 => Ok(Raw::Bytes(self.take(n as usize)?.to_vec())),
            3 => Ok(Raw::Text(
                String::from_utf8(self.take(n as usize)?.to_vec())
                    .map_err(|_| Error::InvalidUtf8)?,
            )),
            4 => {
                let mut a = Vec::new();
                for _ in 0..n {
                    a.push(self.raw(depth + 1)?)
                }
                Ok(Raw::Array(a))
            }
            5 => {
                let mut m = Vec::new();
                let mut last = None;
                for _ in 0..n {
                    let start = self.at;
                    let k = self.raw(depth + 1)?;
                    let kb = &self.bytes[start..self.at];
                    if last.as_ref().is_some_and(|x: &Vec<u8>| kb <= x.as_slice()) {
                        return Err(Error::DuplicateOrUnorderedMapKey);
                    }
                    last = Some(kb.to_vec());
                    let v = self.raw(depth + 1)?;
                    m.push((k, v))
                }
                Ok(Raw::Map(m))
            }
            6 => {
                let v = self.raw(depth + 1)?;
                if n == 2 || n == 3 {
                    let b = bytes(&v)?;
                    if b.is_empty() || b[0] == 0 || b.len() < 9 {
                        return Err(Error::NonCanonical);
                    }
                    let i = BigInt::from_bytes_be(Sign::Plus, b);
                    return Ok(Raw::Int(if n == 2 { i } else { -i - 1 }));
                }
                Ok(tag(n, v))
            }
            _ => Err(Error::Unsupported),
        }
    }
}

fn validate_raw(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    match v {
        Raw::Tag(0, _) => return Err(Error::ProtectedValue),
        Raw::Float(bits) if is_nan_bits(*bits) && *bits != CANONICAL_NAN_BITS => {
            return Err(Error::NonCanonical);
        }
        Raw::Array(a) => {
            for x in a {
                validate_raw(x, depth + 1)?
            }
        }
        Raw::Map(m) => {
            let mut previous = None;
            for (k, v) in m {
                validate_raw(k, depth + 1)?;
                validate_raw(v, depth + 1)?;
                let b = encode_raw(k)?;
                if previous.as_ref().is_some_and(|x: &Vec<u8>| b <= *x) {
                    return Err(Error::DuplicateOrUnorderedMapKey);
                }
                previous = Some(b)
            }
        }
        Raw::Tag(n, x) => {
            validate_tag(*n, x)?;
            validate_raw(x, depth + 1)?
        }
        _ => {}
    }
    Ok(())
}
fn validate_tag(n: u64, v: &Raw) -> Result<()> {
    match n {
        37 => {
            if bytes(v)?.len() != 16 {
                return Err(Error::InvalidTag);
            }
        }
        60000 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let (c, e) = (integer(&a[0])?, integer(&a[1])?);
            let decimal = Decimal::try_new(c.clone(), e.clone())?;
            if decimal.coefficient() != c || decimal.exponent10() != e {
                return Err(Error::NonCanonical);
            }
        }
        60001 => {
            validate_date(text(v)?)?;
        }
        60003 => {
            let s = text(v)?;
            validate_local_date_time(s)?;
        }
        60004 => {
            let s = text(v)?;
            if s.is_empty()
                || s.as_bytes()
                    .iter()
                    .any(|b| !matches!(*b, b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'_'|b'-'|b'+'|b'/'))
            {
                return Err(Error::InvalidTag);
            }
        }
        60002 | 60005 => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Int(_)) || int_u64(&a[1])? >= 1_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        60006 => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Int(_) | Raw::Float(_) | Raw::Tag(60000, _)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        60007 => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Tag(60000, _)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        60008 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            if !matches!(a[2], Raw::Null | Raw::Tag(60009, _)) {
                return Err(Error::InvalidTag);
            }
        }
        60009 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Null | Raw::Tag(37, _)) {
                return Err(Error::InvalidTag);
            }
            validate_fields(&a[1])?;
        }
        60010 => {
            let a = array(v)?;
            if a.len() != 4 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            Snapshot::decode(&a[3])?;
        }
        60013 => {
            let a = array(v)?;
            match a.as_slice() {
                [Raw::Int(x)] if x.is_zero() => {}
                [Raw::Int(x), _] if *x == BigInt::from(1) => {}
                _ => return Err(Error::InvalidTag),
            }
        }
        60015 => {
            if array(v)?.is_empty() {
                return Err(Error::NonCanonical);
            }
        }
        60018 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Tag(60002, _)) || !matches!(a[1], Raw::Tag(60004, _)) {
                return Err(Error::InvalidTag);
            }
            let offset = integer(&a[2])?;
            // Exact zone-database agreement is contextual: this pure codec can
            // validate the pinned shape and legal offset range only. The
            // snapshot's recorded tzdata resolver must confirm the offset.
            if offset < &BigInt::from(-64_800) || offset > &BigInt::from(64_800) {
                return Err(Error::InvalidTag);
            }
        }
        60019 => {
            let a = array(v)?;
            if a.len() != 3
                || !matches!(a[0], Raw::Tag(60013, _))
                || !matches!(a[1], Raw::Tag(60013, _))
                || !matches!(a[2], Raw::Bool(_))
            {
                return Err(Error::InvalidTag);
            }
        }
        60020 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
        }
        60021 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
        }
        60022 => {
            let a = array(v)?;
            if a.len() != 4 || !matches!(a[3], Raw::Bool(false)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
        }
        60023 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let name = text(&a[0])?;
            match name {
                value if UUID_SYSTEM_IDS.contains(&value) => {
                    uuid_array(&a[1])?;
                }
                "sys.RevisionId" | "sys.SnapshotId" | "sys.ConsumerIdentity" => {
                    if bytes(&a[1])?.len() != 32 {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.CheckpointVersion" | "sys.FailureVersion" => {
                    if integer(&a[1])?.sign() == Sign::Minus {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.GitOid" => {
                    let git = array(&a[1])?;
                    if git.len() != 2 {
                        return Err(Error::InvalidTag);
                    }
                    let size = match text(&git[0])? {
                        "sha1" => 20,
                        "sha256" => 32,
                        _ => return Err(Error::InvalidTag),
                    };
                    if bytes(&git[1])?.len() != size {
                        return Err(Error::InvalidTag);
                    }
                }
                _ => return Err(Error::InvalidTag),
            }
        }
        60024 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let flavour = text(&a[0])?;
            let value = text(&a[1])?;
            if !matches!(flavour, "repo" | "posix" | "windows")
                || (flavour == "repo"
                    && (!value.is_ascii()
                        || value.starts_with('/')
                        || value
                            .split('/')
                            .any(|x| x.is_empty() || x == "." || x == "..")))
            {
                return Err(Error::InvalidTag);
            }
        }
        60025 => {
            let a = array(v)?;
            if a.len() != 2 || text(&a[0])? != "sha256" || bytes(&a[1])?.len() != 32 {
                return Err(Error::InvalidTag);
            }
        }
        60026 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            validate_type(&a[0])?;
            validate_value_as_type(&a[1], &a[0])?;
        }
        60027 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let ordered = match a[0] {
                Raw::Bool(value) => value,
                _ => return Err(Error::InvalidTag),
            };
            let rows = array(&a[1])?;
            if !ordered {
                let mut previous = None;
                for row in rows {
                    let encoded = encode_raw(row)?;
                    if previous.as_ref().is_some_and(|x: &Vec<u8>| encoded < *x) {
                        return Err(Error::NonCanonical);
                    }
                    previous = Some(encoded);
                }
            }
        }
        60011 => validate_diagnostic(v, 0)?,
        60012 => validate_present(v, 0)?,
        60016 => validate_error(v, 0)?,
        60014 => {
            if !array(v)?.is_empty() {
                return Err(Error::InvalidTag);
            }
        }
        60017 => {
            if int_u64(v)? >= 86_400_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        _ => return Err(Error::InvalidTag),
    }
    Ok(())
}
fn integer_map<'a>(
    v: &'a Raw,
    required: &[u64],
    optional: &[u64],
) -> Result<BTreeMap<u64, &'a Raw>> {
    let mut result = BTreeMap::new();
    for (key, value) in map(v)? {
        let key = int_u64(key)?;
        if !required.contains(&key) && !optional.contains(&key)
            || result.insert(key, value).is_some()
        {
            return Err(Error::InvalidTag);
        }
    }
    if required.iter().any(|key| !result.contains_key(key)) {
        return Err(Error::InvalidTag);
    }
    Ok(result)
}
fn validate_diagnostic(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3, 4, 5, 6], &[7])?;
    text(fields[&0])?;
    if int_u64(fields[&1])? > 4 {
        return Err(Error::InvalidTag);
    }
    text(fields[&2])?;
    for span in array(fields[&3])? {
        let span = array(span)?;
        if span.len() != 4 {
            return Err(Error::InvalidTag);
        }
        Snapshot::decode(&span[0])?;
        let path = text(&span[1])?;
        if !is_safe_diagnostic_path(path) {
            return Err(Error::InvalidTag);
        }
        let start = int_u64(&span[2])?;
        if int_u64(&span[3])? < start {
            return Err(Error::InvalidTag);
        }
    }
    for note in array(fields[&4])? {
        text(note)?;
    }
    for cause in array(fields[&5])? {
        if let Raw::Tag(60011, inner) = cause {
            validate_diagnostic(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    if !matches!(fields[&6], Raw::Bool(_)) {
        return Err(Error::InvalidTag);
    }
    if let Some(id) = fields.get(&7) {
        uuid_array(id)?;
    }
    Ok(())
}

/// Returns whether a diagnostic span path is safe outside trusted developer
/// inspection. The protocol permits repository-relative UTF-8 paths or the
/// explicit redaction marker, never host-specific path syntax.
fn is_safe_diagnostic_path(path: &str) -> bool {
    if path == "<redacted>" {
        return true;
    }

    let bytes = path.as_bytes();
    if bytes.is_empty()
        || bytes[0] == b'/'
        || bytes.contains(&b'\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.chars().any(char::is_control)
    {
        return false;
    }

    !path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
}
fn validate_error(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3], &[])?;
    text(fields[&0])?;
    text(fields[&1])?;
    for cause in array(fields[&2])? {
        if let Raw::Tag(60016, inner) = cause {
            validate_error(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    for (key, value) in map(fields[&3])? {
        text(key)?;
        validate_raw(value, depth + 1)?;
    }
    Ok(())
}
fn validate_present(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let a = array(v)?;
    if a.len() != 4 {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[0], Raw::Text(_) | Raw::Tag(37, _)) {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[1], Raw::Null) {
        let key = array(&a[1])?;
        match key.as_slice() {
            [Raw::Int(kind), Raw::Text(_) | Raw::Tag(37, _)] if *kind == BigInt::from(0) => {}
            [Raw::Int(kind), table, _] if *kind == BigInt::from(1) => {
                let _ = uuid_array(table)?;
            }
            [Raw::Int(kind), _] if *kind == BigInt::from(3) => {}
            _ => return Err(Error::InvalidTag),
        }
    }
    for (key, value) in map(&a[2])? {
        if !matches!(key, Raw::Text(_) | Raw::Tag(37, _)) {
            return Err(Error::InvalidTag);
        }
        validate_raw(value, depth + 1)?;
    }
    let mut keys = Vec::new();
    for child in array(&a[3])? {
        let Raw::Tag(60012, node) = child else {
            return Err(Error::InvalidTag);
        };
        validate_present(node, depth + 1)?;
        let child_fields = array(node)?;
        if !matches!(child_fields[1], Raw::Null) {
            let k = encode_raw(&child_fields[1])?;
            if keys.contains(&k) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
            keys.push(k)
        }
    }
    Ok(())
}
const UUID_SYSTEM_IDS: [&str; 15] = [
    "sys.DatabaseId",
    "sys.FileId",
    "sys.DefinitionId",
    "sys.ObjectId",
    "sys.RuntimeId",
    "sys.TransactionId",
    "sys.InvocationId",
    "sys.RunId",
    "sys.QueryId",
    "sys.SessionId",
    "sys.ClientId",
    "sys.TraceId",
    "sys.SpanId",
    "sys.SegmentId",
    "sys.BuildId",
];
fn validate_fields(v: &Raw) -> Result<()> {
    let fields = array(v)?;
    let mut prior = None;
    for f in fields {
        let p = array(f)?;
        if p.len() != 2 {
            return Err(Error::InvalidTag);
        }
        match &p[0] {
            Raw::Text(name) if name.nfc().eq(name.chars()) => {}
            Raw::Tag(37, _) => {
                uuid_array(&p[0])?;
            }
            _ => return Err(Error::InvalidTag),
        }
        let k = encode_raw(&p[0])?;
        if prior.as_ref().is_some_and(|x: &Vec<u8>| k <= *x) {
            return Err(Error::DuplicateOrUnorderedMapKey);
        }
        prior = Some(k);
    }
    Ok(())
}

#[cfg(test)]
mod field_key_tests {
    use super::*;

    #[test]
    fn records_reject_noncanonical_field_key_kinds_and_accept_text_or_uuid() {
        let record = |key: Raw| {
            tag(
                60009,
                Raw::Array(vec![
                    Raw::Null,
                    Raw::Array(vec![Raw::Array(vec![key, Raw::Int(1.into())])]),
                ]),
            )
        };

        for key in [
            Raw::Int(1.into()),
            Raw::Bool(false),
            Raw::Bytes(vec![1]),
            Raw::Text("e\u{301}".into()),
        ] {
            let raw = record(key);
            assert_eq!(Value::new(raw.clone()), Err(Error::InvalidTag));
            let mut bytes = Vec::new();
            write_raw(&raw, &mut bytes).unwrap();
            assert_eq!(Value::decode(&bytes), Err(Error::InvalidTag));
        }

        assert!(Value::new(record(Raw::Text("name".into()))).is_ok());
        assert!(Value::new(record(uuid_raw([7; 16]))).is_ok());
    }
}
fn validate_date(s: &str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, x)| matches!(i, 4 | 7) || x.is_ascii_digit())
    {
        return Err(Error::InvalidTag);
    }
    let y = s[..4].parse::<i32>().map_err(|_| Error::InvalidTag)?;
    if !(1..=9999).contains(&y) {
        return Err(Error::InvalidTag);
    }
    let m = s[5..7].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let d = s[8..].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => return Err(Error::InvalidTag),
    };
    if d == 0 || d > max {
        return Err(Error::InvalidTag);
    }
    Ok(())
}
fn validate_local_date_time(s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() != 29
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
    {
        return Err(Error::InvalidTag);
    }
    validate_date(&s[..10])?;
    let hour = s[11..13].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let minute = s[14..16].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let second = s[17..19].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Error::InvalidTag);
    }
    Ok(())
}
fn array(v: &Raw) -> Result<&Vec<Raw>> {
    if let Raw::Array(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn map(v: &Raw) -> Result<&Vec<(Raw, Raw)>> {
    if let Raw::Map(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn bytes(v: &Raw) -> Result<&[u8]> {
    if let Raw::Bytes(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn text(v: &Raw) -> Result<&str> {
    if let Raw::Text(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn integer(v: &Raw) -> Result<&BigInt> {
    if let Raw::Int(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn int_u64(v: &Raw) -> Result<u64> {
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

fn validate_schema(raw: &Raw) -> Result<()> {
    let m = map(raw)?;
    if m.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    let mut got = BTreeMap::new();
    for (k, v) in m {
        got.insert(int_u64(k)?, v);
    }
    if got.keys().copied().collect::<Vec<_>>() != vec![0, 1, 2, 3, 4] || int_u64(got[&0])? != 1 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(got[&1])?;
    let keys = array(got[&2])?;
    let fields = array(got[&3])?;
    let defs = array(got[&4])?;
    let mut ids = Vec::new();
    let mut key_role_ids = Vec::new();
    for f in fields {
        let a = array(f)?;
        if a.len() != 5 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        ids.push(id);
        text(&a[1])?;
        validate_type(&a[2])?;
        let role = int_u64(&a[3])?;
        if role > 2 {
            return Err(Error::InvalidSchema);
        }
        let fallback = array(&a[4])?;
        match fallback.as_slice() {
            [Raw::Int(kind)] if kind.is_zero() => {}
            [Raw::Int(kind), value] if *kind == BigInt::from(1) && role != 2 => {
                validate_value_as_type(value, &a[2]).map_err(|_| Error::InvalidSchema)?;
            }
            [Raw::Int(kind), Raw::Bytes(bytes)]
                if *kind == BigInt::from(2) && role == 2 && bytes.len() == 32 => {}
            _ => return Err(Error::InvalidSchema),
        }
        if role == 0 && fallback.len() != 1 {
            return Err(Error::InvalidSchema);
        }
        if role == 0 {
            key_role_ids.push(id);
        }
    }
    if ids.windows(2).any(|x| x[0] >= x[1]) {
        return Err(Error::InvalidSchema);
    }
    let mut key_ids = Vec::new();
    let mut key_id_set = BTreeSet::new();
    for key in keys {
        let key = uuid_array(key)?;
        if !ids.contains(&key) || !key_id_set.insert(key) {
            return Err(Error::InvalidSchema);
        }
        key_ids.push(key);
    }
    let key_role_set: BTreeSet<_> = key_role_ids.iter().copied().collect();
    if key_id_set != key_role_set || key_ids.len() != key_role_ids.len() {
        return Err(Error::InvalidSchema);
    }
    let mut definition_ids = Vec::new();
    for d in defs {
        let a = array(d)?;
        if a.len() != 3 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        if definition_ids
            .last()
            .is_some_and(|previous| *previous >= id)
        {
            return Err(Error::InvalidSchema);
        }
        definition_ids.push(id);
        validate_definition(int_u64(&a[1])?, &a[2])?;
    }
    for field in fields {
        validate_type_refs(&array(field)?[2], &definition_ids)?;
    }
    for definition in defs {
        validate_definition_refs(
            int_u64(&array(definition)?[1])?,
            &array(definition)?[2],
            &definition_ids,
        )?;
    }
    Ok(())
}
fn validate_type_refs(node: &Raw, definitions: &[[u8; 16]]) -> Result<()> {
    let a = array(node)?;
    match int_u64(&a[0])? {
        5 | 8 => {
            if !definitions.contains(&uuid_array(&a[1])?) {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => validate_type_refs(&a[1], definitions)?,
        3 => {
            for x in array(&a[1])? {
                validate_type_refs(x, definitions)?
            }
        }
        4 => {
            for x in array(&a[1])? {
                validate_type_refs(&array(x)?[1], definitions)?
            }
        }
        6 => {
            for x in array(&a[3])? {
                validate_type_refs(x, definitions)?
            }
        }
        7 => validate_type_refs(&a[1], definitions)?,
        0 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_definition_refs(kind: u64, body: &Raw, definitions: &[[u8; 16]]) -> Result<()> {
    match kind {
        0 => {
            for f in array(body)? {
                validate_type_refs(&array(f)?[2], definitions)?
            }
        }
        1 => {
            for v in array(body)? {
                for f in array(&array(v)?[2])? {
                    validate_type_refs(&array(f)?[2], definitions)?
                }
            }
        }
        2 => validate_type_refs(&array(body)?[0], definitions)?,
        3 | 4 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_definition(kind: u64, body: &Raw) -> Result<()> {
    match kind {
        0 => {
            validate_nested_field_list(array(body)?)?;
        }
        1 => {
            let mut previous = None;
            for variant in array(body)? {
                let v = array(variant)?;
                if v.len() != 3 {
                    return Err(Error::InvalidSchema);
                }
                let id = uuid_array(&v[0])?;
                if previous.is_some_and(|p| p >= id) {
                    return Err(Error::InvalidSchema);
                }
                previous = Some(id);
                text(&v[1])?;
                validate_nested_field_list(array(&v[2])?)?;
            }
        }
        2 => {
            let a = array(body)?;
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[0])?;
            for digest in array(&a[1])? {
                if bytes(digest)?.len() != 32 {
                    return Err(Error::InvalidSchema);
                }
            }
        }
        3 => {
            let a = array(body)?;
            if a.len() != 6 {
                return Err(Error::InvalidSchema);
            }
            let dimension = array(&a[0])?;
            if dimension.len() != 7 || dimension.iter().any(|v| !matches!(v, Raw::Int(_))) {
                return Err(Error::InvalidSchema);
            }
            for value in &a[1..5] {
                integer(value)?;
            }
            if !matches!(a[5], Raw::Bool(_)) {
                return Err(Error::InvalidSchema);
            }
        }
        4 => {
            let a = array(body)?;
            if a.len() != 2 || text(&a[0])?.len() != 3 || int_u64(&a[1])? > 18 {
                return Err(Error::InvalidSchema);
            }
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_nested_field(field: &[Raw]) -> Result<()> {
    if field.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(&field[0])?;
    let name = text(&field[1])?;
    if name.is_empty() {
        return Err(Error::InvalidSchema);
    }
    validate_type(&field[2])?;
    if int_u64(&field[3])? > 2 {
        return Err(Error::InvalidSchema);
    }
    let fallback = array(&field[4])?;
    match fallback.as_slice() {
        [Raw::Int(x)] if x.is_zero() => {}
        [Raw::Int(x), value] if *x == BigInt::from(1) => {
            validate_value_as_type(value, &field[2]).map_err(|_| Error::InvalidSchema)?
        }
        [Raw::Int(x), Raw::Bytes(bytes)] if *x == BigInt::from(2) && bytes.len() == 32 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_nested_field_list(fields: &[Raw]) -> Result<()> {
    let mut previous = None;
    for field in fields {
        let f = array(field)?;
        let id = uuid_array(&f[0])?;
        if previous.is_some_and(|p| p >= id) {
            return Err(Error::InvalidSchema);
        }
        previous = Some(id);
        validate_nested_field(f)?;
    }
    Ok(())
}
fn validate_type(v: &Raw) -> Result<()> {
    let a = array(v)?;
    if a.is_empty() {
        return Err(Error::InvalidSchema);
    }
    match int_u64(&a[0])? {
        0 => {
            if a.len() != 2
                || !matches!(
                    text(&a[1])?,
                    "Bool"
                        | "Int"
                        | "Float"
                        | "Decimal"
                        | "Str"
                        | "Blob"
                        | "Uuid"
                        | "Date"
                        | "Instant"
                        | "LocalDateTime"
                        | "TimeOfDay"
                        | "Duration"
                        | "TimeZone"
                        | "ZonedDateTime"
                        | "Unit"
                )
            {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
        }
        3 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            for x in array(&a[1])? {
                validate_type(x)?;
            }
        }
        4 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let mut last = "";
            for x in array(&a[1])? {
                let p = array(x)?;
                if p.len() != 2 {
                    return Err(Error::InvalidSchema);
                }
                let n = text(&p[0])?;
                if n <= last {
                    return Err(Error::InvalidSchema);
                }
                last = n;
                validate_type(&p[1])?;
            }
        }
        5 | 8 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
        }
        6 => {
            if a.len() != 4 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
            let _ = uuid_array(&a[2])?;
            for x in array(&a[3])? {
                validate_type(x)?;
            }
        }
        7 => {
            if a.len() != 3 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
            let _ = uuid_array(&a[2])?;
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
/// Validates the portable portion of a value against its exact closed type
/// node. Nominal and table semantics are deliberately rejected here because a
/// standalone node cannot prove membership without the complete descriptor.
fn validate_value_as_type(value: &Raw, ty: &Raw) -> Result<()> {
    let node = array(ty)?;
    match int_u64(&node[0])? {
        0 => match text(&node[1])? {
            "Bool" if matches!(value, Raw::Bool(_)) => Ok(()),
            "Int" if matches!(value, Raw::Int(_)) => Ok(()),
            "Float" if matches!(value, Raw::Float(_)) => Ok(()),
            "Decimal" if matches!(value, Raw::Tag(60000, _)) => Ok(()),
            "Str" if matches!(value, Raw::Text(_)) => Ok(()),
            "Blob" if matches!(value, Raw::Bytes(_)) => Ok(()),
            "Uuid" if matches!(value, Raw::Tag(37, _)) => Ok(()),
            "Unit" if matches!(value, Raw::Tag(60014, _)) => Ok(()),
            "Date" if matches!(value, Raw::Tag(60001, _)) => Ok(()),
            "Instant" if matches!(value, Raw::Tag(60002, _)) => Ok(()),
            "LocalDateTime" if matches!(value, Raw::Tag(60003, _)) => Ok(()),
            "TimeOfDay" if matches!(value, Raw::Tag(60017, _)) => Ok(()),
            "Duration" if matches!(value, Raw::Tag(60005, _)) => Ok(()),
            "TimeZone" if matches!(value, Raw::Tag(60004, _)) => Ok(()),
            "ZonedDateTime" if matches!(value, Raw::Tag(60018, _)) => Ok(()),
            _ => Err(Error::InvalidValue),
        },
        1 => {
            for item in array(value)? {
                validate_value_as_type(item, &node[1])?;
            }
            Ok(())
        }
        2 => {
            let Raw::Tag(60013, option) = value else {
                return Err(Error::InvalidValue);
            };
            let option = array(option)?;
            if option.len() == 2 {
                validate_value_as_type(&option[1], &node[1])?;
            }
            Ok(())
        }
        3 => {
            let types = array(&node[1])?;
            if types.is_empty() {
                return if matches!(value, Raw::Tag(60014, _)) {
                    Ok(())
                } else {
                    Err(Error::InvalidValue)
                };
            }
            let Raw::Tag(60015, tuple) = value else {
                return Err(Error::InvalidValue);
            };
            let tuple = array(tuple)?;
            if tuple.len() != types.len() {
                return Err(Error::InvalidValue);
            }
            for (v, t) in tuple.iter().zip(types) {
                validate_value_as_type(v, t)?;
            }
            Ok(())
        }
        4 => {
            let fields = array(&node[1])?;
            let Raw::Tag(60009, record) = value else {
                return Err(Error::InvalidValue);
            };
            let record = array(record)?;
            if record.len() != 2 {
                return Err(Error::InvalidValue);
            }
            if !matches!(record[0], Raw::Null) {
                return Err(Error::InvalidValue);
            }
            let values = array(&record[1])?;
            if values.len() != fields.len() {
                return Err(Error::InvalidValue);
            }
            for (field, entry) in fields.iter().zip(values) {
                let spec = array(field)?;
                let entry = array(entry)?;
                if entry[0] != spec[0] {
                    return Err(Error::InvalidValue);
                }
                validate_value_as_type(&entry[1], &spec[1])?;
            }
            Ok(())
        }
        7 => {
            let Raw::Tag(60006, quantity) = value else {
                return Err(Error::InvalidValue);
            };
            let quantity = array(quantity)?;
            if quantity.len() != 2 || quantity[1] != node[2] {
                return Err(Error::InvalidValue);
            }
            validate_value_as_type(&quantity[0], &node[1])
        }
        8 => {
            uuid_array(&node[1])?;
            let Raw::Tag(60007, money) = value else {
                return Err(Error::InvalidValue);
            };
            let money = array(money)?;
            if money.len() != 2 || !matches!(money[0], Raw::Tag(60000, _)) {
                return Err(Error::InvalidValue);
            };
            if uuid_array(&money[1])? != uuid_array(&node[1])? {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        9 => {
            let Raw::Tag(60019, range) = value else {
                return Err(Error::InvalidValue);
            };
            let range = array(range)?;
            for endpoint in &range[..2] {
                let Raw::Tag(60013, option) = endpoint else {
                    return Err(Error::InvalidValue);
                };
                let option = array(option)?;
                if option.len() == 2 {
                    validate_value_as_type(&option[1], &node[1])?;
                }
            }
            Ok(())
        }
        5 | 6 => Err(Error::InvalidSchema),
        _ => Err(Error::InvalidSchema),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn canonical_uuid_text_is_lowercase_and_round_trips() {
        let bytes = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let text = canonical_uuid_text(bytes);
        assert_eq!(text, "00112233-4455-6677-8899-aabbccddeeff");
        assert_eq!(parse_canonical_uuid_text(&text).unwrap(), bytes);
        assert_eq!(
            Value::uuid(bytes).encode().unwrap(),
            h("d8255000112233445566778899aabbccddeeff")
        );
        for invalid in [
            "001122334455-6677-8899-aabbccddeeff",
            "00112233-4455-6677-8899-AABBCCDDEEFF",
            "00112233-4455-6677-8899-aabbccddeefg",
            "00112233-4455-6677-8899-aabbccddeeff00",
        ] {
            assert!(parse_canonical_uuid_text(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn argument_identity_normalizes_names_before_sorting_and_collision_check() {
        let composed = "é".to_owned();
        let decomposed = "e\u{301}".to_owned();
        let left =
            argument_identity(vec![(composed, Raw::Int(0.into()), Raw::Int(1.into()))]).unwrap();
        let right = argument_identity(vec![(
            decomposed.clone(),
            Raw::Int(0.into()),
            Raw::Int(1.into()),
        )])
        .unwrap();
        assert_eq!(left, right);
        assert!(
            argument_identity(vec![
                ("é".to_owned(), Raw::Int(0.into()), Raw::Int(1.into())),
                (decomposed, Raw::Int(0.into()), Raw::Int(2.into())),
            ])
            .is_err()
        );
    }

    #[test]
    fn date_codec_rejects_year_zero_and_accepts_declared_bounds() {
        let date = |text: &str| Value::new(tag(60001, Raw::Text(text.to_owned())));

        assert_eq!(date("0000-01-01"), Err(Error::InvalidTag));
        assert!(date("0001-01-01").is_ok());
        assert!(date("9999-12-31").is_ok());
    }

    #[test]
    fn values_match_supplied_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/value-vectors.json"
        ))
        .unwrap();
        for vector in fixture.as_array().unwrap() {
            let x = vector["hex"].as_str().unwrap();
            let b = h(x);
            let decoded = Value::decode(&b).unwrap_or_else(|e| panic!("{x}: {e}"));
            assert_eq!(decoded.encode().unwrap(), b, "{x}")
        }
    }
    #[test]
    fn rejects_noncanonical() {
        for x in [
            "1817",
            "c2490000000000000000",
            "fa3f800000",
            "fb7ff8000000000001",
            "9f00ff",
            "a101000100",
        ] {
            assert!(Value::decode(&h(x)).is_err(), "{x}")
        }
    }

    #[test]
    fn bignums_reject_aliases_and_admit_the_first_out_of_range_magnitude() {
        // ORNA-FORMAT-001: tags 2/3 are only the arbitrary-precision forms.
        // Empty, zero, leading-zero, and direct-major-range magnitudes are
        // aliases, so they cannot enter a canonical identity or digest.
        for x in [
            "c240",                   // tag 2, empty magnitude
            "c24100",                 // tag 2, zero magnitude
            "c24101",                 // tag 2, direct uint(1)
            "c248ffffffffffffffff",   // tag 2, direct uint(u64::MAX)
            "c24900ffffffffffffffff", // tag 2, leading-zero magnitude
            "c340",                   // tag 3, empty magnitude
            "c34100",                 // tag 3, zero magnitude
            "c34101",                 // tag 3, direct nint(-2)
            "c348ffffffffffffffff",   // tag 3, direct nint(-u64::MAX - 1)
            "c34900ffffffffffffffff", // tag 3, leading-zero magnitude
        ] {
            assert_eq!(Value::decode(&h(x)), Err(Error::NonCanonical), "{x}");
        }

        for x in [
            "c249010000000000000000", //  2^64
            "c349010000000000000000", // -2^64 - 1
        ] {
            let value = Value::decode(&h(x)).unwrap_or_else(|error| panic!("{x}: {error}"));
            assert_eq!(value.encode().unwrap(), h(x), "{x}");
        }
    }
    #[test]
    fn invocation_handle_requires_canonical_runtime_uuid() {
        let handle = |runtime| {
            Value::new(tag(
                60022,
                Raw::Array(vec![
                    uuid_raw([1; 16]),
                    runtime,
                    Raw::Null,
                    Raw::Bool(false),
                ]),
            ))
        };

        assert!(handle(uuid_raw([2; 16])).is_ok());
        assert!(handle(Raw::Text("runtime".into())).is_err());
        assert!(handle(Raw::Int(2.into())).is_err());
        assert!(handle(uuid_raw([0; 16])).is_ok());
        assert!(handle(tag(37, Raw::Bytes(vec![2; 15]))).is_err());
    }
    #[test]
    fn sys_revision_id_is_a_closed_32_byte_portable_identifier() {
        let revision_id =
            |name: Raw, representation: Raw| tag(60023, Raw::Array(vec![name, representation]));
        let valid = revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 32]));
        let canonical = |raw: Raw| {
            let value = Value::new(raw).expect("valid system value was rejected");
            let encoded = value.encode().unwrap();
            assert_eq!(Value::decode(&encoded).unwrap().raw(), value.raw());
            encoded
        };
        let encodings = [
            canonical(Raw::Text("revision".into())),
            canonical(revision_id(
                Raw::Text("sys.DatabaseId".into()),
                uuid_raw([7; 16]),
            )),
            canonical(tag(
                60025,
                Raw::Array(vec![Raw::Text("sha256".into()), Raw::Bytes(vec![7; 32])]),
            )),
            canonical(valid.clone()),
        ];
        for (index, left) in encodings.iter().enumerate() {
            assert!(encodings[index + 1..].iter().all(|right| left != right));
        }

        let mut noncanonical = encodings[3].clone();
        noncanonical.splice(4..5, [0x78, 0x0e]);
        assert!(Value::decode(&noncanonical).is_err());

        let rejected = [
            revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 31])),
            revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 33])),
            revision_id(Raw::Text("Str".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("Digest".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("sys.Digest".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("sys.RevisionId".into()), uuid_raw([7; 16])),
            revision_id(
                Raw::Text("sys.RevisionId".into()),
                tag(
                    60025,
                    Raw::Array(vec![Raw::Text("sha256".into()), Raw::Bytes(vec![7; 32])]),
                ),
            ),
            tag(60023, Raw::Text("sys.RevisionId".into())),
            tag(60023, Raw::Array(vec![Raw::Text("sys.RevisionId".into())])),
            revision_id(
                Raw::Bytes(b"sys.RevisionId".to_vec()),
                Raw::Bytes(vec![7; 32]),
            ),
        ];
        for raw in rejected {
            assert!(Value::new(raw.clone()).is_err());
            let mut encoded = Vec::new();
            write_raw(&raw, &mut encoded).unwrap();
            assert!(Value::decode(&encoded).is_err());
        }
    }
    #[test]
    fn float_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/float-vectors.json"
        ))
        .unwrap();
        let parse = |text: &str| u64::from_str_radix(text, 16).unwrap();
        let ordered: Vec<u64> = fixture["ascending_total_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| parse(v["bits"].as_str().unwrap()))
            .collect();
        assert!(
            ordered
                .windows(2)
                .all(|p| float_total_cmp(p[0], p[1]) == Ordering::Less)
        );
        for vector in fixture["ordinary_equality"].as_array().unwrap() {
            assert_eq!(
                float_ordinary_eq(
                    parse(vector["left"].as_str().unwrap()),
                    parse(vector["right"].as_str().unwrap())
                ),
                vector["equal"].as_bool().unwrap()
            );
        }
        for vector in fixture["aggregate"].as_array().unwrap() {
            let values: Vec<u64> = vector["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            let actual = if vector["operation"] == "min" {
                float_min(&values)
            } else {
                float_max(&values)
            };
            assert_eq!(actual, Some(parse(vector["result"].as_str().unwrap())));
        }
        for vector in fixture["parquet_statistics"].as_array().unwrap() {
            let values: Vec<u64> = vector["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            assert_eq!(
                values.iter().filter(|x| is_nan_bits(**x)).count(),
                vector["nan_count"].as_u64().unwrap() as usize
            );
            let non_nan: Vec<u64> = values
                .iter()
                .copied()
                .filter(|x| !is_nan_bits(*x))
                .collect();
            let (min, max, key_min, key_max) = if non_nan.is_empty() {
                let canonical: Vec<u64> = values
                    .into_iter()
                    .map(|x| {
                        if is_nan_bits(x) {
                            CANONICAL_NAN_BITS
                        } else {
                            x
                        }
                    })
                    .collect();
                (
                    *canonical
                        .iter()
                        .min_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    *canonical
                        .iter()
                        .max_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    "min_total",
                    "max_total",
                )
            } else {
                (
                    *non_nan.iter().min_by_key(|x| float_total_key(**x)).unwrap(),
                    *non_nan.iter().max_by_key(|x| float_total_key(**x)).unwrap(),
                    "min_non_nan",
                    "max_non_nan",
                )
            };
            assert_eq!(min, parse(vector[key_min].as_str().unwrap()));
            assert_eq!(max, parse(vector[key_max].as_str().unwrap()));
        }
    }
    #[test]
    fn decimal_is_exact() {
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        assert_eq!(
            Value::decimal(12340.into(), (-3).into())
                .unwrap()
                .encode()
                .unwrap(),
            h("d9ea60821904d221")
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into())
                .divide_exact(&Decimal::new(8.into(), 0.into()))
                .unwrap(),
            Decimal::new(125.into(), (-3).into())
        );
        assert_eq!(
            Decimal::new(120.into(), (-2).into())
                .try_subtract(&Decimal::new(34.into(), (-2).into()))
                .unwrap(),
            Decimal::new(86.into(), (-2).into())
        );
        assert_eq!(
            Decimal::new(1.into(), (-1).into())
                .try_subtract(&Decimal::new(25.into(), (-2).into()))
                .unwrap(),
            Decimal::new((-15).into(), (-2).into())
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into())
                .try_subtract(&Decimal::new(1.into(), 0.into()))
                .unwrap(),
            Decimal::new(0.into(), 0.into())
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into()).try_subtract(&Decimal::new(
                1.into(),
                (-(Decimal::MAX_ABS_EXPONENT as i64 + 1)).into()
            )),
            Err(Error::DecimalLimit)
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into()).divide_exact(&Decimal::new(3.into(), 0.into())),
            Err(Error::NonFiniteDecimal)
        );
    }
    #[test]
    fn path_vectors() {
        for (a, b) in [
            ("alice-smith", "alice-smith"),
            ("a~b", "a~7eb"),
            ("foo/bar", "foo~2fbar"),
            ("é", "~c3~a9"),
            ("", "~ff"),
            ("con", "~63on"),
            (".git", "~2egit"),
        ] {
            assert_eq!(path_encode_component(a).unwrap(), b);
            assert_eq!(path_decode_component(b).unwrap(), a)
        }
        for x in ["~e", "~FF", "~61lice", "x~ff", "../x", "/absolute"] {
            assert!(path_decode_component(x).is_err())
        }
        assert_eq!(
            path_encode_key_components(&[
                "018f7f43-7b3a-7cc2-8c11-37e728c96f4a".into(),
                "2026-09-03".into()
            ])
            .unwrap(),
            vec!["018f7f43-7b3a-7cc2-8c11-37e728c96f4a", "2026-09-03.orna"]
        );
        assert!(path_validate_relative_components(&["..".into()]).is_err());
    }
    #[test]
    fn composite_path_decoding_is_canonical_and_bounded() {
        for keys in [
            vec![""],
            vec!["..", "foo/bar", "é"],
            vec!["first.orna", "last.orna"],
            vec!["con", "a\\b", ".git"],
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let encoded = path_encode_key_components(&keys).unwrap();
            let decoded = path_decode_key_components(&encoded).unwrap();
            assert_eq!(decoded, keys);
            assert_eq!(path_encode_key_components(&decoded).unwrap(), encoded);
        }
        assert_eq!(
            path_decode_key_components(&["first.orna".into(), "last.orna.orna".into()]).unwrap(),
            vec!["first.orna", "last.orna"]
        );
        for encoded in [
            vec![],
            vec![""],
            vec![".orna"],
            vec!["a"],
            vec!["a.ORNA"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["x~ff.orna"],
            vec!["~e.orna"],
            vec!["..", "a.orna"],
            vec!["", "a.orna"],
            vec!["/a.orna"],
            vec!["a/b.orna"],
            vec!["a\\b.orna"],
        ] {
            let encoded: Vec<String> = encoded.into_iter().map(String::from).collect();
            assert!(path_decode_key_components(&encoded).is_err(), "{encoded:?}");
        }
        let mut boundary = vec!["a".repeat(200); 5];
        boundary.push(format!("{}.orna", "b".repeat(14)));
        assert_eq!(boundary.join("/").len(), 1024);
        assert!(path_decode_key_components(&boundary).is_ok());
        boundary[5].insert(0, 'b');
        assert!(path_decode_key_components(&boundary).is_err());
        assert!(path_decode_key_components(&[format!("{}.orna", "a".repeat(201))]).is_err());
    }
    #[test]
    fn construction_and_safe_containers_reject_aliases() {
        assert!(Value::new(Raw::Float(0x7ff8_0000_0000_0001)).is_err());
        assert_eq!(
            Value::float_bits(0x7ff8_0000_0000_0001).raw(),
            &Raw::Float(CANONICAL_NAN_BITS)
        );
        assert!(Value::new(tag(60011, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(60016, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(60012, Raw::Array(vec![]))).is_err());
    }

    #[test]
    fn quantity_requires_a_numeric_amount() {
        let raw = tag(
            60006,
            Raw::Array(vec![Raw::Text("not-a-number".into()), uuid_raw([7; 16])]),
        );
        assert_eq!(Value::new(raw.clone()), Err(Error::InvalidTag));

        let mut bytes = Vec::new();
        write_raw(&raw, &mut bytes).unwrap();
        assert_eq!(Value::decode(&bytes), Err(Error::InvalidTag));
    }

    #[test]
    fn error_value_round_trips_nested_causes_and_safe_details() {
        let mut child_details = BTreeMap::new();
        child_details.insert("retryable".into(), Value::new(Raw::Bool(true)).unwrap());
        let child = ErrorValue::new("ORNA-E-CHILD", "child failure", [], child_details).unwrap();

        let mut details = BTreeMap::new();
        details.insert("attempt".into(), Value::int(3.into()));
        details.insert("zeta".into(), Value::new(Raw::Text("safe".into())).unwrap());
        let error = ErrorValue::new("ORNA-E-ROOT", "root failure", [child], details).unwrap();

        assert_eq!(error.code(), "ORNA-E-ROOT");
        assert_eq!(error.message(), "root failure");
        assert_eq!(error.causes().len(), 1);
        assert_eq!(error.causes()[0].code(), "ORNA-E-CHILD");
        assert_eq!(
            error.causes()[0].safe_details()["retryable"],
            Value::new(Raw::Bool(true)).unwrap()
        );
        assert_eq!(error.safe_details()["attempt"], Value::int(3.into()));
        assert_eq!(
            error.safe_details()["zeta"],
            Value::new(Raw::Text("safe".into())).unwrap()
        );

        let encoded = error.encode().unwrap();
        let decoded = ErrorValue::decode(&encoded).unwrap();
        assert_eq!(decoded, error);
        assert_eq!(decoded.value().raw(), error.value().raw());
    }

    #[test]
    fn error_value_preserves_malformed_rejection() {
        let malformed = tag(
            60016,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Text("safe".into())),
                (
                    Raw::Int(2.into()),
                    Raw::Array(vec![Raw::Text("not-error".into())]),
                ),
                (
                    Raw::Int(3.into()),
                    Raw::Map(vec![(Raw::Int(4.into()), Raw::Bool(true))]),
                ),
            ]),
        );
        assert_eq!(Value::new(malformed.clone()), Err(Error::InvalidTag));

        let mut bytes = Vec::new();
        write_raw(&malformed, &mut bytes).unwrap();
        assert_eq!(ErrorValue::decode(&bytes), Err(Error::InvalidTag));
    }

    fn diagnostic_with_path(path: &str) -> Raw {
        tag(
            60011,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text("safe message".into())),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![Raw::Array(vec![
                        Snapshot::cwd([1; 16], [2; 16], 0.into()).unwrap().raw(),
                        Raw::Text(path.into()),
                        Raw::Int(0.into()),
                        Raw::Int(0.into()),
                    ])]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        )
    }

    #[test]
    fn diagnostic_paths_are_relative_utf8_or_redacted_at_both_ovb_boundaries() {
        for path in [
            "",
            "/absolute/main.orna",
            "../main.orna",
            "src/../main.orna",
            "src//main.orna",
            "./src/main.orna",
            "src\\main.orna",
            "C:/src/main.orna",
            "c:src/main.orna",
            "\\\\server\\share\\main.orna",
            "src/\0main.orna",
            "src/\nmain.orna",
        ] {
            let raw = diagnostic_with_path(path);
            assert!(Value::new(raw.clone()).is_err(), "unsafe path was admitted");
            let mut bytes = Vec::new();
            write_raw(&raw, &mut bytes).unwrap();
            assert!(Value::decode(&bytes).is_err(), "unsafe path was decoded");
        }

        for path in ["src/entrée/メイン.orna", "<redacted>"] {
            let value = Value::new(diagnostic_with_path(path)).expect("safe path rejected");
            let bytes = value.encode().unwrap();
            assert!(Value::decode(&bytes).is_ok(), "safe path was not decoded");
        }
    }

    #[test]
    fn sys_value_money_binds_its_currency_witness() {
        let currency = [7u8; 16];
        let other = [8u8; 16];
        let type_node = Raw::Array(vec![Raw::Int(8.into()), uuid_raw(currency)]);
        let amount = tag(
            60000,
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())]),
        );
        let valid = tag(
            60026,
            Raw::Array(vec![
                type_node.clone(),
                tag(60007, Raw::Array(vec![amount.clone(), uuid_raw(currency)])),
            ]),
        );
        assert!(Value::new(valid).is_ok());
        let invalid = tag(
            60026,
            Raw::Array(vec![
                type_node,
                tag(60007, Raw::Array(vec![amount, uuid_raw(other)])),
            ]),
        );
        assert!(Value::new(invalid).is_err());
    }
    #[test]
    fn schema_frozen_fallback_is_type_directed() {
        let table = [9u8; 16];
        let key = [1u8; 16];
        let stored = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |fallback: Raw| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(vec![uuid_raw(key)])),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(key),
                            Raw::Text("id".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(stored),
                            Raw::Text("value".into()),
                            type_int.clone(),
                            Raw::Int(1.into()),
                            fallback,
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(7.into())
            ])))
            .is_ok()
        );
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Text("not-an-int".into())
            ])))
            .is_err()
        );
    }
    #[test]
    fn schema_preserves_key_order_independently_of_field_order() {
        let table = [9u8; 16];
        let first = [1u8; 16];
        let second = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |keys: Vec<Raw>| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(keys)),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(first),
                            Raw::Text("first".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(second),
                            Raw::Text("second".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        let key_order = |descriptor: &SchemaDescriptor| {
            let Raw::Map(entries) = descriptor.raw() else {
                unreachable!()
            };
            let keys = entries
                .iter()
                .find(|(key, _)| int_u64(key).ok() == Some(2))
                .map(|(_, value)| array(value).unwrap())
                .unwrap();
            keys.iter()
                .map(|key| uuid_array(key).unwrap())
                .collect::<Vec<_>>()
        };

        let forward =
            SchemaDescriptor::new(schema(vec![uuid_raw(first), uuid_raw(second)])).unwrap();
        let reverse =
            SchemaDescriptor::new(schema(vec![uuid_raw(second), uuid_raw(first)])).unwrap();
        assert_eq!(key_order(&forward), vec![first, second]);
        assert_eq!(key_order(&reverse), vec![second, first]);
        assert_ne!(
            schema_identity(&forward).unwrap(),
            schema_identity(&reverse).unwrap()
        );
    }

    #[test]
    fn schema_key_lists_reject_missing_duplicate_and_wrong_role_ids() {
        let table = [9u8; 16];
        let first = [1u8; 16];
        let second = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |keys: Vec<Raw>, second_role: i64| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(keys)),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(first),
                            Raw::Text("first".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(second),
                            Raw::Text("second".into()),
                            type_int.clone(),
                            Raw::Int(second_role.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };

        assert!(SchemaDescriptor::new(schema(vec![uuid_raw(first)], 0)).is_err());
        assert!(
            SchemaDescriptor::new(schema(
                vec![uuid_raw(first), uuid_raw(second), uuid_raw(first)],
                0
            ))
            .is_err()
        );
        assert!(SchemaDescriptor::new(schema(vec![uuid_raw(first), uuid_raw(second)], 1)).is_err());
    }

    #[test]
    fn snapshot_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/snapshot-vectors.json"
        ))
        .unwrap();
        let db = h("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let rt = h("101112131415161718191a1b1c1d1e1f").try_into().unwrap();
        let s = Snapshot::cwd(db, rt, 1.into()).unwrap();
        assert_eq!(
            encode_raw(&s.raw()).unwrap(),
            h(
                "8500d82550000102030405060708090a0b0c0d0e0fd82550101112131415161718191a1b1c1d1e1f015820732adfe16f749548c7184797f01a99247b4633cc89d32d5f47d99df94e53727e"
            )
        );
        for name in ["cwd_1", "cwd_2", "commit_sha1"] {
            let vector = fixture[name].as_str().unwrap();
            let raw = Value::decode(&h(vector)).unwrap().raw().clone();
            assert_eq!(
                encode_raw(&Snapshot::decode(&raw).unwrap().raw()).unwrap(),
                h(vector)
            );
        }
        let commit_sha256 = h(
            "8401d82550000102030405060708090a0b0c0d0e0f6673686132353658200404040404040404040404040404040404040404040404040404040404040404",
        );
        let commit_sha256_raw = Value::decode(&commit_sha256).unwrap().raw().clone();
        assert_eq!(
            encode_raw(&Snapshot::decode(&commit_sha256_raw).unwrap().raw()).unwrap(),
            commit_sha256
        );

        let invalid_sha256 = Raw::Array(vec![
            Raw::Int(1.into()),
            uuid_raw(db),
            Raw::Text("sha256".into()),
            Raw::Bytes(vec![4; 31]),
        ]);
        assert!(Snapshot::decode(&invalid_sha256).is_err());
        let invalid = fixture["invalid_bare_cwd"].as_str().unwrap();
        assert!(Value::decode(&h(invalid)).is_ok());
        assert!(Snapshot::decode(Value::decode(&h(invalid)).unwrap().raw()).is_err());
    }
    #[test]
    fn fixture_path_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/path-vectors.json"
        ))
        .unwrap();
        for vector in fixture["round_trip"].as_array().unwrap() {
            let source = vector["value"].as_str().unwrap();
            let encoded = vector["encoded"].as_str().unwrap();
            assert_eq!(path_encode_component(source).unwrap(), encoded);
            assert_eq!(path_decode_component(encoded).unwrap(), source);
            assert_eq!(
                path_decode_key_components(&[format!("{encoded}.orna")]).unwrap(),
                vec![source]
            );
        }
        for vector in fixture["reject_encoded"].as_array().unwrap() {
            assert!(path_decode_component(vector["encoded"].as_str().unwrap()).is_err());
            assert!(
                path_decode_key_components(&[format!(
                    "{}.orna",
                    vector["encoded"].as_str().unwrap()
                )])
                .is_err()
            );
        }
        for vector in fixture["portable_collisions"].as_array().unwrap() {
            let left = path_encode_component(vector["left"].as_str().unwrap()).unwrap();
            let right = path_encode_component(vector["right"].as_str().unwrap()).unwrap();
            assert_eq!(
                path_collision_key(&left),
                vector["collision_key"].as_str().unwrap()
            );
            assert_eq!(path_collision_key(&left), path_collision_key(&right));
        }
        for vector in fixture["limits"]["component"].as_array().unwrap() {
            let input = "a".repeat(vector["encoded_length"].as_u64().unwrap() as usize);
            assert_eq!(
                path_encode_component(&input).is_ok(),
                vector["expect"] == "pass"
            );
        }
        for vector in fixture["limits"]["table_relative_path"].as_array().unwrap() {
            let size = vector["encoded_length"].as_u64().unwrap() as usize;
            let mut remaining = size - 5;
            let mut components = Vec::new();
            while remaining > 200 {
                components.push("a".repeat(200));
                remaining -= 201;
            }
            components.push("a".repeat(remaining));
            assert_eq!(
                path_encode_key_components(&components).is_ok(),
                vector["expect"] == "pass"
            );
        }
    }
    #[test]
    fn fixture_numeric_vectors_are_exact() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/numeric-vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["money_add"]["a"], "0.1");
        assert_eq!(fixture["money_add"]["b"], "0.2");
        assert_eq!(fixture["money_add"]["result"], "0.3");
        assert_eq!(fixture["mile_per_hour_mps"], "0.44704");
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        // 1609.344 / 3600 has the finite exact decimal supplied by the fixture.
        let mile = Decimal::new(1_609_344.into(), (-3).into());
        let seconds = Decimal::new(3_600.into(), 0.into());
        assert_eq!(
            mile.divide_exact(&seconds).unwrap(),
            Decimal::new(44_704.into(), (-5).into())
        );
    }
}
