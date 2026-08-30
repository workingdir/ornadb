//! Canonical `ORNA-ROWS/1` encoding and decoding.

use super::*;
/// An error from canonical `ORNA-ROWS/1` encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowsCodecError {
    /// The complete Rows frame exceeds the shared opaque payload bound.
    PayloadTooLarge { actual: usize, maximum: usize },
    /// The frame does not start with the exact Rows magic.
    InvalidMagic,
    /// The frame version is not supported.
    UnsupportedVersion(u16),
    /// The frame ended before a complete field was available.
    Truncated,
    /// Bytes remained after the complete Rows frame.
    TrailingBytes,
    /// The column count is outside the accepted bound.
    ColumnCountExceeded { actual: usize, maximum: usize },
    /// The row count is outside the accepted bound.
    RowCountExceeded { actual: usize, maximum: usize },
    /// The cell count is outside the accepted bound.
    CellCountExceeded { actual: usize, maximum: usize },
    /// The checked row/column product exceeds the accepted bound.
    CellProductExceeded {
        rows: usize,
        columns: usize,
        maximum: usize,
    },
    /// One column name is not valid UTF-8.
    InvalidColumnNameUtf8 { column: usize },
    /// One column name is empty.
    EmptyColumnName { column: usize },
    /// One column name repeats an earlier exact byte name.
    DuplicateColumnName { first: usize, duplicate: usize },
    /// A column type form is not part of ORNA-ROWS/1.
    UnknownTypeForm { column: usize, type_form: u8 },
    /// A column nullable byte is not zero or one.
    InvalidNullable { column: usize, value: u8 },
    /// A declared column type is not active in the supplied revision.
    InactiveType {
        column: usize,
        type_form: u8,
        type_id: TypeId,
    },
    /// A declared value type is opaque and therefore cannot be a Rows cell.
    OpaqueColumnType { column: usize, type_id: TypeId },
    /// A row's cell count differs from the declared column count.
    RowWidthMismatch {
        row: usize,
        expected: usize,
        actual: usize,
    },
    /// A cell's ORV5 marker is not the exact ORV5 marker.
    InvalidCellMarker { row: usize, column: usize },
    /// A decoded cell does not re-encode to its exact supplied ORV5 bytes.
    NonCanonicalCell { row: usize, column: usize },
    /// An ORV5 cell could not be decoded against the active revision.
    CellValue {
        row: usize,
        column: usize,
        source: ValueCodecError,
    },
    /// A decoded cell violates the declared ResultRows shape.
    ResultRows { source: ResultRowsError },
    /// The Rows value cannot be registered under the active standard snapshot.
    OpaqueValue { source: OpaqueValueError },
}

impl fmt::Display for RowsCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLarge { .. } => formatter.write_str("Rows payload exceeds its bound"),
            Self::InvalidMagic => formatter.write_str("invalid ORNA-ROWS/1 magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported ORNA-ROWS version {version}")
            }
            Self::Truncated => formatter.write_str("truncated ORNA-ROWS frame"),
            Self::TrailingBytes => formatter.write_str("trailing bytes after ORNA-ROWS frame"),
            Self::ColumnCountExceeded { .. } => {
                formatter.write_str("Rows column count exceeds its bound")
            }
            Self::RowCountExceeded { .. } => {
                formatter.write_str("Rows row count exceeds its bound")
            }
            Self::CellCountExceeded { .. } => {
                formatter.write_str("Rows cell count exceeds its bound")
            }
            Self::CellProductExceeded { .. } => {
                formatter.write_str("Rows cell product exceeds its bound")
            }
            Self::InvalidColumnNameUtf8 { .. } => {
                formatter.write_str("Rows column name is not valid UTF-8")
            }
            Self::EmptyColumnName { .. } => formatter.write_str("Rows column name is empty"),
            Self::DuplicateColumnName { .. } => {
                formatter.write_str("Rows column name is duplicated")
            }
            Self::UnknownTypeForm { .. } => formatter.write_str("Rows column type form is unknown"),
            Self::InvalidNullable { .. } => formatter.write_str("Rows nullable flag is invalid"),
            Self::InactiveType { .. } => formatter.write_str("Rows column type is not active"),
            Self::OpaqueColumnType { .. } => {
                formatter.write_str("opaque value types are not valid Rows columns")
            }
            Self::RowWidthMismatch { .. } => formatter.write_str("Rows row width does not match"),
            Self::InvalidCellMarker { .. } => formatter.write_str("Rows cell is not ORV5"),
            Self::NonCanonicalCell { .. } => formatter.write_str("Rows cell is not canonical ORV5"),
            Self::CellValue { source, .. } => write!(formatter, "Rows cell is invalid: {source}"),
            Self::ResultRows { source } => source.fmt(formatter),
            Self::OpaqueValue { source } => source.fmt(formatter),
        }
    }
}

