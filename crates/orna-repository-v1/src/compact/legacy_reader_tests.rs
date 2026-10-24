//! Legacy compact reader tests.
//!
//! Format-1/2 snapshots keep their compact readers after the compact writer's
//! retirement (ORNA-UPGRADE-002, ORNA-ROW-013). These tests build such
//! snapshots with the test-only fixture writer,
//! `Repository::prepare_compact_publication`, and prove the readers: manifest
//! inventory, hydration, observation, schema admission and ordinary-commit
//! preservation. They live inside the crate so that `cargo test --lib` runs
//! them; production builds never contain the fixture writer.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use parquet::{
    basic::{Compression, Encoding, PageType},
    column::reader::ColumnReader,
    data_type::{BoolType, ByteArray, ByteArrayType, Int32Type, Int64Type},
    file::{
        metadata::{KeyValue, ParquetMetaDataWriter},
        properties::{WriterProperties, WriterVersion},
        reader::{FileReader, SerializedFileReader},
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

use super::{CompactCommittedRow, resolve_legacy_authoritative_rows};
use crate::{
    CompactManifest, CompactPublicationPlan, CompactSegment, CompactSegmentRole, GitCommitRef,
    GitDeclaredObjectSetState, GitObjectKind, GitObjectState, IndexGeneration, ManagedFileChange,
    ManagedPath, Repository, RepositoryError, RuntimeGeneration, WorktreeState, test_support,
};

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("../../tests/fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

fn git_state(
    repository: &Repository,
    root: &Path,
) -> (
    Option<GitCommitRef>,
    IndexGeneration,
    WorktreeState,
    String,
    String,
) {
    (
        repository.head().unwrap(),
        repository.index_generation().unwrap(),
        repository.worktree_state().unwrap(),
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        git(root, &["config", "--local", "--null", "--list"]),
    )
}

fn with_remote(root: &Path) {
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
}

fn continuity_remote(root: &Path) -> TempDir {
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "--bare", "."]);
    let remote_path = remote.path().to_str().unwrap();
    git(root, &["remote", "add", "origin", remote_path]);
    git(root, &["push", "origin", "HEAD:refs/heads/main"]);
    remote
}

fn with_partial_clone(root: &Path) {
    with_remote(root);
    git(root, &["config", "extensions.partialClone", "origin"]);
    git(root, &["config", "remote.origin.promisor", "true"]);
    git(
        root,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
}

fn compact_segment(table: Uuid, ordinal: u64, bytes: Vec<u8>) -> CompactSegment {
    compact_segment_with_manifest_columns(table, ordinal, bytes, compact_columns())
}

fn compact_segment_with_manifest_columns(
    table: Uuid,
    ordinal: u64,
    payload: Vec<u8>,
    columns: Vec<u8>,
) -> CompactSegment {
    let segment_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, ordinal | 0x8000_0000_0000_0000);
    let path = ManagedPath::new(format!(
        ".orna/storage/{table}/data/{}/{segment_id}.parquet",
        &segment_id.to_string()[..2]
    ))
    .unwrap();
    let physical_columns = compact_columns();
    let bytes = compact_parquet(table, ordinal, &physical_columns, payload);
    let key = CanonicalValue::new(OvbRaw::Int(ordinal.into()))
        .unwrap()
        .encode()
        .unwrap();
    CompactSegment::new(
        segment_id,
        CompactSegmentRole::Data,
        compact_schema_fingerprint(table),
        "test-encoder-v1",
        path,
        bytes,
        key.clone(),
        key,
        1,
        columns,
        true,
        false,
    )
    .unwrap()
}

fn compact_parquet(table: Uuid, ordinal: u64, columns: &[u8], payload: Vec<u8>) -> Vec<u8> {
    let schema = Arc::new(
        parse_message_type(&format!(
            "message schema {{ REQUIRED INT64 f_{}; }}",
            compact_field_id().simple()
        ))
        .unwrap(),
    );
    let metadata = vec![
        KeyValue::new(
            "orna.profile".to_owned(),
            Some("compact-storage-v1".to_owned()),
        ),
        KeyValue::new("orna.table".to_owned(), Some(table.to_string())),
        KeyValue::new(
            "orna.schema.sha256".to_owned(),
            Some(hex_digest(&compact_schema_fingerprint(table))),
        ),
        KeyValue::new(
            "orna.schema.ovb".to_owned(),
            Some(orna_value_v1::base64_encode(&compact_schema(table).encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(orna_value_v1::base64_encode(columns))),
        KeyValue::new(
            "orna.encoder".to_owned(),
            Some("test-encoder-v1".to_owned()),
        ),
        KeyValue::new(
            "orna.test.payload".to_owned(),
            Some(String::from_utf8_lossy(&payload).into_owned()),
        ),
    ];
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    column
        .typed::<Int64Type>()
        .write_batch(&[i64::try_from(ordinal).unwrap()], None, None)
        .unwrap();
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();

    // parquet-rs uses its writer-version setting for FileMetaData.version.
    // The Orna profile requires V2 pages while retaining FileMetaData.version 1.
    let footer_length =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as usize;
    let footer = bytes.len() - 8 - footer_length;
    assert_eq!(&bytes[footer..footer + 2], &[0x15, 0x04]);
    bytes[footer + 1] = 0x02;
    with_page_checksums(bytes)
}

fn date_segment(table: Uuid, ordinal: u64, days: &[i32]) -> CompactSegment {
    let segment_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, ordinal | 0x8000_0000_0000_0000);
    let path = ManagedPath::new(format!(
        ".orna/storage/{table}/data/{}/{segment_id}.parquet",
        &segment_id.to_string()[..2]
    ))
    .unwrap();
    let schema = date_schema(table);
    let columns = date_columns();
    let bytes = date_parquet(table, &schema, &columns, days);
    let bound = CanonicalValue::new(OvbRaw::Int(ordinal.into()))
        .unwrap()
        .encode()
        .unwrap();
    CompactSegment::new(
        segment_id,
        CompactSegmentRole::Data,
        date_schema_fingerprint(table),
        "test-encoder-v1",
        path,
        bytes,
        bound.clone(),
        bound,
        u64::try_from(days.len()).unwrap(),
        columns,
        true,
        false,
    )
    .unwrap()
}

fn date_parquet(table: Uuid, schema: &SchemaDescriptor, columns: &[u8], days: &[i32]) -> Vec<u8> {
    let schema_descriptor = Arc::new(
        parse_message_type(&format!(
            "message schema {{ REQUIRED INT32 f_{} (DATE); }}",
            date_field_id().simple()
        ))
        .unwrap(),
    );
    let metadata = vec![
        KeyValue::new(
            "orna.profile".to_owned(),
            Some("compact-storage-v1".to_owned()),
        ),
        KeyValue::new("orna.table".to_owned(), Some(table.to_string())),
        KeyValue::new(
            "orna.schema.sha256".to_owned(),
            Some(hex_digest(&schema_fingerprint(schema))),
        ),
        KeyValue::new(
            "orna.schema.ovb".to_owned(),
            Some(orna_value_v1::base64_encode(&schema.encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(orna_value_v1::base64_encode(columns))),
        KeyValue::new(
            "orna.encoder".to_owned(),
            Some("test-encoder-v1".to_owned()),
        ),
    ];
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema_descriptor, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    column
        .typed::<Int32Type>()
        .write_batch(days, None, None)
        .unwrap();
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();

    let footer_length =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as usize;
    let footer = bytes.len() - 8 - footer_length;
    assert_eq!(&bytes[footer..footer + 2], &[0x15, 0x04]);
    bytes[footer + 1] = 0x02;
    with_page_checksums(bytes)
}

fn compact_columns() -> Vec<u8> {
    compact_columns_for_field(compact_field_id())
}

fn compact_field_id() -> Uuid {
    Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001)
}

fn date_field_id() -> Uuid {
    Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0002)
}

fn compact_schema(table: Uuid) -> SchemaDescriptor {
    compact_schema_for_type(table, compact_field_id(), "Int")
}

fn date_schema(table: Uuid) -> SchemaDescriptor {
    compact_schema_for_type(table, date_field_id(), "Date")
}

fn compact_schema_for_type(table: Uuid, field: Uuid, type_name: &str) -> SchemaDescriptor {
    SchemaDescriptor::new(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (
            OvbRaw::Int(1.into()),
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
        ),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![OvbRaw::Tag(
                37,
                Box::new(OvbRaw::Bytes(field.as_bytes().to_vec())),
            )]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(field.as_bytes().to_vec()))),
                OvbRaw::Text(format!("f_{}", field.simple())),
                OvbRaw::Array(vec![
                    OvbRaw::Int(0.into()),
                    OvbRaw::Text(type_name.to_owned()),
                ]),
                OvbRaw::Int(0.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ])]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]))
    .unwrap()
}

fn compact_schema_fingerprint(table: Uuid) -> [u8; 32] {
    schema_fingerprint(&compact_schema(table))
}

fn date_schema_fingerprint(table: Uuid) -> [u8; 32] {
    schema_fingerprint(&date_schema(table))
}

