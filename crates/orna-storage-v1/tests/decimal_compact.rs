use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactOvbProfile, CompactParquetKeySource};
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
const KEY_A: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
    0x1f,
];
const KEY_B: [u8; 16] = [
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
    0x2f,
];
const KEY_C: [u8; 16] = [
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e,
    0x3f,
];

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn decimal_type() -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Decimal".into())])
}

fn decimal_raw(coefficient: i64, exponent10: i64) -> OvbRaw {
    OvbRaw::Tag(
        60000,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(coefficient.into()),
            OvbRaw::Int(exponent10.into()),
        ])),
    )
}

fn field(id: [u8; 16]) -> OvbRaw {
    OvbRaw::Array(vec![
        uuid_raw(id),
        OvbRaw::Text(format!("f_{}", Uuid::from_bytes(id).simple())),
        decimal_type(),
        OvbRaw::Int(0.into()),
        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
    ])
}

fn profile(key_ids: &[[u8; 16]]) -> CompactOvbProfile {
    let mut fields = key_ids.to_vec();
    fields.sort();
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(*TABLE.as_bytes())),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(key_ids.iter().copied().map(uuid_raw).collect()),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(fields.into_iter().map(field).collect()),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

fn descriptor(
    id: [u8; 16],
    encoding: &str,
    parameters: Vec<OvbRaw>,
) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(id)]),
        OvbRaw::Array(vec![OvbRaw::Text(format!(
            "f_{}",
            Uuid::from_bytes(id).simple()
        ))]),
        decimal_type(),
        OvbRaw::Text(encoding.into()),
        OvbRaw::Array(parameters),
    ])
}

