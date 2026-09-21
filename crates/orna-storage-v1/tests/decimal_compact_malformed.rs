use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactOvbProfile, CompactParquetError, CompactParquetKeySource};
use parquet::{
    basic::{Compression, Encoding},
    data_type::{ByteArray, ByteArrayType, FixedLenByteArray, FixedLenByteArrayType, Int32Type, Int64Type},
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

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn decimal_type() -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Decimal".into())])
}

fn profile() -> CompactOvbProfile {
    let field = OvbRaw::Array(vec![
        uuid_raw(KEY),
        OvbRaw::Text(format!("f_{}", Uuid::from_bytes(KEY).simple())),
        decimal_type(),
        OvbRaw::Int(0.into()),
        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
    ]);
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(*TABLE.as_bytes())),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![uuid_raw(KEY)]),
        ),
        (OvbRaw::Int(3.into()), OvbRaw::Array(vec![field])),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

fn descriptor(encoding: &str, parameters: Vec<OvbRaw>) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(KEY)]),
        OvbRaw::Array(vec![OvbRaw::Text(format!(
            "f_{}",
            Uuid::from_bytes(KEY).simple()
        ))]),
        decimal_type(),
        OvbRaw::Text(encoding.into()),
        OvbRaw::Array(parameters),
    ])
}

fn hex_digest(value: [u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
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
        KeyValue::new("orna.encoder".into(), Some("decimal-malformed-proof-v1".into())),
    ]
}

#[derive(Clone, Copy)]
enum KeyLevel {
    Required,
    Optional,
    Repeated,
}

#[derive(Clone, Copy)]
enum Physical {
    Int32,
    Int64,
    Fixed(usize),
}

fn decimal_parquet(
    profile: &CompactOvbProfile,
    physical: Physical,
    annotation: Option<&str>,
    level: KeyLevel,
    encoding: Encoding,
    descriptor_encoding: &str,
    parameters: Vec<OvbRaw>,
    values: &[i64],
) -> Vec<u8> {
    let repetition = match level {
        KeyLevel::Required => "REQUIRED",
        KeyLevel::Optional => "OPTIONAL",
        KeyLevel::Repeated => "REPEATED",
    };
    let annotation = annotation.map_or_else(String::new, |value| format!(" ({value})"));
    let physical_name = match physical {
        Physical::Int32 => "INT32".to_owned(),
        Physical::Int64 => "INT64".to_owned(),
        Physical::Fixed(width) => format!("FIXED_LEN_BYTE_ARRAY({width})"),
    };
    let message = format!(
        "message schema {{ {repetition} {physical_name} f_{}{annotation}; }}",
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(encoding)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(
                profile,
                descriptor(descriptor_encoding, parameters),
            )))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    match physical {
        Physical::Int32 => {
            let encoded = values.iter().map(|value| *value as i32).collect::<Vec<_>>();
            match level {
                KeyLevel::Required => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded, None, None)
                    .unwrap(),
                KeyLevel::Optional => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded[1..], Some(&[0, 1]), None)
                    .unwrap(),
                KeyLevel::Repeated => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded, Some(&[1, 1]), Some(&[0, 1]))
                    .unwrap(),
            };
        }
        Physical::Int64 => {
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
                    .write_batch(values, Some(&[1, 1]), Some(&[0, 1]))
                    .unwrap(),
            };
        }
        Physical::Fixed(width) => {
            let encoded = values
                .iter()
                .map(|value| {
                    let full = value.to_be_bytes();
                    let fill = if *value < 0 { 0xff } else { 0 };
                    let mut bytes = vec![fill; width];
                    if width >= 8 {
                        bytes[width - 8..].copy_from_slice(&full);
                    } else {
                        bytes.copy_from_slice(&full[8 - width..]);
                    }
                    FixedLenByteArray::from(bytes)
                })
                .collect::<Vec<_>>();
            match level {
                KeyLevel::Required => column
                    .typed::<FixedLenByteArrayType>()
                    .write_batch(&encoded, None, None)
                    .unwrap(),
                KeyLevel::Optional => column
                    .typed::<FixedLenByteArrayType>()
                    .write_batch(&encoded[1..], Some(&[0, 1]), None)
                    .unwrap(),
                KeyLevel::Repeated => column
                    .typed::<FixedLenByteArrayType>()
                    .write_batch(&encoded, Some(&[1, 1]), Some(&[0, 1]))
                    .unwrap(),
            };
        }
    }
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}