fn schema_fingerprint(schema: &SchemaDescriptor) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.schema.v1\0");
    digest.update(schema.encode().unwrap());
    digest.finalize().into()
}

fn compact_columns_for_field(field_id: Uuid) -> Vec<u8> {
    compact_columns_for_mapping(field_id, "Int", "int64")
}

fn date_columns() -> Vec<u8> {
    compact_columns_for_mapping(date_field_id(), "Date", "date")
}

fn compact_columns_for_mapping(field_id: Uuid, type_name: &str, encoding: &str) -> Vec<u8> {
    let parameters = if encoding == "ovb" {
        vec![OvbRaw::Int(1.into())]
    } else {
        Vec::new()
    };
    compact_columns_for_mapping_with_parameters(field_id, type_name, encoding, parameters)
}

fn compact_columns_for_mapping_with_parameters(
    field_id: Uuid,
    type_name: &str,
    encoding: &str,
    parameters: Vec<OvbRaw>,
) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Array(vec![OvbRaw::Array(vec![
        OvbRaw::Array(vec![OvbRaw::Tag(
            37,
            Box::new(OvbRaw::Bytes(field_id.as_bytes().to_vec())),
        )]),
        OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", field_id.simple()))]),
        OvbRaw::Array(vec![
            OvbRaw::Int(0.into()),
            OvbRaw::Text(type_name.to_owned()),
        ]),
        OvbRaw::Text(encoding.to_owned()),
        OvbRaw::Array(parameters),
    ])]))
    .unwrap()
    .encode()
    .unwrap()
}

fn compact_schema_with_roles(table: Uuid, fields: &[(Uuid, &str, u8)]) -> SchemaDescriptor {
    compact_schema_with_roles_type(table, fields, "Int")
}

fn compact_schema_with_roles_type(
    table: Uuid,
    fields: &[(Uuid, &str, u8)],
    type_name: &str,
) -> SchemaDescriptor {
    let mut fields = fields.to_vec();
    fields.sort_by_key(|(field, _, _)| *field);
    SchemaDescriptor::new(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (
            OvbRaw::Int(1.into()),
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
        ),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(
                fields
                    .iter()
                    .filter(|(_, _, role)| *role == 0)
                    .map(|(field, _, _)| {
                        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(field.as_bytes().to_vec())))
                    })
                    .collect(),
            ),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(
                fields
                    .into_iter()
                    .map(|(field, name, role)| {
                        OvbRaw::Array(vec![
                            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(field.as_bytes().to_vec()))),
                            OvbRaw::Text(name.to_owned()),
                            OvbRaw::Array(vec![
                                OvbRaw::Int(0.into()),
                                OvbRaw::Text(type_name.to_owned()),
                            ]),
                            OvbRaw::Int(role.into()),
                            if role == 2 {
                                OvbRaw::Array(vec![
                                    OvbRaw::Int(2.into()),
                                    OvbRaw::Bytes(vec![0; 32]),
                                ])
                            } else {
                                OvbRaw::Array(vec![OvbRaw::Int(0.into())])
                            },
                        ])
                    })
                    .collect(),
            ),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]))
    .unwrap()
}

fn compact_primitive_parquet(
    table: Uuid,
    schema: &SchemaDescriptor,
    physical_field: Uuid,
    type_name: &str,
    columns: &[u8],
) -> Vec<u8> {
    let (physical_type, write_value) = match type_name {
        "Int" => ("INT64", false),
        "Bool" => ("BOOLEAN", true),
        _ => panic!("test fixture supports only Int and Bool"),
    };
    let schema_descriptor = Arc::new(
        parse_message_type(&format!(
            "message schema {{ REQUIRED {physical_type} f_{}; }}",
            physical_field.simple()
        ))
        .unwrap(),
    );
    let metadata = vec![
        KeyValue::new(
            "orna.profile".to_owned(),
            Some("compact-storage-v1".to_owned()),
        ),
        KeyValue::new("orna.table".to_owned(), Some(table.to_string())),
        KeyValue::new(
            "orna.schema.sha256".to_owned(),
            Some(hex_digest(&schema_fingerprint(schema))),
        ),
        KeyValue::new(
            "orna.schema.ovb".to_owned(),
            Some(orna_value_v1::base64_encode(&schema.encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(orna_value_v1::base64_encode(columns))),
        KeyValue::new(
            "orna.encoder".to_owned(),
            Some("test-encoder-v1".to_owned()),
        ),
    ];
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema_descriptor, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    if write_value {
        column
            .typed::<BoolType>()
            .write_batch(&[true], None, None)
            .unwrap();
    } else {
        column
            .typed::<Int64Type>()
            .write_batch(&[1], None, None)
            .unwrap();
    }
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();

    let footer_length =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as usize;
    let footer = bytes.len() - 8 - footer_length;
    assert_eq!(&bytes[footer..footer + 2], &[0x15, 0x04]);
    bytes[footer + 1] = 0x02;
    with_page_checksums(bytes)
}

fn compact_ovb_parquet(
    table: Uuid,
    schema: &SchemaDescriptor,
    physical_field: Uuid,
    columns: &[u8],
    values: &[Vec<u8>],
    annotation: Option<&str>,
) -> Vec<u8> {
    let annotation = annotation.unwrap_or_default();
    let schema_descriptor = Arc::new(
        parse_message_type(&format!(
            "message schema {{ REQUIRED BYTE_ARRAY f_{}{}; }}",
            physical_field.simple(),
            annotation
        ))
        .unwrap(),
    );
    let metadata = vec![
        KeyValue::new(
            "orna.profile".to_owned(),
            Some("compact-storage-v1".to_owned()),
        ),
        KeyValue::new("orna.table".to_owned(), Some(table.to_string())),
        KeyValue::new(
            "orna.schema.sha256".to_owned(),
            Some(hex_digest(&schema_fingerprint(schema))),
        ),
        KeyValue::new(
            "orna.schema.ovb".to_owned(),
            Some(orna_value_v1::base64_encode(&schema.encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(orna_value_v1::base64_encode(columns))),
        KeyValue::new(
            "orna.encoder".to_owned(),
            Some("test-encoder-v1".to_owned()),
        ),
    ];
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_encoding(Encoding::PLAIN)
            .set_writer_version(WriterVersion::PARQUET_2_0)
            .set_key_value_metadata(Some(metadata))
            .build(),
    );
    let mut bytes = Vec::new();
    let mut writer = SerializedFileWriter::new(&mut bytes, schema_descriptor, properties).unwrap();
    let mut row_group = writer.next_row_group().unwrap();
    let mut column = row_group.next_column().unwrap().unwrap();
    let values = values
        .iter()
        .map(|value| ByteArray::from(value.as_slice()))
        .collect::<Vec<_>>();
    column
        .typed::<ByteArrayType>()
        .write_batch(&values, None, None)
        .unwrap();
    column.close().unwrap();
    row_group.close().unwrap();
    writer.close().unwrap();

    let footer_length =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as usize;
    let footer = bytes.len() - 8 - footer_length;
    assert_eq!(&bytes[footer..footer + 2], &[0x15, 0x04]);
    bytes[footer + 1] = 0x02;
    with_page_checksums(bytes)
}

fn compact_segment_for_schema(
    table: Uuid,
    ordinal: u64,
    role: CompactSegmentRole,
    schema: &SchemaDescriptor,
    physical_field: Uuid,
    type_name: &str,
    columns: Vec<u8>,
) -> CompactSegment {
    let segment_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, ordinal | 0x8000_0000_0000_0000);
    let path = ManagedPath::new(format!(
        ".orna/storage/{table}/data/{}/{segment_id}.parquet",
        &segment_id.to_string()[..2]
    ))
    .unwrap();
    let key = CanonicalValue::new(OvbRaw::Int(ordinal.into()))
        .unwrap()
        .encode()
        .unwrap();
    CompactSegment::new(
        segment_id,
        role,
        schema_fingerprint(schema),
        "test-encoder-v1",
        path,
        compact_primitive_parquet(table, schema, physical_field, type_name, &columns),
        key.clone(),
        key,
        1,
        columns,
        true,
        false,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn compact_ovb_segment_for_schema(
    table: Uuid,
    ordinal: u64,
    role: CompactSegmentRole,
    schema: &SchemaDescriptor,
    physical_field: Uuid,
    columns: Vec<u8>,
    values: &[Vec<u8>],
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    annotation: Option<&str>,
) -> CompactSegment {
    let segment_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, ordinal | 0x8000_0000_0000_0000);
    let path = ManagedPath::new(format!(
        ".orna/storage/{table}/data/{}/{segment_id}.parquet",
        &segment_id.to_string()[..2]
    ))
    .unwrap();
    CompactSegment::new(
        segment_id,
        role,
        schema_fingerprint(schema),
        "test-encoder-v1",
        path,
        compact_ovb_parquet(table, schema, physical_field, &columns, values, annotation),
        min_key,
        max_key,
        u64::try_from(values.len()).unwrap(),
        columns,
        true,
        false,
    )
    .unwrap()
}

fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
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

/// Builds a compact candidate with the test-only legacy fixture writer.
fn compact_plan(
    repository: &Repository,
    table: Uuid,
    intent: [u8; 16],
    segments: &[CompactSegment],
) -> CompactPublicationPlan {
    let head = repository.head().unwrap().unwrap();
    let base = repository
        .read_compact_manifest(&head, table)
        .unwrap()
        .unwrap_or_else(|| CompactManifest::empty(table, compact_schema_fingerprint(table)));
    repository
        .prepare_compact_publication(
            &head,
            repository.index_generation().unwrap(),
            base,
            intent,
            [intent[0]; 32],
            segments,
            "orna: publish compact runtime data",
        )
        .unwrap()
}

/// Makes a fixture-writer candidate the committed `HEAD`, as a compact
/// publication by an older build left it; this build cannot publish one.
fn adopt_compact_fixture(repository: &Repository, plan: &CompactPublicationPlan) {
    git(
        repository.worktree(),
        &[
            "reset",
            "--quiet",
            "--hard",
            plan.candidate_commit().as_str(),
        ],
    );
}

struct FilteredCompactClone {
    _fixture: TempDir,
    clone: PathBuf,
    materialized_table: Uuid,
    materialized_object: String,
    promised_table: Uuid,
    promised_object: String,
    unrelated_promised_object: String,
}

/// Builds a compact snapshot in a real file-protocol filtered clone. Metadata
/// and one segment are intentionally materialized before the promisor endpoint
/// is disabled; the other segment and an unrelated blob must remain promised.
fn filtered_compact_clone() -> Option<FilteredCompactClone> {
    let fixture = TempDir::new().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", "origin.git"]);
    git(fixture.path(), &["init", "-b", "main", "seed"]);
    test_support::configure_fixture_git_identity(&seed);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    fs::write(seed.join("main.orna"), "module main;\n").ok()?;
    fs::write(seed.join("ordinary.txt"), "base\n").ok()?;
    fs::create_dir_all(seed.join(".orna")).ok()?;
    fs::write(seed.join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);

    let repository = Repository::discover(&seed).ok()?;
    let materialized_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0610);
    adopt_compact_fixture(
        &repository,
        &compact_plan(
            &repository,
            materialized_table,
            [70; 16],
            &[compact_segment(
                materialized_table,
                70,
                b"materialized inventory segment".to_vec(),
            )],
        ),
    );

    let promised_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0611);
    adopt_compact_fixture(
        &repository,
        &compact_plan(
            &repository,
            promised_table,
            [71; 16],
            &[compact_segment(
                promised_table,
                71,
                b"promised inventory segment".to_vec(),
            )],
        ),
    );

    let head = repository.head().ok()??;
    let materialized_manifest = repository
        .read_compact_manifest(&head, materialized_table)
        .ok()??;
    let promised_manifest = repository
        .read_compact_manifest(&head, promised_table)
        .ok()??;
    let materialized_object = materialized_manifest
        .entries()
        .first()?
        .git_object_id()
        .to_owned();
    let promised_object = promised_manifest
        .entries()
        .first()?
        .git_object_id()
        .to_owned();
    fs::write(seed.join("unrelated-promised.txt"), "unrelated promised\n").ok()?;
    git(&seed, &["add", "unrelated-promised.txt"]);
    git(&seed, &["commit", "-m", "add unrelated promised blob"]);
    let unrelated_promised_object = git(&seed, &["rev-parse", "HEAD:unrelated-promised.txt"]);

    git(&origin, &["config", "uploadpack.allowFilter", "true"]);
    let origin_url = format!("file://{}", origin.display());
    git(&seed, &["remote", "add", "origin", &origin_url]);
    git(&seed, &["push", "origin", "HEAD:refs/heads/main"]);
    git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let output = Command::new("git")
        .current_dir(fixture.path())
        .args([
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &origin_url,
            "partial",
        ])
        .output()
        .ok()?;
    if !output.status.success()
        || git(
            &clone,
            &["config", "--local", "--get", "remote.origin.promisor"],
        ) != "true"
    {
        return None;
    }

    for path in [
        format!(".orna/storage/{materialized_table}/manifest.orna"),
        format!(".orna/storage/{materialized_table}/shards/00000000.orna"),
        materialized_manifest
            .entries()
            .first()?
            .relative_path()
            .as_path()
            .display()
            .to_string(),
        format!(".orna/storage/{promised_table}/manifest.orna"),
        format!(".orna/storage/{promised_table}/shards/00000000.orna"),
    ] {
        let selector = format!("HEAD:{path}");
        let output = Command::new("git")
            .current_dir(&clone)
            .args(["cat-file", "blob", &selector])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
    }

    for object in [&promised_object, &unrelated_promised_object] {
        let output = Command::new("git")
            .current_dir(&clone)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", object])
            .output()
            .ok()?;
        if output.status.success() {
            return None;
        }
    }
    git(
        &clone,
        &[
            "config",
            "remote.origin.url",
            "file:///nonexistent-disabled-promisor",
        ],
    );
    Some(FilteredCompactClone {
        _fixture: fixture,
        clone,
        materialized_table,
        materialized_object,
        promised_table,
        promised_object,
        unrelated_promised_object,
    })
}

fn no_lazy_object_inventory(root: &Path) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["rev-list", "--objects", "--missing=print", "--all"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "no-lazy object inventory: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8(output.stdout).unwrap(),
        git(root, &["count-objects", "-v"])
    )
}