impl Error for RowsCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CellValue { source, .. } => Some(source),
            Self::ResultRows { source } => Some(source),
            Self::OpaqueValue { source } => Some(source),
            _ => None,
        }
    }
}

/// Encodes one complete `ResultRows` value as the canonical `ORNA-ROWS/1`
/// payload and verifies it against the registered V8 Rows codec.
pub fn encode_rows(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &ResultRows,
) -> Result<Vec<u8>, RowsCodecError> {
    let columns = rows.columns();
    let row_values = rows.rows();
    validate_rows_shape_bounds(columns.len(), row_values.len())?;

    let mut writer = RowsWriter::new();
    writer.bytes(orna_core::value::ROWS_MAGIC)?;
    writer.u16(orna_core::value::ROWS_FRAME_VERSION)?;
    writer.u32(u32::try_from(columns.len()).map_err(|_| {
        RowsCodecError::ColumnCountExceeded {
            actual: columns.len(),
            maximum: MAX_ROWS_COLUMNS,
        }
    })?)?;
    for (column_index, column) in columns.iter().enumerate() {
        let name = column.name().as_bytes();
        writer.u32(
            u32::try_from(name.len()).map_err(|_| RowsCodecError::PayloadTooLarge {
                actual: name.len(),
                maximum: MAX_ROWS_PAYLOAD_LENGTH,
            })?,
        )?;
        writer.bytes(name)?;
        let (type_form, type_id) = rows_type_wire(active, column.resolved_type(), column_index)?;
        writer.byte(type_form)?;
        writer.bytes(&type_id.to_bytes())?;
        writer.byte(u8::from(column.nullable()))?;
    }
    writer.u32(u32::try_from(row_values.len()).map_err(|_| {
        RowsCodecError::RowCountExceeded {
            actual: row_values.len(),
            maximum: MAX_ROWS_ROWS,
        }
    })?)?;

    for (row_index, row) in row_values.iter().enumerate() {
        if row.values().len() != columns.len() {
            return Err(RowsCodecError::RowWidthMismatch {
                row: row_index,
                expected: columns.len(),
                actual: row.values().len(),
            });
        }
        writer.u32(u32::try_from(row.values().len()).map_err(|_| {
            RowsCodecError::CellCountExceeded {
                actual: row.values().len(),
                maximum: MAX_ROWS_COLUMNS,
            }
        })?)?;
        for (column_index, (column, value)) in columns.iter().zip(row.values()).enumerate() {
            validate_rows_value(active, column, value, row_index, column_index)?;
            let encoded = encode_constructed_value(active, registry, value).map_err(|source| {
                RowsCodecError::CellValue {
                    row: row_index,
                    column: column_index,
                    source,
                }
            })?;
            if encoded.len() > MAX_ROWS_PAYLOAD_LENGTH {
                return Err(RowsCodecError::PayloadTooLarge {
                    actual: encoded.len(),
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                });
            }
            writer.u32(u32::try_from(encoded.len()).map_err(|_| {
                RowsCodecError::PayloadTooLarge {
                    actual: encoded.len(),
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                }
            })?)?;
            writer.bytes(&encoded)?;
        }
    }

    let payload = writer.finish();
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, &payload)
        .map_err(|source| RowsCodecError::OpaqueValue { source })?;
    Ok(payload)
}