fn ovb_parquet(
    profile: &CompactOvbProfile,
    level: KeyLevel,
    encoding: Encoding,
    values: &[Vec<u8>],
) -> Vec<u8> {
    let message = format!(
        "message schema {{ {} BYTE_ARRAY f_{}; }}",
        match level {
            KeyLevel::Required => "REQUIRED",
            KeyLevel::Optional => "OPTIONAL",
            KeyLevel::Repeated => "REPEATED",
        },
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(encoding)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(
                profile,
                descriptor("ovb", vec![OvbRaw::Int(1.into())]),
            )))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    let encoded = values
        .iter()
        .map(|value| ByteArray::from(value.clone()))
        .collect::<Vec<_>>();
    match level {
        KeyLevel::Required => column
            .typed::<ByteArrayType>()
            .write_batch(&encoded, None, None)
            .unwrap(),
        KeyLevel::Optional => column
            .typed::<ByteArrayType>()
            .write_batch(&encoded[1..], Some(&[0, 1]), None)
            .unwrap(),
        KeyLevel::Repeated => column
            .typed::<ByteArrayType>()
            .write_batch(&encoded, Some(&[1, 1]), Some(&[0, 1]))
            .unwrap(),
    };
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}


fn footer_start(bytes: &[u8]) -> usize {
    let length = u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap());
    bytes.len() - 8 - length as usize
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

fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    let footer = footer_start(&bytes);
    let source = &bytes;
    let mut chunks = metadata
        .row_groups()
        .iter()
        .enumerate()
        .flat_map(|(row_group, group)| {
            group.columns().iter().enumerate().map(move |(column, meta)| {
                let start = meta.dictionary_page_offset().unwrap_or_else(|| meta.data_page_offset()) as usize;
                let end = start + meta.compressed_size() as usize;
                let data_offset = meta.data_page_offset() as usize - start;
                let (data, shift, inserted) = checksummed_chunk(&source[start..end], data_offset);
                (row_group, column, start, end, data, shift, inserted)
            })
        })
        .collect::<Vec<_>>();
    chunks.sort_by_key(|chunk| chunk.2);
    let mut output = bytes[..chunks.first().map_or(footer, |chunk| chunk.2)].to_vec();
    let mut cursor = output.len();
    let mut updates = Vec::with_capacity(chunks.len());
    for (row_group, column, start, end, chunk, shift, inserted) in chunks {
        output.extend(&bytes[cursor..start]);
        let new_start = output.len();
        output.extend(chunk);
        updates.push((row_group, column, new_start, new_start + shift, inserted));
        cursor = end;
    }
    output.extend(&bytes[cursor..footer]);
    let mut row_groups = metadata.row_groups().to_vec();
    for (row_group, group) in row_groups.iter_mut().enumerate() {
        let mut builder = group.clone().into_builder();
        let mut columns = builder.take_columns();
        for (column, meta) in columns.iter_mut().enumerate() {
            let Some((_, _, start, data_offset, inserted)) = updates
                .iter()
                .find(|(rg, col, _, _, _)| *rg == row_group && *col == column)
            else {
                continue;
            };
            *meta = meta
                .clone()
                .into_builder()
                .set_data_page_offset(*data_offset as i64)
                .set_dictionary_page_offset(meta.dictionary_page_offset().map(|_| *start as i64))
                .set_total_compressed_size(meta.compressed_size() + *inserted as i64)
                .build()
                .unwrap();
        }
        *group = builder.set_column_metadata(columns).build().unwrap();
    }
    let metadata = parquet::file::metadata::ParquetMetaData::new(
        metadata.file_metadata().clone(),
        row_groups,
    );
    let mut footer_bytes = Vec::new();
    parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer_bytes, &metadata)
        .finish()
        .unwrap();
    output.extend(footer_bytes);
    output
}

fn checksummed_chunk(bytes: &[u8], data_offset: usize) -> (Vec<u8>, usize, usize) {
    let mut output = Vec::with_capacity(bytes.len() + 8);
    let mut cursor = 0;
    let mut inserted = 0;
    let mut shift = 0;
    while cursor < bytes.len() {
        let header = page_header(&bytes[cursor..]);
        let body_start = cursor + header.encoded_len;
        let body_end = body_start + header.compressed_len;
        if cursor == data_offset {
            shift = inserted;
        }
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
            let mut page_header = bytes[cursor..body_start].to_vec();
            let insertion = header.next_field_offset.unwrap_or(header.stop_offset);
            page_header.splice(insertion..insertion, checksum_field.iter().copied());
            if let Some(next) = header.next_field_offset {
                let position = next + checksum_field.len();
                let delta = header.next_field_id.unwrap().checked_sub(4).unwrap();
                page_header[position] = (page_header[position] & 0x0f) | (delta << 4);
            }
            output.extend(page_header);
            output.extend(&bytes[body_start..body_end]);
            inserted += checksum_field.len();
        }
        cursor = body_end;
    }
    if data_offset == bytes.len() {
        shift = inserted;
    }
    (output, shift, inserted)
}