#[test]
fn compact_manifest_inventory_discovers_every_table_without_mutating_repository_or_runtime_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let first_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0604);
    let first = compact_plan(
        &repo,
        first_table,
        [60; 16],
        &[compact_segment(
            first_table,
            60,
            b"first compact table".to_vec(),
        )],
    );
    adopt_compact_fixture(&repo, &first);

    let second_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0605);
    let second = compact_plan(
        &repo,
        second_table,
        [61; 16],
        &[compact_segment(
            second_table,
            61,
            b"second compact table".to_vec(),
        )],
    );
    adopt_compact_fixture(&repo, &second);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());
    let index_before = fs::read(root.path().join(".git/index")).unwrap();
    let worktree_before = fs::read(root.path().join("main.orna")).unwrap();
    let runtime = RuntimeGeneration::new(604);
    let runtime_before = repo.cwd_generation(runtime).unwrap();
    let journal_before = repo.read_publication_journal().unwrap();

    let inventory = repo.read_compact_manifest_inventory(&head).unwrap();

    assert_eq!(inventory.len(), 2);
    assert_eq!(inventory[&first_table].table(), first_table);
    assert_eq!(inventory[&first_table].entries().len(), 1);
    assert_eq!(inventory[&second_table].table(), second_table);
    assert_eq!(inventory[&second_table].entries().len(), 1);
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(root.path().join("main.orna")).unwrap(),
        worktree_before
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
    assert_eq!(repo.cwd_generation(runtime).unwrap(), runtime_before);
    assert_eq!(repo.read_publication_journal().unwrap(), journal_before);
}

