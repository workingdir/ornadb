//! Canonical CBOR (SOV-3) value plumbing for format-3 graphs.
//!
//! One value type, one reader and one writer serve every format-3 object: node
//! envelopes, stored values, schema descriptors and overflow references. The
//! reader rejects non-canonical encodings as it parses (minimal heads, sorted
//! map keys, format-3 tags only), so a decoded value re-encodes to the exact
//! bytes it came from. Node envelopes are decoded here as well, because that
//! mapping is part of the same wire contract (store-format-3 section 2).

use super::*;

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

impl CborValue {
    fn encode(&self, output: &mut Vec<u8>) {
        match self {
            Self::Unsigned(value) => uint(output, *value),
            Self::Negative(value) => {
                cbor_head(output, 1, *value);
            }
            Self::Bytes(value) => bytes(output, value),
            Self::Text(value) => {
                cbor_head(output, 3, value.len() as u64);
                output.extend_from_slice(value.as_bytes());
            }
            Self::Array(values) => {
                array(output, values.len() as u64);
                for value in values {
                    value.encode(output);
                }
            }
            Self::Map(entries) => {
                cbor_head(output, 5, entries.len() as u64);
                for (key, value) in entries {
                    key.encode(output);
                    value.encode(output);
                }
            }
            Self::Tag(tag, value) => {
                cbor_head(output, 6, *tag);
                value.encode(output);
            }
            Self::Bool(value) => output.push(if *value { 0xf5 } else { 0xf4 }),
            Self::Float64(value) => {
                output.push(0xfb);
                output.extend_from_slice(&value.to_be_bytes());
            }
            Self::Null => null(output),
        }
    }
}

/// A canonical CBOR reader over one complete byte string. Node decoding needs
/// the parsed value *and* the reader's end position to prove the bytes were
/// canonical as a node envelope, not just as a value.
pub(super) struct CborReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> CborReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }

    pub(super) fn value(&mut self) -> Result<CborValue, GraphError> {
        let head = *self
            .bytes
            .get(self.cursor)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 1;
        let major = head >> 5;
        let additional = head & 0x1f;
        if major == 7 {
            return match additional {
                20 => Ok(CborValue::Bool(false)),
                21 => Ok(CborValue::Bool(true)),
                22 => Ok(CborValue::Null),
                27 => Ok(CborValue::Float64(self.read_u64()?)),
                _ => Err(GraphError::NonCanonicalData),
            };
        }
        let length = self.argument(additional)?;
        match major {
            0 => Ok(CborValue::Unsigned(length)),
            // Every negative head is an Orna Int; values below -2^64 use tag 3.
            1 => Ok(CborValue::Negative(length)),
            2 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                let end = self
                    .cursor
                    .checked_add(length)
                    .ok_or(GraphError::NonCanonicalData)?;
                let bytes = self
                    .bytes
                    .get(self.cursor..end)
                    .ok_or(GraphError::NonCanonicalData)?
                    .to_vec();
                self.cursor = end;
                Ok(CborValue::Bytes(bytes))
            }
            3 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                let end = self
                    .cursor
                    .checked_add(length)
                    .ok_or(GraphError::NonCanonicalData)?;
                let text = std::str::from_utf8(
                    self.bytes
                        .get(self.cursor..end)
                        .ok_or(GraphError::NonCanonicalData)?,
                )
                .map_err(|_| GraphError::NonCanonicalData)?
                .to_owned();
                self.cursor = end;
                Ok(CborValue::Text(text))
            }
            4 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                if length > NODE_DATA_LIMIT {
                    return Err(GraphError::NodeDataTooLarge(length));
                }
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(self.value()?);
                }
                Ok(CborValue::Array(values))
            }
            5 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                if length > NODE_DATA_LIMIT {
                    return Err(GraphError::NodeDataTooLarge(length));
                }
                let mut entries = Vec::with_capacity(length);
                let mut previous_key = None;
                for _ in 0..length {
                    let key_start = self.cursor;
                    let key = self.value()?;
                    let key_bytes = self.bytes[key_start..self.cursor].to_vec();
                    if previous_key
                        .as_ref()
                        .is_some_and(|previous| previous >= &key_bytes)
                    {
                        return Err(GraphError::NonCanonicalData);
                    }
                    previous_key = Some(key_bytes);
                    entries.push((key, self.value()?));
                }
                Ok(CborValue::Map(entries))
            }
            6 => {
                if !orna_value::is_format3_node_tag(length) {
                    return Err(GraphError::NonCanonicalData);
                }
                let payload = self.value()?;
                // A bignum spells only an Int beyond the 64-bit heads: at least
                // nine big-endian bytes without a leading zero.
                if matches!(length, 2 | 3)
                    && !matches!(&payload, CborValue::Bytes(bytes) if bytes.len() >= 9 && bytes[0] != 0)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                Ok(CborValue::Tag(length, Box::new(payload)))
            }
            _ => Err(GraphError::NonCanonicalData),
        }
    }

    fn argument(&mut self, additional: u8) -> Result<u64, GraphError> {
        let (value, width) = match additional {
            0..=23 => (u64::from(additional), 0),
            24 => (u64::from(self.read_u8()?), 1),
            25 => (u64::from(self.read_u16()?), 2),
            26 => (u64::from(self.read_u32()?), 4),
            27 => (self.read_u64()?, 8),
            _ => return Err(GraphError::NonCanonicalData),
        };
        if width != 0
            && ((width == 1 && value < 24)
                || (width == 2 && value <= u64::from(u8::MAX))
                || (width == 4 && value <= u64::from(u16::MAX))
                || (width == 8 && value <= u64::from(u32::MAX)))
        {
            return Err(GraphError::NonCanonicalData);
        }
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, GraphError> {
        let value = *self
            .bytes
            .get(self.cursor)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 1;
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 2)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 2;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 4;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 8)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 8;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

