use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactKeyError, CompactOvbProfile, CompactParquetKeySource};
use parquet::{
    basic::{Compression, Encoding, Type},
    data_type::{ByteArray, ByteArrayType},
    file::{
        metadata::KeyValue,
        properties::{WriterProperties, WriterVersion},
        reader::{FileReader, SerializedFileReader},
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};

const TABLE: Uuid = Uuid::from_u128(1);
const STR: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
    0x1f,
];
const UUID: [u8; 16] = [
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
    0x2f,
];
const DATE: [u8; 16] = [
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e,
    0x3f,
];
const INSTANT: [u8; 16] = [
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e,
    0x4f,
];

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn logical_type(name: &str) -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(name.into())])
}

fn field(id: [u8; 16], logical_type_name: &str) -> OvbRaw {
    OvbRaw::Array(vec![
        uuid_raw(id),
        OvbRaw::Text(format!("f_{}", Uuid::from_bytes(id).simple())),
        logical_type(logical_type_name),
        OvbRaw::Int(0.into()),
        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
    ])
}

fn profile() -> CompactOvbProfile {
    let key_ids = [STR, UUID, DATE, INSTANT];
    let mut fields = vec![
        field(STR, "Str"),
        field(UUID, "Uuid"),
        field(DATE, "Date"),
        field(INSTANT, "Instant"),
    ];
    fields.sort_by_key(|field| match field {
        OvbRaw::Array(parts) => match &parts[0] {
            OvbRaw::Tag(37, value) => match value.as_ref() {
                OvbRaw::Bytes(bytes) => bytes.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        },
        _ => Vec::new(),
    });
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(*TABLE.as_bytes())),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(key_ids.into_iter().map(uuid_raw).collect()),
        ),
        (OvbRaw::Int(3.into()), OvbRaw::Array(fields)),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

fn descriptor(id: [u8; 16], logical_type_name: &str) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(id)]),
        OvbRaw::Array(vec![OvbRaw::Text(format!(
            "f_{}",
            Uuid::from_bytes(id).simple()
        ))]),
        logical_type(logical_type_name),
        OvbRaw::Text("ovb".into()),
        OvbRaw::Array(vec![OvbRaw::Int(1.into())]),
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
        KeyValue::new("orna.encoder".into(), Some("ovb-fallback-proof-v1".into())),
    ]
}

fn encoded(value: OvbRaw) -> Vec<u8> {
    CanonicalValue::new(value).unwrap().encode().unwrap()
}

fn uuid_value(value: [u8; 16]) -> OvbRaw {
    uuid_raw(value)
}

fn date_value(value: &str) -> OvbRaw {
    OvbRaw::Tag(60001, Box::new(OvbRaw::Text(value.into())))
}

fn instant_value(seconds: i64, nanoseconds: i64) -> OvbRaw {
    OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(seconds.into()),
            OvbRaw::Int(nanoseconds.into()),
        ])),
    )
}

fn tuple(values: Vec<OvbRaw>) -> Vec<u8> {
    encoded(OvbRaw::Tag(60015, Box::new(OvbRaw::Array(values))))
}

fn parquet(profile: &CompactOvbProfile, physical_ids: &[[u8; 16]], values: &[Vec<Vec<u8>>]) -> Vec<u8> {
    assert_eq!(physical_ids.len(), values.len());
    let mut message = String::from("message schema {");
    for id in physical_ids {
        message.push_str(&format!(
            " REQUIRED BYTE_ARRAY f_{};",
            Uuid::from_bytes(*id).simple()
        ));
    }
    message.push('}');
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let logical_names = physical_ids
        .iter()
        .map(|id| match id {
            &STR => "Str",
            &UUID => "Uuid",
            &DATE => "Date",
            &INSTANT => "Instant",
            _ => panic!("unknown fixture field"),
        })
        .collect::<Vec<_>>();
    let descriptors = physical_ids
        .iter()
        .copied()
        .zip(logical_names)
        .map(|(id, name)| descriptor(id, name))
        .collect();
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(true)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(profile, descriptors)))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    for column_values in values {
        let mut column = row_group.next_column().unwrap().unwrap();
        let values = column_values
            .iter()
            .cloned()
            .map(ByteArray::from)
            .collect::<Vec<_>>();
        column
            .typed::<ByteArrayType>()
            .write_batch(&values, None, None)
            .unwrap();
        column.close().unwrap();
    }
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}

