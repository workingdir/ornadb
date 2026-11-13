//! Canonical CBOR (SOV-3) value plumbing for format-3 graphs.
//!
//! One value type serves every format-3 object: node envelopes, stored values,
//! schema descriptors and overflow references. The byte rules (shortest heads,
//! sorted map keys, the canonical NaN, bignums) live in `orna-cbor`. This
//! module adds the format-3 rules that reader cannot see: every tag is a
//! format-3 node tag, and no array or map holds more than `NODE_DATA_LIMIT`
//! entries. Node envelopes are mapped to typed nodes in `node_decode`
//! (store-format-3 section 2).

use super::*;

use orna_cbor::{Build, Encode, Limits, Reader, View, head};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CborValue {
    Unsigned(u64),
    Negative(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<CborValue>),
    Map(Vec<(CborValue, CborValue)>),
    Tag(u64, Box<CborValue>),
    Bool(bool),
    Float64(u64),
    Null,
}

/// The data model the shared canonical reader builds for format-3 values.
struct CborModel;

impl Build for CborModel {
    type Value = CborValue;

    fn null() -> CborValue {
        CborValue::Null
    }

    fn boolean(value: bool) -> CborValue {
        CborValue::Bool(value)
    }

    fn unsigned(value: u64) -> CborValue {
        CborValue::Unsigned(value)
    }

    fn negative(magnitude: u64) -> CborValue {
        CborValue::Negative(magnitude)
    }

    fn bignum(negative: bool, magnitude: &[u8]) -> CborValue {
        let tag = if negative { 3 } else { 2 };
        CborValue::Tag(tag, Box::new(CborValue::Bytes(magnitude.to_vec())))
    }

    fn float(bits: u64) -> CborValue {
        CborValue::Float64(bits)
    }

    fn bytes(value: &[u8]) -> CborValue {
        CborValue::Bytes(value.to_vec())
    }

    fn text(value: &str) -> CborValue {
        CborValue::Text(value.to_owned())
    }

    fn array(values: Vec<CborValue>) -> CborValue {
        CborValue::Array(values)
    }

    fn map(entries: Vec<(CborValue, CborValue)>) -> CborValue {
        CborValue::Map(entries)
    }

    fn tag(number: u64, value: CborValue) -> CborValue {
        CborValue::Tag(number, Box::new(value))
    }
}

impl Encode for CborValue {
    fn view(&self) -> View<'_, Self> {
        match self {
            Self::Unsigned(value) => View::Unsigned(*value),
            Self::Negative(value) => View::Negative(*value),
            Self::Bytes(value) => View::Bytes(value.as_slice()),
            Self::Text(value) => View::Text(value.as_str()),
            Self::Array(values) => View::Array(values.as_slice()),
            Self::Map(entries) => View::Map(entries.as_slice()),
            Self::Tag(number, value) => View::Tag(*number, value.as_ref()),
            Self::Bool(value) => View::Boolean(*value),
            Self::Float64(bits) => View::Float(*bits),
            Self::Null => View::Null,
        }
    }
}

/// The canonical bytes of `value`. The encoder sorts map entries by encoded
/// key and refuses two keys that encode alike; that refusal is reported as
/// non-canonical data, as the reader reports the same bytes.
pub(crate) fn canonical_value_bytes(value: &CborValue) -> Result<Vec<u8>, GraphError> {
    orna_cbor::encode(value).map_err(|_| GraphError::NonCanonicalData)
}

pub(super) fn validate_canonical_value(bytes: &[u8]) -> Result<(), GraphError> {
    decode_canonical_cbor(bytes).map(|_| ())
}

pub(crate) fn decode_canonical_cbor(bytes: &[u8]) -> Result<CborValue, GraphError> {
    decode_canonical_cbor_limited(bytes, NODE_DATA_LIMIT)
}

pub(crate) fn decode_canonical_content(bytes: &[u8]) -> Result<CborValue, GraphError> {
    decode_canonical_cbor_limited(bytes, crate::row_store::ROW_CANONICAL_LIMIT)
}

fn decode_canonical_cbor_limited(bytes: &[u8], maximum: usize) -> Result<CborValue, GraphError> {
    if bytes.len() > maximum {
        return Err(GraphError::InvalidLength(bytes.len() as u64));
    }
    read_canonical_item(bytes)
}

/// Reads exactly one canonical item that fills `bytes`, then applies the
/// format-3 rules. The byte reader has already proved the item re-encodes to
/// `bytes`, so no second encoding pass is needed. Every byte-level refusal is
/// reported as non-canonical data.
pub(super) fn read_canonical_item(bytes: &[u8]) -> Result<CborValue, GraphError> {
    let limits = Limits {
        max_depth: usize::MAX,
        max_items: usize::MAX,
    };
    let mut reader = Reader::<CborModel>::new(bytes, limits);
    let value = reader.value().map_err(|_| GraphError::NonCanonicalData)?;
    if !reader.is_finished() {
        return Err(GraphError::NonCanonicalData);
    }
    check_format3_shape(&value)?;
    Ok(value)
}

/// The format-3 rules the byte reader does not apply: every tag is a
/// format-3 node tag, and every array or map holds at most `NODE_DATA_LIMIT`
/// entries.
fn check_format3_shape(value: &CborValue) -> Result<(), GraphError> {
    match value {
        CborValue::Array(values) => {
            check_container_length(values.len())?;
            values.iter().try_for_each(check_format3_shape)
        }
        CborValue::Map(entries) => {
            check_container_length(entries.len())?;
            entries.iter().try_for_each(|(key, value)| {
                check_format3_shape(key)?;
                check_format3_shape(value)
            })
        }
        CborValue::Tag(number, payload) => {
            if !orna_value::is_format3_node_tag(*number) {
                return Err(GraphError::NonCanonicalData);
            }
            check_format3_shape(payload)
        }
        _ => Ok(()),
    }
}

fn check_container_length(length: usize) -> Result<(), GraphError> {
    if length > NODE_DATA_LIMIT {
        return Err(GraphError::NodeDataTooLarge(length));
    }
    Ok(())
}

pub(super) fn array(output: &mut Vec<u8>, length: u64) {
    head(output, 4, length);
}

pub(super) fn bytes(output: &mut Vec<u8>, value: &[u8]) {
    head(output, 2, value.len() as u64);
    output.extend_from_slice(value);
}

pub(super) fn uint(output: &mut Vec<u8>, value: u64) {
    head(output, 0, value);
}

pub(super) fn null(output: &mut Vec<u8>) {
    output.push(0xf6);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-cutover writer panicked on duplicate keys; the writer now
    /// reports the reader's error for the same bytes.
    #[test]
    fn duplicate_map_keys_are_refused_as_non_canonical() {
        let value = CborValue::Map(vec![
            (CborValue::Unsigned(1), CborValue::Null),
            (CborValue::Unsigned(1), CborValue::Bool(true)),
        ]);
        assert_eq!(
            canonical_value_bytes(&value),
            Err(GraphError::NonCanonicalData)
        );
    }

    /// Keys sort by their encoded bytes: unsigned 1 (0x01) before text "b" (0x61 0x62).
    #[test]
    fn map_keys_sort_by_encoded_bytes() {
        let value = CborValue::Map(vec![
            (CborValue::Text("b".to_owned()), CborValue::Null),
            (CborValue::Unsigned(1), CborValue::Null),
        ]);
        assert_eq!(
            canonical_value_bytes(&value),
            Ok(vec![0xa2, 0x01, 0xf6, 0x61, 0x62, 0xf6])
        );
    }
}
