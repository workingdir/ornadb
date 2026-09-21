use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactOvbProfile, CompactParquetError, CompactParquetKeySource};
use parquet::{
    basic::{Compression, Encoding},
    data_type::Int64Type,
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

fn instant_type() -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Instant".into())])
}

fn profile() -> CompactOvbProfile {
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(TABLE.as_bytes().to_vec())))),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY.to_vec())))]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY.to_vec()))),
                OvbRaw::Text(format!("f_{}", Uuid::from_bytes(KEY).simple())),
                instant_type(),
                OvbRaw::Int(0.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ])]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

fn descriptor(encoding: &str, parameters: Vec<OvbRaw>) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY.to_vec())))]),
        OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", Uuid::from_bytes(KEY).simple()))]),
        instant_type(),
        OvbRaw::Text(encoding.into()),
        OvbRaw::Array(parameters),
    ])
}

fn hex_digest(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn metadata(profile: &CompactOvbProfile, column_descriptor: OvbRaw) -> Vec<KeyValue> {
    let columns = CanonicalValue::new(OvbRaw::Array(vec![column_descriptor]))
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
        KeyValue::new("orna.encoder".into(), Some("instant-malformed-proof-v1".into())),
    ]
}

#[derive(Clone, Copy)]
enum KeyLevel {
    Required,
    Optional,
    Repeated,
}

fn instant_parquet(
    profile: &CompactOvbProfile,
    annotation: Option<&str>,
    level: KeyLevel,
    encoding: Encoding,
    values: &[i64],
) -> Vec<u8> {
    let repetition = match level {
        KeyLevel::Required => "REQUIRED",
        KeyLevel::Optional => "OPTIONAL",
        KeyLevel::Repeated => "REPEATED",
    };
    let annotation = annotation.map_or_else(String::new, |value| format!(" ({value})"));
    let message = format!(
        "message schema {{ {repetition} INT64 f_{}{annotation}; }}",
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(encoding)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(profile, descriptor("instant_ns", Vec::new()))))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    match level {
        KeyLevel::Required => column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .unwrap(),
        KeyLevel::Optional => column
            .typed::<Int64Type>()
            .write_batch(&values[1..], Some(&[0, 1]), None)
            .unwrap(),
        KeyLevel::Repeated => column
            .typed::<Int64Type>()
            .write_batch(values, Some(&[0, 1]), Some(&[0, 1]))
            .unwrap(),
    };
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}
fn instant_dictionary_parquet(profile: &CompactOvbProfile, values: &[i64]) -> Vec<u8> {
    let message = format!(
        "message schema {{ REQUIRED INT64 f_{} (TIMESTAMP(NANOS,true)); }}",
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(true)
            .set_key_value_metadata(Some(metadata(
                profile,
                descriptor("instant_ns", Vec::new()),
            )))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    column
        .typed::<Int64Type>()
        .write_batch(values, None, None)
        .unwrap();
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
                (start, end, checksummed_chunk(&bytes[start..end]) )
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

#[derive(Clone, Copy)]
enum MetadataCorruption {
    DictionaryOffset(i64),
    DataOffset(i64),
    CompressedSize(i64),
    ColumnValues(i64),
    RowGroupRows(i64),
}

fn corrupt_metadata(bytes: Vec<u8>, corruption: MetadataCorruption) -> Vec<u8> {
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    let mut groups = metadata.row_groups().to_vec();
    let mut group_builder = groups[0].clone().into_builder();
    let mut columns = group_builder.take_columns();
    let column = &mut columns[0];
    let mut column_builder = column.clone().into_builder();
    match corruption {
        MetadataCorruption::DictionaryOffset(offset) => {
            *column = column_builder.set_dictionary_page_offset(Some(offset)).build().unwrap();
        }
        MetadataCorruption::DataOffset(offset) => {
            *column = column_builder.set_data_page_offset(offset).build().unwrap();
        }
        MetadataCorruption::CompressedSize(size) => {
            *column = column_builder.set_total_compressed_size(size).build().unwrap();
        }
        MetadataCorruption::ColumnValues(values) => {
            *column = column_builder.set_num_values(values).build().unwrap();
        }
        MetadataCorruption::RowGroupRows(rows) => {
            *column = column_builder.build().unwrap();
            groups[0] = group_builder
                .set_num_rows(rows)
                .set_column_metadata(columns)
                .build()
                .unwrap();
            let rewritten = parquet::file::metadata::ParquetMetaData::new(
                metadata.file_metadata().clone(),
                groups,
            );
            let mut footer = Vec::new();
            parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer, &rewritten)
                .finish()
                .unwrap();
            let mut output = bytes[..footer_start(&bytes)].to_vec();
            output.extend(footer);
            return output;
        }
    }
    groups[0] = group_builder.set_column_metadata(columns).build().unwrap();
    let rewritten =
        parquet::file::metadata::ParquetMetaData::new(metadata.file_metadata().clone(), groups);
    let mut footer = Vec::new();
    parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer, &rewritten)
        .finish()
        .unwrap();
    let mut output = bytes[..footer_start(&bytes)].to_vec();
    output.extend(footer);
    output
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
                ((i64::from(checksum as i32) << 1) ^ (i64::from(checksum as i32) >> 31)) as u64,
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

#[test]
fn rejects_instant_annotation_and_unit_mismatches_before_key_use() {
    let profile = profile();
    for (name, annotation) in [
        ("missing annotation", None),
        ("wrong timestamp unit", Some("TIMESTAMP(MICROS,true)")),
        ("wrong adjusted-to-utc flag", Some("TIMESTAMP(NANOS,false)")),
    ] {
        let bytes = instant_parquet(&profile, annotation, KeyLevel::Required, Encoding::PLAIN, &[0]);
        assert!(
            matches!(
                CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1),
                Err(CompactParquetError::UnsupportedKeyMapping)
            ),
            "{name}: malformed Instant annotation must fail closed before key use"
        );
    }
}