#[test]
fn compact_manifest_inventory_hydration_plans_all_tables_without_fetching_promised_segments() {
    let fixture = filtered_compact_clone()
        .expect("compact planner evidence requires a real filtered clone with promised blobs");
    let repo = Repository::discover(&fixture.clone).unwrap();
    let head = repo.head().unwrap().unwrap();

    assert!(matches!(
        repo.observe_git_object(&fixture.materialized_object)
            .unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        }
    ));
    assert_eq!(
        repo.observe_git_object(&fixture.promised_object).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repo.observe_git_object(&fixture.unrelated_promised_object)
            .unwrap(),
        GitObjectState::Promised
    );

    fs::write(fixture.clone.join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(&fixture.clone, &["add", "ordinary.txt"]);
    fs::write(fixture.clone.join("main.orna"), "unstaged ordinary\n").unwrap();
    let fetch_head = fixture.clone.join(git(
        &fixture.clone,
        &["rev-parse", "--git-path", "FETCH_HEAD"],
    ));
    fs::write(&fetch_head, b"inventory-planner-sentinel\n").unwrap();
    let before = git_state(&repo, &fixture.clone);
    let objects_before = no_lazy_object_inventory(&fixture.clone);
    let index_before = fs::read(fixture.clone.join(".git/index")).unwrap();
    let worktree_before = fs::read(fixture.clone.join("main.orna")).unwrap();
    let runtime = RuntimeGeneration::new(610);
    let runtime_before = repo.cwd_generation(runtime).unwrap();
    let journal_before = repo.read_publication_journal().unwrap();
    let fetch_head_before = fs::read(&fetch_head).unwrap();

    let plan = repo
        .plan_compact_manifest_inventory_hydration(&head)
        .unwrap();

    assert_eq!(plan.len(), 2);
    assert!(matches!(
        plan[&fixture.materialized_table].get(&fixture.materialized_object),
        Some(GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        })
    ));
    assert_eq!(
        plan[&fixture.promised_table].get(&fixture.promised_object),
        Some(&GitObjectState::Promised)
    );
    for object in [&fixture.promised_object, &fixture.unrelated_promised_object] {
        assert!(
            !Command::new("git")
                .current_dir(&fixture.clone)
                .env("GIT_NO_LAZY_FETCH", "1")
                .args(["cat-file", "-e", object])
                .output()
                .unwrap()
                .status
                .success(),
            "planner fetched a promised object despite the disabled promisor"
        );
    }
    assert_eq!(
        repo.observe_git_object(&fixture.promised_object).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repo.observe_git_object(&fixture.unrelated_promised_object)
            .unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(no_lazy_object_inventory(&fixture.clone), objects_before);
    assert_eq!(git_state(&repo, &fixture.clone), before);
    assert_eq!(
        fs::read(fixture.clone.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(fixture.clone.join("main.orna")).unwrap(),
        worktree_before
    );
    assert_eq!(repo.cwd_generation(runtime).unwrap(), runtime_before);
    assert_eq!(repo.read_publication_journal().unwrap(), journal_before);
    assert_eq!(fs::read(&fetch_head).unwrap(), fetch_head_before);
}

#[test]
fn compact_manifest_inventory_hydration_rejects_missing_malformed_and_orphan_closure_without_mutation()
 {
    for failure in ["missing-segment", "malformed-shard", "orphan"] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::new_v4();
        let plan = compact_plan(
            &repo,
            table,
            [72; 16],
            &[compact_segment(
                table,
                72,
                b"inventory closure witness".to_vec(),
            )],
        );
        let manifest = repo
            .read_compact_manifest(plan.candidate_commit(), table)
            .unwrap()
            .unwrap();
        let manifest_path =
            ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
        let shard_path =
            ManagedPath::new(format!(".orna/storage/{table}/shards/00000000.orna")).unwrap();
        let segment_path = manifest.entries()[0].relative_path().clone();
        let manifest_bytes = git_bytes(
            root.path(),
            &[
                "show",
                &format!(
                    "{}:{}",
                    plan.candidate_commit(),
                    manifest_path.as_path().display()
                ),
            ],
        );
        let shard_bytes = git_bytes(
            root.path(),
            &[
                "show",
                &format!(
                    "{}:{}",
                    plan.candidate_commit(),
                    shard_path.as_path().display()
                ),
            ],
        );
        let segment_bytes = git_bytes(
            root.path(),
            &[
                "show",
                &format!(
                    "{}:{}",
                    plan.candidate_commit(),
                    segment_path.as_path().display()
                ),
            ],
        );
        let changes = match failure {
            "missing-segment" => vec![
                ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                ManagedFileChange::new(shard_path, Some(shard_bytes)),
            ],
            "malformed-shard" => vec![
                ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                ManagedFileChange::new(
                    shard_path,
                    Some(b"not a compact manifest shard\n".to_vec()),
                ),
                ManagedFileChange::new(segment_path, Some(segment_bytes)),
            ],
            "orphan" => vec![
                ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                ManagedFileChange::new(shard_path, Some(shard_bytes)),
                ManagedFileChange::new(segment_path, Some(segment_bytes)),
                ManagedFileChange::new(
                    ManagedPath::new(format!(
                        ".orna/storage/{table}/data/01/{}.parquet",
                        Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0612)
                    ))
                    .unwrap(),
                    Some(b"orphan inventory segment".to_vec()),
                ),
            ],
            _ => unreachable!(),
        };
        let head = repo.head().unwrap().unwrap();
        let candidate = repo
            .build_private_commit(&head, &changes, "test: malformed inventory planner closure")
            .unwrap();
        repo.advance_current_ref(&head, &candidate).unwrap();

        fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
        let before = git_state(&repo, root.path());
        let objects_before = no_lazy_object_inventory(root.path());
        let index_before = fs::read(root.path().join(".git/index")).unwrap();
        let worktree_before = fs::read(root.path().join("main.orna")).unwrap();
        let runtime = RuntimeGeneration::new(611);
        let runtime_before = repo.cwd_generation(runtime).unwrap();
        let journal_before = repo.read_publication_journal().unwrap();

        assert!(matches!(
            repo.plan_compact_manifest_inventory_hydration(candidate.commit()),
            Err(RepositoryError::InvalidCompactManifest)
        ));
        assert_eq!(git_state(&repo, root.path()), before);
        assert_eq!(no_lazy_object_inventory(root.path()), objects_before);
        assert_eq!(
            fs::read(root.path().join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            fs::read(root.path().join("main.orna")).unwrap(),
            worktree_before
        );
        assert_eq!(repo.cwd_generation(runtime).unwrap(), runtime_before);
        assert_eq!(repo.read_publication_journal().unwrap(), journal_before);
    }
}