fn expected_row(text: &str, id: [u8; 16], date: &str, seconds: i64, nanoseconds: i64) -> Vec<u8> {
    tuple(vec![
        OvbRaw::Text(text.into()),
        uuid_value(id),
        date_value(date),
        instant_value(seconds, nanoseconds),
    ])
}

#[test]
fn ovb_fallback_dictionary_decodes_all_four_logical_kinds_in_declared_order_and_exact_index() {
    let profile = profile();
    let expected = vec![
        expected_row("alpha", UUID, "2024-01-01", -1, 999_999_999),
        expected_row("alpha", UUID, "2024-01-01", 0, 0),
        expected_row("alpha", UUID, "2024-01-02", -1, 999_999_999),
        expected_row("alpha", DATE, "2024-01-01", -1, 999_999_999),
        expected_row("beta", UUID, "2024-01-01", 0, 0),
    ];
    let bytes = parquet(
        &profile,
        &[INSTANT, STR, DATE, UUID],
        &[
            vec![
                encoded(instant_value(-1, 999_999_999)),
                encoded(instant_value(0, 0)),
                encoded(instant_value(-1, 999_999_999)),
                encoded(instant_value(-1, 999_999_999)),
                encoded(instant_value(0, 0)),
            ],
            vec![
                encoded(OvbRaw::Text("alpha".into())),
                encoded(OvbRaw::Text("alpha".into())),
                encoded(OvbRaw::Text("alpha".into())),
                encoded(OvbRaw::Text("alpha".into())),
                encoded(OvbRaw::Text("beta".into())),
            ],
            vec![
                encoded(date_value("2024-01-01")),
                encoded(date_value("2024-01-01")),
                encoded(date_value("2024-01-02")),
                encoded(date_value("2024-01-01")),
                encoded(date_value("2024-01-01")),
            ],
            vec![
                encoded(uuid_value(UUID)),
                encoded(uuid_value(UUID)),
                encoded(uuid_value(UUID)),
                encoded(uuid_value(DATE)),
                encoded(uuid_value(UUID)),
            ],
        ],
    );
    let reader = SerializedFileReader::new(Bytes::copy_from_slice(&bytes)).unwrap();
    let columns = reader.metadata().row_group(0).columns();
    assert_eq!(columns.len(), 4);
    let schema_columns = reader.metadata().file_metadata().schema_descr().columns();
    assert!(columns
        .iter()
        .enumerate()
        .all(|(index, _)| schema_columns[index].physical_type() == Type::BYTE_ARRAY));
    assert!(columns
        .iter()
        .all(|column| matches!(column.compression(), Compression::ZSTD(_))));
    assert!(columns
        .iter()
        .all(|column| column.dictionary_page_offset().is_some()));
    assert!(columns.iter().all(|column| {
        column
            .encodings()
            .any(|encoding| encoding == Encoding::RLE_DICTIONARY)
    }));

    let decoded = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 5).unwrap();
    assert_eq!(decoded, expected);

    let index = profile
        .exact_key_index(decoded.iter().map(Vec::as_slice))
        .unwrap();
    assert!(matches!(
        profile.validate_against_index(&expected[2], &index),
        Err(CompactKeyError::DuplicateKey)
    ));
    let unseen = expected_row("alpha", DATE, "2024-01-02", 0, 0);
    assert!(profile.validate_against_index(&unseen, &index).is_ok());
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