fn expected_instant(nanoseconds: i64) -> Vec<u8> {
    let seconds = nanoseconds.div_euclid(1_000_000_000);
    let remainder = nanoseconds.rem_euclid(1_000_000_000);
    CanonicalValue::new(OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(seconds.into()),
            OvbRaw::Int(remainder.into()),
        ])),
    ))
    .unwrap()
    .encode()
    .unwrap()
}

#[test]
fn preserves_signed_nanosecond_boundaries_without_invalid_remainders() {
    let profile = profile();
    let values = [i64::MIN, i64::MAX];
    let bytes = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Required,
        Encoding::PLAIN,
        &values,
    );
    let keys =
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap();
    assert_eq!(
        keys,
        values
            .into_iter()
            .map(expected_instant)
            .collect::<Vec<_>>()
    );
}

#[test]
fn rejects_truncated_instant_nanosecond_page_before_logical_use() {
    let profile = profile();
    let bytes = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Required,
        Encoding::PLAIN,
        &[0],
    );
    let truncated = bytes[..footer_start(&bytes)].to_vec();
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &truncated, 1).is_err(),
        "truncated nanosecond page must fail closed before logical Instant use"
    );
}

#[test]
fn rejects_null_and_repeated_instant_key_levels() {
    let profile = profile();
    let nullable = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Optional,
        Encoding::PLAIN,
        &[0, 1],
    );
    assert!(
        matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &nullable, 2),
            Err(CompactParquetError::UnsupportedKeyMapping | CompactParquetError::NullKey)
        ),
        "nullable Instant primary keys must fail closed before key use"
    );

    let repeated = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Repeated,
        Encoding::PLAIN,
        &[0, 1],
    );
    assert!(
        matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &repeated, 1),
            Err(CompactParquetError::UnsupportedKeyMapping | CompactParquetError::NullKey)
        ),
        "repeated Instant primary keys must fail closed before key use"
    );
}

