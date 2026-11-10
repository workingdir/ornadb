//! Compact Parquet page framing validation.
//!
//! The validator walks the serialized page headers of one already-opened
//! Parquet file and enforces the compact profile's uncompressed-page ceiling
//! and checksum requirement. It is byte-level and has no repository
//! dependency.

use super::CompactError;
use super::model::COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES;

/// Validates the complete compact page framing required at a storage-reader
/// boundary. This is intentionally shared with repository admission so a
/// direct reader cannot accept a weaker Parquet profile than publication.
pub fn validate_compact_page_uncompressed_sizes(
    bytes: &[u8],
    row_groups: &[parquet::file::metadata::RowGroupMetaData],
) -> Result<(), CompactError> {
    scan_compact_page_headers(bytes, row_groups, true, true)
}

#[doc(hidden)]
pub fn scan_compact_page_headers(
    bytes: &[u8],
    row_groups: &[parquet::file::metadata::RowGroupMetaData],
    require_checksums: bool,
    require_data_page_v2: bool,
) -> Result<(), CompactError> {
    for row_group in row_groups {
        let mut row_group_uncompressed_len = 0usize;
        for column in row_group.columns() {
            let start = column
                .dictionary_page_offset()
                .unwrap_or_else(|| column.data_page_offset());
            let length = column.compressed_size();
            let start = usize::try_from(start).map_err(|_| CompactError::InvalidManifest)?;
            let length = usize::try_from(length).map_err(|_| CompactError::InvalidManifest)?;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(CompactError::InvalidManifest)?;
            let mut cursor = start;
            while cursor < end {
                let header = parse_compact_page_header(&bytes[cursor..end])?;
                if (require_checksums && !header.has_checksum)
                    || header.uncompressed_len > COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES
                    || (require_data_page_v2 && !matches!(header.page_type, 2 | 3))
                {
                    return Err(CompactError::InvalidManifest);
                }
                row_group_uncompressed_len = row_group_uncompressed_len
                    .checked_add(header.uncompressed_len)
                    .filter(|size| *size <= COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES)
                    .ok_or(CompactError::InvalidManifest)?;
                cursor = cursor
                    .checked_add(header.encoded_len)
                    .and_then(|value| value.checked_add(header.compressed_len))
                    .filter(|value| *value <= end)
                    .ok_or(CompactError::InvalidManifest)?;
            }
            if cursor != end {
                return Err(CompactError::InvalidManifest);
            }
        }
    }
    Ok(())
}

struct CompactPageHeader {
    encoded_len: usize,
    compressed_len: usize,
    uncompressed_len: usize,
    has_checksum: bool,
    page_type: i32,
}

fn parse_compact_page_header(bytes: &[u8]) -> Result<CompactPageHeader, CompactError> {
    let mut cursor = 0;
    let mut last_field = 0_i16;
    let mut compressed_len = None;
    let mut uncompressed_len = None;
    let mut has_checksum = false;
    let mut page_type = None;
    loop {
        let tag = take_compact_byte(bytes, &mut cursor)?;
        let kind = tag & 0x0f;
        if kind == 0 {
            break;
        }
        let delta = i16::from(tag >> 4);
        let field = if delta == 0 {
            decode_compact_i16(bytes, &mut cursor)?
        } else {
            last_field
                .checked_add(delta)
                .ok_or(CompactError::InvalidManifest)?
        };
        if field <= last_field {
            return Err(CompactError::InvalidManifest);
        }
        last_field = field;
        if field == 1 && kind == 5 {
            page_type = Some(decode_compact_i32(bytes, &mut cursor)?);
        } else if field == 2 && kind == 5 {
            let value = decode_compact_i32(bytes, &mut cursor)?;
            uncompressed_len =
                Some(usize::try_from(value).map_err(|_| CompactError::InvalidManifest)?);
        } else if field == 3 && kind == 5 {
            let value = decode_compact_i32(bytes, &mut cursor)?;
            compressed_len =
                Some(usize::try_from(value).map_err(|_| CompactError::InvalidManifest)?);
        } else {
            if field == 4 && kind == 5 {
                has_checksum = true;
            }
            skip_compact_value(bytes, &mut cursor, kind, 0)?;
        }
    }
    Ok(CompactPageHeader {
        encoded_len: cursor,
        compressed_len: compressed_len.ok_or(CompactError::InvalidManifest)?,
        uncompressed_len: uncompressed_len.ok_or(CompactError::InvalidManifest)?,
        has_checksum,
        page_type: page_type.ok_or(CompactError::InvalidManifest)?,
    })
}

