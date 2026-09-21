use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::Uuid;
use orna_storage_v1::{CompactOvbProfile, CompactParquetError, CompactParquetKeySource};
use parquet::{
    basic::{Compression, Encoding},
    data_type::{ByteArray, ByteArrayType, Int32Type},
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

#[derive(Clone, Copy, Debug)]
enum Kind {
    Str,
    Uuid,
    Date,
    Instant,
}

#[derive(Clone, Copy)]
enum Level {
    Required,
    Optional,
    Repeated,
}

#[derive(Clone, Copy)]
enum Physical {
    ByteArray,
    Int32,
}

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn type_node(kind: Kind) -> OvbRaw {
    let name = match kind {
        Kind::Str => "Str",
        Kind::Uuid => "Uuid",
        Kind::Date => "Date",
        Kind::Instant => "Instant",
    };
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(name.into())])
}

fn profile(kind: Kind) -> CompactOvbProfile {
    let field = OvbRaw::Array(vec![
        uuid_raw(KEY),
        OvbRaw::Text(format!("f_{}", Uuid::from_bytes(KEY).simple())),
        type_node(kind),
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

fn descriptor(logical: Kind, encoding: &str, parameters: Vec<OvbRaw>) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(KEY)]),
        OvbRaw::Array(vec![OvbRaw::Text(format!(
            "f_{}",
            Uuid::from_bytes(KEY).simple()
        ))]),
        type_node(logical),
        OvbRaw::Text(encoding.into()),
        OvbRaw::Array(parameters),
    ])
}

fn hex(value: [u8; 32]) -> String {
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
            Some(hex(profile.schema_fingerprint())),
        ),
        KeyValue::new(
            "orna.schema.ovb".into(),
            Some(BASE64.encode(profile.schema().encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
        KeyValue::new("orna.encoder".into(), Some("ovb-fallback-malformed-v1".into())),
    ]
}

fn parquet(
    profile: &CompactOvbProfile,
    descriptor_kind: Kind,
    descriptor_encoding: &str,
    parameters: Vec<OvbRaw>,
    annotation: Option<&str>,
    physical: Physical,
    level: Level,
    encoding: Encoding,
    values: &[Vec<u8>],
) -> Vec<u8> {
    parquet_with_logical(
        profile,
        descriptor_kind,
        descriptor_kind,
        descriptor_encoding,
        parameters,
        annotation,
        physical,
        level,
        encoding,
        values,
    )
}

fn parquet_with_logical(
    profile: &CompactOvbProfile,
    descriptor_kind: Kind,
    logical_kind: Kind,
    descriptor_encoding: &str,
    parameters: Vec<OvbRaw>,
    annotation: Option<&str>,
    physical: Physical,
    level: Level,
    encoding: Encoding,
    values: &[Vec<u8>],
) -> Vec<u8> {
    let repetition = match level {
        Level::Required => "REQUIRED",
        Level::Optional => "OPTIONAL",
        Level::Repeated => "REPEATED",
    };
    let annotation = annotation.map_or_else(String::new, |value| format!(" ({value})"));
    let physical_name = match physical {
        Physical::ByteArray => "BYTE_ARRAY".to_owned(),
        Physical::Int32 => "INT32".to_owned(),
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
                descriptor(
                    logical_kind,
                    descriptor_encoding,
                    parameters,
                ),
            )))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    match physical {
        Physical::ByteArray => {
            let encoded = values
                .iter()
                .map(|value| ByteArray::from(value.clone()))
                .collect::<Vec<_>>();
            match level {
                Level::Required => column
                    .typed::<ByteArrayType>()
                    .write_batch(&encoded, None, None)
                    .unwrap(),
                Level::Optional => column
                    .typed::<ByteArrayType>()
                    .write_batch(&encoded[1..], Some(&[0, 1]), None)
                    .unwrap(),
                Level::Repeated => column
                    .typed::<ByteArrayType>()
                    .write_batch(&encoded, Some(&[0, 1]), Some(&[0, 1]))
                    .unwrap(),
            };
        }
        Physical::Int32 => {
            let encoded = vec![0_i32; values.len()];
            match level {
                Level::Required => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded, None, None)
                    .unwrap(),
                Level::Optional => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded[1..], Some(&[0, 1]), None)
                    .unwrap(),
                Level::Repeated => column
                    .typed::<Int32Type>()
                    .write_batch(&encoded, Some(&[0, 1]), Some(&[0, 1]))
                    .unwrap(),
            };
        }
    }
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}
fn dictionary_parquet(profile: &CompactOvbProfile, values: &[Vec<u8>]) -> Vec<u8> {
    let message = format!(
        "message schema {{ REQUIRED BYTE_ARRAY f_{}; }}",
        Uuid::from_bytes(KEY).simple()
    );
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(true)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata(
                profile,
                descriptor(Kind::Str, "ovb", vec![OvbRaw::Int(1.into())]),
            )))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    let encoded = values
        .iter()
        .cloned()
        .map(ByteArray::from)
        .collect::<Vec<_>>();
    column
        .typed::<ByteArrayType>()
        .write_batch(&encoded, None, None)
        .unwrap();
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();
    with_page_checksums(bytes)
}


