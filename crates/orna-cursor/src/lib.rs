//! Ordered, typed transaction cursors and primary-key component codecs.
//!
//! Cursor codecs are source-owned and intentionally separate from the OVB
//! bytes retained by storage, mutations, and public row boundaries. Failures
//! carry only a stable, payload-free code; callers map it into their own error
//! type at the boundary.

use std::{cmp::Ordering, str};

use num_bigint::{BigInt, Sign};
use orna_value::{OvbRaw, Value, compare_primary_keys};
use sha2::{Digest, Sha256};

/// A payload-free cursor or key codec failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorError {
    code: &'static str,
}

impl CursorError {
    /// Creates an error carrying a static, safe diagnostic code.
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    /// Returns the stable diagnostic code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for CursorError {}

/// The subset of v1 key types the transactional relation seam can materialize
/// and compare without delegating ordering to an OVB byte representation.
///
/// `Float` is deliberately absent: ORNA-KEY-003 forbids it as a primary-key
/// component. More elaborate nominal key forms remain outside this bounded
/// transaction seam until their runtime representations are available here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionKeyType {
    Bool,
    Int,
    Decimal,
    Str,
}

impl TransactionKeyType {
    pub fn write_schema_bytes(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(match self {
            Self::Bool => b"Bool",
            Self::Int => b"Int",
            Self::Decimal => b"Decimal",
            Self::Str => b"Str",
        });
        output.push(0);
    }

    pub fn validate(&self, value: &Value) -> Result<(), CursorError> {
        let valid = match (self, value.raw()) {
            (Self::Bool, OvbRaw::Bool(_))
            | (Self::Int, OvbRaw::Int(_))
            | (Self::Str, OvbRaw::Text(_)) => true,
            (Self::Decimal, OvbRaw::Tag(60000, value)) => {
                matches!(value.as_ref(), OvbRaw::Array(parts) if matches!(parts.as_slice(), [OvbRaw::Int(_), OvbRaw::Int(_)]))
            }
            _ => false,
        };
        valid
            .then_some(())
            .ok_or_else(|| CursorError::new("ORNA-EVAL-TABLE-KEY"))
    }
}

/// Declared table key metadata retained after transaction admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionTableKey {
    pub table: String,
    pub fields: Vec<String>,
    pub types: Vec<TransactionKeyType>,
}

impl TransactionTableKey {
    pub fn new(
        table: String,
        fields: Vec<String>,
        types: Vec<TransactionKeyType>,
    ) -> Result<Self, String> {
        if fields.is_empty() || fields.len() != types.len() {
            return Err("transactional source seam requires a typed explicit table key".into());
        }
        Ok(Self {
            table,
            fields,
            types,
        })
    }

    pub fn cursor_schema_digest(&self) -> [u8; 32] {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ORNA-TXN-CURSOR-SCHEMA\0");
        bytes.extend_from_slice(self.table.as_bytes());
        bytes.push(0);
        for (field, ty) in self.fields.iter().zip(&self.types) {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
            ty.write_schema_bytes(&mut bytes);
        }
        Sha256::digest(bytes).into()
    }

    pub fn values_from_encoded(&self, encoded: &[u8]) -> Result<Vec<Value>, CursorError> {
        let values = table_key_components(encoded, self.fields.len())?;
        for (value, ty) in values.iter().zip(&self.types) {
            ty.validate(value)?;
        }
        Ok(values)
    }

    pub fn key_from_values(&self, values: &[Value]) -> Result<TransactionKey, CursorError> {
        if values.len() != self.types.len() {
            return Err(CursorError::new("ORNA-EVAL-TABLE-KEY"));
        }
        for (value, ty) in values.iter().zip(&self.types) {
            ty.validate(value)?;
        }
        Ok(TransactionKey::typed(
            encoded_table_key(values)?,
            values.to_vec(),
        ))
    }

    pub fn key_from_encoded(&self, encoded: &[u8]) -> Result<TransactionKey, CursorError> {
        let values = self.values_from_encoded(encoded)?;
        Ok(TransactionKey::typed(encoded_table_key(&values)?, values))
    }

    /// Reads one published canonical text key as its single component value.
    pub fn key_from_canonical_text(&self, encoded: &[u8]) -> Option<TransactionKey> {
        if self.types.len() != 1 {
            return None;
        }
        let text = std::str::from_utf8(encoded).ok()?;
        let value = Value::new(orna_value::Raw::Text(text.to_owned())).ok()?;
        self.types[0].validate(&value).ok()?;
        self.key_from_values(std::slice::from_ref(&value)).ok()
    }