fn page_header(bytes: &[u8]) -> PageHeader {
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

#[test]
fn rejects_decimal_physical_type_precision_scale_and_width_mismatches() {
    let profile = profile();
    let cases = [
        (
            "wrong physical type",
            decimal_parquet(
                &profile,
                Physical::Int64,
                Some("DECIMAL(9,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
                &[120],
            ),
        ),
        (
            "wrong descriptor precision",
            decimal_parquet(
                &profile,
                Physical::Int32,
                Some("DECIMAL(9,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(10.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
                &[120],
            ),
        ),
        (
            "wrong descriptor scale",
            decimal_parquet(
                &profile,
                Physical::Int32,
                Some("DECIMAL(9,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(9.into()), OvbRaw::Int(3.into()), OvbRaw::Int(4.into())],
                &[120],
            ),
        ),
        (
            "invalid precision",
            decimal_parquet(
                &profile,
                Physical::Int32,
                Some("DECIMAL(9,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(0.into()), OvbRaw::Int(0.into()), OvbRaw::Int(4.into())],
                &[120],
            ),
        ),
        (
            "invalid scale",
            decimal_parquet(
                &profile,
                Physical::Int32,
                Some("DECIMAL(9,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(9.into()), OvbRaw::Int(10.into()), OvbRaw::Int(4.into())],
                &[120],
            ),
        ),
        (
            "wrong fixed width",
            decimal_parquet(
                &profile,
                Physical::Fixed(9),
                Some("DECIMAL(20,2)"),
                KeyLevel::Required,
                Encoding::PLAIN,
                "decimal",
                vec![OvbRaw::Int(20.into()), OvbRaw::Int(2.into()), OvbRaw::Int(3.into())],
                &[120],
            ),
        ),
    ];
    for (name, bytes) in cases {
        assert!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
            "{name}: malformed Decimal descriptor must fail closed"
        );
    }
}

#[test]
fn rejects_decimal_annotation_and_unit_mismatches() {
    let profile = profile();
    for (name, physical, annotation) in [
        ("missing annotation", Physical::Int32, None),
        (
            "wrong precision annotation",
            Physical::Int64,
            Some("DECIMAL(10,2)"),
        ),
        ("wrong scale annotation", Physical::Int32, Some("DECIMAL(9,3)")),
    ] {
        let bytes = decimal_parquet(
            &profile,
            physical,
            annotation,
            KeyLevel::Required,
            Encoding::PLAIN,
            "decimal",
            vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
            &[120],
        );
        assert!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
            "{name}: malformed Decimal annotation must fail closed"
        );
    }
}

#[test]
fn rejects_unsupported_decimal_value_page_encoding() {
    let profile = profile();
    let bytes = decimal_parquet(
        &profile,
        Physical::Int32,
        Some("DECIMAL(9,2)"),
        KeyLevel::Required,
        Encoding::DELTA_BINARY_PACKED,
        "decimal",
        vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
        &[120],
    );
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
        "unsupported Decimal value encoding must fail closed"
    );
}

#[test]
fn rejects_nullable_and_repeated_decimal_key_levels() {
    let profile = profile();
    let nullable = decimal_parquet(
        &profile,
        Physical::Int32,
        Some("DECIMAL(9,2)"),
        KeyLevel::Optional,
        Encoding::PLAIN,
        "decimal",
        vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
        &[0, 120],
    );
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &nullable, 2).is_err(),
        "nullable Decimal primary keys must fail closed"
    );

    let repeated = decimal_parquet(
        &profile,
        Physical::Int32,
        Some("DECIMAL(9,2)"),
        KeyLevel::Repeated,
        Encoding::PLAIN,
        "decimal",
        vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
        &[0, 120],
    );
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &repeated, 1).is_err(),
        "repeated Decimal primary keys must fail closed"
    );
}

#[test]
fn rejects_truncated_and_malformed_decimal_pages() {
    let profile = profile();
    let bytes = decimal_parquet(
        &profile,
        Physical::Int32,
        Some("DECIMAL(9,2)"),
        KeyLevel::Required,
        Encoding::PLAIN,
        "decimal",
        vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
        &[120],
    );
    let truncated = bytes[..footer_start(&bytes)].to_vec();
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &truncated, 1).is_err(),
        "truncated Decimal page must fail closed"
    );

    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let column = &reader.metadata().row_groups()[0].columns()[0];
    let start = column.data_page_offset() as usize;
    let end = start + column.compressed_size() as usize;
    let mut malformed = bytes;
    malformed[(start + end) / 2] ^= 0xff;
    assert!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &malformed, 1).is_err(),
        "malformed Decimal page must fail closed"
    );
}

#[test]
fn rejects_decimal_row_count_mismatch_before_key_use() {
    let profile = profile();
    let bytes = decimal_parquet(
        &profile,
        Physical::Int32,
        Some("DECIMAL(9,2)"),
        KeyLevel::Required,
        Encoding::PLAIN,
        "decimal",
        vec![OvbRaw::Int(9.into()), OvbRaw::Int(2.into()), OvbRaw::Int(4.into())],
        &[120],
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
fn rejects_malformed_and_noncanonical_decimal_ovb_values() {
    let profile = profile();
    let noncanonical = vec![0xd9, 0xea, 0x60, 0x82, 0x18, 0x78, 0x22];
    let malformed = vec![0xd9, 0xea, 0x60, 0x01];
    for (name, value) in [("noncanonical", noncanonical), ("malformed", malformed)] {
        let bytes = ovb_parquet(&profile, KeyLevel::Required, Encoding::PLAIN, &[value]);
        assert!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).is_err(),
            "{name} Decimal tag-60000 value must fail closed"
        );
    }
}