/// Wraps a canonical Rows payload as one registered opaque runtime value.
pub fn encode_rows_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &ResultRows,
) -> Result<RuntimeValue, RowsCodecError> {
    let payload = encode_rows(active, registry, rows)?;
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|source| RowsCodecError::OpaqueValue { source })
}

/// Decodes and validates one complete canonical `ORNA-ROWS/1` payload into
/// the existing immutable [`ResultRows`] model.
pub fn decode_rows(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ResultRows, RowsCodecError> {
    if encoded.len() > MAX_ROWS_PAYLOAD_LENGTH {
        return Err(RowsCodecError::PayloadTooLarge {
            actual: encoded.len(),
            maximum: MAX_ROWS_PAYLOAD_LENGTH,
        });
    }
    let mut reader = RowsReader::new(encoded);
    let magic = reader.take(orna_core::value::ROWS_MAGIC.len())?;
    if magic != orna_core::value::ROWS_MAGIC {
        return Err(RowsCodecError::InvalidMagic);
    }
    let version = reader.u16()?;
    if version != orna_core::value::ROWS_FRAME_VERSION {
        return Err(RowsCodecError::UnsupportedVersion(version));
    }

    let column_count = reader.usize_u32()?;
    if !(1..=MAX_ROWS_COLUMNS).contains(&column_count) {
        return Err(RowsCodecError::ColumnCountExceeded {
            actual: column_count,
            maximum: MAX_ROWS_COLUMNS,
        });
    }
    let minimum_columns = column_count
        .checked_mul(ROWS_COLUMN_MIN_BYTES)
        .and_then(|bytes| bytes.checked_add(4));
    if minimum_columns.is_none_or(|minimum| reader.remaining() < minimum) {
        return Err(RowsCodecError::Truncated);
    }
    let mut names = BTreeMap::new();
    let mut columns = Vec::with_capacity(column_count);
    for column_index in 0..column_count {
        let name_length = reader.usize_u32()?;
        let name_bytes = reader.take(name_length)?;
        let name =
            std::str::from_utf8(name_bytes).map_err(|_| RowsCodecError::InvalidColumnNameUtf8 {
                column: column_index,
            })?;
        if name.is_empty() {
            return Err(RowsCodecError::EmptyColumnName {
                column: column_index,
            });
        }
        if let Some(first) = names.insert(name.to_owned(), column_index) {
            return Err(RowsCodecError::DuplicateColumnName {
                first,
                duplicate: column_index,
            });
        }
        let type_form = reader.byte()?;
        let type_id = TypeId::from_bytes(reader.array::<16>()?);
        let nullable = reader.byte()?;
        if nullable > 1 {
            return Err(RowsCodecError::InvalidNullable {
                column: column_index,
                value: nullable,
            });
        }
        let resolved_type = rows_type_resolved(active, type_form, type_id, column_index)?;
        let column = ResultColumn::new(name, resolved_type, nullable == 1)
            .map_err(|source| RowsCodecError::ResultRows { source })?;
        columns.push(column);
    }

    let row_count = reader.usize_u32()?;
    if row_count > MAX_ROWS_ROWS {
        return Err(RowsCodecError::RowCountExceeded {
            actual: row_count,
            maximum: MAX_ROWS_ROWS,
        });
    }
    if row_count
        .checked_mul(column_count)
        .is_none_or(|cells| cells > MAX_ROWS_CELLS)
    {
        return Err(RowsCodecError::CellProductExceeded {
            rows: row_count,
            columns: column_count,
            maximum: MAX_ROWS_CELLS,
        });
    }

    let mut rows = Vec::with_capacity(row_count);
    for row_index in 0..row_count {
        let cell_count = reader.usize_u32()?;
        if cell_count != column_count {
            return Err(RowsCodecError::RowWidthMismatch {
                row: row_index,
                expected: column_count,
                actual: cell_count,
            });
        }
        let mut values = Vec::with_capacity(cell_count);
        for column_index in 0..cell_count {
            let length = reader.usize_u32()?;
            if length > MAX_ROWS_PAYLOAD_LENGTH {
                return Err(RowsCodecError::PayloadTooLarge {
                    actual: length,
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                });
            }
            let cell = reader.take(length)?;
            if cell.get(..4) != Some(b"ORV5".as_slice()) {
                return Err(RowsCodecError::InvalidCellMarker {
                    row: row_index,
                    column: column_index,
                });
            }
            let value = decode_constructed_value(active, registry, cell).map_err(|source| {
                RowsCodecError::CellValue {
                    row: row_index,
                    column: column_index,
                    source,
                }
            })?;
            let canonical =
                encode_constructed_value(active, registry, &value).map_err(|source| {
                    RowsCodecError::CellValue {
                        row: row_index,
                        column: column_index,
                        source,
                    }
                })?;
            if canonical != cell {
                return Err(RowsCodecError::NonCanonicalCell {
                    row: row_index,
                    column: column_index,
                });
            }
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    reader.require_finished()?;

    let result =
        ResultRows::new(columns, rows).map_err(|source| RowsCodecError::ResultRows { source })?;
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, encoded)
        .map_err(|source| RowsCodecError::OpaqueValue { source })?;
    Ok(result)
}

fn validate_rows_shape_bounds(columns: usize, rows: usize) -> Result<(), RowsCodecError> {
    if !(1..=MAX_ROWS_COLUMNS).contains(&columns) {
        return Err(RowsCodecError::ColumnCountExceeded {
            actual: columns,
            maximum: MAX_ROWS_COLUMNS,
        });
    }
    if rows > MAX_ROWS_ROWS {
        return Err(RowsCodecError::RowCountExceeded {
            actual: rows,
            maximum: MAX_ROWS_ROWS,
        });
    }
    if rows
        .checked_mul(columns)
        .is_none_or(|cells| cells > MAX_ROWS_CELLS)
    {
        return Err(RowsCodecError::CellProductExceeded {
            rows,
            columns,
            maximum: MAX_ROWS_CELLS,
        });
    }
    Ok(())
}

fn rows_type_wire(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
    column: usize,
) -> Result<(u8, TypeId), RowsCodecError> {
    let (type_form, type_id) = match resolved_type {
        ResolvedType::Scalar(scalar) => (
            0x01,
            supported_scalar_type_id(scalar).ok_or(RowsCodecError::InactiveType {
                column,
                type_form: 0x01,
                type_id: TypeId::from_bytes([0; 16]),
            })?,
        ),
        ResolvedType::Named(type_id) => (0x02, type_id),
        ResolvedType::Reference { target } => (0x03, target),
        ResolvedType::Value(type_id) => (0x04, type_id),
    };
    validate_rows_declared_type(active, type_form, type_id, column)?;
    Ok((type_form, type_id))
}

fn rows_type_resolved(
    active: &ActiveDatabaseRevision,
    type_form: u8,
    type_id: TypeId,
    column: usize,
) -> Result<ResolvedType, RowsCodecError> {
    validate_rows_declared_type(active, type_form, type_id, column)?;
    match type_form {
        0x01 => supported_scalar_from_type_id(type_id)
            .map(ResolvedType::scalar)
            .ok_or(RowsCodecError::InactiveType {
                column,
                type_form,
                type_id,
            }),
        0x02 => Ok(ResolvedType::named(type_id)),
        0x03 => Ok(ResolvedType::reference(type_id)),
        0x04 => Ok(ResolvedType::value(type_id)),
        _ => Err(RowsCodecError::UnknownTypeForm { column, type_form }),
    }
}

fn validate_rows_declared_type(
    active: &ActiveDatabaseRevision,
    type_form: u8,
    type_id: TypeId,
    column: usize,
) -> Result<(), RowsCodecError> {
    match type_form {
        0x01 => {
            if supported_scalar_from_type_id(type_id).is_none() {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x02 => {
            let active_named = active.catalogue().enum_type_by_id(type_id).is_some()
                || active
                    .catalogue()
                    .record_value_type_by_id(type_id)
                    .is_some()
                || active
                    .catalogue_hash_context()
                    .standard()
                    .is_some_and(|standard| {
                        standard.catalogue().enum_type_by_id(type_id).is_some()
                            || standard
                                .catalogue()
                                .record_value_type_by_id(type_id)
                                .is_some()
                    });
            if !active_named {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x03 => {
            let active_reference = active.catalogue().object_type_by_id(type_id).is_some()
                || active
                    .catalogue_hash_context()
                    .standard()
                    .is_some_and(|standard| {
                        standard.catalogue().object_type_by_id(type_id).is_some()
                    });
            if !active_reference {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x04 => {
            let definition = active.catalogue().value_type_by_id(type_id).or_else(|| {
                active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
            });
            let Some(definition) = definition else {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            };
            if definition.kind() == ValueTypeKind::Opaque {
                return Err(RowsCodecError::OpaqueColumnType { column, type_id });
            }
        }
        _ => return Err(RowsCodecError::UnknownTypeForm { column, type_form }),
    }
    Ok(())
}

fn validate_rows_value(
    active: &ActiveDatabaseRevision,
    column: &ResultColumn,
    value: &RuntimeValue,
    row: usize,
    column_index: usize,
) -> Result<(), RowsCodecError> {
    if let RuntimeValue::Opaque(opaque) = value {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::OpaqueValueNotAccepted {
                row,
                column: column_index,
                opaque_type: opaque.opaque_type(),
            },
        });
    }
    if let Some(carrier) = orna_core::invocation::invocation_carrier_kind(value) {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::InvocationCarrierNotAccepted {
                row,
                column: column_index,
                carrier,
            },
        });
    }
    if let RuntimeValue::Constructed(constructed) = value {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::ConstructedValueNotAccepted {
                row,
                column: column_index,
                descriptor: constructed.descriptor().clone(),
            },
        });
    }
    if value.is_null() && !column.nullable() {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::NullInNonNullableColumn {
                row,
                column: column_index,
            },
        });
    }
    let RuntimeType::Flat(actual) = value.runtime_type() else {
        unreachable!("constructed values are rejected above");
    };
    if actual != column.resolved_type() {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::ValueTypeMismatch {
                row,
                column: column_index,
                expected: column.resolved_type(),
                actual,
            },
        });
    }
    let (type_form, type_id) = rows_type_wire(active, column.resolved_type(), column_index)?;
    match type_form {
        0x02 if active.catalogue().enum_type_by_id(type_id).is_none()
            && active
                .catalogue()
                .record_value_type_by_id(type_id)
                .is_none()
            && active
                .catalogue_hash_context()
                .standard()
                .is_none_or(|standard| {
                    standard.catalogue().enum_type_by_id(type_id).is_none()
                        && standard
                            .catalogue()
                            .record_value_type_by_id(type_id)
                            .is_none()
                }) =>
        {
            return Err(RowsCodecError::InactiveType {
                column: column_index,
                type_form,
                type_id,
            });
        }
        _ => {}
    }
    Ok(())
}