fn canonical(raw: OvbRaw) -> Vec<u8> {
    CanonicalValue::new(raw).unwrap().encode().unwrap()
}

fn valid_value(kind: Kind) -> Vec<u8> {
    match kind {
        Kind::Str => canonical(OvbRaw::Text("alpha".into())),
        Kind::Uuid => canonical(uuid_raw(KEY)),
        Kind::Date => canonical(OvbRaw::Tag(
            60001,
            Box::new(OvbRaw::Text("2024-02-29".into())),
        )),
        Kind::Instant => canonical(OvbRaw::Tag(
            60002,
            Box::new(OvbRaw::Array(vec![OvbRaw::Int((-1).into()), OvbRaw::Int(1.into())])),
        )),
    }
}

fn malformed_values(kind: Kind) -> Vec<(&'static str, Vec<u8>)> {
    match kind {
        Kind::Str => vec![
            ("wrong tag", canonical(uuid_raw(KEY))),
            ("wrong payload", vec![0xd9, 0xea, 0x61, 0x41]),
            ("noncanonical CBOR", vec![0x78, 0x01, b'a']),
        ],
        Kind::Uuid => vec![
            ("wrong tag", canonical(OvbRaw::Text("alpha".into()))),
            ("wrong payload", vec![0xd8, 0x25, 0x41, 0x00]),
            (
                "wrong UUID width",
                {
                    let mut bytes = vec![0xd8, 0x25, 0x4f];
                    bytes.extend([0; 15]);
                    bytes
                },
            ),
            (
                "noncanonical CBOR",
                {
                    let mut bytes = vec![0xd9, 0x00, 0x25, 0x50];
                    bytes.extend([0; 16]);
                    bytes
                },
            ),
        ],
        Kind::Date => vec![
            ("wrong tag", canonical(uuid_raw(KEY))),
            (
                "wrong payload",
                {
                    let mut bytes = vec![0xd9, 0xea, 0x61, 0x50];
                    bytes.extend([0; 16]);
                    bytes
                },
            ),
            ("invalid Gregorian date", b"\xd9\xea\x61\x6a2024-02-30".to_vec()),
            ("noncanonical date text", b"\xd9\xea\x61\x692024-2-29".to_vec()),
            ("noncanonical CBOR", b"\xd9\xea\x61\x78\x0a2024-02-29".to_vec()),
        ],
        Kind::Instant => vec![
            ("wrong tag", canonical(uuid_raw(KEY))),
            ("wrong payload shape", b"\xd9\xea\x62\x81\x00".to_vec()),
            ("negative nanosecond", b"\xd9\xea\x62\x82\x00\x20".to_vec()),
            (
                "nanosecond out of range",
                b"\xd9\xea\x62\x82\x00\x1a\x3b\x9a\xca\x00".to_vec(),
            ),
            ("noncanonical CBOR", b"\xd9\xea\x62\x98\x02\x00\x00".to_vec()),
        ],
    }
}