#[test]
fn compact_manifest_inventory_hydration_rejects_later_invalid_table_without_partial_result_or_mutation()
 {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let first = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0613);
    adopt_compact_fixture(
        &repo,
        &compact_plan(
            &repo,
            first,
            [73; 16],
            &[compact_segment(
                first,
                73,
                b"first inventory table".to_vec(),
            )],
        ),
    );

    let second = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0614);
    let plan = compact_plan(
        &repo,
        second,
        [74; 16],
        &[compact_segment(
            second,
            74,
            b"missing second inventory segment".to_vec(),
        )],
    );
    let manifest_path = ManagedPath::new(format!(".orna/storage/{second}/manifest.orna")).unwrap();
    let shard_path =
        ManagedPath::new(format!(".orna/storage/{second}/shards/00000000.orna")).unwrap();
    let manifest_bytes = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                manifest_path.as_path().display()
            ),
        ],
    );
    let shard_bytes = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                shard_path.as_path().display()
            ),
        ],
    );
    let head = repo.head().unwrap().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[
                ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                ManagedFileChange::new(shard_path, Some(shard_bytes)),
            ],
            "test: commit later invalid compact inventory table",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    let before = git_state(&repo, root.path());
    let objects_before = no_lazy_object_inventory(root.path());

    assert!(matches!(
        repo.plan_compact_manifest_inventory_hydration(candidate.commit()),
        Err(RepositoryError::InvalidCompactManifest)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(no_lazy_object_inventory(root.path()), objects_before);
}

#[test]
fn compact_manifest_inventory_rejects_unreferenced_shard_or_segment_artifacts() {
    for artifact in [
        "shards/00000001.orna".to_owned(),
        format!(
            "data/01/{}.parquet",
            Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0609)
        ),
    ] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0608);
        let plan = compact_plan(
            &repo,
            table,
            [64; 16],
            &[compact_segment(
                table,
                64,
                b"declared compact segment".to_vec(),
            )],
        );
        adopt_compact_fixture(&repo, &plan);
        let head = repo.head().unwrap().unwrap();
        let orphan = repo
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(
                    ManagedPath::new(format!(".orna/storage/{table}/{artifact}")).unwrap(),
                    Some(b"unreferenced compact artifact".to_vec()),
                )],
                "test: commit orphan compact inventory artifact",
            )
            .unwrap();
        repo.advance_current_ref(&head, &orphan).unwrap();

        assert!(matches!(
            repo.read_compact_manifest_inventory(orphan.commit()),
            Err(RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_manifest_inventory_rejects_symlinked_manifest_shard_and_segment_entries() {
    for entry in ["manifest", "shard", "segment"] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0607);
        let plan = compact_plan(
            &repo,
            table,
            [63; 16],
            &[compact_segment(table, 63, b"symlink witness".to_vec())],
        );
        adopt_compact_fixture(&repo, &plan);
        let head = repo.head().unwrap().unwrap();
        let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
        let path = match entry {
            "manifest" => format!(".orna/storage/{table}/manifest.orna"),
            "shard" => format!(".orna/storage/{table}/shards/00000000.orna"),
            "segment" => manifest
                .entries()
                .first()
                .unwrap()
                .relative_path()
                .as_path()
                .to_str()
                .unwrap()
                .to_owned(),
            _ => unreachable!(),
        };
        let symlink_source = root.path().join("compact-inventory-symlink-source");
        fs::write(&symlink_source, "not-a-compact-file").unwrap();
        let object = git(
            root.path(),
            &[
                "hash-object",
                "-w",
                "--",
                symlink_source.file_name().unwrap().to_str().unwrap(),
            ],
        );
        git(root.path(), &["read-tree", head.as_str()]);
        let cacheinfo = format!("120000,{object},{path}");
        git(
            root.path(),
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
        );
        let tree = git(root.path(), &["write-tree"]);
        let commit = git(
            root.path(),
            &[
                "commit-tree",
                &tree,
                "-p",
                head.as_str(),
                "-m",
                "test: symlink compact inventory entry",
            ],
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit, head.as_str()],
        );
        let corrupted = repo.head().unwrap().unwrap();

        assert!(matches!(
            repo.read_compact_manifest_inventory(&corrupted),
            Err(RepositoryError::InvalidCompactManifest)
        ));
        assert!(matches!(
            repo.plan_compact_manifest_inventory_hydration(&corrupted),
            Err(RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_manifest_inventory_rejects_missing_or_malformed_manifest_closure_without_mutation() {
    for missing_manifest in [true, false] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::new_v4();
        let plan = compact_plan(
            &repo,
            table,
            [62; 16],
            &[compact_segment(table, 62, b"closure witness".to_vec())],
        );
        let manifest = repo
            .read_compact_manifest(plan.candidate_commit(), table)
            .unwrap()
            .unwrap();
        let segment = manifest.entries().first().unwrap();
        let segment_bytes = git_bytes(
            root.path(),
            &[
                "show",
                &format!(
                    "{}:{}",
                    plan.candidate_commit(),
                    segment.relative_path().as_path().display()
                ),
            ],
        );
        let head = repo.head().unwrap().unwrap();
        let mut changes = vec![ManagedFileChange::new(
            segment.relative_path().clone(),
            Some(segment_bytes),
        )];
        if !missing_manifest {
            let manifest_path =
                ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
            let manifest_bytes = git_bytes(
                root.path(),
                &[
                    "show",
                    &format!(
                        "{}:{}",
                        plan.candidate_commit(),
                        manifest_path.as_path().display()
                    ),
                ],
            );
            changes.push(ManagedFileChange::new(manifest_path, Some(manifest_bytes)));
        }
        let malformed = repo
            .build_private_commit(&head, &changes, "test: commit incomplete compact closure")
            .unwrap();
        repo.advance_current_ref(&head, &malformed).unwrap();

        fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
        let before = git_state(&repo, root.path());
        let index_before = fs::read(root.path().join(".git/index")).unwrap();
        let worktree_before = fs::read(root.path().join("main.orna")).unwrap();
        let runtime = RuntimeGeneration::new(605);
        let runtime_before = repo.cwd_generation(runtime).unwrap();
        let journal_before = repo.read_publication_journal().unwrap();

        assert!(matches!(
            repo.read_compact_manifest_inventory(malformed.commit()),
            Err(RepositoryError::InvalidCompactManifest)
        ));
        assert_eq!(git_state(&repo, root.path()), before);
        assert_eq!(
            fs::read(root.path().join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            fs::read(root.path().join("main.orna")).unwrap(),
            worktree_before
        );
        assert_eq!(repo.cwd_generation(runtime).unwrap(), runtime_before);
        assert_eq!(repo.read_publication_journal().unwrap(), journal_before);
    }
}

#[test]
fn compact_publication_accepts_a_valid_date_parquet_fixture() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u128(4);
    let days = [-1_i32, 0, 1];
    let schema = date_schema(table);
    let schema_digest = date_schema_fingerprint(table);
    let segment = date_segment(table, 1, &days);
    let head = repo.head().unwrap().unwrap();
    let plan = repo
        .prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, schema_digest),
            [68; 16],
            [69; 32],
            &[segment],
            "test: publish date compact segment",
        )
        .unwrap();
    let manifest = repo
        .read_compact_manifest(plan.candidate_commit(), table)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.schema(), schema_digest);
    assert_eq!(manifest.entries().len(), 1);

    let verified = repo
        .read_verified_compact_segment(plan.candidate_commit(), table, &manifest.entries()[0])
        .unwrap();
    let reader = SerializedFileReader::new(bytes::Bytes::from(verified)).unwrap();
    assert_eq!(reader.metadata().file_metadata().version(), 1);
    let column = reader.metadata().row_group(0).column(0);
    let schema_column = reader.metadata().file_metadata().schema_descr().column(0);
    assert_eq!(schema_column.physical_type(), parquet::basic::Type::INT32);
    assert_eq!(
        schema_column.logical_type_ref(),
        Some(&parquet::basic::LogicalType::Date)
    );
    assert!(
        column
            .encodings()
            .any(|encoding| encoding == Encoding::PLAIN)
    );
    assert!(matches!(column.compression(), Compression::ZSTD(_)));

    let group = reader.get_row_group(0).unwrap();
    let ColumnReader::Int32ColumnReader(mut date_reader) = group.get_column_reader(0).unwrap()
    else {
        panic!("verified Date segment did not reach an Int32 decoder");
    };
    let mut values = Vec::new();
    let (records, values_read, levels_read) = date_reader
        .read_records(days.len(), None, None, &mut values)
        .unwrap();
    assert_eq!(
        (records, values_read, levels_read),
        (days.len(), days.len(), days.len())
    );
    assert_eq!(values, days);
    assert_eq!(schema_digest, schema_fingerprint(&schema));
}

#[test]
fn compact_manifest_observation_uses_the_committed_manifest_segment_set() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0042);
    let plan = compact_plan(
        &repo,
        table,
        [42; 16],
        &[compact_segment(table, 42, b"manifest-backed".to_vec())],
    );
    adopt_compact_fixture(&repo, &plan);
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let object = manifest.entries()[0].git_object_id().to_owned();

    assert_eq!(
        repo.observe_compact_manifest_segment_blobs(&head, table)
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    let hydration = repo.plan_compact_manifest_hydration(&head, table).unwrap();
    assert_eq!(hydration.len(), 1);
    assert!(matches!(
        hydration.get(&object),
        Some(GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        })
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn compact_manifest_observation_keeps_a_declared_promised_segment_unhydrated() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0043);
    let plan = compact_plan(
        &repo,
        table,
        [43; 16],
        &[compact_segment(
            table,
            43,
            b"promised-manifest-segment".to_vec(),
        )],
    );
    adopt_compact_fixture(&repo, &plan);
    let head = repo.head().unwrap().unwrap();
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let object = manifest.entries()[0].git_object_id().to_owned();
    let object_path = root
        .path()
        .join(".git/objects")
        .join(&object[..2])
        .join(&object[2..]);
    assert!(object_path.is_file());
    with_partial_clone(root.path());
    fs::remove_file(&object_path).unwrap();
    let head_before = repo.head().unwrap();
    let refs_before = git(
        root.path(),
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let index_before = fs::read(root.path().join(".git/index")).unwrap();
    let worktree_before = fs::read(root.path().join("main.orna")).unwrap();

    assert_eq!(
        repo.observe_compact_manifest_segment_blobs(&head, table)
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    let hydration = repo.plan_compact_manifest_hydration(&head, table).unwrap();
    assert_eq!(
        hydration,
        BTreeMap::from([(object.clone(), GitObjectState::Promised)])
    );
    assert!(!object_path.exists(), "planner hydrated a promised segment");
    assert_eq!(repo.head().unwrap(), head_before);
    assert_eq!(
        git(
            root.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        refs_before
    );
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(root.path().join("main.orna")).unwrap(),
        worktree_before
    );
}

#[test]
fn compact_manifest_hydration_range_selects_only_overlapping_segments() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0045);
    let plan = compact_plan(
        &repo,
        table,
        [45; 16],
        &[
            compact_segment(table, 45, b"range-before".to_vec()),
            compact_segment(table, 46, b"range-selected".to_vec()),
        ],
    );
    adopt_compact_fixture(&repo, &plan);
    let head = repo.head().unwrap().unwrap();
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let lower = CanonicalValue::new(OvbRaw::Int(46.into()))
        .unwrap()
        .encode()
        .unwrap();
    let upper = lower.clone();
    let invalid_upper = CanonicalValue::new(OvbRaw::Int(45.into()))
        .unwrap()
        .encode()
        .unwrap();
    let selected = manifest
        .entries()
        .iter()
        .find(|entry| entry.min_key() == lower.as_slice())
        .unwrap();
    let unselected = manifest
        .entries()
        .iter()
        .find(|entry| entry.min_key() != lower.as_slice())
        .unwrap();
    let unselected_object = unselected.git_object_id().to_owned();
    let unselected_path = root
        .path()
        .join(".git/objects")
        .join(&unselected_object[..2])
        .join(&unselected_object[2..]);
    let selected_object = selected.git_object_id().to_owned();

    let fetch_head = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "FETCH_HEAD"]));
    with_partial_clone(root.path());
    let before = git_state(&repo, root.path());
    let index_before = fs::read(root.path().join(".git/index")).unwrap();
    let worktree_before = fs::read(root.path().join("main.orna")).unwrap();
    fs::remove_file(&unselected_path).unwrap();
    fs::write(&fetch_head, b"range-sentinel\n").unwrap();
    let fetch_head_before = fs::read(&fetch_head).unwrap();

    let hydration = repo
        .plan_compact_manifest_hydration_for_key_range(&head, table, &lower, &upper)
        .unwrap();
    assert_eq!(hydration.len(), 1);
    assert!(matches!(
        hydration.get(&selected_object),
        Some(GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        })
    ));
    assert!(!hydration.contains_key(&unselected_object));
    assert_eq!(
        repo.observe_git_object(&unselected_object).unwrap(),
        GitObjectState::Promised
    );
    assert!(!unselected_path.exists());
    assert!(matches!(
        repo.plan_compact_manifest_hydration_for_key_range(&head, table, &lower, &invalid_upper),
        Err(RepositoryError::InvalidCompactManifest)
    ));
    assert!(matches!(
        repo.plan_compact_manifest_hydration_for_key_range(&head, table, &[], &upper),
        Err(RepositoryError::InvalidCompactManifest)
    ));
    assert_eq!(repo.head().unwrap(), before.0);
    assert_eq!(
        git(
            root.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        before.3
    );
    assert_eq!(
        git(root.path(), &["config", "--local", "--null", "--list"]),
        before.4
    );
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(root.path().join("main.orna")).unwrap(),
        worktree_before
    );
    assert_eq!(fs::read(&fetch_head).unwrap(), fetch_head_before);
}