#[doc(hidden)]
pub fn skip_compact_value(
    bytes: &[u8],
    cursor: &mut usize,
    kind: u8,
    depth: usize,
) -> Result<(), CompactError> {
    if depth > 64 {
        return Err(CompactError::InvalidManifest);
    }
    match kind {
        1 | 2 => Ok(()),
        3 => {
            take_compact_byte(bytes, cursor)?;
            Ok(())
        }
        4..=6 => {
            let _ = decode_compact_varint(bytes, cursor)?;
            Ok(())
        }
        7 => {
            let end = cursor
                .checked_add(8)
                .filter(|end| *end <= bytes.len())
                .ok_or(CompactError::InvalidManifest)?;
            *cursor = end;
            Ok(())
        }
        8 => {
            let length = usize::try_from(decode_compact_varint(bytes, cursor)?)
                .map_err(|_| CompactError::InvalidManifest)?;
            let end = cursor
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(CompactError::InvalidManifest)?;
            *cursor = end;
            Ok(())
        }
        9 | 10 => {
            let size_and_kind = take_compact_byte(bytes, cursor)?;
            let size = if size_and_kind >> 4 == 15 {
                usize::try_from(decode_compact_varint(bytes, cursor)?)
                    .map_err(|_| CompactError::InvalidManifest)?
            } else {
                usize::from(size_and_kind >> 4)
            };
            let item_kind = size_and_kind & 0x0f;
            if size > bytes.len() {
                return Err(CompactError::InvalidManifest);
            }
            for _ in 0..size {
                skip_compact_value(bytes, cursor, item_kind, depth + 1)?;
            }
            Ok(())
        }
        11 => {
            let size = usize::try_from(decode_compact_varint(bytes, cursor)?)
                .map_err(|_| CompactError::InvalidManifest)?;
            if size == 0 {
                return Ok(());
            }
            let kinds = take_compact_byte(bytes, cursor)?;
            let key_kind = kinds >> 4;
            let value_kind = kinds & 0x0f;
            for _ in 0..size {
                skip_compact_value(bytes, cursor, key_kind, depth + 1)?;
                skip_compact_value(bytes, cursor, value_kind, depth + 1)?;
            }
            Ok(())
        }
        12 => skip_compact_struct(bytes, cursor, depth + 1),
        _ => Err(CompactError::InvalidManifest),
    }
}

fn skip_compact_struct(bytes: &[u8], cursor: &mut usize, depth: usize) -> Result<(), CompactError> {
    let mut last_field = 0_i16;
    loop {
        let tag = take_compact_byte(bytes, cursor)?;
        let kind = tag & 0x0f;
        if kind == 0 {
            return Ok(());
        }
        let delta = i16::from(tag >> 4);
        let field = if delta == 0 {
            decode_compact_i16(bytes, cursor)?
        } else {
            last_field
                .checked_add(delta)
                .ok_or(CompactError::InvalidManifest)?
        };
        if field <= last_field {
            return Err(CompactError::InvalidManifest);
        }
        last_field = field;
        skip_compact_value(bytes, cursor, kind, depth)?;
    }
}

#[doc(hidden)]
pub fn take_compact_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, CompactError> {
    let value = *bytes.get(*cursor).ok_or(CompactError::InvalidManifest)?;
    *cursor = cursor.checked_add(1).ok_or(CompactError::InvalidManifest)?;
    Ok(value)
}

fn decode_compact_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, CompactError> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = take_compact_byte(bytes, cursor)?;
        value |= u64::from(byte & 0x7f)
            .checked_shl(shift)
            .ok_or(CompactError::InvalidManifest)?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CompactError::InvalidManifest)
}

#[doc(hidden)]
pub fn decode_compact_i16(bytes: &[u8], cursor: &mut usize) -> Result<i16, CompactError> {
    let value = decode_compact_varint(bytes, cursor)?;
    let value = i64::try_from(value).map_err(|_| CompactError::InvalidManifest)?;
    i16::try_from((value >> 1) ^ -(value & 1)).map_err(|_| CompactError::InvalidManifest)
}

#[doc(hidden)]
pub fn decode_compact_i32(bytes: &[u8], cursor: &mut usize) -> Result<i32, CompactError> {
    let value = decode_compact_varint(bytes, cursor)?;
    let value = i64::try_from(value).map_err(|_| CompactError::InvalidManifest)?;
    i32::try_from((value >> 1) ^ -(value & 1)).map_err(|_| CompactError::InvalidManifest)
}
