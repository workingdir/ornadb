//! Test-only Parquet page-checksum helpers for legacy compact fixtures.
//!
//! Compiled only under `cfg(test)` or the `legacy-fixture-writer` feature.
//! Production builds never contain these helpers (ORNA-UPGRADE-003).

use parquet::file::{
    metadata::ParquetMetaDataWriter,
    reader::{FileReader, SerializedFileReader},
};
use parquet::basic::PageType;
pub(crate) fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
    let reader = SerializedFileReader::new(bytes::Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    assert_eq!(metadata.num_row_groups(), 1);
    assert_eq!(metadata.row_group(0).num_columns(), 1);
    let column = metadata.row_group(0).column(0);
    let start = usize::try_from(column.data_page_offset()).unwrap();
    let length = usize::try_from(column.compressed_size()).unwrap();
    let header = compact_page_header(&bytes[start..start + length]);
    assert!(!header.has_checksum);
    let body_start = start + header.encoded_len;
    let body_end = body_start + header.compressed_len;
    let checksum = crc32(&bytes[body_start..body_end]) as i32;
    let checksum_bytes = compact_crc_field(header.checksum_predecessor, checksum);

    let mut data = bytes[..footer_start(&bytes)].to_vec();
    let insertion = start + header.next_field_offset.unwrap_or(header.stop_offset);
    let checksum_len = checksum_bytes.len();
    data.splice(insertion..insertion, checksum_bytes);
    if let Some(next) = header.next_field_offset {
        let position = start + next + checksum_len;
        let delta = header.next_field_id.unwrap().checked_sub(4).unwrap();
        assert!((1..=15).contains(&delta));
        data[position] = (data[position] & 0x0f) | (delta << 4);
    }

    let mut metadata = metadata.into_builder();
    let mut row_groups = metadata.take_row_groups();
    let row_group = row_groups.pop().unwrap();
    let mut row_group = row_group.into_builder();
    let mut columns = row_group.take_columns();
    let column = columns.pop().unwrap();
    let compressed_size = column.compressed_size();
    columns.push(
        column
            .into_builder()
            .set_total_compressed_size(
                compressed_size + i64::try_from(data.len() - footer_start(&bytes)).unwrap(),
            )
            .build()
            .unwrap(),
    );
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
    let mut pages = group.get_column_page_reader(0).unwrap();
    let page = pages.next().unwrap().unwrap();
    assert!(page.is_data_page());
    assert_eq!(page.page_type(), PageType::DATA_PAGE_V2);
    assert!(pages.next().is_none());
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