#[test]
fn compact_manifest_range_hydration_materializes_selected_promises_only() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0046);
    let plan = compact_plan(
        &repo,
        table,
        [46; 16],
        &[
            compact_segment(table, 45, b"range-before-promised".to_vec()),
            compact_segment(table, 46, b"range-selected-promised".to_vec()),
        ],
    );
    adopt_compact_fixture(&repo, &plan);
    let head = repo.head().unwrap().unwrap();
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let selected_key = CanonicalValue::new(OvbRaw::Int(46.into()))
        .unwrap()
        .encode()
        .unwrap();
    let selected = manifest
        .entries()
        .iter()
        .find(|entry| entry.min_key() == selected_key.as_slice())
        .unwrap();
    let unselected = manifest
        .entries()
        .iter()
        .find(|entry| entry.min_key() != selected_key.as_slice())
        .unwrap();
    let selected_object = selected.git_object_id().to_owned();
    let unselected_object = unselected.git_object_id().to_owned();
    let selected_path = root
        .path()
        .join(".git/objects")
        .join(&selected_object[..2])
        .join(&selected_object[2..]);
    let unselected_path = root
        .path()
        .join(".git/objects")
        .join(&unselected_object[..2])
        .join(&unselected_object[2..]);

    let remote = continuity_remote(root.path());
    let remote_unselected_path = remote
        .path()
        .join("objects")
        .join(&unselected_object[..2])
        .join(&unselected_object[2..]);
    assert!(remote_unselected_path.is_file());
    fs::remove_file(remote_unselected_path).unwrap();
    git(
        root.path(),
        &["config", "extensions.partialClone", "origin"],
    );
    git(root.path(), &["config", "remote.origin.promisor", "true"]);
    git(
        root.path(),
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
    assert!(selected_path.is_file());
    assert!(unselected_path.is_file());
    let before = git_state(&repo, root.path());
    let index_before = fs::read(root.path().join(".git/index")).unwrap();
    let worktree_before = fs::read(root.path().join("main.orna")).unwrap();
    let fetch_head = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "FETCH_HEAD"]));
    fs::write(&fetch_head, b"range-hydration-sentinel\n").unwrap();
    let fetch_head_before = fs::read(&fetch_head).unwrap();
    fs::remove_file(&selected_path).unwrap();
    fs::remove_file(&unselected_path).unwrap();
    assert_eq!(
        repo.observe_git_object(&selected_object).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repo.observe_git_object(&unselected_object).unwrap(),
        GitObjectState::Promised
    );

    let hydrated = repo
        .hydrate_compact_manifest_segments_for_key_range(&head, table, &selected_key, &selected_key)
        .unwrap();

    assert_eq!(hydrated.len(), 1);
    assert!(matches!(
        hydrated.get(&selected_object),
        Some(GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        })
    ));
    assert_eq!(
        repo.observe_git_object(&selected_object).unwrap(),
        hydrated[&selected_object]
    );
    assert_eq!(
        repo.observe_git_object(&unselected_object).unwrap(),
        GitObjectState::Promised
    );
    assert!(!unselected_path.exists());
    assert_eq!(repo.head().unwrap(), before.0);
    assert_eq!(
        git(
            root.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        before.3
    );
    assert_eq!(
        git(root.path(), &["config", "--local", "--null", "--list"]),
        before.4
    );
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(root.path().join("main.orna")).unwrap(),
        worktree_before
    );
    assert_eq!(fs::read(&fetch_head).unwrap(), fetch_head_before);
}

#[test]
fn compact_manifest_hydration_is_manifest_bound_and_preserves_git_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0044);
    let plan = compact_plan(
        &repo,
        table,
        [44; 16],
        &[compact_segment(
            table,
            44,
            b"manifest-bound-hydration".to_vec(),
        )],
    );
    adopt_compact_fixture(&repo, &plan);
    let head = repo.head().unwrap().unwrap();
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let object = manifest.entries()[0].git_object_id().to_owned();
    let object_path = root
        .path()
        .join(".git/objects")
        .join(&object[..2])
        .join(&object[2..]);

    let _remote = continuity_remote(root.path());
    git(
        root.path(),
        &["config", "extensions.partialClone", "origin"],
    );
    git(root.path(), &["config", "remote.origin.promisor", "true"]);
    git(
        root.path(),
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
    assert!(object_path.is_file());
    fs::remove_file(&object_path).unwrap();
    assert_eq!(
        repo.observe_git_object(&object).unwrap(),
        GitObjectState::Promised
    );

    let fetch_head = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "FETCH_HEAD"]));
    fs::write(&fetch_head, b"sentinel\n").unwrap();
    let before = git_state(&repo, root.path());
    let fetch_head_before = fs::read(&fetch_head).unwrap();
    let hydrated = repo
        .hydrate_compact_manifest_segments(&head, table)
        .unwrap();

    assert!(matches!(
        hydrated.get(&object),
        Some(GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        })
    ));
    assert_eq!(repo.observe_git_object(&object).unwrap(), hydrated[&object]);
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(fs::read(&fetch_head).unwrap(), fetch_head_before);
}

#[test]
fn compact_publication_rejects_schema_identity_and_type_substitution() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let key = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0701);
    let substituted = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0702);
    let schema = compact_schema_with_roles(table, &[(key, "key", 0)]);
    let head = repo.head().unwrap().unwrap();

    for (ordinal, physical_field, type_name, columns) in [
        (
            701,
            substituted,
            "Int",
            compact_columns_for_mapping(substituted, "Int", "int64"),
        ),
        (
            702,
            key,
            "Bool",
            compact_columns_for_mapping(key, "Bool", "bool"),
        ),
    ] {
        let segment = compact_segment_for_schema(
            table,
            ordinal,
            CompactSegmentRole::Data,
            &schema,
            physical_field,
            type_name,
            columns,
        );
        assert!(matches!(
            repo.prepare_compact_publication(
                &head,
                repo.index_generation().unwrap(),
                CompactManifest::empty(table, schema_fingerprint(&schema)),
                [ordinal as u8; 16],
                [ordinal as u8; 32],
                &[segment],
                "reject compact schema substitution",
            ),
            Err(RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_publication_accepts_immutable_ovb_int_and_bool_key_fallbacks() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();

    let int_table = Uuid::new_v4();
    let int_field = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0741);
    let int_schema = compact_schema_with_roles_type(int_table, &[(int_field, "key", 0)], "Int");
    let int_value = CanonicalValue::new(OvbRaw::Int(
        "123456789012345678901234567890".parse().unwrap(),
    ))
    .unwrap()
    .encode()
    .unwrap();
    let int_segment = compact_ovb_segment_for_schema(
        int_table,
        741,
        CompactSegmentRole::Data,
        &int_schema,
        int_field,
        compact_columns_for_mapping(int_field, "Int", "ovb"),
        std::slice::from_ref(&int_value),
        int_value.clone(),
        int_value.clone(),
        None,
    );
    assert!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(int_table, schema_fingerprint(&int_schema)),
            [74; 16],
            [74; 32],
            &[int_segment],
            "accept immutable ovb int fallback",
        )
        .is_ok()
    );

    let bool_table = Uuid::new_v4();
    let bool_field = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0742);
    let bool_schema = compact_schema_with_roles_type(bool_table, &[(bool_field, "key", 0)], "Bool");
    let bool_values = [
        CanonicalValue::new(OvbRaw::Bool(false))
            .unwrap()
            .encode()
            .unwrap(),
        CanonicalValue::new(OvbRaw::Bool(true))
            .unwrap()
            .encode()
            .unwrap(),
    ];
    let bool_segment = compact_ovb_segment_for_schema(
        bool_table,
        742,
        CompactSegmentRole::Data,
        &bool_schema,
        bool_field,
        compact_columns_for_mapping(bool_field, "Bool", "ovb"),
        &bool_values,
        bool_values[0].clone(),
        bool_values[1].clone(),
        None,
    );
    assert!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(bool_table, schema_fingerprint(&bool_schema)),
            [75; 16],
            [75; 32],
            &[bool_segment],
            "accept immutable ovb bool fallback",
        )
        .is_ok()
    );
}

