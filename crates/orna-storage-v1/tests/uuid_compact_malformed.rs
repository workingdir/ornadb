use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactOvbProfile, CompactParquetError, CompactParquetKeySource};
use parquet::{
    basic::{Compression, Encoding},
    data_type::{FixedLenByteArray, FixedLenByteArrayType},
    file::{
        metadata::KeyValue,
        properties::{WriterProperties, WriterVersion},
        reader::{FileReader, SerializedFileReader},
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};

const TABLE: Uuid = Uuid::from_u128(1);
const KEY: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
    0x1f,
];
const KEY_2: [u8; 16] = [
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
    0x2f,
];

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn uuid_type() -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Uuid".into())])
}

fn profile() -> CompactOvbProfile {
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(*TABLE.as_bytes())),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![uuid_raw(KEY)]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                uuid_raw(KEY),
                OvbRaw::Text(format!("f_{}", Uuid::from_bytes(KEY).simple())),
                uuid_type(),
                OvbRaw::Int(0.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ])]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

/// The descriptor is deliberately explicit: [field_path, physical_path,
/// [0,"Uuid"], "uuid", [Null]]. The reader must validate this declaration
/// against the physical Parquet column before any UUID bytes become a key.
fn uuid_descriptor() -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(KEY)]),
        OvbRaw::Array(vec![OvbRaw::Text(format!(
            "f_{}",
            Uuid::from_bytes(KEY).simple()
        ))]),
        uuid_type(),
        OvbRaw::Text("uuid".into()),
        OvbRaw::Array(vec![OvbRaw::Null]),
    ])
}