    pub fn key_from_row(&self, row: &Value) -> Result<TransactionKey, CursorError> {
        let values = self
            .fields
            .iter()
            .map(|field| {
                record_field(row, field).ok_or_else(|| CursorError::new("ORNA-EVAL-TABLE-KEY"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.key_from_values(&values)
    }
}

impl std::ops::Deref for TransactionTableKey {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}

/// An encoded key plus its admitted primary-key component types. The latter is
/// private transaction metadata, not part of the stored OVB value.
#[derive(Clone, Debug)]
pub struct TransactionKey {
    encoded: Vec<u8>,
    components: Option<Vec<Value>>,
}

impl TransactionKey {
    pub fn new(encoded: Vec<u8>) -> Self {
        Self {
            encoded,
            components: None,
        }
    }

    pub fn typed(encoded: Vec<u8>, components: Vec<Value>) -> Self {
        Self {
            encoded,
            components: Some(components),
        }
    }

    pub fn into_encoded(self) -> Vec<u8> {
        self.encoded
    }

    pub fn components(&self) -> Option<&[Value]> {
        self.components.as_deref()
    }
}

impl AsRef<[u8]> for TransactionKey {
    fn as_ref(&self) -> &[u8] {
        &self.encoded
    }
}

impl PartialEq for TransactionKey {
    fn eq(&self, other: &Self) -> bool {
        self.encoded == other.encoded
    }
}

impl Eq for TransactionKey {}

impl Ord for TransactionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        if let (Some(left), Some(right)) = (&self.components, &other.components)
            && left.len() == right.len()
        {
            let tuple = |values: &[Value]| {
                Value::new(OvbRaw::Tag(
                    60015,
                    Box::new(OvbRaw::Array(
                        values.iter().map(|value| value.raw().clone()).collect(),
                    )),
                ))
            };
            if let (Ok(left), Ok(right)) = (tuple(left), tuple(right)) {
                return compare_primary_keys(&left, &right)
                    .unwrap_or_else(|_| self.encoded.cmp(&other.encoded));
            }
        }
        match (Value::decode(&self.encoded), Value::decode(&other.encoded)) {
            (Ok(left), Ok(right)) => compare_primary_keys(&left, &right)
                .unwrap_or_else(|_| self.encoded.cmp(&other.encoded)),
            _ => self.encoded.cmp(&other.encoded),
        }
    }
}

pub fn decimal_key_cmp(left: &OvbRaw, right: &OvbRaw) -> Ordering {
    let (OvbRaw::Array(left), OvbRaw::Array(right)) = (left, right) else {
        unreachable!("validated Decimal has coefficient and exponent");
    };
    let (OvbRaw::Int(left_coefficient), OvbRaw::Int(left_exponent)) = (&left[0], &left[1]) else {
        unreachable!("validated Decimal has integer coefficient and exponent");
    };
    let (OvbRaw::Int(right_coefficient), OvbRaw::Int(right_exponent)) = (&right[0], &right[1])
    else {
        unreachable!("validated Decimal has integer coefficient and exponent");
    };
    match left_coefficient.sign().cmp(&right_coefficient.sign()) {
        Ordering::Equal => {}
        ordering => return ordering,
    }
    if left_coefficient.sign() == Sign::NoSign {
        return Ordering::Equal;
    }
    let left_digits = BigInt::from(
        left_coefficient
            .to_str_radix(10)
            .trim_start_matches('-')
            .len(),
    );
    let right_digits = BigInt::from(
        right_coefficient
            .to_str_radix(10)
            .trim_start_matches('-')
            .len(),
    );
    let left_magnitude = left_exponent + left_digits;
    let right_magnitude = right_exponent + right_digits;
    let magnitude = left_magnitude.cmp(&right_magnitude);
    if magnitude != Ordering::Equal {
        return if left_coefficient.sign() == Sign::Minus {
            magnitude.reverse()
        } else {
            magnitude
        };
    }
    let exponent = if left_exponent < right_exponent {
        left_exponent
    } else {
        right_exponent
    };
    let left_shift = (left_exponent - exponent)
        .to_string()
        .parse::<usize>()
        .expect("equal Decimal magnitudes bound scale adjustment by coefficient digits");
    let right_shift = (right_exponent - exponent)
        .to_string()
        .parse::<usize>()
        .expect("equal Decimal magnitudes bound scale adjustment by coefficient digits");
    let ordering = (left_coefficient * BigInt::from(10_u8).pow(left_shift as u32))
        .cmp(&(right_coefficient * BigInt::from(10_u8).pow(right_shift as u32)));
    if left_coefficient.sign() == Sign::Minus {
        ordering.reverse()
    } else {
        ordering
    }
}

impl PartialOrd for TransactionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Relation cursors are source-owned and intentionally separate from the OVB
// bytes retained by storage, mutations, and public row boundaries. The
// evaluator requires a continuation cursor to increase lexicographically,
// while canonical OVB encodings of negative integers do not have that
// property in numeric order.
pub const TRANSACTION_INT_CURSOR_PREFIX: &[u8] = b"ORNA-TXN-INT-CURSOR\0";
pub const TRANSACTION_CURSOR_PREFIX: &[u8] = b"ORNA-TXN-CURSOR\0";
pub const TRANSACTION_TYPED_CURSOR_VERSION: u8 = 1;

pub fn ordered_integer_cursor(value: &BigInt) -> Vec<u8> {
    let (sign, magnitude) = value.to_bytes_be();
    let length = u64::try_from(magnitude.len())
        .expect("bounded cursor integer magnitude fits a u64 length")
        .to_be_bytes();
    let mut encoded = Vec::new();
    match sign {
        Sign::Minus => {
            encoded.push(0);
            encoded.extend(length.map(|byte| !byte));
            encoded.extend(magnitude.into_iter().map(|byte| !byte));
        }
        Sign::NoSign => encoded.push(1),
        Sign::Plus => {
            encoded.push(2);
            encoded.extend_from_slice(&length);
            encoded.extend_from_slice(&magnitude);
        }
    }
    encoded
}

pub fn decode_ordered_integer_cursor(
    encoded: &[u8],
    complemented: bool,
) -> Result<(BigInt, usize), CursorError> {
    let decode = |byte: u8| if complemented { !byte } else { byte };
    let Some(&kind) = encoded.first() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let kind = decode(kind);
    if kind == 1 {
        return Ok((BigInt::ZERO, 1));
    }
    if kind != 0 && kind != 2 || encoded.len() < 9 {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let mut length_bytes = [0; 8];
    for (index, byte) in length_bytes.iter_mut().enumerate() {
        *byte = decode(encoded[index + 1]);
    }
    if kind == 0 {
        length_bytes.iter_mut().for_each(|byte| *byte = !*byte);
    }
    let length = usize::try_from(u64::from_be_bytes(length_bytes))
        .map_err(|_| CursorError::new("ORNA-EVAL-VALUE"))?;
    let end = 9usize
        .checked_add(length)
        .ok_or_else(|| CursorError::new("ORNA-EVAL-VALUE"))?;
    if length == 0 || encoded.len() < end {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let mut magnitude = encoded[9..end].to_vec();
    if complemented {
        magnitude.iter_mut().for_each(|byte| *byte = !*byte);
    }
    if kind == 0 {
        magnitude.iter_mut().for_each(|byte| *byte = !*byte);
    }
    if magnitude.first() == Some(&0) {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let sign = if kind == 0 { Sign::Minus } else { Sign::Plus };
    Ok((BigInt::from_bytes_be(sign, &magnitude), end))
}

pub fn encode_ordered_string_cursor(value: &str, output: &mut Vec<u8>) {
    for byte in value.bytes() {
        match byte {
            0..=253 => output.push(byte + 1),
            254 => output.extend_from_slice(&[255, 0]),
            255 => output.extend_from_slice(&[255, 1]),
        }
    }
    output.push(0);
}

pub fn decode_ordered_string_cursor(encoded: &[u8]) -> Result<(String, usize), CursorError> {
    let mut bytes = Vec::new();
    let mut index = 0;
    while index < encoded.len() {
        match encoded[index] {
            0 => {
                let value =
                    String::from_utf8(bytes).map_err(|_| CursorError::new("ORNA-EVAL-VALUE"))?;
                return Ok((value, index + 1));
            }
            1..=254 => bytes.push(encoded[index] - 1),
            255 => {
                let Some(&escape) = encoded.get(index + 1) else {
                    return Err(CursorError::new("ORNA-EVAL-VALUE"));
                };
                match escape {
                    0 => bytes.push(254),
                    1 => bytes.push(255),
                    _ => return Err(CursorError::new("ORNA-EVAL-VALUE")),
                }
                index += 1;
            }
        }
        index += 1;
    }
    Err(CursorError::new("ORNA-EVAL-VALUE"))
}

pub fn encode_ordered_decimal_cursor(
    value: &Value,
    output: &mut Vec<u8>,
) -> Result<(), CursorError> {
    let OvbRaw::Tag(60000, value) = value.raw() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let OvbRaw::Array(parts) = value.as_ref() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let [OvbRaw::Int(coefficient), OvbRaw::Int(exponent)] = parts.as_slice() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let sign = coefficient.sign();
    let negative = sign == Sign::Minus;
    output.push(match sign {
        Sign::Minus => 0,
        Sign::NoSign => 1,
        Sign::Plus => 2,
    });
    if sign == Sign::NoSign {
        return Ok(());
    }
    let digits = coefficient
        .to_str_radix(10)
        .trim_start_matches('-')
        .as_bytes()
        .to_vec();
    let adjusted = exponent + BigInt::from(digits.len());
    let mut exponent_cursor = ordered_integer_cursor(&adjusted);
    if negative {
        exponent_cursor.iter_mut().for_each(|byte| *byte = !*byte);
    }
    output.extend(exponent_cursor);
    for digit in digits {
        let digit = digit - b'0' + 1;
        output.push(if negative { !digit } else { digit });
    }
    output.push(if negative { 255 } else { 0 });
    Ok(())
}

pub fn decode_ordered_decimal_cursor(encoded: &[u8]) -> Result<(Value, usize), CursorError> {
    let Some(&sign) = encoded.first() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    if sign == 1 {
        return Value::decimal(BigInt::ZERO, BigInt::ZERO)
            .map(|value| (value, 1))
            .map_err(|_| CursorError::new("ORNA-EVAL-VALUE"));
    }
    if sign != 0 && sign != 2 {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let negative = sign == 0;
    let (adjusted, exponent_bytes) = decode_ordered_integer_cursor(&encoded[1..], negative)?;
    let digits_start = 1usize
        .checked_add(exponent_bytes)
        .ok_or_else(|| CursorError::new("ORNA-EVAL-VALUE"))?;
    let terminal = if negative { 255 } else { 0 };
    let mut digits = Vec::new();
    let mut digits_end = digits_start;
    while let Some(&encoded_byte) = encoded.get(digits_end) {
        if encoded_byte == terminal {
            break;
        }
        let byte = if negative {
            !encoded_byte
        } else {
            encoded_byte
        };
        if !(1..=10).contains(&byte) {
            return Err(CursorError::new("ORNA-EVAL-VALUE"));
        }
        digits.push(b'0' + byte - 1);
        digits_end += 1;
    }
    if digits.is_empty() || encoded.get(digits_end) != Some(&terminal) {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    digits_end += 1;
    let mut coefficient =
        BigInt::parse_bytes(&digits, 10).ok_or_else(|| CursorError::new("ORNA-EVAL-VALUE"))?;
    if negative {
        coefficient = -coefficient;
    }
    let exponent = adjusted - BigInt::from(digits.len());
    let value =
        Value::decimal(coefficient, exponent).map_err(|_| CursorError::new("ORNA-EVAL-VALUE"))?;
    let mut canonical = Vec::new();
    encode_ordered_decimal_cursor(&value, &mut canonical)?;
    if canonical.as_slice() != &encoded[..digits_end] {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    Ok((value, digits_end))
}

pub fn encode_typed_cursor_value(
    ty: &TransactionKeyType,
    value: &Value,
    output: &mut Vec<u8>,
) -> Result<(), CursorError> {
    ty.validate(value)?;
    match (ty, value.raw()) {
        (TransactionKeyType::Bool, OvbRaw::Bool(value)) => output.push(u8::from(*value)),
        (TransactionKeyType::Int, OvbRaw::Int(value)) => {
            output.extend(ordered_integer_cursor(value))
        }
        (TransactionKeyType::Decimal, _) => encode_ordered_decimal_cursor(value, output)?,
        (TransactionKeyType::Str, OvbRaw::Text(value)) => {
            encode_ordered_string_cursor(value, output)
        }
        _ => return Err(CursorError::new("ORNA-EVAL-VALUE")),
    }
    Ok(())
}

pub fn decode_typed_cursor_value(
    ty: &TransactionKeyType,
    encoded: &[u8],
) -> Result<(Value, usize), CursorError> {
    match ty {
        TransactionKeyType::Bool => match encoded.first() {
            Some(0) => Ok((
                Value::new(OvbRaw::Bool(false)).expect("false is canonical"),
                1,
            )),
            Some(1) => Ok((
                Value::new(OvbRaw::Bool(true)).expect("true is canonical"),
                1,
            )),
            _ => Err(CursorError::new("ORNA-EVAL-VALUE")),
        },
        TransactionKeyType::Int => {
            let (value, used) = decode_ordered_integer_cursor(encoded, false)?;
            Ok((Value::int(value), used))
        }
        TransactionKeyType::Decimal => decode_ordered_decimal_cursor(encoded),
        TransactionKeyType::Str => {
            let (value, used) = decode_ordered_string_cursor(encoded)?;
            Ok((
                Value::new(OvbRaw::Text(value)).expect("decoded UTF-8 is canonical"),
                used,
            ))
        }
    }
}

pub fn transaction_typed_cursor(
    key: &TransactionKey,
    key_schema: &TransactionTableKey,
) -> Result<Vec<u8>, CursorError> {
    let values = key_schema.values_from_encoded(key.as_ref())?;
    let mut encoded = vec![TRANSACTION_TYPED_CURSOR_VERSION];
    for (value, ty) in values.iter().zip(&key_schema.types) {
        encode_typed_cursor_value(ty, value, &mut encoded)?;
    }
    Ok(encoded)
}

pub fn transaction_typed_cursor_key(
    encoded: &[u8],
    key_schema: &TransactionTableKey,
) -> Result<TransactionKey, CursorError> {
    if encoded.first() != Some(&TRANSACTION_TYPED_CURSOR_VERSION) {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let mut values = Vec::with_capacity(key_schema.types.len());
    let mut offset = 1;
    for ty in &key_schema.types {
        let (value, used) = decode_typed_cursor_value(ty, &encoded[offset..])?;
        offset = offset
            .checked_add(used)
            .ok_or_else(|| CursorError::new("ORNA-EVAL-VALUE"))?;
        values.push(value);
    }
    if offset != encoded.len() {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    key_schema.key_from_values(&values)
}

pub fn transaction_int_cursor(key: &TransactionKey) -> Option<Vec<u8>> {
    let value = Value::decode(key.as_ref()).ok()?;
    let OvbRaw::Int(value) = value.raw() else {
        return None;
    };
    let (sign, magnitude) = value.to_bytes_be();
    let length = u64::try_from(magnitude.len()).ok()?.to_be_bytes();
    let mut cursor = Vec::from(TRANSACTION_INT_CURSOR_PREFIX);
    match sign {
        // Complementing both the fixed-width length and magnitude reverses
        // the magnitude order within the negative partition.
        Sign::Minus => {
            cursor.push(0);
            cursor.extend(length.map(|byte| !byte));
            cursor.extend(magnitude.into_iter().map(|byte| !byte));
        }
        Sign::NoSign => cursor.push(1),
        Sign::Plus => {
            cursor.push(2);
            cursor.extend_from_slice(&length);
            cursor.extend_from_slice(&magnitude);
        }
    }
    Some(cursor)
}

pub fn transaction_int_cursor_key(cursor: &[u8]) -> Result<TransactionKey, CursorError> {
    let Some(encoded) = cursor.strip_prefix(TRANSACTION_INT_CURSOR_PREFIX) else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let Some((&kind, encoded)) = encoded.split_first() else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    let value = match kind {
        0 | 2 => {
            let Some((encoded_length, encoded_magnitude)) = encoded.split_at_checked(8) else {
                return Err(CursorError::new("ORNA-EVAL-VALUE"));
            };
            let encoded_length: [u8; 8] = encoded_length
                .try_into()
                .expect("split cursor length is exactly eight bytes");
            let length = if kind == 0 {
                u64::from_be_bytes(encoded_length.map(|byte| !byte))
            } else {
                u64::from_be_bytes(encoded_length)
            };
            // Zero has exactly one canonical cursor representation: kind 1
            // with no payload. Accepting an empty magnitude here would let a
            // forged negative or positive cursor alias zero.
            if length == 0 {
                return Err(CursorError::new("ORNA-EVAL-VALUE"));
            }
            if usize::try_from(length).ok() != Some(encoded_magnitude.len()) {
                return Err(CursorError::new("ORNA-EVAL-VALUE"));
            }
            let magnitude = if kind == 0 {
                encoded_magnitude.iter().map(|byte| !byte).collect()
            } else {
                encoded_magnitude.to_vec()
            };
            if magnitude.first() == Some(&0) {
                return Err(CursorError::new("ORNA-EVAL-VALUE"));
            }
            BigInt::from_bytes_be(if kind == 0 { Sign::Minus } else { Sign::Plus }, &magnitude)
        }
        1 if encoded.is_empty() => BigInt::from(0),
        _ => return Err(CursorError::new("ORNA-EVAL-VALUE")),
    };
    Value::int(value)
        .encode()
        .map(TransactionKey::new)
        .map_err(|_| CursorError::new("ORNA-EVAL-VALUE"))
}

pub fn transaction_cursor_key(
    cursor: &[u8],
    key_schema: &TransactionTableKey,
) -> Result<TransactionKey, CursorError> {
    let encoded = cursor
        .strip_prefix(TRANSACTION_CURSOR_PREFIX)
        .ok_or_else(|| CursorError::new("ORNA-EVAL-VALUE"))?;
    let Some((schema, encoded)) = encoded.split_at_checked(32) else {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    };
    if schema != key_schema.cursor_schema_digest() {
        return Err(CursorError::new("ORNA-EVAL-VALUE"));
    }
    let encoded = if key_schema.types.as_slice() == [TransactionKeyType::Int] {
        transaction_int_cursor_key(encoded)?.into_encoded()
    } else {
        transaction_typed_cursor_key(encoded, key_schema)?.into_encoded()
    };
    key_schema.key_from_encoded(&encoded)
}

pub fn transaction_continuation_cursor(
    key: &TransactionKey,
    key_schema: &TransactionTableKey,
) -> Vec<u8> {
    let mut cursor = Vec::from(TRANSACTION_CURSOR_PREFIX);
    cursor.extend_from_slice(&key_schema.cursor_schema_digest());
    if key_schema.types.as_slice() == [TransactionKeyType::Int] {
        cursor.extend_from_slice(
            &transaction_int_cursor(key).expect("typed Int key has a signed integer cursor"),
        );
    } else {
        cursor.extend_from_slice(
            &transaction_typed_cursor(key, key_schema).expect("typed key has a canonical cursor"),
        );
    }
    cursor
}

pub fn record_field(row: &Value, field: &str) -> Option<Value> {
    let OvbRaw::Map(entries) = row.raw() else {
        return None;
    };
    entries.iter().find_map(|(key, value)| match key {
        OvbRaw::Text(name) if name == field => Value::new(value.clone()).ok(),
        _ => None,
    })
}

pub fn encoded_key(key: &Value) -> Result<Vec<u8>, CursorError> {
    key.encode()
        .map_err(|_| CursorError::new("ORNA-EVAL-TABLE-KEY"))
}

pub fn encoded_table_key(values: &[Value]) -> Result<Vec<u8>, CursorError> {
    match values {
        [key] => encoded_key(key),
        _ => Value::new(OvbRaw::Array(
            values.iter().map(|value| value.raw().clone()).collect(),
        ))
        .map_err(|_| CursorError::new("ORNA-EVAL-TABLE-KEY"))
        .and_then(|key| encoded_key(&key)),
    }
}

pub fn table_key_components(encoded: &[u8], arity: usize) -> Result<Vec<Value>, CursorError> {
    let key = Value::decode(encoded).map_err(|_| CursorError::new("ORNA-EVAL-TABLE-KEY"))?;
    match (arity, key.raw()) {
        (_, OvbRaw::Tag(60015, tuple)) => match tuple.as_ref() {
            OvbRaw::Array(values) if values.len() == arity => values
                .iter()
                .cloned()
                .map(|value| Value::new(value).map_err(|_| CursorError::new("ORNA-EVAL-TABLE-KEY")))
                .collect(),
            _ => Err(CursorError::new("ORNA-EVAL-TABLE-KEY")),
        },
        (1, _) => Ok(vec![key]),
        (_, OvbRaw::Array(values)) if values.len() == arity => values
            .iter()
            .cloned()
            .map(|value| Value::new(value).map_err(|_| CursorError::new("ORNA-EVAL-TABLE-KEY")))
            .collect(),
        _ => Err(CursorError::new("ORNA-EVAL-TABLE-KEY")),
    }
}