#[test]
fn compact_publication_rejects_immutable_ovb_fallback_mismatches() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let field = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0743);
    let schema = compact_schema_with_roles_type(table, &[(field, "key", 0)], "Int");
    let head = repo.head().unwrap().unwrap();
    let canonical_int = CanonicalValue::new(OvbRaw::Int(7.into()))
        .unwrap()
        .encode()
        .unwrap();
    let canonical_bool = CanonicalValue::new(OvbRaw::Bool(true))
        .unwrap()
        .encode()
        .unwrap();
    let cases = [
        (
            743,
            vec![0x18, 0x07],
            compact_columns_for_mapping(field, "Int", "ovb"),
            None,
        ),
        (
            744,
            canonical_bool,
            compact_columns_for_mapping(field, "Int", "ovb"),
            None,
        ),
        (
            745,
            canonical_int.clone(),
            compact_columns_for_mapping_with_parameters(field, "Int", "ovb", Vec::new()),
            None,
        ),
        (
            746,
            canonical_int.clone(),
            compact_columns_for_mapping(field, "Int", "ovb"),
            Some(" (UTF8)"),
        ),
    ];
    for (ordinal, value, columns, annotation) in cases {
        let segment = compact_ovb_segment_for_schema(
            table,
            ordinal,
            CompactSegmentRole::Data,
            &schema,
            field,
            columns,
            std::slice::from_ref(&value),
            canonical_int.clone(),
            canonical_int.clone(),
            annotation,
        );
        assert!(matches!(
            repo.prepare_compact_publication(
                &head,
                repo.index_generation().unwrap(),
                CompactManifest::empty(table, schema_fingerprint(&schema)),
                [ordinal as u8; 16],
                [ordinal as u8; 32],
                &[segment],
                "reject immutable ovb fallback mismatch",
            ),
            Err(RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_publication_rejects_a_schema_owned_by_another_table() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let schema_table = Uuid::new_v4();
    let key = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0703);
    let schema = compact_schema_with_roles(schema_table, &[(key, "key", 0)]);
    let segment = compact_segment_for_schema(
        table,
        703,
        CompactSegmentRole::Data,
        &schema,
        key,
        "Int",
        compact_columns_for_mapping(key, "Int", "int64"),
    );
    let head = repo.head().unwrap().unwrap();

    assert!(matches!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, schema_fingerprint(&schema)),
            [70; 16],
            [70; 32],
            &[segment],
            "reject compact foreign-table schema",
        ),
        Err(RepositoryError::InvalidCompactManifest)
    ));
}

#[test]
fn compact_publication_requires_complete_unique_known_stored_mappings() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let key = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0711);
    let stored = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0712);
    let unknown = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0713);
    let schema = compact_schema_with_roles(table, &[(key, "key", 0), (stored, "stored", 1)]);
    let head = repo.head().unwrap().unwrap();

    for (ordinal, physical_field, columns) in [
        (711, key, compact_columns_for_mapping(key, "Int", "int64")),
        (
            712,
            unknown,
            compact_columns_for_mapping(unknown, "Int", "int64"),
        ),
        (
            713,
            key,
            CanonicalValue::new(OvbRaw::Array(vec![
                OvbRaw::Array(vec![
                    OvbRaw::Array(vec![OvbRaw::Tag(
                        37,
                        Box::new(OvbRaw::Bytes(key.as_bytes().to_vec())),
                    )]),
                    OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", key.simple()))]),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".to_owned())]),
                    OvbRaw::Text("int64".to_owned()),
                    OvbRaw::Array(Vec::new()),
                ]),
                OvbRaw::Array(vec![
                    OvbRaw::Array(vec![OvbRaw::Tag(
                        37,
                        Box::new(OvbRaw::Bytes(key.as_bytes().to_vec())),
                    )]),
                    OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", key.simple()))]),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".to_owned())]),
                    OvbRaw::Text("int64".to_owned()),
                    OvbRaw::Array(Vec::new()),
                ]),
            ]))
            .unwrap()
            .encode()
            .unwrap(),
        ),
    ] {
        let segment = compact_segment_for_schema(
            table,
            ordinal,
            CompactSegmentRole::Data,
            &schema,
            physical_field,
            "Int",
            columns,
        );
        assert!(matches!(
            repo.prepare_compact_publication(
                &head,
                repo.index_generation().unwrap(),
                CompactManifest::empty(table, schema_fingerprint(&schema)),
                [ordinal as u8; 16],
                [ordinal as u8; 32],
                &[segment],
                "reject incomplete compact descriptor",
            ),
            Err(RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_publication_excludes_computed_fields_and_deletion_non_keys() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let key = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0721);
    let computed = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0722);
    let computed_schema =
        compact_schema_with_roles(table, &[(key, "key", 0), (computed, "computed", 2)]);
    let head = repo.head().unwrap().unwrap();
    let accepted = compact_segment_for_schema(
        table,
        721,
        CompactSegmentRole::Data,
        &computed_schema,
        key,
        "Int",
        compact_columns_for_mapping(key, "Int", "int64"),
    );
    assert!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, schema_fingerprint(&computed_schema)),
            [72; 16],
            [72; 32],
            &[accepted],
            "accept compact computed omission",
        )
        .is_ok()
    );

    let stored = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0723);
    let deletion_schema =
        compact_schema_with_roles(table, &[(key, "key", 0), (stored, "stored", 1)]);
    let deletion = compact_segment_for_schema(
        table,
        722,
        CompactSegmentRole::Deletion,
        &deletion_schema,
        stored,
        "Int",
        compact_columns_for_mapping(stored, "Int", "int64"),
    );
    assert!(matches!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, schema_fingerprint(&deletion_schema)),
            [73; 16],
            [73; 32],
            &[deletion],
            "reject compact deletion stored field",
        ),
        Err(RepositoryError::InvalidCompactManifest)
    ));
}

#[test]
fn compact_publication_rejects_a_descriptor_that_does_not_match_parquet_leaves() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let head = repo.head().unwrap().unwrap();
    let segment = compact_segment_with_manifest_columns(
        table,
        1,
        b"compact object\n".to_vec(),
        compact_columns_for_field(Uuid::from_u64_pair(7, 39)),
    );

    assert!(matches!(
        repo.prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, compact_schema_fingerprint(table)),
            [39; 16],
            [39; 32],
            &[segment],
            "orna: publish compact runtime data",
        ),
        Err(RepositoryError::InvalidCompactManifest)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
}

#[test]
fn compact_manifest_recovery_rejects_a_same_shape_descriptor_with_the_wrong_identity() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let plan = compact_plan(
        &repo,
        table,
        [40; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let head = repo.head().unwrap().unwrap();
    let manifest_path = ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
    let shard_path =
        ManagedPath::new(format!(".orna/storage/{table}/shards/00000000.orna")).unwrap();
    let manifest = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                manifest_path.as_path().display()
            ),
        ],
    );
    let shard = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                shard_path.as_path().display()
            ),
        ],
    );
    let expected_columns = orna_value_v1::base64_encode(&compact_columns());
    let malformed_columns =
        orna_value_v1::base64_encode(&compact_columns_for_field(Uuid::from_u64_pair(7, 40)));
    let malformed_shard = String::from_utf8(shard)
        .unwrap()
        .replacen(&expected_columns, &malformed_columns, 1)
        .into_bytes();
    let expected_hash = manifest_hash_for(&manifest, "shards/00000000.orna");
    let malformed_manifest = String::from_utf8(manifest)
        .unwrap()
        .replacen(
            &expected_hash,
            &hex_digest(&Sha256::digest(&malformed_shard)),
            1,
        )
        .into_bytes();
    let malformed = repo
        .build_private_commit(
            &head,
            &[
                ManagedFileChange::new(manifest_path, Some(malformed_manifest)),
                ManagedFileChange::new(shard_path, Some(malformed_shard)),
            ],
            "test: commit malformed compact descriptor",
        )
        .unwrap();
    repo.advance_current_ref(&head, &malformed).unwrap();

    assert!(matches!(
        repo.read_compact_manifest(malformed.commit(), table),
        Err(RepositoryError::InvalidCompactManifest)
    ));
}

#[test]
fn compact_publication_canonically_shards_at_256_and_preserves_ordinary_git_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();

    let segments = (0..257)
        .map(|ordinal| compact_segment(table, ordinal, format!("segment-{ordinal}\n").into_bytes()))
        .collect::<Vec<_>>();
    let plan = compact_plan(&repo, table, [41; 16], &segments);
    let head = plan.candidate_commit().clone();
    let manifest = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    assert_eq!(manifest.entries().len(), 257);
    assert_eq!(manifest.next_generation(), 2);
    assert!(
        manifest
            .entries()
            .iter()
            .all(|entry| entry.generation() == 1)
    );
    let manifest_text = git(
        root.path(),
        &[
            "show",
            &format!("{}:.orna/storage/{table}/manifest.orna", head),
        ],
    );
    assert_eq!(manifest_text.matches("entries: 256").count(), 1);
    assert_eq!(manifest_text.matches("entries: 1").count(), 1);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged ordinary\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
}