fn hex_digest(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn metadata(profile: &CompactOvbProfile) -> Vec<KeyValue> {
    let columns = CanonicalValue::new(OvbRaw::Array(vec![uuid_descriptor()]))
        .unwrap()
        .encode()
        .unwrap();
    vec![
        KeyValue::new("orna.profile".into(), Some("compact-storage-v1".into())),
        KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
        KeyValue::new(
            "orna.schema.sha256".into(),
            Some(hex_digest(profile.schema_fingerprint())),
        ),
        KeyValue::new(
            "orna.schema.ovb".into(),
            Some(BASE64.encode(profile.schema().encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
        KeyValue::new("orna.encoder".into(), Some("uuid-malformed-proof-v1".into())),
    ]
}

#[derive(Clone, Copy)]
enum KeyLevel {
    Required,
    Optional,
    Repeated,
}

fn parquet(
    profile: &CompactOvbProfile,
    width: usize,
    annotation: Option<&str>,
    level: KeyLevel,
    encoding: Encoding,
    values: &[[u8; 16]],
) -> Vec<u8> {
    let repetition = match level {
        KeyLevel::Required => "REQUIRED",
        KeyLevel::Optional => "OPTIONAL",
        KeyLevel::Repeated => "REPEATED",
    };
    let annotation = annotation.map_or_else(String::new, |value| format!(" ({value})"));
    let message = format!(
        "message schema {{ {repetition} FIXED_LEN_BYTE_ARRAY({width}) f_{}{annotation}; }}",
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(encoding)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(profile)))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    let encoded_values = values
        .iter()
        .map(|value| FixedLenByteArray::from(value[..width].to_vec()))
        .collect::<Vec<_>>();
    match level {
        KeyLevel::Required => column
            .typed::<FixedLenByteArrayType>()
            .write_batch(&encoded_values, None, None)
            .unwrap(),
        KeyLevel::Optional => column
            .typed::<FixedLenByteArrayType>()
            // One absent row followed by one present row.
            .write_batch(&encoded_values[1..], Some(&[0, 1]), None)
            .unwrap(),
        KeyLevel::Repeated => column
            .typed::<FixedLenByteArrayType>()
            // One row containing two repeated UUID values.
            .write_batch(&encoded_values, Some(&[1, 1]), Some(&[0, 1]))
            .unwrap(),
    };
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}

struct PageHeader {
    encoded_len: usize,
    compressed_len: usize,
    checksum_predecessor: u8,
    has_checksum: bool,
    next_field_offset: Option<usize>,
    next_field_id: Option<u8>,
    stop_offset: usize,
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
        8 => *cursor += compact_read_varint(bytes, cursor) as usize,
        9 | 10 => {
            let size_and_kind = compact_byte(bytes, cursor);
            let size = if size_and_kind >> 4 == 15 {
                compact_read_varint(bytes, cursor) as usize
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
fn page_header(bytes: &[u8]) -> PageHeader {
    let mut cursor = 0;
    let mut previous = 0;
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
            return PageHeader {
                encoded_len: cursor,
                compressed_len: compressed_len.unwrap(),
                checksum_predecessor: checksum_predecessor.unwrap_or(previous),
                has_checksum,
                next_field_offset,
                next_field_id,
                stop_offset: field_offset,
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
        if field == 4 {
            has_checksum = true;
        }
        if field == 3 && kind == 5 {
            compressed_len = Some(compact_i32(bytes, &mut cursor) as usize);
        } else {
            compact_skip(bytes, &mut cursor, kind);
        }
        previous = field;
    }
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

fn checksummed_chunk(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() + 8);
    let mut cursor = 0;
    while cursor < bytes.len() {
        let header = page_header(&bytes[cursor..]);
        let body_start = cursor + header.encoded_len;
        let body_end = body_start + header.compressed_len;
        if header.has_checksum {
            output.extend(&bytes[cursor..body_end]);
        } else {
            let checksum = crc32(&bytes[body_start..body_end]);
            let delta = 4_u8.checked_sub(header.checksum_predecessor).unwrap();
            let mut checksum_field = vec![(delta << 4) | 5];
            compact_varint(
                ((i64::from(checksum as i32) << 1) ^ (i64::from(checksum as i32) >> 31))
                    as u64,
                &mut checksum_field,
            );
            let mut rewritten = bytes[cursor..body_start].to_vec();
            let insertion = header.next_field_offset.unwrap_or(header.stop_offset);
            rewritten.splice(insertion..insertion, checksum_field.iter().copied());
            if let Some(next) = header.next_field_offset {
                let position = next + checksum_field.len();
                let delta = header.next_field_id.unwrap().checked_sub(4).unwrap();
                rewritten[position] = (rewritten[position] & 0x0f) | (delta << 4);
            }
            output.extend(rewritten);
            output.extend(&bytes[body_start..body_end]);
        }
        cursor = body_end;
    }
    output
}

fn footer_start(bytes: &[u8]) -> usize {
    let length = u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap());
    bytes.len() - 8 - length as usize
}

fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    let footer = footer_start(&bytes);
    let mut chunks = metadata
        .row_groups()
        .iter()
        .flat_map(|group| {
            group.columns().iter().map(|meta| {
                let start = meta.dictionary_page_offset().unwrap_or_else(|| meta.data_page_offset()) as usize;
                let end = start + meta.compressed_size() as usize;
                (start, end, checksummed_chunk(&bytes[start..end]))
            })
        })
        .collect::<Vec<_>>();
    chunks.sort_by_key(|chunk| chunk.0);
    let mut data = bytes[..chunks.first().map_or(footer, |chunk| chunk.0)].to_vec();
    let mut cursor = data.len();
    let mut updates = Vec::new();
    for (start, end, chunk) in chunks {
        data.extend(&bytes[cursor..start]);
        let new_start = data.len();
        data.extend(chunk.iter());
        updates.push((start, end, new_start, chunk.len()));
        cursor = end;
    }
    data.extend(&bytes[cursor..footer]);
    let mut groups = metadata.row_groups().to_vec();
    for group in &mut groups {
        let mut builder = group.clone().into_builder();
        let mut columns = builder.take_columns();
        for column in &mut columns {
            let old_start = column.dictionary_page_offset().unwrap_or_else(|| column.data_page_offset()) as usize;
            let Some((_, _, new_start, new_len)) = updates.iter().find(|item| item.0 == old_start) else { continue };
            let shift = *new_start as i64 - old_start as i64;
            let replacement = column.clone().into_builder()
                .set_data_page_offset(column.data_page_offset() + shift)
                .set_dictionary_page_offset(column.dictionary_page_offset().map(|offset| offset + shift))
                .set_total_compressed_size(column.compressed_size() + (*new_len as i64 - column.compressed_size()))
                .build().unwrap();
            *column = replacement;
        }
        *group = builder.set_column_metadata(columns).build().unwrap();
    }
    let rewritten = parquet::file::metadata::ParquetMetaData::new(metadata.file_metadata().clone(), groups);
    let mut new_footer = Vec::new();
    parquet::file::metadata::ParquetMetaDataWriter::new(&mut new_footer, &rewritten).finish().unwrap();
    data.extend(new_footer);
    data
}

#[test]
fn rejects_uuid_width_and_logical_annotation_mismatches_before_key_use() {
    let profile = profile();
    let cases = [
        (
            "wrong fixed width",
            parquet(
                &profile,
                15,
                None,
                KeyLevel::Required,
                Encoding::PLAIN,
                &[KEY],
            ),
        ),
        (
            "missing UUID annotation",
            parquet(
                &profile,
                16,
                None,
                KeyLevel::Required,
                Encoding::PLAIN,
                &[KEY],
            ),
        ),
        (
            "wrong UUID annotation",
            parquet(
                &profile,
                16,
                Some("DECIMAL(10, 0)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                &[KEY],
            ),
        ),
    ];
    for (name, bytes) in cases {
        assert!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
            "{name}: malformed UUID metadata must fail closed before key use"
        );
    }
}

#[test]
fn rejects_nullable_and_repeated_uuid_key_levels() {
    let profile = profile();
    let nullable = parquet(
        &profile,
        16,
        Some("UUID"),
        KeyLevel::Optional,
        Encoding::PLAIN,
        &[KEY_2, KEY],
    );
    let nullable_result =
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &nullable, 2);
    assert!(
        nullable_result.is_err(),
        "nullable UUID primary keys must fail closed before key use"
    );

    let repeated = parquet(
        &profile,
        16,
        Some("UUID"),
        KeyLevel::Repeated,
        Encoding::PLAIN,
        &[KEY, KEY_2],
    );
    let repeated_result =
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &repeated, 1);
    assert!(
        repeated_result.is_err(),
        "repeated UUID primary keys must fail closed before key use"
    );
}

#[test]
fn rejects_unsupported_uuid_value_page_encoding() {
    let profile = profile();
    let bytes = parquet(
        &profile,
        16,
        Some("UUID"),
        KeyLevel::Required,
        Encoding::DELTA_BYTE_ARRAY,
        &[KEY],
    );
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
        "unsupported UUID value encoding must fail closed before key use"
    );
}

#[test]
fn rejects_uuid_row_count_mismatch_before_logical_key_use() {
    let profile = profile();
    let bytes = parquet(
        &profile,
        16,
        Some("UUID"),
        KeyLevel::Required,
        Encoding::PLAIN,
        &[KEY],
    );
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
        Err(CompactParquetError::RowCountMismatch {
            expected: 2,
            observed: 1
        })
    ));
}