pub(crate) fn canonical_value_bytes(value: &CborValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
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
    let mut reader = CborReader::new(bytes);
    let value = reader.value()?;
    if !reader.is_finished() || canonical_value_bytes(&value) != bytes {
        return Err(GraphError::NonCanonicalData);
    }
    Ok(value)
}

pub(super) fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => {
            output.extend_from_slice(&[prefix | 24, value as u8]);
        }
        256..=65_535 => {
            output.push(prefix | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push(prefix | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(prefix | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
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

fn head(output: &mut Vec<u8>, major: u8, value: u64) {
    let base = major << 5;
    match value {
        0..=23 => output.push(base | value as u8),
        24..=255 => {
            output.push(base | 24);
            output.push(value as u8);
        }
        256..=65_535 => {
            output.push(base | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push(base | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(base | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// Decodes one node envelope's data bytes into the node it describes. Every
/// field is checked against the format before it is returned, so a node that
/// decodes here is one the format admits.
pub(super) fn decode_node(
    value: CborValue,
    algorithm: GitHashAlgorithm,
) -> Result<NodeData, GraphError> {
    let fields = array_fields(value)?;
    let version = unsigned(fields.first())?;
    if version != 1 {
        return Err(GraphError::NonCanonicalData);
    }
    let kind = NodeKind::from_number(unsigned(fields.get(1))?)?;
    match kind {
        NodeKind::StoreRoot => Ok(NodeData::StoreRoot {
            relation_map: oid(bytes_field(fields.get(2))?, algorithm)?,
        }),
        NodeKind::OrderedLeaf => {
            let domain = canonical_value_bytes(fields.get(2).ok_or(GraphError::NonCanonicalData)?);
            check_domain(&domain)?;
            let entries =
                array_fields(fields.get(3).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| {
                        let fields = array_fields(value)?;
                        if fields.len() != 2 {
                            return Err(GraphError::NonCanonicalData);
                        }
                        Ok(OrderedLeafEntry {
                            key: canonical_value_bytes(
                                fields.first().ok_or(GraphError::NonCanonicalData)?,
                            ),
                            value: canonical_value_bytes(
                                fields.get(1).ok_or(GraphError::NonCanonicalData)?,
                            ),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::OrderedLeaf { domain, entries })
        }
        NodeKind::OrderedBranch => {
            let domain = canonical_value_bytes(fields.get(2).ok_or(GraphError::NonCanonicalData)?);
            check_domain(&domain)?;
            let height = bounded_height(unsigned(fields.get(3))?)?;
            let entries =
                array_fields(fields.get(4).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| {
                        let fields = array_fields(value)?;
                        if fields.len() != 3 {
                            return Err(GraphError::NonCanonicalData);
                        }
                        let row_count = unsigned(fields.get(2))?;
                        check_length(row_count)?;
                        Ok(OrderedBranchEntry {
                            inclusive_max_key: canonical_value_bytes(
                                fields.first().ok_or(GraphError::NonCanonicalData)?,
                            ),
                            child: oid(bytes_field(fields.get(1))?, algorithm)?,
                            row_count,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::OrderedBranch {
                domain,
                height,
                entries,
            })
        }
        NodeKind::ValueOverflow => Ok(NodeData::ValueOverflow {
            encoded_length: checked_length(unsigned(fields.get(2))?)?,
            semantic_digest: digest(bytes_field(fields.get(3))?)?,
            byte_root: oid(bytes_field(fields.get(4))?, algorithm)?,
            dependency_root: match fields.get(5).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(oid(bytes, algorithm)?),
                _ => return Err(GraphError::NonCanonicalData),
            },
        }),
        NodeKind::ByteIndex => {
            let height = bounded_height(unsigned(fields.get(2))?)?;
            let total_length = checked_length(unsigned(fields.get(3))?)?;
            let entries = decode_byte_entries(fields.get(4), algorithm)?;
            Ok(NodeData::ByteIndex {
                height,
                total_length,
                entries,
            })
        }
        NodeKind::BlobDescriptor => {
            let length = checked_length(unsigned(fields.get(2))?)?;
            let raw_sha256 = digest(bytes_field(fields.get(3))?)?;
            let byte_root = match fields.get(4).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(oid(bytes, algorithm)?),
                _ => return Err(GraphError::NonCanonicalData),
            };
            if (length == 0) != byte_root.is_none() {
                return Err(GraphError::InvalidEmptyBlob);
            }
            Ok(NodeData::BlobDescriptor {
                length,
                raw_sha256,
                byte_root,
            })
        }
        NodeKind::Schema => Ok(NodeData::Schema {
            encoded_length: checked_length(unsigned(fields.get(2))?)?,
            schema_digest: digest(bytes_field(fields.get(3))?)?,
            byte_root: oid(bytes_field(fields.get(4))?, algorithm)?,
        }),
        NodeKind::DependencyIndex => {
            let height = bounded_height(unsigned(fields.get(2))?)?;
            let children =
                array_fields(fields.get(3).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| oid(bytes_value(&value)?, algorithm))
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::DependencyIndex { height, children })
        }
    }
}

fn decode_byte_entries(
    value: Option<&CborValue>,
    algorithm: GitHashAlgorithm,
) -> Result<Vec<ByteIndexEntry>, GraphError> {
    let entries = array_fields(value.cloned().ok_or(GraphError::NonCanonicalData)?)?
        .into_iter()
        .map(|value| {
            let fields = array_fields(value)?;
            if fields.len() != 3 {
                return Err(GraphError::NonCanonicalData);
            }
            let chunk_sha256 = match fields.get(2).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(digest(bytes)?),
                _ => return Err(GraphError::NonCanonicalData),
            };
            Ok(ByteIndexEntry {
                span_length: unsigned(fields.first())?,
                target: oid(bytes_field(fields.get(1))?, algorithm)?,
                chunk_sha256,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries)
}

fn array_fields(value: CborValue) -> Result<Vec<CborValue>, GraphError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn unsigned(value: Option<&CborValue>) -> Result<u64, GraphError> {
    match value.ok_or(GraphError::NonCanonicalData)? {
        CborValue::Unsigned(value) => Ok(*value),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn bytes_value(value: &CborValue) -> Result<&[u8], GraphError> {
    match value {
        CborValue::Bytes(value) => Ok(value),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn bytes_field(value: Option<&CborValue>) -> Result<&[u8], GraphError> {
    bytes_value(value.ok_or(GraphError::NonCanonicalData)?)
}

fn digest(value: &[u8]) -> Result<[u8; 32], GraphError> {
    value
        .try_into()
        .map_err(|_| GraphError::InvalidDigestLength)
}

fn oid(value: &[u8], algorithm: GitHashAlgorithm) -> Result<NativeObjectId, GraphError> {
    NativeObjectId::new(algorithm, value)
}

fn bounded_height(value: u64) -> Result<u8, GraphError> {
    let height = u8::try_from(value).map_err(|_| GraphError::HeightExceeded(u8::MAX))?;
    check_height(height)?;
    Ok(height)
}

fn checked_length(value: u64) -> Result<u64, GraphError> {
    check_length(value)?;
    Ok(value)
}