#[test]
fn compact_publication_rejects_a_noncanonical_reordered_candidate_manifest() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let segments = (1..=2)
        .map(|ordinal| compact_segment(table, ordinal, format!("segment-{ordinal}\n").into_bytes()))
        .collect::<Vec<_>>();
    let plan = compact_plan(&repo, table, [44; 16], &segments);

    let head = repo.head().unwrap().unwrap();
    let manifest_path = ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
    let shard_path =
        ManagedPath::new(format!(".orna/storage/{table}/shards/00000000.orna")).unwrap();
    let manifest = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                manifest_path.as_path().display()
            ),
        ],
    );
    let shard = git_bytes(
        root.path(),
        &[
            "show",
            &format!(
                "{}:{}",
                plan.candidate_commit(),
                shard_path.as_path().display()
            ),
        ],
    );
    let reordered_shard = reorder_two_canonical_shard_entries(&shard);
    let reordered_hash = hex_digest(&Sha256::digest(&reordered_shard));
    let original_manifest_hash: [u8; 32] = Sha256::digest(&manifest).into();
    let original_hash = manifest_hash_for(&manifest, "shards/00000000.orna");
    let reordered_manifest = String::from_utf8(manifest)
        .unwrap()
        .replacen(&original_hash, &reordered_hash, 1)
        .into_bytes();
    let reordered_manifest_hash: [u8; 32] = Sha256::digest(&reordered_manifest).into();
    let candidate = repo
        .build_private_commit(
            &head,
            &[
                ManagedFileChange::new(manifest_path, Some(reordered_manifest)),
                ManagedFileChange::new(shard_path, Some(reordered_shard)),
            ],
            "test: commit reordered compact records",
        )
        .unwrap();
    assert_ne!(original_manifest_hash, reordered_manifest_hash);
    repo.advance_current_ref(&head, &candidate).unwrap();
    let state_before_read = git_state(&repo, root.path());
    let error = repo
        .read_compact_manifest(candidate.commit(), table)
        .unwrap_err();
    assert!(
        matches!(error, RepositoryError::InvalidCompactManifest),
        "unexpected reordered-manifest rejection: {error:?}"
    );
    assert_eq!(git_state(&repo, root.path()), state_before_read);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged ordinary\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
}

fn git_bytes(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn reorder_two_canonical_shard_entries(bytes: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).unwrap();
    let entries = text
        .strip_prefix("{entries: [")
        .unwrap()
        .strip_suffix("]}\n")
        .unwrap();
    let (first, second) = entries.split_once("}, {").unwrap();
    format!("{{entries: [{{{second}}}, {{{first}}}]}}\n").into_bytes()
}

fn manifest_hash_for(bytes: &[u8], shard: &str) -> String {
    let text = std::str::from_utf8(bytes).unwrap();
    let marker = format!("file: \"{shard}\", hash: \"");
    let hash = text.split_once(&marker).unwrap().1;
    hash[..64].to_owned()
}

fn hex_digest(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn ordinary_commit_preserves_verified_compact_manifest_and_segment_identities() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let plan = compact_plan(
        &repo,
        table,
        [67; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    adopt_compact_fixture(&repo, &plan);

    let published = repo.head().unwrap().unwrap();
    let manifest_path = format!(".orna/storage/{table}/manifest.orna");
    let manifest_object = git(
        root.path(),
        &["rev-parse", &format!("{published}:{manifest_path}")],
    );
    let manifest = repo
        .read_compact_manifest(&published, table)
        .unwrap()
        .unwrap();
    let entry = manifest.entries().first().unwrap();
    let segment_path = entry.relative_path().as_path().display().to_string();
    let segment_object = entry.git_object_id().to_owned();
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{published}:{segment_path}")],
        ),
        segment_object
    );

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let staged_before = git(root.path(), &["diff", "--cached", "--binary"]);
    let unstaged_before = git(root.path(), &["diff", "--binary"]);

    fs::write(root.path().join("ordinary-commit.txt"), "ordinary commit\n").unwrap();
    git(root.path(), &["add", "ordinary-commit.txt"]);
    git(
        root.path(),
        &[
            "commit",
            "-m",
            "ordinary commit after compact publication",
            "ordinary-commit.txt",
        ],
    );

    let current = repo.head().unwrap().unwrap();
    assert_ne!(current, published);
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{current}:{manifest_path}")],
        ),
        manifest_object
    );
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{current}:{segment_path}")],
        ),
        segment_object
    );
    assert_eq!(
        repo.read_compact_manifest(&current, table)
            .unwrap()
            .unwrap(),
        manifest
    );
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--binary"]),
        staged_before
    );
    assert_eq!(git(root.path(), &["diff", "--binary"]), unstaged_before);
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
}

fn canonical_int(value: u64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Int(value.into())).unwrap()
}

fn encoded_int(value: u64) -> Vec<u8> {
    canonical_int(value).encode().unwrap()
}

fn legacy_row(key: u64, value: Option<u64>) -> CompactCommittedRow {
    CompactCommittedRow {
        key: canonical_int(key),
        value: value.map(canonical_int),
    }
}

#[test]
fn legacy_resolution_keeps_the_greatest_generation_and_drops_deleted_keys() {
    let rows = resolve_legacy_authoritative_rows(vec![
        (1, vec![legacy_row(1, Some(10)), legacy_row(2, Some(20))]),
        (3, vec![legacy_row(1, Some(30))]),
        (2, vec![legacy_row(1, Some(40)), legacy_row(3, Some(50))]),
        (4, vec![legacy_row(2, None)]),
    ])
    .unwrap();
    let resolved: Vec<(CanonicalValue, CanonicalValue)> = rows
        .iter()
        .map(|row| (row.key().clone(), row.value().clone()))
        .collect();
    assert_eq!(
        resolved,
        vec![
            (canonical_int(1), canonical_int(30)),
            (canonical_int(3), canonical_int(50)),
        ]
    );
}

#[test]
fn legacy_resolution_rejects_two_mutations_of_one_key_at_one_generation() {
    let duplicate = resolve_legacy_authoritative_rows(vec![
        (7, vec![legacy_row(1, Some(1))]),
        (7, vec![legacy_row(1, Some(2))]),
    ]);
    assert!(matches!(
        duplicate,
        Err(RepositoryError::InvalidCompactManifest)
    ));
}

#[test]
fn legacy_authoritative_read_resolves_key_only_generations_and_deletions_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0801);
    let key = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0802);
    let schema = compact_schema_with_roles(table, &[(key, "key", 0)]);
    let head = repo.head().unwrap().unwrap();
    let data = compact_ovb_segment_for_schema(
        table,
        801,
        CompactSegmentRole::Data,
        &schema,
        key,
        compact_columns_for_mapping(key, "Int", "ovb"),
        &[encoded_int(1), encoded_int(2), encoded_int(3)],
        encoded_int(1),
        encoded_int(3),
        None,
    );
    let first = repo
        .prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            CompactManifest::empty(table, schema_fingerprint(&schema)),
            [80; 16],
            [80; 32],
            &[data],
            "publish legacy key-only generation one",
        )
        .unwrap();
    adopt_compact_fixture(&repo, &first);

    let head = repo.head().unwrap().unwrap();
    let base = repo.read_compact_manifest(&head, table).unwrap().unwrap();
    let deletion = compact_ovb_segment_for_schema(
        table,
        802,
        CompactSegmentRole::Deletion,
        &schema,
        key,
        compact_columns_for_mapping(key, "Int", "ovb"),
        &[encoded_int(2)],
        encoded_int(2),
        encoded_int(2),
        None,
    );
    let second = repo
        .prepare_compact_publication(
            &head,
            repo.index_generation().unwrap(),
            base,
            [81; 16],
            [81; 32],
            &[deletion],
            "publish legacy key-only deletion",
        )
        .unwrap();
    adopt_compact_fixture(&repo, &second);

    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());
    let tables = repo.read_legacy_authoritative_tables(&head).unwrap();
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].table(), table);
    assert_eq!(tables[0].schema(), schema_fingerprint(&schema));
    let keys: Vec<CanonicalValue> = tables[0]
        .rows()
        .iter()
        .map(|row| row.key().clone())
        .collect();
    assert_eq!(keys, vec![canonical_int(1), canonical_int(3)]);
}

#[test]
fn legacy_authoritative_read_refuses_format_two_commits_without_mutation() {
    let root = repository();
    fs::write(
        root.path().join(".orna/format.orna"),
        "{repository_format: 2, row_profile: \"ORR-2\", blob_profile: \"OGB-1\", semantic_profile: \"SOV-2\", compact_profile: \"compact-storage-v2\"}\n",
    )
    .unwrap();
    git(root.path(), &["add", ".orna/format.orna"]);
    git(root.path(), &["commit", "-m", "record format two"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());
    assert!(matches!(
        repo.read_legacy_authoritative_tables(&head),
        Err(RepositoryError::InvalidFormatMigration)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn legacy_authoritative_read_keeps_the_snapshot_cause_for_an_unknown_commit() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let unknown = GitCommitRef::from_verified_commit("1".repeat(40), 40).unwrap();
    // The pin's own refusal is reported, not collapsed into a migration error.
    assert!(matches!(
        repo.read_legacy_authoritative_tables(&unknown),
        Err(RepositoryError::SnapshotNotFound | RepositoryError::InvalidSelector)
    ));
}