#[test]
fn rejects_ovb_descriptor_physical_and_parquet_contract_for_all_kinds() {
    for kind in [Kind::Str, Kind::Uuid, Kind::Date, Kind::Instant] {
        let profile = profile(kind);
        let value = valid_value(kind);
        let cases = [
            (
                "wrong logical type",
                parquet_with_logical(
                    &profile,
                    kind,
                    match kind {
                        Kind::Str => Kind::Uuid,
                        Kind::Uuid => Kind::Str,
                        Kind::Date => Kind::Str,
                        Kind::Instant => Kind::Str,
                    },
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    None,
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "wrong encoding",
                parquet(
                    &profile,
                    kind,
                    "utf8",
                    Vec::new(),
                    None,
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "wrong parameters",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(0.into())],
                    None,
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "Parquet logical STRING annotation",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    Some("STRING"),
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "Parquet converted UTF8 annotation",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    Some("UTF8"),
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "wrong physical type",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    None,
                    Physical::Int32,
                    Level::Required,
                    Encoding::PLAIN,
                    std::slice::from_ref(&value),
                ),
                true,
            ),
            (
                "optional levels",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    None,
                    Physical::ByteArray,
                    Level::Optional,
                    Encoding::PLAIN,
                    &[b"unused".to_vec(), value.clone()],
                ),
                true,
            ),
            (
                "repeated levels",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    None,
                    Physical::ByteArray,
                    Level::Repeated,
                    Encoding::PLAIN,
                    &[value.clone(), value.clone()],
                ),
                true,
            ),
            (
                "unsupported page encoding",
                parquet(
                    &profile,
                    kind,
                    "ovb",
                    vec![OvbRaw::Int(1.into())],
                    None,
                    Physical::ByteArray,
                    Level::Required,
                    Encoding::DELTA_BYTE_ARRAY,
                    std::slice::from_ref(&value),
                ),
                false,
            ),
        ];
        for (name, bytes, category_is_mapping) in cases {
            let result = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1);
            if category_is_mapping {
                assert!(
                    matches!(
                        result,
                        Err(CompactParquetError::UnsupportedKeyMapping)
                            | Err(CompactParquetError::NullKey)
                            | Err(CompactParquetError::RowCountMismatch { .. })
                    ),
                    "{kind:?} {name}: descriptor/physical contract must reject with a stable error"
                );
            } else {
                assert!(
                    matches!(result, Err(CompactParquetError::UnsupportedValueEncoding)),
                    "{kind:?} {name}: unsupported page encoding must be rejected before value use"
                );
            }
        }
    }
}

#[test]
fn rejects_malformed_noncanonical_and_wrong_typed_ovb_for_each_kind() {
    for kind in [Kind::Str, Kind::Uuid, Kind::Date, Kind::Instant] {
        let profile = profile(kind);
        for (name, value) in malformed_values(kind) {
            let bytes = parquet(
                &profile,
                kind,
                "ovb",
                vec![OvbRaw::Int(1.into())],
                None,
                Physical::ByteArray,
                Level::Required,
                Encoding::PLAIN,
                &[value],
            );
            let result = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1);
            assert!(
                matches!(
                    result,
                    Err(CompactParquetError::InvalidMetadata)
                        | Err(CompactParquetError::UnsupportedKeyMapping)
                ),
                "{kind:?} {name}: malformed fallback must fail at the reader boundary"
            );
        }
    }
}

#[test]
fn rejects_truncated_ovb_page_and_row_count_mismatch() {
    for kind in [Kind::Str, Kind::Uuid, Kind::Date, Kind::Instant] {
        let profile = profile(kind);
        let value = valid_value(kind);
        let bytes = parquet(
            &profile,
            kind,
            "ovb",
            vec![OvbRaw::Int(1.into())],
            None,
            Physical::ByteArray,
            Level::Required,
            Encoding::PLAIN,
            &[value],
        );
        let truncated = bytes[..footer_start(&bytes)].to_vec();
        assert!(
            matches!(
                CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &truncated, 1),
                Err(CompactParquetError::InvalidParquet)
                    | Err(CompactParquetError::InvalidMetadata)
            ),
            "{kind:?}: truncated fallback page must fail before logical use"
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
            Err(CompactParquetError::RowCountMismatch {
                expected: 2,
                observed: 1
            })
        ));
    }
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