struct RowsWriter {
    bytes: Vec<u8>,
}

impl RowsWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), RowsCodecError> {
        let next =
            self.bytes
                .len()
                .checked_add(value.len())
                .ok_or(RowsCodecError::PayloadTooLarge {
                    actual: usize::MAX,
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                })?;
        if next > MAX_ROWS_PAYLOAD_LENGTH {
            return Err(RowsCodecError::PayloadTooLarge {
                actual: next,
                maximum: MAX_ROWS_PAYLOAD_LENGTH,
            });
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn byte(&mut self, value: u8) -> Result<(), RowsCodecError> {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), RowsCodecError> {
        self.bytes(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), RowsCodecError> {
        self.bytes(&value.to_be_bytes())
    }
}

struct RowsReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> RowsReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], RowsCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RowsCodecError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(RowsCodecError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], RowsCodecError> {
        self.take(LENGTH)?
            .try_into()
            .map_err(|_| RowsCodecError::Truncated)
    }

    fn byte(&mut self) -> Result<u8, RowsCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, RowsCodecError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn usize_u32(&mut self) -> Result<usize, RowsCodecError> {
        usize::try_from(u32::from_be_bytes(self.array()?)).map_err(|_| RowsCodecError::Truncated)
    }

    fn require_finished(&self) -> Result<(), RowsCodecError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(RowsCodecError::TrailingBytes)
        }
    }
}
