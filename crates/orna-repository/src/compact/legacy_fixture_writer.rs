//! Test-only Parquet page-checksum helpers for legacy compact fixtures.
//!
//! Compiled only under `cfg(test)` or the `legacy-fixture-writer` feature.
//! Production builds never contain these helpers (ORNA-UPGRADE-003).

use parquet::basic::PageType;
use parquet::file::{
    metadata::ParquetMetaDataWriter,
    reader::{FileReader, SerializedFileReader},
};
/// One column's data page, with the CRC field its header needs.
struct PatchedPage {
    /// Offset of the page header in the unpatched file.
    start: usize,
    /// `total_compressed_size` of the column chunk in the unpatched file.
    compressed_size: usize,
    /// Offset where the CRC field is spliced in (the position of the header
    /// field that follows the compressed size).
    at: usize,
    /// The header field after the CRC slot, which has to move one tag delta.
    next_field_id: Option<u8>,
    checksum: Vec<u8>,
}

/// Inserts the profile-required page CRC into every column's data page header.
///
/// The compact profile requires a page checksum on every page (1.0.0
/// `23-storage.md:162`), while parquet-rs writes headers without one. Each
/// column of the row group therefore gets its CRC field spliced into its single
/// data page header, and the footer is re-encoded with the shifted page offsets
/// and grown column sizes.
pub(crate) fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
    let reader = SerializedFileReader::new(bytes::Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    assert_eq!(metadata.num_row_groups(), 1);
    let row_group = metadata.row_group(0);
    let data_end = footer_start(&bytes);

    let mut patched = Vec::with_capacity(row_group.num_columns());
    for index in 0..row_group.num_columns() {
        let column = row_group.column(index);
        assert_eq!(
            column.dictionary_page_offset(),
            None,
            "fixture pages are written without a dictionary page"
        );
        let start = usize::try_from(column.data_page_offset()).unwrap();
        let length = usize::try_from(column.compressed_size()).unwrap();
        let header = compact_page_header(&bytes[start..start + length]);
        assert!(!header.has_checksum);
        let body_start = start + header.encoded_len;
        let body_end = body_start + header.compressed_len;
        let checksum = crc32(&bytes[body_start..body_end]) as i32;
        patched.push(PatchedPage {
            start,
            compressed_size: length,
            at: start + header.next_field_offset.unwrap_or(header.stop_offset),
            next_field_id: header.next_field_offset.and(header.next_field_id),
            checksum: compact_crc_field(header.checksum_predecessor, checksum),
        });
    }
    assert!(
        patched
            .iter()
            .all(|page| page.start + page.compressed_size <= data_end),
        "a column chunk runs past the footer"
    );

    let mut data = bytes[..data_end].to_vec();
    let mut shift = 0usize;
    for page in &patched {
        let at = page.at + shift;
        data.splice(at..at, page.checksum.iter().copied());
        if let Some(next_id) = page.next_field_id {
            let position = at + page.checksum.len();
            let delta = next_id.checked_sub(4).unwrap();
            assert!((1..=15).contains(&delta));
            data[position] = (data[position] & 0x0f) | (delta << 4);
        }
        shift += page.checksum.len();
    }

    let mut metadata = metadata.into_builder();
    let mut row_groups = metadata.take_row_groups();
    let row_group = row_groups.pop().unwrap();
    let original_columns = row_group.columns().to_vec();
    let mut row_group = row_group.into_builder();
    row_group.take_columns();
    let mut shift = 0i64;
    let mut columns = Vec::with_capacity(patched.len());
    for (index, page) in patched.iter().enumerate() {
        let checksum_len = i64::try_from(page.checksum.len()).unwrap();
        let compressed_size = i64::try_from(page.compressed_size).unwrap();
        columns.push(
            original_columns[index]
                .clone()
                .into_builder()
                .set_data_page_offset(i64::try_from(page.start).unwrap() + shift)
                .set_total_compressed_size(compressed_size + checksum_len)
                .build()
                .unwrap(),
        );
        shift += checksum_len;
    }
    let row_group = row_group.set_column_metadata(columns).build().unwrap();
    let metadata = metadata.add_row_group(row_group).build();
    let mut footer = Vec::new();
    ParquetMetaDataWriter::new(&mut footer, &metadata)
        .finish()
        .unwrap();
    data.extend(footer);

    let reader = SerializedFileReader::new(bytes::Bytes::from(data.clone())).unwrap();
    assert_eq!(reader.metadata().file_metadata().version(), 1);
    let group = reader.get_row_group(0).unwrap();
    assert_eq!(
        group.num_columns(),
        patched.len(),
        "every column survives the footer rewrite"
    );
    for index in 0..patched.len() {
        let mut pages = group.get_column_page_reader(index).unwrap();
        let page = pages.next().unwrap().unwrap();
        assert!(page.is_data_page());
        assert_eq!(page.page_type(), PageType::DATA_PAGE_V2);
        assert!(pages.next().is_none());
    }
    // The patched headers now advertise the checksum the profile requires.
    let data_region = footer_start(&data);
    for index in 0..patched.len() {
        let column = reader.metadata().row_group(0).column(index);
        let start = usize::try_from(column.data_page_offset()).unwrap();
        let length = usize::try_from(column.compressed_size()).unwrap();
        assert!(start <= data_region && start + length <= data_region);
        let header = compact_page_header(&data[start..start + length]);
        assert!(header.has_checksum, "column {index} has no page checksum");
    }
    data
}