fn hex(value: [u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn metadata(profile: &CompactOvbProfile, descriptors: Vec<OvbRaw>) -> Vec<KeyValue> {
    let columns = CanonicalValue::new(OvbRaw::Array(descriptors))
        .unwrap()
        .encode()
        .unwrap();
    vec![
        KeyValue::new("orna.profile".into(), Some("compact-storage-v1".into())),
        KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
        KeyValue::new(
            "orna.schema.sha256".into(),
            Some(hex(profile.schema_fingerprint())),
        ),
        KeyValue::new(
            "orna.schema.ovb".into(),
            Some(BASE64.encode(profile.schema().encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
        KeyValue::new("orna.encoder".into(), Some("decimal-proof-v1".into())),
    ]
}

enum PhysicalValues {
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    Fixed { width: usize, values: Vec<Vec<u8>> },
    ByteArray(Vec<Vec<u8>>),
}

struct Column {
    id: [u8; 16],
    precision: u8,
    scale: u8,
    encoding: &'static str,
    parameters: Vec<OvbRaw>,
    values: PhysicalValues,
}

fn parquet(profile: &CompactOvbProfile, columns: Vec<Column>) -> Vec<u8> {
    parquet_with_dictionary(profile, columns, false)
}

fn parquet_with_dictionary(
    profile: &CompactOvbProfile,
    columns: Vec<Column>,
    dictionary_enabled: bool,
) -> Vec<u8> {
    let mut message = String::from("message schema {");
    for column in &columns {
        let name = format!("f_{}", Uuid::from_bytes(column.id).simple());
        match &column.values {
            PhysicalValues::Int32(_) => message.push_str(&format!(
                " REQUIRED INT32 {name} (DECIMAL({}, {}));",
                column.precision, column.scale
            )),
            PhysicalValues::Int64(_) => message.push_str(&format!(
                " REQUIRED INT64 {name} (DECIMAL({}, {}));",
                column.precision, column.scale
            )),
            PhysicalValues::Fixed { width, .. } => message.push_str(&format!(
                " REQUIRED FIXED_LEN_BYTE_ARRAY({width}) {name} (DECIMAL({}, {}));",
                column.precision, column.scale
            )),
            PhysicalValues::ByteArray(_) => {
                message.push_str(&format!(" REQUIRED BYTE_ARRAY {name};"))
            }
        }
    }
    message.push('}');
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let descriptors = columns
        .iter()
        .map(|column| descriptor(column.id, column.encoding, column.parameters.clone()))
        .collect();
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(dictionary_enabled)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(profile, descriptors)))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    for column in columns {
        let mut parquet_column = row_group.next_column().unwrap().unwrap();
        match column.values {
            PhysicalValues::Int32(values) => {
                parquet_column
                    .typed::<Int32Type>()
                    .write_batch(&values, None, None)
                    .unwrap();
            }
            PhysicalValues::Int64(values) => {
                parquet_column
                    .typed::<Int64Type>()
                    .write_batch(&values, None, None)
                    .unwrap();
            }
            PhysicalValues::Fixed { width, values } => {
                let values = values
                    .into_iter()
                    .map(|value| {
                        assert_eq!(value.len(), width);
                        FixedLenByteArray::from(value)
                    })
                    .collect::<Vec<_>>();
                parquet_column
                    .typed::<FixedLenByteArrayType>()
                    .write_batch(&values, None, None)
                    .unwrap();
            }
            PhysicalValues::ByteArray(values) => {
                let values = values
                    .into_iter()
                    .map(ByteArray::from)
                    .collect::<Vec<_>>();
                parquet_column
                    .typed::<ByteArrayType>()
                    .write_batch(&values, None, None)
                    .unwrap();
            }
        }
        parquet_column.close().unwrap();
    }
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}

fn standard_parameters(precision: u8, scale: u8, width: u8) -> Vec<OvbRaw> {
    vec![
        OvbRaw::Int(i64::from(precision).into()),
        OvbRaw::Int(i64::from(scale).into()),
        OvbRaw::Int(i64::from(width).into()),
    ]
}

fn expected(value: OvbRaw) -> Vec<u8> {
    CanonicalValue::new(value).unwrap().encode().unwrap()
}

fn expected_tuple(values: &[OvbRaw]) -> Vec<u8> {
    expected(OvbRaw::Tag(60015, Box::new(OvbRaw::Array(values.to_vec()))))
}

fn signed_be(value: i128, width: usize) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    assert!((1..=16).contains(&width));
    bytes[16 - width..].to_vec()
}

#[test]
fn standard_decimal_int32_decodes_canonical_coefficient_and_exponent() {
    let profile = profile(&[KEY_A]);
    let bytes = parquet(
        &profile,
        vec![Column {
            id: KEY_A,
            precision: 9,
            scale: 2,
            encoding: "decimal",
            parameters: standard_parameters(9, 2, 4),
            values: PhysicalValues::Int32(vec![123450]),
        }],
    );
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
        vec![expected(decimal_raw(12345, -1))]
    );
}

#[test]
fn standard_decimal_int64_decodes_negative_value_without_rounding() {
    let profile = profile(&[KEY_A]);
    let bytes = parquet(
        &profile,
        vec![Column {
            id: KEY_A,
            precision: 18,
            scale: 3,
            encoding: "decimal",
            parameters: standard_parameters(18, 3, 8),
            values: PhysicalValues::Int64(vec![-9876543210]),
        }],
    );
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
        vec![expected(decimal_raw(-987654321, -2))]
    );
}

#[test]
fn standard_decimal_fixed_width_preserves_exact_large_integer() {
    let profile = profile(&[KEY_A]);
    let bytes = parquet(
        &profile,
        vec![Column {
            id: KEY_A,
            precision: 20,
            scale: 1,
            encoding: "decimal",
            parameters: standard_parameters(20, 1, 9),
            values: PhysicalValues::Fixed {
                width: 9,
                values: vec![signed_be(12_345_678_901_234_567_890_i128, 9)],
            },
        }],
    );
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
        vec![expected(decimal_raw(1_234_567_890_123_456_789, 0))]
    );
}

#[test]
fn explicit_decimal_ovb_fallback_preserves_tag_60000_value() {
    let profile = profile(&[KEY_A]);
    let value = expected(decimal_raw(12345, -3));
    let bytes = parquet(
        &profile,
        vec![Column {
            id: KEY_A,
            precision: 1,
            scale: 0,
            encoding: "ovb",
            parameters: vec![OvbRaw::Int(1.into())],
            values: PhysicalValues::ByteArray(vec![value]),
        }],
    );
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
        vec![expected(decimal_raw(12345, -3))]
    );
}

