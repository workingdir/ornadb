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
        let result = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1);
        assert!(result.is_err(), "{name}: malformed UUID metadata must fail closed");
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
    assert!(nullable_result.is_err());

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
    assert!(repeated_result.is_err());
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
    let result = CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1);
    println!("encoding result: {result:?}");
    assert!(result.is_err());
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