#[test]
fn rejects_unsupported_instant_value_page_encoding() {
    let profile = profile();
    let bytes = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Required,
        Encoding::DELTA_BINARY_PACKED,
        &[0],
    );
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1),
        Err(CompactParquetError::UnsupportedValueEncoding)
    ));
}

#[test]
fn rejects_instant_row_count_mismatch_before_logical_key_use() {
    let profile = profile();
    let bytes = instant_parquet(
        &profile,
        Some("TIMESTAMP(NANOS,true)"),
        KeyLevel::Required,
        Encoding::PLAIN,
        &[0],
    );
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
        Err(CompactParquetError::RowCountMismatch {
            expected: 2,
            observed: 1
        })
    ));
}

#[test]
fn rejects_corrupt_instant_dictionary_page_body_before_key_use() {
    let profile = profile();
    let bytes = instant_dictionary_parquet(&profile, &[1, 1, 2, 1]);
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let column = &reader.metadata().row_groups()[0].columns()[0];
    assert!(
        column.encodings().any(|encoding| encoding == Encoding::RLE_DICTIONARY),
        "fixture must contain a dictionary-encoded data page"
    );
    let dictionary_start = column.dictionary_page_offset().unwrap() as usize;
    let data_start = column.data_page_offset() as usize;
    let dictionary_header = page_header(&bytes[dictionary_start..]);
    let dictionary_body = dictionary_start + dictionary_header.encoded_len;
    assert!(dictionary_body < data_start, "fixture must contain a dictionary page");
    let mut corrupted = bytes;
    corrupted[dictionary_body] ^= 0x80;
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &corrupted, 4),
        Err(CompactParquetError::InvalidParquet)
    ));
}

#[test]
fn rejects_dictionary_page_and_physical_metadata_boundaries() {
    let profile = profile();
    let bytes = instant_dictionary_parquet(&profile, &[1, 1, 2, 1]);
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let column = &reader.metadata().row_groups()[0].columns()[0];
    let dictionary_offset = column.dictionary_page_offset().unwrap();
    let data_offset = column.data_page_offset();
    let cases = [
        (
            "dictionary offset into data page",
            MetadataCorruption::DictionaryOffset(data_offset),
        ),
        (
            "data offset into dictionary page",
            MetadataCorruption::DataOffset(dictionary_offset + 1),
        ),
        (
            "compressed boundary before page body",
            MetadataCorruption::CompressedSize(1),
        ),
    ];
    for (name, corruption) in cases {
        let malformed = corrupt_metadata(bytes.clone(), corruption);
        assert!(
            matches!(
                CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &malformed, 4),
                Err(CompactParquetError::InvalidParquet)
            ),
            "{name}: malformed physical metadata must fail closed"
        );
    }
}

#[test]
fn rejects_dictionary_column_and_row_group_boundary_mismatches() {
    let profile = profile();
    let bytes = instant_dictionary_parquet(&profile, &[1, 1, 2, 1]);
    for (name, corruption) in [
        ("column value count", MetadataCorruption::ColumnValues(3)),
        ("row-group row count", MetadataCorruption::RowGroupRows(5)),
    ] {
        let malformed = corrupt_metadata(bytes.clone(), corruption);
        let result = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &malformed, 4);
        match name {
            "column value count" => assert!(matches!(
                result,
                Err(CompactParquetError::InvalidParquet)
            )),
            "row-group row count" => assert!(matches!(
                result,
                Err(CompactParquetError::RowCountMismatch {
                    expected: 4,
                    observed: 5
                })
            )),
            _ => unreachable!("unknown metadata boundary fixture"),
        }
    }
}
#[test]
fn rejects_dictionary_encoded_keys_outside_declared_order() {
    let profile = profile();
    let bytes = instant_dictionary_parquet(&profile, &[2, 1]);
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
        Err(CompactParquetError::InvalidParquet)
    ));
}