#[test]
fn decimal_composite_uses_declared_order_and_exact_decimal_sorting() {
    let profile = profile(&[KEY_A, KEY_B]);
    let bytes = parquet(
        &profile,
        vec![
            Column {
                id: KEY_B,
                precision: 18,
                scale: 1,
                encoding: "decimal",
                parameters: standard_parameters(18, 1, 8),
                values: PhysicalValues::Int64(vec![90, 10]),
            },
            Column {
                id: KEY_A,
                precision: 9,
                scale: 2,
                encoding: "decimal",
                parameters: standard_parameters(9, 2, 4),
                values: PhysicalValues::Int32(vec![101, 110]),
            },
        ],
    );
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap(),
        vec![
            expected_tuple(&[decimal_raw(101, -2), decimal_raw(9, 0)]),
            expected_tuple(&[decimal_raw(11, -1), decimal_raw(1, 0)]),
        ]
    );
}

#[test]
fn dictionary_decimal_mixed_physical_forms_decode_canonical_values_in_key_order() {
    let profile = profile(&[KEY_A, KEY_B, KEY_C]);
    let values_a = vec![101, 101, 110, 110, 111, 111];
    let values_b = vec![9000, 9000, 1000, 1000, 2000, 2000];
    let values_c = vec![
        signed_be(123450, 9),
        signed_be(123450, 9),
        signed_be(123451, 9),
        signed_be(123451, 9),
        signed_be(123450, 9),
        signed_be(123450, 9),
    ];
    let bytes = parquet_with_dictionary(
        &profile,
        vec![
            Column {
                id: KEY_C,
                precision: 20,
                scale: 1,
                encoding: "decimal",
                parameters: standard_parameters(20, 1, 9),
                values: PhysicalValues::Fixed {
                    width: 9,
                    values: values_c,
                },
            },
            Column {
                id: KEY_A,
                precision: 9,
                scale: 2,
                encoding: "decimal",
                parameters: standard_parameters(9, 2, 4),
                values: PhysicalValues::Int32(values_a),
            },
            Column {
                id: KEY_B,
                precision: 18,
                scale: 3,
                encoding: "decimal",
                parameters: standard_parameters(18, 3, 8),
                values: PhysicalValues::Int64(values_b),
            },
        ],
        true,
    );
    let reader = SerializedFileReader::new(Bytes::copy_from_slice(&bytes)).unwrap();
    let columns = reader.metadata().row_group(0).columns();
    assert!(columns
        .iter()
        .all(|column| matches!(column.compression(), Compression::ZSTD(_))));
    assert!(columns.iter().all(|column| {
        column
            .encodings()
            .any(|encoding| encoding == Encoding::RLE_DICTIONARY)
    }));
    assert_eq!(
        CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 6).unwrap(),
        vec![
            expected_tuple(&[
                decimal_raw(101, -2),
                decimal_raw(9, 0),
                decimal_raw(12345, 0),
            ]),
            expected_tuple(&[
                decimal_raw(101, -2),
                decimal_raw(9, 0),
                decimal_raw(12345, 0),
            ]),
            expected_tuple(&[
                decimal_raw(11, -1),
                decimal_raw(1, 0),
                decimal_raw(123451, -1),
            ]),
            expected_tuple(&[
                decimal_raw(11, -1),
                decimal_raw(1, 0),
                decimal_raw(123451, -1),
            ]),
            expected_tuple(&[
                decimal_raw(111, -2),
                decimal_raw(2, 0),
                decimal_raw(12345, 0),
            ]),
            expected_tuple(&[
                decimal_raw(111, -2),
                decimal_raw(2, 0),
                decimal_raw(12345, 0),
            ]),
        ]
    );
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

fn footer_start(bytes: &[u8]) -> usize {
    let length = u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap());
    bytes.len() - 8 - length as usize
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