fn footer_start(bytes: &[u8]) -> usize {
    let footer_length =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as usize;
    bytes.len() - 8 - footer_length
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut value = !0_u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(value & 1);
            value = (value >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !value
}

struct CompactPageHeaderFixture {
    encoded_len: usize,
    compressed_len: usize,
    checksum_predecessor: u8,
    next_field_offset: Option<usize>,
    next_field_id: Option<u8>,
    stop_offset: usize,
    has_checksum: bool,
}

fn compact_page_header(bytes: &[u8]) -> CompactPageHeaderFixture {
    let mut cursor = 0;
    let mut previous = 0_u8;
    let mut compressed_len = None;
    let mut next_field_offset = None;
    let mut next_field_id = None;
    let mut checksum_predecessor = None;
    let mut has_checksum = false;
    loop {
        let field_offset = cursor;
        let tag = compact_byte(bytes, &mut cursor);
        let kind = tag & 0x0f;
        if kind == 0 {
            return CompactPageHeaderFixture {
                encoded_len: cursor,
                compressed_len: compressed_len.unwrap(),
                checksum_predecessor: checksum_predecessor.unwrap_or(previous),
                next_field_offset,
                next_field_id,
                stop_offset: field_offset,
                has_checksum,
            };
        }
        let delta = tag >> 4;
        let field = if delta == 0 {
            compact_i16(bytes, &mut cursor) as u8
        } else {
            previous.checked_add(delta).unwrap()
        };
        if field > 4 && next_field_offset.is_none() {
            next_field_offset = Some(field_offset);
            next_field_id = Some(field);
            checksum_predecessor = Some(previous);
        }
        if field == 3 && kind == 5 {
            compressed_len = Some(usize::try_from(compact_i32(bytes, &mut cursor)).unwrap());
        } else {
            if field == 4 && kind == 5 {
                has_checksum = true;
            }
            compact_skip(bytes, &mut cursor, kind);
        }
        previous = field;
    }
}

fn compact_crc_field(previous_field: u8, checksum: i32) -> Vec<u8> {
    assert_eq!(previous_field, 3);
    let mut bytes = vec![0x15];
    compact_varint(
        ((i64::from(checksum) << 1) ^ (i64::from(checksum) >> 31)) as u64,
        &mut bytes,
    );
    bytes
}

fn compact_byte(bytes: &[u8], cursor: &mut usize) -> u8 {
    let value = bytes[*cursor];
    *cursor += 1;
    value
}

fn compact_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn compact_read_varint(bytes: &[u8], cursor: &mut usize) -> u64 {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = compact_byte(bytes, cursor);
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
    }
    panic!("invalid compact fixture varint");
}

fn compact_i16(bytes: &[u8], cursor: &mut usize) -> i16 {
    let value = compact_read_varint(bytes, cursor) as i64;
    ((value >> 1) ^ -(value & 1)).try_into().unwrap()
}

fn compact_i32(bytes: &[u8], cursor: &mut usize) -> i32 {
    let value = compact_read_varint(bytes, cursor) as i64;
    ((value >> 1) ^ -(value & 1)).try_into().unwrap()
}

fn compact_skip(bytes: &[u8], cursor: &mut usize, kind: u8) {
    match kind {
        1 | 2 => {}
        3 => *cursor += 1,
        4..=6 => {
            let _ = compact_read_varint(bytes, cursor);
        }
        7 => *cursor += 8,
        8 => {
            let length = usize::try_from(compact_read_varint(bytes, cursor)).unwrap();
            *cursor += length;
        }
        9 | 10 => {
            let size_and_kind = compact_byte(bytes, cursor);
            let size = if size_and_kind >> 4 == 15 {
                usize::try_from(compact_read_varint(bytes, cursor)).unwrap()
            } else {
                usize::from(size_and_kind >> 4)
            };
            for _ in 0..size {
                compact_skip(bytes, cursor, size_and_kind & 0x0f);
            }
        }
        12 => loop {
            let tag = compact_byte(bytes, cursor);
            if tag & 0x0f == 0 {
                break;
            }
            if tag >> 4 == 0 {
                let _ = compact_i16(bytes, cursor);
            }
            compact_skip(bytes, cursor, tag & 0x0f);
        },
        _ => panic!("unsupported compact fixture field"),
    }
}