fn footer_start(bytes: &[u8]) -> usize {
    let length = u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap());
    bytes.len() - 8 - length as usize
}
fn corrupt_dictionary_page_offset(bytes: Vec<u8>, offset_delta: i64) -> Vec<u8> {
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let metadata = reader.metadata().clone();
    let mut groups = metadata.row_groups().to_vec();
    let mut group_builder = groups[0].clone().into_builder();
    let mut columns = group_builder.take_columns();
    let column = &mut columns[0];
    let dictionary_offset = column.dictionary_page_offset().unwrap();
    *column = column
        .clone()
        .into_builder()
        .set_dictionary_page_offset(Some(dictionary_offset + offset_delta))
        .build()
        .unwrap();
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

#[test]
fn rejects_malformed_rle_dictionary_ovb_dictionary_and_page_boundaries() {
    let profile = profile(Kind::Str);
    let values = vec![
        canonical(OvbRaw::Text("alpha".into())),
        canonical(OvbRaw::Text("alpha".into())),
        canonical(OvbRaw::Text("beta".into())),
        canonical(OvbRaw::Text("beta".into())),
    ];
    let bytes = dictionary_parquet(&profile, &values);
    let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
    let column = &reader.metadata().row_groups()[0].columns()[0];
    assert!(
        column.dictionary_page_offset().is_some(),
        "fixture must contain a dictionary page"
    );
    assert!(
        column
            .encodings()
            .any(|encoding| encoding == Encoding::RLE_DICTIONARY),
        "fixture must contain an RLE_DICTIONARY BYTE_ARRAY data page"
    );

    let malformed_metadata = corrupt_dictionary_page_offset(bytes.clone(), 1);
    let result = CompactParquetKeySource::decode_verified_bytes(
        &profile,
        TABLE,
        &malformed_metadata,
        values.len() as u64,
    );
    assert!(
        matches!(
            result.as_ref(),
            Err(CompactParquetError::InvalidParquet)
        )
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "invalid compact Parquet data"
    );

    let dictionary_start = column.dictionary_page_offset().unwrap() as usize;
    let dictionary_end = dictionary_start + column.compressed_size() as usize;
    let mut malformed_page = bytes.clone();
    malformed_page.remove(dictionary_end - 1);
    assert!(matches!(
        CompactParquetKeySource::decode_verified_bytes(
            &profile,
            TABLE,
            &malformed_page,
            values.len() as u64,
        ),
        Err(CompactParquetError::InvalidParquet)
    ));
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
                let data_offset = meta.data_page_offset() as usize - start;
                let (data, shift, inserted) = checksummed_chunk(&bytes[start..end], data_offset);
                (start, end, data, shift, inserted)
            })
        })
        .collect::<Vec<_>>();
    chunks.sort_by_key(|chunk| chunk.0);
    let mut output = bytes[..chunks.first().map_or(footer, |chunk| chunk.0)].to_vec();
    let mut cursor = output.len();
    let mut updates = Vec::new();
    for (start, end, data, shift, inserted) in chunks {
        output.extend(&bytes[cursor..start]);
        let new_start = output.len();
        output.extend(data);
        updates.push((start, end, new_start, new_start + shift, inserted));
        cursor = end;
    }
    output.extend(&bytes[cursor..footer]);
    let mut groups = metadata.row_groups().to_vec();
    for group in &mut groups {
        let mut builder = group.clone().into_builder();
        let mut columns = builder.take_columns();
        for column in &mut columns {
            let old_start = column.dictionary_page_offset().unwrap_or_else(|| column.data_page_offset()) as usize;
            let Some((_, _, new_start, data_offset, inserted)) = updates.iter().find(|item| item.0 == old_start) else {
                continue;
            };
            let shift = *new_start as i64 - old_start as i64;
            *column = column
                .clone()
                .into_builder()
                .set_data_page_offset(column.data_page_offset() + shift)
                .set_dictionary_page_offset(column.dictionary_page_offset().map(|offset| offset + shift))
                .set_total_compressed_size(column.compressed_size() + *inserted as i64)
                .build()
                .unwrap();
            let _ = data_offset;
        }
        *group = builder.set_column_metadata(columns).build().unwrap();
    }
    let rewritten = parquet::file::metadata::ParquetMetaData::new(metadata.file_metadata().clone(), groups);
    let mut footer_bytes = Vec::new();
    parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer_bytes, &rewritten)
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
