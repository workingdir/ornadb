use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ed25519_dalek::{Signer, SigningKey};
use fs2::FileExt;
use orna_foundation_v1::{CanonicalValue, GitHash, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{
    CheckoutExecutionError, CheckoutTarget, CommittedTreeEntryKind, CompactManifest,
    CompactRuntimeReceipt, CompactSegment, CompactSegmentRole, GitDeclaredObjectSetState,
    GitObjectKind, GitObjectState, GitRepositoryMode, IndexGeneration, ManagedPath, NativeObjectId,
    OrnaInternalRef, RemoteContinuity, Repository, RequiredInternalRef, RuntimeGeneration,
    WorktreeState,
};
use parquet::{
    basic::{Compression, Encoding, PageType},
    column::reader::ColumnReader,
    data_type::{BoolType, Int32Type, Int64Type},
    file::{
        metadata::{KeyValue, ParquetMetaDataWriter},
        properties::{WriterProperties, WriterVersion},
        reader::{FileReader, SerializedFileReader},
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

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

fn decode_oid(object_id: &str) -> Vec<u8> {
    object_id
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

fn repository_with_object_format(format: &str) -> Option<TempDir> {
    let temp = TempDir::new().unwrap();
    let output = Command::new("git")
        .current_dir(temp.path())
        .args(["init", "-b", "main"])
        .arg(format!("--object-format={format}"))
        .output()
        .unwrap();
    if !output.status.success() {
        return None;
    }
    git(
        temp.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(temp.path(), &["config", "user.name", "Repository test"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "-m", "initial"]);
    Some(temp)
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    git(
        temp.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(temp.path(), &["config", "user.name", "Repository test"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "format 1\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

fn configure_hostile_git_environment(command: &mut Command, hostile: &Path) {
    command
        .env("GIT_DIR", hostile.join(".git"))
        .env("GIT_WORK_TREE", hostile)
        .env("GIT_INDEX_FILE", hostile.join(".git/index"))
        .env("GIT_COMMON_DIR", hostile.join(".git"))
        .env("GIT_OBJECT_DIRECTORY", hostile.join(".git/objects"))
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            hostile.join(".git/objects"),
        )
        .env("GIT_CEILING_DIRECTORIES", hostile)
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0")
        .env("GIT_CONFIG_SYSTEM", hostile.join("system.gitconfig"))
        .env("GIT_CONFIG_GLOBAL", hostile.join("global.gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "0")
        .env("GIT_CONFIG", hostile.join("hostile.gitconfig"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", hostile)
        .env("GIT_IMPLICIT_WORK_TREE", "0")
        .env("GIT_PREFIX", "hostile")
        .env("GIT_NAMESPACE", "hostile")
        .env("GIT_REPLACE_REF_BASE", "refs/replace-hostile")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", hostile.join("grafts"))
        .env("GIT_SHALLOW_FILE", hostile.join("shallow"));
}

fn git_state(
    repository: &Repository,
    root: &Path,
) -> (
    Option<orna_repository_v1::GitCommitRef>,
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

fn required_internal_ref(object_id: &str) -> RequiredInternalRef {
    RequiredInternalRef::new(
        OrnaInternalRef::new("refs/orna/ids/0123456789abcdef").unwrap(),
        NativeObjectId::new(object_id).unwrap(),
    )
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
            Some(base64(&compact_schema(table).encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(base64(columns))),
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
            Some(base64(&schema.encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(base64(columns))),
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
        OvbRaw::Array(Vec::new()),
    ])]))
    .unwrap()
    .encode()
    .unwrap()
}

fn compact_schema_with_roles(table: Uuid, fields: &[(Uuid, &str, u8)]) -> SchemaDescriptor {
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
                                OvbRaw::Text("Int".to_owned()),
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
            Some(base64(&schema.encode().unwrap())),
        ),
        KeyValue::new("orna.columns.ovb".to_owned(), Some(base64(columns))),
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

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((word >> 18) & 0x3f) as usize] as char);
        encoded.push(ALPHABET[((word >> 12) & 0x3f) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(word & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
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

fn compact_plan(
    repository: &Repository,
    table: Uuid,
    intent: [u8; 16],
    segments: &[CompactSegment],
) -> orna_repository_v1::CompactPublicationPlan {
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

fn publish_compact_repository_boundary(
    repository: &Repository,
    plan: orna_repository_v1::CompactPublicationPlan,
) -> Result<IndexGeneration, orna_repository_v1::RepositoryError> {
    repository
        .publish_compact_repository_boundary(plan)
        .map(|pending| pending.index().clone())
}

fn compact_receipt_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x5a; 32])
}

fn provision_compact_receipt_trust_root(repository: &Repository, key: &SigningKey) {
    repository.runtime_paths().ensure_exists().unwrap();
    fs::write(
        repository
            .runtime_paths()
            .compact_runtime_receipt_public_key(),
        key.verifying_key().to_bytes(),
    )
    .unwrap();
}

fn compact_runtime_receipt(
    pending: &orna_repository_v1::CompactPublicationPending,
    key: &SigningKey,
) -> CompactRuntimeReceipt {
    let signing_bytes = CompactRuntimeReceipt::signing_bytes(
        pending.runtime_intent_id(),
        pending.cleanup_watermark(),
        pending.commit(),
        pending.journal_verifier(),
    )
    .unwrap();
    CompactRuntimeReceipt::new(
        pending.runtime_intent_id(),
        pending.cleanup_watermark(),
        pending.commit().clone(),
        pending.journal_verifier(),
        key.sign(&signing_bytes).to_bytes(),
    )
    .unwrap()
}

/// Builds a real filtered clone when the installed Git supports file-protocol
/// filtering. `None` means the platform cannot provide the fixture; callers
/// must then assert the conservative unavailable boundary rather than claim a
/// promised object from configuration alone.
fn filtered_clone() -> Option<(TempDir, PathBuf, String)> {
    let fixture = TempDir::new().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", "origin.git"]);
    git(fixture.path(), &["clone", "origin.git", "seed"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    git(&seed, &["config", "user.name", "Repository test"]);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    fs::write(seed.join("visible.txt"), "visible\n").ok()?;
    fs::write(seed.join("promised.txt"), "promised\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["push", "origin", "HEAD"]);
    git(&origin, &["config", "uploadpack.allowFilter", "true"]);
    let origin_url = format!("file://{}", origin.display());
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
    let promised = git(&seed, &["rev-parse", "HEAD:promised.txt"]);
    let missing = Command::new("git")
        .current_dir(&clone)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["cat-file", "-e", &promised])
        .output()
        .ok()?;
    if missing.status.success() {
        return None;
    }
    Some((fixture, clone, promised))
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
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    git(&seed, &["config", "user.name", "Repository test"]);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    fs::write(seed.join("main.orna"), "module main;\n").ok()?;
    fs::write(seed.join("ordinary.txt"), "base\n").ok()?;
    fs::create_dir_all(seed.join(".orna")).ok()?;
    fs::write(seed.join(".orna/format.orna"), "format 1\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);

    let repository = Repository::discover(&seed).ok()?;
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repository, &signing_key);
    let materialized_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0610);
    let materialized_pending = repository
        .publish_compact_repository_boundary(compact_plan(
            &repository,
            materialized_table,
            [70; 16],
            &[compact_segment(
                materialized_table,
                70,
                b"materialized inventory segment".to_vec(),
            )],
        ))
        .ok()?;
    repository
        .finish_compact_with_receipt(&compact_runtime_receipt(
            &materialized_pending,
            &signing_key,
        ))
        .ok()?;

    let promised_table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0611);
    let promised_pending = repository
        .publish_compact_repository_boundary(compact_plan(
            &repository,
            promised_table,
            [71; 16],
            &[compact_segment(
                promised_table,
                71,
                b"promised inventory segment".to_vec(),
            )],
        ))
        .ok()?;
    repository
        .finish_compact_with_receipt(&compact_runtime_receipt(&promised_pending, &signing_key))
        .ok()?;

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
fn discovers_head_index_worktree_and_per_worktree_runtime_area() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    assert_eq!(repo.worktree(), root.path());
    assert!(repo.head().unwrap().is_some());
    assert!(repo.index_generation().unwrap().tree().is_some());
    assert!(repo.worktree_state().unwrap().is_clean());
    assert_eq!(
        repo.runtime_paths().root(),
        root.path()
            .join(git(root.path(), &["rev-parse", "--git-path", "orna"]))
    );
    assert!(
        !repo
            .runtime_paths()
            .root()
            .starts_with(root.path().join(".orna"))
    );
    repo.runtime_paths().ensure_exists().unwrap();
    assert!(
        repo.runtime_paths()
            .state_db()
            .starts_with(repo.runtime_paths().root())
    );
    let cwd = repo.cwd_generation(RuntimeGeneration::new(9)).unwrap();
    assert_eq!(cwd.runtime().get(), 9);
}

#[test]
fn hostile_git_routing_child() {
    let Some(root) = std::env::var_os("ORNA_TEST_GIT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let repo = Repository::discover(&root).unwrap();
    let expected_head = std::env::var("ORNA_TEST_EXPECTED_HEAD").unwrap();
    let expected_index_tree = std::env::var("ORNA_TEST_EXPECTED_INDEX_TREE").unwrap();
    let expected_runtime = PathBuf::from(std::env::var_os("ORNA_TEST_EXPECTED_RUNTIME").unwrap());

    assert_eq!(
        repo.head().unwrap().as_ref().map(|head| head.as_str()),
        Some(expected_head.as_str())
    );
    assert_eq!(
        repo.index_generation()
            .unwrap()
            .tree()
            .map(|tree| tree.as_str()),
        Some(expected_index_tree.as_str())
    );
    assert!(!repo.worktree_state().unwrap().is_clean());
    let cwd = repo.cwd_generation(RuntimeGeneration::new(41)).unwrap();
    assert_eq!(
        cwd.head().map(|head| head.as_str()),
        Some(expected_head.as_str())
    );
    assert_eq!(cwd.branch(), Some("main"));
    assert!(!cwd.worktree().is_clean());
    assert_eq!(repo.runtime_paths().root(), expected_runtime);
}

#[test]
fn hostile_git_routing_cannot_redirect_repository_observations() {
    let root = repository();
    let hostile = repository();
    fs::write(hostile.path().join("hostile-only.txt"), "hostile\n").unwrap();
    git(hostile.path(), &["add", "hostile-only.txt"]);
    git(hostile.path(), &["commit", "-m", "hostile"]);

    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "root staged\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "root unstaged\n").unwrap();
    let expected_head = repo.head().unwrap();
    let expected_index = repo.index_generation().unwrap();
    let expected_runtime = repo.runtime_paths().root().to_owned();

    let expected_head = expected_head.unwrap();
    let expected_index_tree = expected_index.tree().unwrap().clone();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "hostile_git_routing_child", "--nocapture"]);
    child
        .env("ORNA_TEST_GIT_ROOT", root.path())
        .env("ORNA_TEST_EXPECTED_HEAD", expected_head.as_str())
        .env(
            "ORNA_TEST_EXPECTED_INDEX_TREE",
            expected_index_tree.as_str(),
        )
        .env("ORNA_TEST_EXPECTED_RUNTIME", &expected_runtime);
    configure_hostile_git_environment(&mut child, hostile.path());
    assert!(child.status().unwrap().success());
}

#[test]
fn linked_worktree_gets_its_own_git_resolved_runtime_area() {
    let root = repository();
    let linked = TempDir::new().unwrap();
    let linked_path = linked.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked_path.to_str().unwrap(),
        ],
    );
    let main = Repository::discover(root.path()).unwrap();
    let other = Repository::discover(&linked_path).unwrap();
    assert_ne!(main.runtime_paths().root(), other.runtime_paths().root());
    assert_eq!(
        other.runtime_paths().root(),
        Path::new(&git(&linked_path, &["rev-parse", "--git-path", "orna"]))
    );
}

#[test]
fn managed_staging_preserves_unrelated_index_and_worktree_changes() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join(".orna/format.orna"), "format 2\n").unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.index_generation().unwrap();
    let after = repo
        .stage_managed(&before, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    let stale = repo
        .stage_managed(&before, &[ManagedPath::new("main.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        stale,
        orna_repository_v1::RepositoryError::StaleIndex { .. }
    ));
    assert!(ManagedPath::new("../ordinary.txt").is_err());
    assert!(ManagedPath::new(".git/config").is_err());
}

#[test]
fn managed_unstage_and_coordination_lock_preserve_unrelated_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary staged\n").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "format staged\n").unwrap();
    git(root.path(), &["add", "ordinary.txt", ".orna/format.orna"]);
    let staged = repo.index_generation().unwrap();
    let after = repo
        .unstage_managed(&staged, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(staged, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary staged"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "format 1"
    );

    repo.runtime_paths().ensure_exists().unwrap();
    fs::create_dir_all(repo.runtime_paths().locks()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(repo.runtime_paths().locks().join("coordination.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let busy = repo
        .stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        busy,
        orna_repository_v1::RepositoryError::RepositoryBusy
    ));
    lock.unlock().unwrap();
    // Advisory locks are held by the owner FD rather than the directory entry:
    // after owner death/close, a new repository operation recovers directly.
    repo.stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();

    // A normal Git writer owns this filename while it updates `index`; Orna
    // refuses rather than replacing an index under that writer.
    let index = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "index"]));
    let generation = repo.index_generation().unwrap();
    let git_lock = index.with_extension("lock");
    fs::write(&git_lock, "ordinary Git writer").unwrap();
    assert!(matches!(
        repo.stage_managed(
            &generation,
            &[ManagedPath::new(".orna/format.orna").unwrap()]
        ),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    fs::remove_file(git_lock).unwrap();
}

#[test]
fn snapshot_resolution_accepts_commits_and_rejects_non_commits_and_malformed_ids() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["hash-object", "-w", "ordinary.txt"]);
    let blob = git(root.path(), &["hash-object", "ordinary.txt"]);
    let tree = git(root.path(), &["write-tree"]);
    assert!(repo.resolve_snapshot(&blob).is_err());
    assert!(repo.resolve_snapshot(&tree).is_err());
    assert!(repo.resolve_snapshot("abc").is_err());
    let dangling = git(
        root.path(),
        &["commit-tree", "HEAD^{tree}", "-m", "dangling"],
    );
    assert!(matches!(
        repo.resolve_snapshot(&dangling),
        Err(orna_repository_v1::RepositoryError::SnapshotNotReachable)
    ));
    git(root.path(), &["switch", "--detach", &dangling]);
    assert_eq!(repo.resolve_snapshot("HEAD").unwrap().as_str(), dangling);
}

#[test]
fn exact_committed_oid_resolution_reuses_commit_and_reachability_guards() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let head_bytes = decode_oid(head.as_str());
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.resolve_committed_oid(GitHash::Sha1, &head_bytes)
            .unwrap()
            .as_str(),
        head.as_str()
    );
    assert_eq!(git_state(&repo, root.path()), before);

    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 20]),
        Err(orna_repository_v1::RepositoryError::SnapshotNotFound)
    ));
    let blob = git(root.path(), &["hash-object", "ordinary.txt"]);
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &decode_oid(&blob)),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));

    let unreachable = git(
        root.path(),
        &["commit-tree", "HEAD^{tree}", "-m", "unreachable"],
    );
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &decode_oid(&unreachable)),
        Err(orna_repository_v1::RepositoryError::SnapshotNotReachable)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn exact_committed_oid_resolution_rejects_malformed_and_foreign_formats() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();

    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 19]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha256, &[0; 32]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
}

#[test]
fn exact_committed_oid_resolution_supports_sha256_repositories() {
    let Some(root) = repository_with_object_format("sha256") else {
        return;
    };
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let head_bytes = decode_oid(head.as_str());

    assert_eq!(head_bytes.len(), 32);
    assert_eq!(
        repo.resolve_committed_oid(GitHash::Sha256, &head_bytes)
            .unwrap()
            .as_str(),
        head.as_str()
    );
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 20]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
}

#[test]
fn committed_file_reads_are_bounded_and_do_not_mutate_repository_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.read_committed_file(&head, Path::new("ordinary.txt"), 64)
            .unwrap(),
        b"base\n"
    );
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("ordinary.txt"), 4),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new(".orna"), 64),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("../ordinary.txt"), 64),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("missing.orna"), 64),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn committed_tree_listing_is_bounded_and_does_not_mutate_repository_state() {
    let root = repository();
    fs::create_dir_all(root.path().join("nested")).unwrap();
    fs::write(
        root.path().join("nested/tool.orna"),
        "module nested.tool;\n",
    )
    .unwrap();
    git(root.path(), &["add", "nested/tool.orna"]);
    git(root.path(), &["commit", "-m", "add nested source"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    let entries = repo.list_committed_tree(&head, 8).unwrap();
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("main.orna")
            && entry.kind() == CommittedTreeEntryKind::File { executable: false }
    }));
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("nested/tool.orna")
            && entry.kind() == CommittedTreeEntryKind::File { executable: false }
    }));
    assert!(matches!(
        repo.list_committed_tree(&head, entries.len().saturating_sub(1)),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[cfg(unix)]
#[test]
fn committed_tree_listing_classifies_symlinks_without_following_them() {
    let root = repository();
    std::os::unix::fs::symlink("main.orna", root.path().join("linked.orna")).unwrap();
    git(root.path(), &["add", "linked.orna"]);
    git(root.path(), &["commit", "-m", "add source symlink"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    let entries = repo.list_committed_tree(&head, 8).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("linked.orna")
            && entry.kind() == CommittedTreeEntryKind::Symlink
    }));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn named_branch_snapshot_resolution_stays_pinned_after_branch_moves() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "published"]);

    // ORNA-SYS-106/139: resolving a named selector returns the immutable
    // commit it named at resolution time, rather than a live branch handle.
    let first = repo.resolve_snapshot("published").unwrap();
    let initial = git(root.path(), &["rev-parse", "published"]);
    assert_eq!(first.as_str(), initial);

    git(root.path(), &["switch", "published"]);
    fs::write(root.path().join("ordinary.txt"), "published advance\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "advance published"]);
    let advanced = git(root.path(), &["rev-parse", "published"]);

    // The first snapshot stays pinned; a later selector resolution observes
    // the branch's new immutable commit.
    assert_eq!(first.as_str(), initial);
    let second = repo.resolve_snapshot("published").unwrap();
    assert_eq!(second.as_str(), advanced);
    assert_ne!(first, second);
}

#[test]
fn checkout_preflight_classifies_a_local_branch_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(17)).unwrap();

    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(17))
        .unwrap();

    assert_eq!(plan.cwd(), &before);
    assert_eq!(plan.expected_head(), before.head());
    assert_eq!(plan.target().branch_name(), Some("experiment"));
    assert_eq!(plan.target().commit(), before.head().unwrap());
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(17)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
}

#[test]
fn checkout_preflight_classifies_a_commit_as_detached_and_rejects_ambiguity() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();

    let plan = repo
        .plan_checkout(&head, RuntimeGeneration::new(18))
        .unwrap();
    assert!(matches!(plan.target(), CheckoutTarget::Detached { .. }));
    assert_eq!(plan.target().commit().as_str(), head);
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before
    );

    git(root.path(), &["tag", "main"]);
    let before_ambiguous = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();
    assert!(matches!(
        repo.plan_checkout("main", RuntimeGeneration::new(18)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before_ambiguous
    );
}

#[test]
fn checkout_rejects_a_full_commit_id_branch_name_without_mutating_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["branch", &head]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("local.txt"), "unstaged local\n").unwrap();
    let runtime = RuntimeGeneration::new(181);
    let before = git_state(&repo, root.path());
    let before_cwd = repo.cwd_generation(runtime).unwrap();

    assert!(matches!(
        repo.plan_checkout(&head, runtime),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(repo.cwd_generation(runtime).unwrap(), before_cwd);
}

#[test]
fn checkout_preflight_invalid_selector_is_strictly_non_mutating() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(19)).unwrap();
    assert!(matches!(
        repo.plan_checkout("-force", RuntimeGeneration::new(19)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(19)).unwrap(),
        before
    );
}

#[test]
fn checkout_preflight_revalidation_accepts_unchanged_state_and_rejects_worktree_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(20))
        .unwrap();
    assert!(repo.verify_checkout_preflight(&plan).is_ok());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_preflight_revalidation_rejects_index_head_and_branch_tip_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(21))
        .unwrap();

    fs::write(root.path().join("ordinary.txt"), "index drift\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(22))
        .unwrap();
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "head drift"],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(23))
        .unwrap();
    let next = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "tip drift",
        ],
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/experiment",
            &next,
            &git(root.path(), &["rev-parse", "experiment"]),
        ],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_preflight_rejects_same_commit_detached_attachment_drift_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();

    let target = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["switch", "--detach", &target]);
    let before_verification = repo.cwd_generation(RuntimeGeneration::new(24)).unwrap();

    // CHECKOUT-1 steps 1 and 6 / ORNA-BRANCH-002: attachment is part of
    // the state-bound preflight even where the selected commit is unchanged.
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(24)).unwrap(),
        before_verification
    );
    assert!(git(root.path(), &["branch", "--show-current"]).is_empty());
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), target);
}

#[test]
fn checkout_force_authorization_is_canonical_and_stale_state_is_rejected() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let branch = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    let unchanged = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    assert_eq!(branch.force_token(), unchanged.force_token());
    assert!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token()))
            .is_ok()
    );
    assert!(matches!(
        repo.authorize_checkout_force(&branch, false, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, None),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let detached = repo
        .plan_checkout(&head, RuntimeGeneration::new(24))
        .unwrap();
    assert_ne!(branch.force_token(), detached.force_token());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_force_authorization_rejects_changed_bytes_for_an_ignored_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "local-cache/\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local cache"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    let ignored_path = root.path().join("local-cache").join("keep.bin");
    fs::create_dir_all(ignored_path.parent().unwrap()).unwrap();
    fs::write(&ignored_path, b"ignored before\0\xff\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(42))
        .unwrap();
    let token = plan.force_token();
    let status_before = git(root.path(), &["status", "--porcelain=v2"]);

    // Ignored bytes are absent from ordinary status and checkout path sets,
    // but remain part of the dirty worktree that a force plan must fence.
    fs::write(&ignored_path, b"ignored after\0\xfe\n").unwrap();

    assert_eq!(
        git(root.path(), &["status", "--porcelain=v2"]),
        status_before
    );
    assert!(matches!(
        repo.authorize_checkout_force(&plan, true, Some(&token)),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn same_commit_checkout_switches_attachment_without_discarding_local_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(26))
        .unwrap();

    repo.execute_same_commit_checkout(&plan).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    let detached = repo
        .plan_checkout(
            &git(root.path(), &["rev-parse", "HEAD"]),
            RuntimeGeneration::new(26),
        )
        .unwrap();
    repo.execute_same_commit_checkout(&detached).unwrap();
    assert!(git(root.path(), &["branch", "--show-current"]).is_empty());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn same_commit_checkout_rejects_a_divergent_target_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(27)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(27))
        .unwrap();

    assert!(matches!(
        repo.execute_same_commit_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(27)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn divergent_checkout_carries_nonconflicting_git_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(29))
        .unwrap();
    assert!(plan.git().conflicting_paths().is_empty());

    repo.execute_nonconflicting_git_checkout(&plan).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("target.orna")).unwrap(),
        "target source\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn divergent_checkout_logical_validation_rejection_fences_git_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(31)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(31))
        .unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout_with_validation(&plan, |repository, observed| {
            assert_eq!(
                repository.head().unwrap(),
                observed.expected_head().cloned()
            );
            assert_eq!(observed.target().branch_name(), Some("experiment"));
            Err::<(), _>("candidate schema assertion rejected")
        }),
        Err(CheckoutExecutionError::Validation(
            "candidate schema assertion rejected"
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(31)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert!(!root.path().join("target.orna").exists());
}

#[test]
fn divergent_checkout_rechecks_git_state_after_candidate_validation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(42))
        .unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout_with_validation(&plan, |_, _| {
            fs::write(root.path().join("main.orna"), "changed during validation\n").unwrap();
            Ok::<(), ()>(())
        }),
        Err(CheckoutExecutionError::Repository(
            orna_repository_v1::RepositoryError::CheckoutPlanStale
        ))
    ));
    assert_eq!(git_state(&repo, root.path()).0, before.0);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "changed during validation\n"
    );
    assert!(!root.path().join("target.orna").exists());
}

#[test]
fn divergent_checkout_refuses_conflicting_git_state_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(30)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(30))
        .unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(30)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "local");
}

#[test]
fn divergent_checkout_refuses_to_overwrite_an_untracked_target_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("new.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "new.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("new.orna"), "untracked local\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(33)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(33))
        .unwrap();

    assert_eq!(
        plan.git().conflicting_paths(),
        &[ManagedPath::new("new.orna").unwrap()]
    );
    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(33)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(root.path().join("new.orna")).unwrap(),
        "untracked local\n"
    );
}

#[test]
fn divergent_checkout_refuses_to_overwrite_an_ignored_target_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "new.orna\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local source"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("new.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "-f", "new.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("new.orna"), "ignored local\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(38)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();

    assert_eq!(
        plan.git().conflicting_paths(),
        &[ManagedPath::new("new.orna").unwrap()]
    );
    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(38)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(root.path().join("new.orna")).unwrap(),
        "ignored local\n"
    );
}

#[test]
fn checkout_subplan_classifies_target_and_local_path_sets() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    let ordinary = ManagedPath::new("ordinary.txt").unwrap();
    assert_eq!(plan.git().affected_paths(), std::slice::from_ref(&ordinary));
    assert_eq!(
        plan.git().conflicting_paths(),
        std::slice::from_ref(&ordinary)
    );
    assert_eq!(
        plan.git().discardable_paths(),
        std::slice::from_ref(&ordinary)
    );

    fs::write(root.path().join("main.orna"), "carried\n").unwrap();
    let carried = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    assert_eq!(
        carried.git().affected_paths(),
        &[
            ManagedPath::new("main.orna").unwrap(),
            ManagedPath::new("ordinary.txt").unwrap()
        ]
    );
    assert_eq!(
        carried.git().conflicting_paths(),
        plan.git().conflicting_paths()
    );
    assert_ne!(carried.force_token(), plan.force_token());
}

#[test]
fn checkout_discard_set_requires_the_canonical_force_witness_and_exact_paths() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged local\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(28))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(28)).unwrap();

    let _validated = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );

    assert!(matches!(
        repo.validate_checkout_discard_set(&plan, true, Some(&token), &[]),
        Err(orna_repository_v1::RepositoryError::CheckoutDiscardSetMismatch)
    ));
    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            false,
            Some(&token),
            plan.git().discardable_paths(),
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged local\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn checkout_discard_set_rejects_a_force_plan_after_target_branch_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(34))
        .unwrap();
    let token = plan.force_token();
    let target = git(root.path(), &["rev-parse", "experiment"]);
    let successor = git(
        root.path(),
        &[
            "commit-tree",
            "experiment^{tree}",
            "-p",
            "experiment",
            "-m",
            "target branch moved after planning",
        ],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/experiment", &successor, &target],
    );
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths()
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn admitted_force_discard_is_fenced_against_later_target_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(35))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();

    let target = git(root.path(), &["rev-parse", "experiment"]);
    let successor = git(
        root.path(),
        &[
            "commit-tree",
            "experiment^{tree}",
            "-p",
            "experiment",
            "-m",
            "target branch moved after admission",
        ],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/experiment", &successor, &target],
    );
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.verify_validated_checkout_discard(&discard),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn pre_execution_force_checkout_recovery_retains_drifted_intent_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(36))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    assert!(repo.has_pending_pre_execution_checkout().unwrap());

    let restarted = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("main.orna"), "interleaved source\n").unwrap();
    let before = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before);
    assert!(restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");

    fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
}

#[test]
fn pending_force_checkout_recovery_fences_other_git_visible_checkout_executors() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    git(root.path(), &["branch", "same-commit"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let force_plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(37))
        .unwrap();
    let force_token = force_plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &force_plan,
            true,
            Some(&force_token),
            force_plan.git().discardable_paths(),
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let safe_plan = repo
        .plan_checkout("same-commit", RuntimeGeneration::new(37))
        .unwrap();
    let before = git_state(&repo, root.path());
    assert!(matches!(
        repo.execute_same_commit_checkout(&safe_plan),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
}

#[test]
fn restarted_force_discard_recovery_requires_the_recorded_canonical_witness() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    git(root.path(), &["branch", "same-commit"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let restarted = Repository::discover(root.path()).unwrap();
    let unrelated = restarted
        .plan_checkout("same-commit", RuntimeGeneration::new(38))
        .unwrap();
    let before_unrelated = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.execute_same_commit_checkout(&unrelated),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before_unrelated);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");

    fs::write(root.path().join("ordinary.txt"), "stale local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let stale = restarted
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();
    assert_ne!(stale.force_token(), token);
    let stale_discard = restarted
        .validate_checkout_discard_set(
            &stale,
            true,
            Some(&stale.force_token()),
            stale.git().discardable_paths(),
        )
        .unwrap();
    let before_stale = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.persist_validated_checkout_discard(&stale_discard),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before_stale);
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "stale local\n"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "stale local");
    assert!(restarted.has_pending_pre_execution_checkout().unwrap());

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn force_checkout_discards_only_the_consented_paths_and_carries_unrelated_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(39))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    repo.execute_validated_force_checkout(&discard).unwrap();
    assert!(!repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}

#[test]
fn interrupted_force_checkout_after_discard_journal_recovers_recorded_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(41))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_after_discard_with_test_hook(
            &discard,
            &mut interrupt
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert!(
        !restarted
            .runtime_paths()
            .root()
            .join("checkout-journal.bin")
            .exists()
    );
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}

#[test]
fn interrupted_force_checkout_after_discard_journal_recovers_detached_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    let target = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout(&target, RuntimeGeneration::new(42))
        .unwrap();
    assert!(matches!(
        plan.target(),
        CheckoutTarget::Detached { commit } if commit.as_str() == target
    ));
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_after_discard_with_test_hook(
            &discard,
            &mut interrupt
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert!(
        !restarted
            .runtime_paths()
            .root()
            .join("checkout-journal.bin")
            .exists()
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "");
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), target);
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "target\n"
    );
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}

#[test]
fn interrupted_force_checkout_after_git_switch_recovers_selected_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "local-cache/\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local cache"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let ignored_path = root.path().join("local-cache").join("keep.bin");
    let ignored_bytes = b"ignored local bytes\0\xff\n".to_vec();
    fs::create_dir_all(ignored_path.parent().unwrap()).unwrap();
    fs::write(&ignored_path, &ignored_bytes).unwrap();
    git(root.path(), &["check-ignore", "-q", "local-cache/keep.bin"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(40))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_with_test_hook(&discard, &mut interrupt),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
    assert_eq!(fs::read(ignored_path).unwrap(), ignored_bytes);
}

#[test]
fn malformed_discarded_checkout_journal_attachment_fails_before_recovery_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "beta"]);
    git(root.path(), &["switch", "beta"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("beta", RuntimeGeneration::new(41))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_with_test_hook(&discard, &mut interrupt),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));

    let journal_path = repo.runtime_paths().root().join("checkout-journal.bin");
    let mut journal = fs::read(&journal_path).unwrap();
    let discarded_branch = journal
        .windows(b"main".len())
        .rposition(|window| window == b"main")
        .expect("discarded branch is journalled");
    journal[discarded_branch..discarded_branch + b"main".len()].copy_from_slice(b"beta");
    fs::write(&journal_path, &journal).unwrap();

    let restarted = Repository::discover(root.path()).unwrap();
    let before = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::InvalidCheckoutJournal)
    ));
    assert_eq!(git_state(&restarted, root.path()), before);
    assert_eq!(fs::read(&journal_path).unwrap(), journal);
}

#[test]
fn checkout_discard_set_logical_validation_rejection_fences_force_admission() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(32))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(32)).unwrap();

    assert!(matches!(
        repo.validate_checkout_discard_set_with_validation(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            |repository, observed| {
                assert_eq!(
                    repository.head().unwrap(),
                    observed.expected_head().cloned()
                );
                assert_eq!(observed.target().branch_name(), Some("experiment"));
                Err::<(), _>("candidate schema assertion rejected")
            },
        ),
        Err(orna_repository_v1::CheckoutExecutionError::Validation(
            "candidate schema assertion rejected"
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(32)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn verify_cwd_rejects_a_worktree_only_interleaving() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let observed = repo.cwd_generation(RuntimeGeneration::new(7)).unwrap();
    fs::write(root.path().join("main.orna"), "changed only in worktree\n").unwrap();
    assert!(matches!(
        repo.verify_cwd(&observed),
        Err(orna_repository_v1::RepositoryError::StaleCwd)
    ));
}

#[test]
fn ordinary_git_add_interleavings_make_stage_and_unstage_stale() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new(".orna/format.orna").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "candidate stage\n").unwrap();
    let expected_stage = repo.index_generation().unwrap();
    let mut stage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race one\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.stage_managed_with_test_hook(
            &expected_stage,
            std::slice::from_ref(&managed),
            &mut stage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race one"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "format 1"
    );

    let expected_unstage = repo.index_generation().unwrap();
    let mut unstage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race two\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.unstage_managed_with_test_hook(
            &expected_unstage,
            std::slice::from_ref(&managed),
            &mut unstage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race two"
    );
}

#[test]
fn conflicted_index_fails_closed_and_errors_redact_local_paths() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["switch", "-c", "other"]);
    fs::write(root.path().join("ordinary.txt"), "other\n").unwrap();
    git(root.path(), &["commit", "-am", "other"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "main\n").unwrap();
    git(root.path(), &["commit", "-am", "main"]);
    let output = Command::new("git")
        .current_dir(root.path())
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(matches!(
        repo.index_generation(),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    let error = Repository::discover(root.path().join("missing").join("file")).unwrap_err();
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(&root.path().display().to_string()));
}

#[test]
fn explicit_snapshot_branch_and_remote_preserve_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    assert_eq!(repo.resolve_snapshot("HEAD").unwrap(), head);
    let index = repo.index_generation().unwrap();
    repo.create_branch_at_head("experiment").unwrap();
    assert!(repo.create_branch_at_head("experiment").is_err());
    assert_eq!(repo.index_generation().unwrap(), index);
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/experiment"]),
        head.as_str()
    );
    git(
        root.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/orna.git",
        ],
    );
    assert_eq!(repo.remote_names().unwrap(), vec!["origin"]);
}

#[test]
fn observes_an_ordinary_repository_and_materialized_head_without_mutation() {
    let root = repository();
    with_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Ordinary);
    assert!(!capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert!(format!("{capabilities:?}").contains("Ordinary"));
    assert!(!format!("{capabilities:?}").contains("example.invalid"));
    assert!(!format!("{capabilities:?}").contains("account"));
    assert!(!format!("{capabilities:?}").contains("secret"));
    assert!(!format!("{capabilities:?}").contains(&root.path().display().to_string()));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    assert!(matches!(
        repo.observe_git_object(&head).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            size: 1..
        }
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn remote_continuity_reports_missing_internal_refs_without_mutation() {
    let root = repository();
    let _remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_remote_continuity("origin", &[required_internal_ref(&head)]),
        RemoteContinuity::Missing
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn local_internal_and_tracking_refs_cannot_substitute_remote_continuity() {
    let root = repository();
    let _remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let required = required_internal_ref(&head);

    // These locally materialized names can be stale after a plain-Git transfer.
    // They are not evidence that the configured remote retained Orna continuity.
    git(
        root.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &head],
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/remotes/origin/orna/ids/0123456789abcdef",
            &head,
        ],
    );
    let before = git_state(&repo, root.path());

    let continuity = repo.observe_remote_continuity("origin", std::slice::from_ref(&required));

    assert_eq!(continuity, RemoteContinuity::Missing);
    assert!(!continuity.permits_continuity_claim());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn remote_continuity_reports_matching_and_stale_internal_refs_without_mutation() {
    let root = repository();
    let remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let expected = head.to_ascii_uppercase();
    let required = required_internal_ref(&expected);
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &head],
    );
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/unrelated", &head],
    );
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Continuous
    );
    assert_eq!(git_state(&repo, root.path()), before);
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let stale = git(
        root.path(),
        &["commit-tree", &tree, "-p", &head, "-m", "stale continuity"],
    );
    let refspec = format!("{stale}:refs/heads/stale-continuity");
    git(root.path(), &["push", "origin", &refspec]);
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &stale],
    );
    let stale_before = git_state(&repo, root.path());
    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Stale
    );
    assert_eq!(git_state(&repo, root.path()), stale_before);
}

#[test]
fn remote_continuity_fails_closed_for_invalid_or_ambiguous_input_and_remote_failure() {
    let root = repository();
    let remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let required = required_internal_ref(&head);
    let before = git_state(&repo, root.path());

    assert!(OrnaInternalRef::new("refs/heads/main").is_err());
    assert!(OrnaInternalRef::new("refs/orna/ids/contains\x7fdel").is_err());
    assert!(NativeObjectId::new("not-a-native-object-id").is_err());
    assert_eq!(
        repo.observe_remote_continuity("origin", &[]),
        RemoteContinuity::InvalidEvidence
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", &[required.clone(), required.clone()]),
        RemoteContinuity::InvalidEvidence
    );
    let wrong_native_format = RequiredInternalRef::new(
        OrnaInternalRef::new("refs/orna/ids/0123456789abcdef").unwrap(),
        NativeObjectId::new("0123456789012345678901234567890123456789012345678901234567890123")
            .unwrap(),
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", &[wrong_native_format]),
        RemoteContinuity::InvalidEvidence
    );
    assert_eq!(
        repo.observe_remote_continuity("missing", std::slice::from_ref(&required)),
        RemoteContinuity::Unverifiable
    );
    git(
        remote.path(),
        &["update-ref", "refs/orna/unexpected", &head],
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Missing
    );
    assert_eq!(git_state(&repo, root.path()), before);

    let unreachable = repository();
    with_remote(unreachable.path());
    let unreachable_repo = Repository::discover(unreachable.path()).unwrap();
    let unreachable_head = git(unreachable.path(), &["rev-parse", "HEAD"]);
    let unreachable_before = git_state(&unreachable_repo, unreachable.path());
    assert_eq!(
        unreachable_repo
            .observe_remote_continuity("origin", &[required_internal_ref(&unreachable_head)]),
        RemoteContinuity::Unverifiable
    );
    assert_eq!(
        git_state(&unreachable_repo, unreachable.path()),
        unreachable_before
    );
}

#[test]
fn distinguishes_sparse_checkout_from_partial_clone() {
    let root = repository();
    git(root.path(), &["sparse-checkout", "init", "--no-cone"]);
    git(
        root.path(),
        &["sparse-checkout", "set", "main.orna", "ordinary.txt"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::SparseCheckout);
    assert!(capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn sparse_checkout_omits_worktree_paths_without_promising_their_objects() {
    let root = repository();
    let blob = git(root.path(), &["rev-parse", "HEAD:ordinary.txt"]);
    git(root.path(), &["sparse-checkout", "init", "--no-cone"]);
    git(root.path(), &["sparse-checkout", "set", "main.orna"]);
    assert!(!root.path().join("ordinary.txt").exists());

    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    assert_eq!(
        repo.observe_git_capabilities().unwrap().mode(),
        GitRepositoryMode::SparseCheckout
    );
    assert!(matches!(
        repo.observe_git_object(&blob).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            size: 1..
        }
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_a_partial_clone_and_a_promised_object_without_hydrating() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // A configuration-only repository never authorizes a Promised result.
        let root = repository();
        with_partial_clone(root.path());
        let repo = Repository::discover(root.path()).unwrap();
        assert_eq!(
            repo.observe_git_object("0000000000000000000000000000000000000000")
                .unwrap(),
            GitObjectState::Unavailable
        );
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::PartialClone);
    assert!(!capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(capabilities.promisor_remotes(), &["origin".to_owned()]);
    assert_eq!(
        repo.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(git_state(&repo, &clone), before);
    drop(fixture);
}

#[test]
fn filtered_clone_keeps_promised_unavailable_and_materialized_objects_distinct_without_mutation() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support.  A
        // configuration-only repository is covered separately and must never
        // manufacture a promised result.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let materialized = git(&clone, &["rev-parse", "HEAD"]);
    let unavailable = "0".repeat(materialized.len());

    assert!(matches!(
        repo.observe_git_object(&materialized).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            size: 1..
        }
    ));
    assert_eq!(
        repo.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repo.observe_git_object(&unavailable).unwrap(),
        GitObjectState::Unavailable
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn declared_object_set_requires_every_object_to_be_materialized_or_proven_promised() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support. A
        // configuration-only repository cannot prove a missing object is
        // promised, so it is not evidence for declared-set completeness.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let materialized = git(&clone, &["rev-parse", "HEAD"]);
    let unavailable = "0".repeat(materialized.len());

    assert_eq!(
        repo.observe_declared_git_object_set(&[&materialized, &promised])
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    assert_eq!(
        repo.observe_declared_git_object_set(&[&materialized, &unavailable])
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );
    assert_eq!(
        repo.observe_declared_git_object_set(&["not-a-native-object-id"])
            .unwrap(),
        GitDeclaredObjectSetState::Malformed
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn declared_segment_blob_set_rejects_non_blobs_without_hydrating_promises() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support. A
        // configuration-only repository cannot prove a missing blob is
        // promised, so it is not evidence for segment completeness.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let blob = git(&clone, &["rev-parse", "HEAD:visible.txt"]);
    let tree = git(&clone, &["rev-parse", "HEAD^{tree}"]);
    let unavailable = "0".repeat(blob.len());

    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&blob, &promised])
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&tree]).unwrap(),
        GitDeclaredObjectSetState::Malformed
    );
    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&unavailable])
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn compact_manifest_observation_fails_closed_without_manifest_metadata() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_compact_manifest_segment_blobs(&head, Uuid::nil())
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn empty_compact_manifest_is_a_valid_committed_snapshot() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let schema = compact_schema_fingerprint(table);
    let manifest_path = ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
    let manifest = format!(
        "{{profile: \"compact-storage-v1\", table: \"{table}\", schema: \"{}\", next_generation: 1, shards: []}}\n",
        hex_digest(&schema),
    )
    .into_bytes();
    let head = repo.head().unwrap().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                manifest_path,
                Some(manifest),
            )],
            "test: commit empty compact manifest",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert_eq!(
        repo.read_compact_manifest(&candidate.commit().clone(), table)
            .unwrap(),
        Some(CompactManifest::empty(table, schema))
    );
}

#[test]
fn compact_manifest_inventory_discovers_every_table_without_mutating_repository_or_runtime_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
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
    let first_pending = repo.publish_compact_repository_boundary(first).unwrap();
    repo.finish_compact_with_receipt(&compact_runtime_receipt(&first_pending, &signing_key))
        .unwrap();

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
    let second_pending = repo.publish_compact_repository_boundary(second).unwrap();
    repo.finish_compact_with_receipt(&compact_runtime_receipt(&second_pending, &signing_key))
        .unwrap();

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
fn compact_manifest_inventory_ignores_policy_only_table_roots() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let initial = repo.head().unwrap().unwrap();
    assert!(
        repo.plan_compact_manifest_inventory_hydration(&initial)
            .unwrap()
            .is_empty()
    );
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0606);
    let head = repo.head().unwrap().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new(format!(".orna/storage/{table}/policy.orna")).unwrap(),
                Some(b"{placement: \"automatic\"}\n".to_vec()),
            )],
            "test: commit policy-only compact root",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(
        repo.read_compact_manifest_inventory(candidate.commit())
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.plan_compact_manifest_inventory_hydration(candidate.commit())
            .unwrap()
            .is_empty()
    );
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
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                orna_repository_v1::ManagedFileChange::new(shard_path, Some(shard_bytes)),
            ],
            "malformed-shard" => vec![
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                orna_repository_v1::ManagedFileChange::new(
                    shard_path,
                    Some(b"not a compact manifest shard\n".to_vec()),
                ),
                orna_repository_v1::ManagedFileChange::new(segment_path, Some(segment_bytes)),
            ],
            "orphan" => vec![
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                orna_repository_v1::ManagedFileChange::new(shard_path, Some(shard_bytes)),
                orna_repository_v1::ManagedFileChange::new(segment_path, Some(segment_bytes)),
                orna_repository_v1::ManagedFileChange::new(
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
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let first = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0613);
    let pending = repo
        .publish_compact_repository_boundary(compact_plan(
            &repo,
            first,
            [73; 16],
            &[compact_segment(
                first,
                73,
                b"first inventory table".to_vec(),
            )],
        ))
        .unwrap();
    repo.finish_compact_with_receipt(&compact_runtime_receipt(&pending, &signing_key))
        .unwrap();

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
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(manifest_bytes)),
                orna_repository_v1::ManagedFileChange::new(shard_path, Some(shard_bytes)),
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
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
        let signing_key = compact_receipt_signing_key();
        provision_compact_receipt_trust_root(&repo, &signing_key);
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
        let pending = repo.publish_compact_repository_boundary(plan).unwrap();
        repo.finish_compact_with_receipt(&compact_runtime_receipt(&pending, &signing_key))
            .unwrap();
        let head = repo.head().unwrap().unwrap();
        let orphan = repo
            .build_private_commit(
                &head,
                &[orna_repository_v1::ManagedFileChange::new(
                    ManagedPath::new(format!(".orna/storage/{table}/{artifact}")).unwrap(),
                    Some(b"unreferenced compact artifact".to_vec()),
                )],
                "test: commit orphan compact inventory artifact",
            )
            .unwrap();
        repo.advance_current_ref(&head, &orphan).unwrap();

        assert!(matches!(
            repo.read_compact_manifest_inventory(orphan.commit()),
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
        ));
    }
}

#[test]
fn compact_manifest_inventory_rejects_symlinked_manifest_shard_and_segment_entries() {
    for entry in ["manifest", "shard", "segment"] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let signing_key = compact_receipt_signing_key();
        provision_compact_receipt_trust_root(&repo, &signing_key);
        let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0607);
        let plan = compact_plan(
            &repo,
            table,
            [63; 16],
            &[compact_segment(table, 63, b"symlink witness".to_vec())],
        );
        let pending = repo.publish_compact_repository_boundary(plan).unwrap();
        repo.finish_compact_with_receipt(&compact_runtime_receipt(&pending, &signing_key))
            .unwrap();
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
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
        ));
        assert!(matches!(
            repo.plan_compact_manifest_inventory_hydration(&corrupted),
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
        let mut changes = vec![orna_repository_v1::ManagedFileChange::new(
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
            changes.push(orna_repository_v1::ManagedFileChange::new(
                manifest_path,
                Some(manifest_bytes),
            ));
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
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
    let pending = repo.publish_compact_repository_boundary(plan).unwrap();
    let manifest = repo
        .read_compact_manifest(pending.commit(), table)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.schema(), schema_digest);
    assert_eq!(manifest.entries().len(), 1);

    let verified = repo
        .read_verified_compact_segment(pending.commit(), table, &manifest.entries()[0])
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
    publish_compact_repository_boundary(&repo, plan).unwrap();
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
    publish_compact_repository_boundary(&repo, plan).unwrap();
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
    publish_compact_repository_boundary(&repo, plan).unwrap();
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
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
    ));
    assert!(matches!(
        repo.plan_compact_manifest_hydration_for_key_range(&head, table, &[], &upper),
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
    publish_compact_repository_boundary(&repo, plan).unwrap();
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
    publish_compact_repository_boundary(&repo, plan).unwrap();
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
fn observes_combined_sparse_and_partial_capabilities() {
    let root = repository();
    with_partial_clone(root.path());
    git(root.path(), &["config", "core.sparseCheckout", "true"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Combined);
    assert!(capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_malformed_configuration_and_object_ids_without_leaking_state() {
    let root = repository();
    with_remote(root.path());
    git(
        root.path(),
        &["config", "extensions.partialClone", "missing-remote"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_capabilities().unwrap().mode(),
        GitRepositoryMode::Malformed
    );
    assert_eq!(
        repo.observe_git_object("not-a-native-object-id").unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_an_unavailable_object_distinct_from_a_promised_object() {
    let root = repository();
    with_partial_clone(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    // Valid promisor configuration alone is not proof that an arbitrary
    // object is promised by a reachable local commit or tree.
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Unavailable
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_materialized_tree_blob_and_tag_without_mutation() {
    let root = repository();
    git(root.path(), &["tag", "-a", "release", "-m", "release"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let blob = git(root.path(), &["rev-parse", "HEAD:ordinary.txt"]);
    let tag = git(root.path(), &["rev-parse", "release^{tag}"]);

    for (object, kind) in [
        (tree, GitObjectKind::Tree),
        (blob, GitObjectKind::Blob),
        (tag, GitObjectKind::Tag),
    ] {
        assert!(matches!(
            repo.observe_git_object(&object).unwrap(),
            GitObjectState::Materialized { kind: observed, .. } if observed == kind
        ));
    }
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_native_objects_with_case_insensitive_hex_spelling() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    // This is the fixed SHA-1 for the `main.orna` bytes written by
    // `repository()`. Keeping the object ID explicit makes the case change
    // deterministic instead of depending on a particular HEAD hash.
    let object = "8bffabe30647ae6f01aec31fbcc2ad23dde3dcbd";
    assert_eq!(git(root.path(), &["rev-parse", "HEAD:main.orna"]), object);
    let uppercase = object.to_ascii_uppercase();
    assert_ne!(object, uppercase);

    assert_eq!(
        repo.observe_git_object(&uppercase).unwrap(),
        repo.observe_git_object(object).unwrap()
    );
}

#[test]
fn reports_a_corrupt_local_object_as_malformed_without_mutation() {
    let root = repository();
    let object = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let object_directory = root.path().join(".git").join("objects").join("aa");
    fs::create_dir_all(&object_directory).unwrap();
    fs::write(object_directory.join(&object[2..]), b"not a Git object").unwrap();
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_object(object).unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn managed_materialization_is_atomic_and_conflict_fenced() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let path = ManagedPath::new("generated/books/one.orna").unwrap();

    repo.materialize_managed_file(&path, None, Some(b"first"))
        .unwrap();
    assert_eq!(
        repo.managed_file_bytes(&path).unwrap(),
        Some(b"first".to_vec())
    );
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"first"
    );

    fs::write(root.path().join(path.as_path()), b"editor change").unwrap();
    assert!(matches!(
        repo.materialize_managed_file(&path, Some(b"first"), Some(b"second")),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"editor change"
    );
    repo.materialize_managed_file(&path, Some(b"editor change"), None)
        .unwrap();
    assert!(!root.path().join(path.as_path()).exists());
}

#[cfg(unix)]
#[test]
fn managed_materialization_rejects_symlinked_parents() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("generated")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("generated/linked")).unwrap();
    let path = ManagedPath::new("generated/linked/row.orna").unwrap();

    assert!(matches!(
        repo.materialize_managed_file(&path, None, Some(b"row")),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
    assert!(!outside.path().join("row.orna").exists());
}

#[test]
fn private_candidate_uses_head_and_preserves_ordinary_cwd_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let worktree_before = repo.worktree_state().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    assert_ne!(candidate.commit(), &head);
    assert_eq!(repo.head().unwrap().unwrap(), head);
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.worktree_state().unwrap(), worktree_before);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        git(
            root.path(),
            &[
                "show",
                &format!("{}:generated/row.orna", candidate.commit())
            ]
        ),
        "candidate row"
    );
    assert_eq!(
        git(
            root.path(),
            &["show", &format!("{}:ordinary.txt", candidate.commit())]
        ),
        "base"
    );
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{}^", candidate.commit())]
        ),
        head.as_str()
    );
}

#[test]
fn capture_rejects_a_tracked_case_folded_loose_sibling_without_mutation() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let proposed = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, std::slice::from_ref(&proposed)),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join("Contact/Alice.orna")).unwrap(),
        b"committed row\n"
    );
    assert!(!root.path().join(proposed.as_path()).exists());
}

#[test]
fn capture_rejects_a_case_folded_table_root_without_mutation() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let proposed = ManagedPath::new("contact/Alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, std::slice::from_ref(&proposed)),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join("Contact/Alice.orna")).unwrap(),
        b"committed row\n"
    );
    assert!(!root.path().join(proposed.as_path()).exists());
}

#[test]
fn capture_rejects_case_folded_planned_loose_siblings_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let upper = ManagedPath::new("Contact/Alice.orna").unwrap();
    let lower = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, &[upper, lower]),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(!root.path().join("Contact/Alice.orna").exists());
    assert!(!root.path().join("Contact/alice.orna").exists());
}

#[test]
fn private_candidate_accepts_a_case_only_rekey_after_deleting_the_old_path() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"old row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let old_path = ManagedPath::new("Contact/Alice.orna").unwrap();
    let new_path = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.capture_managed_publication_state(
            &head,
            &index,
            &[old_path.clone(), new_path.clone()]
        )
        .unwrap(),
        vec![Some(b"old row\n".to_vec()), None]
    );
    let candidate = repo
        .build_private_commit(
            &head,
            &[
                orna_repository_v1::ManagedFileChange::new(old_path, None),
                orna_repository_v1::ManagedFileChange::new(
                    new_path,
                    Some(b"rekeyed row\n".to_vec()),
                ),
            ],
            "orna: case-only re-key",
        )
        .unwrap();

    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        git(
            root.path(),
            &[
                "ls-tree",
                "-r",
                "--name-only",
                candidate.commit().as_str(),
                "--",
                "Contact/alice.orna",
            ],
        ),
        "Contact/alice.orna"
    );
    assert_eq!(
        git(
            root.path(),
            &[
                "ls-tree",
                "-r",
                "--name-only",
                candidate.commit().as_str(),
                "--",
                "Contact/Alice.orna",
            ],
        ),
        ""
    );
}

#[test]
fn capture_allows_an_exact_tracked_loose_path_update() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let path = ManagedPath::new("Contact/Alice.orna").unwrap();

    assert_eq!(
        repo.capture_managed_publication_state(&head, &index, &[path.clone(), path])
            .unwrap(),
        vec![
            Some(b"committed row\n".to_vec()),
            Some(b"committed row\n".to_vec())
        ]
    );
}

#[test]
fn private_candidate_advances_current_branch_with_compare_and_set() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    repo.advance_current_ref(&head, &candidate).unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    let index_after = repo.index_generation().unwrap();
    assert_eq!(index_after.tree(), index_before.tree());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    let status = git(root.path(), &["status", "--short"]);
    assert!(status.contains("ordinary.txt"));
    assert!(status.contains("main.orna"));
    assert!(status.contains("generated/row.orna"));
    assert!(matches!(
        repo.advance_current_ref(&head, &candidate),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
}

#[test]
fn published_candidate_reconciles_only_managed_index_entries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    let reconciled = repo
        .reconcile_published_index(&index_before, &candidate, &[managed])
        .unwrap();
    assert_eq!(reconciled.head(), Some(candidate.commit()));
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
}

#[test]
fn publication_journal_round_trips_atomically_and_advances_monotonically() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new(
        head.clone(),
        candidate.commit().clone(),
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap(),
        Some(journal.clone())
    );
    let mut resumed = repo.read_publication_journal().unwrap().unwrap();
    resumed
        .advance(orna_repository_v1::PublicationJournalStage::RefAdvanced)
        .unwrap();
    assert!(matches!(
        resumed.advance(orna_repository_v1::PublicationJournalStage::Complete),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    repo.write_publication_journal(&resumed).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::RefAdvanced
    );
    repo.clear_publication_journal().unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn publish_candidate_completes_ref_index_and_worktree_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [1; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    repo.mark_runtime_complete([1; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn publication_pauses_for_an_existing_git_index_lock_before_ref_change() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [9; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    fs::write(root.path().join(".git/index.lock"), b"ordinary writer\n").unwrap();
    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
}

#[test]
fn publication_rejects_a_known_managed_edit_before_ref_advance() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [10; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join(managed.as_path()), b"editor row\n").unwrap();

    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"editor row\n"
    );
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_resumes_after_ref_and_index_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [2; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&journal.old_head().clone(), &candidate)
        .unwrap();
    repo.reconcile_published_index(&index_before, &candidate, std::slice::from_ref(&managed))
        .unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([2; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn recovery_after_ref_advance_preserves_unrelated_partial_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(
        root.path().join("ordinary.txt"),
        "staged ordinary\nunstaged ordinary\n",
    )
    .unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let staged_before = git(root.path(), &["diff", "--cached", "--binary"]);
    let unstaged_before = git(root.path(), &["diff", "--binary"]);

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [29; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--binary"]),
        staged_before
    );
    assert_eq!(git(root.path(), &["diff", "--binary"]), unstaged_before);
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );

    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([29; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn runtime_completion_preserves_the_journal_when_head_moved_after_reconciliation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [28; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary commit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(
        root.path(),
        &["commit", "-m", "ordinary commit after publication"],
    );

    assert!(matches!(
        repo.mark_runtime_complete([28; 16], &mut journal),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::WorktreeReconciled
    );
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_keeps_a_pre_ref_publication_pending() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [8; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_rejects_a_post_ref_journal_whose_bytes_differ_from_the_candidate() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [19; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"journal row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap(), Some(candidate.commit().clone()));
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_rejects_a_journal_candidate_outside_the_recorded_head_lineage() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let old_head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let unrelated = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "unrelated recovery candidate",
        ],
    );
    git(root.path(), &["switch", "--detach", &unrelated]);
    let unrelated_head = repo.head().unwrap().unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        old_head,
        unrelated_head,
        index_before.tree().unwrap().clone(),
        [18; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *journal.new_head());
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_preserves_post_ref_external_conflict_and_can_resume() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [3; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join(managed.as_path()), b"editor").unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"editor"
    );
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::IndexReconciled
    );

    fs::write(root.path().join(managed.as_path()), b"candidate row\n").unwrap();
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([3; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
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
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
    let expected_columns = base64(&compact_columns());
    let malformed_columns = base64(&compact_columns_for_field(Uuid::from_u64_pair(7, 40)));
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
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(malformed_manifest)),
                orna_repository_v1::ManagedFileChange::new(shard_path, Some(malformed_shard)),
            ],
            "test: commit malformed compact descriptor",
        )
        .unwrap();
    repo.advance_current_ref(&head, &malformed).unwrap();

    assert!(matches!(
        repo.read_compact_manifest(malformed.commit(), table),
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
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
    publish_compact_repository_boundary(&repo, plan).unwrap();

    let head = repo.head().unwrap().unwrap();
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
                orna_repository_v1::ManagedFileChange::new(manifest_path, Some(reordered_manifest)),
                orna_repository_v1::ManagedFileChange::new(shard_path, Some(reordered_shard)),
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
        matches!(
            error,
            orna_repository_v1::RepositoryError::InvalidCompactManifest
        ),
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
fn compact_publication_refuses_missing_or_corrupt_referenced_objects_before_ref_advance() {
    for corrupt in [false, true] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::new_v4();
        let segment = compact_segment(table, 1, b"compact object\n".to_vec());
        let plan = compact_plan(
            &repo,
            table,
            if corrupt { [43; 16] } else { [42; 16] },
            &[segment],
        );
        let head = repo.head().unwrap().unwrap();
        let object = plan.manifest().entries()[0].git_object_id();
        let object_path = root
            .path()
            .join(".git/objects")
            .join(&object[..2])
            .join(&object[2..]);
        assert!(object_path.is_file());
        #[cfg(unix)]
        fs::set_permissions(
            &object_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        if corrupt {
            fs::write(&object_path, b"corrupt").unwrap();
        } else {
            fs::remove_file(&object_path).unwrap();
        }
        assert!(matches!(
            publish_compact_repository_boundary(&repo, plan),
            Err(orna_repository_v1::RepositoryError::GitOperationFailed)
        ));
        assert_eq!(repo.head().unwrap(), Some(head));
        assert_eq!(repo.read_publication_journal().unwrap(), None);
    }
}

#[test]
fn compact_manifest_journal_recovery_proves_candidate_before_reconciling() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged ordinary\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();

    let plan = compact_plan(
        &repo,
        table,
        [44; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    publish_compact_repository_boundary(&repo, plan).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
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
    let persisted = repo.read_publication_journal().unwrap().unwrap();
    assert_eq!(persisted.compact_manifest().unwrap().object_count(), 1);
    assert_eq!(persisted.compact_manifest().unwrap().generation(), 1);
}

#[test]
fn compact_post_ref_recovery_preserves_unrelated_partial_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(
        root.path().join("ordinary.txt"),
        "staged ordinary\nunstaged ordinary\n",
    )
    .unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let staged_before = git(root.path(), &["diff", "--cached", "--binary"]);
    let unstaged_before = git(root.path(), &["diff", "--binary"]);

    let plan = compact_plan(
        &repo,
        table,
        [53; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let head = repo.head().unwrap().unwrap();
    let candidate = plan.candidate_commit().clone();
    repo.persist_compact_publication(&plan).unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/main",
            candidate.as_str(),
            head.as_str(),
        ],
    );

    let recovery = repo
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovery,
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(ref pending)
            if pending.commit() == &candidate
    ));
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--binary"]),
        staged_before
    );
    assert_eq!(git(root.path(), &["diff", "--binary"]), unstaged_before);
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
    assert!(
        repo.read_compact_manifest(&candidate, table)
            .unwrap()
            .is_some()
    );
    assert!(repo.read_publication_journal().unwrap().is_some());
}

#[test]
fn compact_prepared_publication_stale_head_retains_journal_and_requires_reconciliation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let intent = [55; 16];
    let plan = compact_plan(
        &repo,
        table,
        intent,
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let candidate = plan.candidate_commit().clone();
    let index_at_preparation = repo.index_generation().unwrap();

    repo.persist_compact_publication(&plan).unwrap();
    let prepared = repo.read_publication_journal().unwrap().unwrap();
    let watermark = prepared.compact_manifest().unwrap().cleanup_watermark();
    assert_eq!(prepared.runtime_intent_id(), Some(intent));
    assert_eq!(
        prepared.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );

    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "human branch advance"],
    );
    let current = repo.head().unwrap().unwrap();
    let index_after_human_commit = repo.index_generation().unwrap();
    assert_ne!(current, *index_at_preparation.head().unwrap());
    assert_eq!(index_after_human_commit.tree(), index_at_preparation.tree());

    assert!(matches!(
        repo.publish_compact_repository_boundary(plan),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(repo.head().unwrap(), Some(current.clone()));
    assert_eq!(
        repo.read_publication_journal().unwrap(),
        Some(prepared.clone())
    );

    let recovery = repo
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap();
    match recovery {
        orna_repository_v1::CompactPublicationRecovery::ReconciliationRequired(value) => {
            assert_eq!(value.candidate(), &candidate);
            assert_eq!(value.current_head(), &current);
            assert_eq!(value.runtime_intent_id(), intent);
            assert_eq!(value.cleanup_watermark(), watermark);
            assert_eq!(
                value.stage(),
                orna_repository_v1::PublicationJournalStage::Prepared
            );
        }
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(_) => {
            panic!("stale compact candidate must require explicit reconciliation")
        }
    }
    assert_eq!(repo.head().unwrap(), Some(current));
    assert_eq!(repo.read_publication_journal().unwrap(), Some(prepared));
}

#[test]
fn compact_recovery_rejects_an_unknown_profile_without_clearing_the_receipt() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let plan = compact_plan(
        &repo,
        table,
        [54; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let candidate = plan.candidate_commit().clone();
    publish_compact_repository_boundary(&repo, plan).unwrap();

    let manifest_path = ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
    let manifest = git_bytes(
        root.path(),
        &[
            "show",
            &format!("{}:{}", candidate, manifest_path.as_path().display()),
        ],
    );
    let unsupported = String::from_utf8(manifest)
        .unwrap()
        .replacen("compact-storage-v1", "unsupported-profile-v9", 1)
        .into_bytes();
    let malformed = repo
        .build_private_commit(
            &candidate,
            &[orna_repository_v1::ManagedFileChange::new(
                manifest_path,
                Some(unsupported),
            )],
            "test: commit unsupported compact profile",
        )
        .unwrap();
    repo.advance_current_ref(&candidate, &malformed).unwrap();

    assert!(matches!(
        repo.recover_compact_publication_boundary(),
        Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
    ));
    assert_eq!(repo.head().unwrap(), Some(malformed.commit().clone()));
    assert!(repo.read_publication_journal().unwrap().is_some());
}

#[test]
fn compact_manifest_journal_keeps_the_runtime_prefix_at_pre_ref_and_unproven_post_ref_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let plan = compact_plan(
        &repo,
        table,
        [48; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let head = repo.head().unwrap().unwrap();
    let candidate = plan.candidate_commit().clone();
    repo.persist_compact_publication(&plan).unwrap();
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(repo.head().unwrap(), Some(head.clone()));
    assert!(repo.read_publication_journal().unwrap().is_some());

    publish_compact_repository_boundary(&repo, plan).unwrap();
    let manifest_path = format!("{}:.orna/storage/{table}/manifest.orna", candidate);
    let manifest_object = git(root.path(), &["rev-parse", &manifest_path]);
    let object_path = root
        .path()
        .join(".git/objects")
        .join(&manifest_object[..2])
        .join(&manifest_object[2..]);
    #[cfg(unix)]
    fs::set_permissions(
        &object_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    fs::remove_file(&object_path).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert_eq!(repo.head().unwrap(), Some(candidate.clone()));
    assert!(repo.read_publication_journal().unwrap().is_some());
}

#[test]
fn compact_publication_rebuilds_from_the_current_manifest_after_a_stale_head() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let table = Uuid::new_v4();
    let stale_segment = compact_segment(table, 1, b"stale candidate\n".to_vec());

    let winner = compact_plan(
        &repo,
        table,
        [46; 16],
        &[compact_segment(table, 2, b"winner\n".to_vec())],
    );
    let pending = repo.publish_compact_repository_boundary(winner).unwrap();
    let receipt = compact_runtime_receipt(&pending, &signing_key);
    repo.finish_compact_with_receipt(&receipt).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);

    let rebuilt = repo
        .rebuild_compact_publication_with_watermark(
            table,
            compact_schema_fingerprint(table),
            [45; 16],
            [45; 32],
            &[stale_segment],
            "orna: publish compact runtime data",
        )
        .unwrap();
    let generation = rebuilt.manifest().next_generation();
    publish_compact_repository_boundary(&repo, rebuilt).unwrap();
    assert_eq!(generation, 3);
    assert!(
        repo.read_compact_manifest(&repo.head().unwrap().unwrap(), table)
            .unwrap()
            .unwrap()
            .entries()
            .iter()
            .any(|entry| entry.generation() == 2)
    );
}

#[test]
fn ordinary_commit_preserves_verified_compact_manifest_and_segment_identities() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let table = Uuid::new_v4();
    let plan = compact_plan(
        &repo,
        table,
        [67; 16],
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let pending = repo.publish_compact_repository_boundary(plan).unwrap();
    let receipt = compact_runtime_receipt(&pending, &signing_key);
    repo.finish_compact_with_receipt(&receipt).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);

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

#[test]
fn compact_publication_refuses_missing_manifest_or_shard_witness_before_ref_advance() {
    for (ordinal, witness_path) in [(70, "manifest.orna"), (71, "shards/00000000.orna")] {
        let root = repository();
        let repo = Repository::discover(root.path()).unwrap();
        let table = Uuid::new_v4();
        let plan = compact_plan(
            &repo,
            table,
            [ordinal as u8; 16],
            &[compact_segment(
                table,
                ordinal,
                b"compact object\n".to_vec(),
            )],
        );
        let head = repo.head().unwrap().unwrap();
        let object = git(
            root.path(),
            &[
                "rev-parse",
                &format!(
                    "{}:.orna/storage/{table}/{witness_path}",
                    plan.candidate_commit()
                ),
            ],
        );
        let object_path = root
            .path()
            .join(".git/objects")
            .join(&object[..2])
            .join(&object[2..]);
        #[cfg(unix)]
        fs::set_permissions(
            &object_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        fs::remove_file(&object_path).unwrap();

        let error = publish_compact_repository_boundary(&repo, plan).unwrap_err();
        assert!(
            matches!(
                error,
                orna_repository_v1::RepositoryError::InvalidPublicationJournal
                    | orna_repository_v1::RepositoryError::GitOperationFailed
            ),
            "unexpected missing-witness rejection: {error:?}"
        );
        assert_eq!(repo.head().unwrap(), Some(head));
        assert_eq!(repo.read_publication_journal().unwrap(), None);
    }
}

#[test]
fn compact_runtime_fence_rejects_a_runtime_failure_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let table = Uuid::new_v4();
    let intent = [49; 16];
    let plan = compact_plan(
        &repo,
        table,
        intent,
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let candidate = plan.candidate_commit().clone();
    let pending = repo.publish_compact_repository_boundary(plan).unwrap();
    assert_eq!(pending.runtime_intent_id(), intent);
    assert_eq!(pending.cleanup_watermark(), [49; 32]);
    let recovered = repo
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap();
    let recovered_again = repo
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap();
    assert_eq!(recovered, recovered_again);
    assert!(matches!(
        recovered,
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(ref value)
            if value == &pending
    ));
    assert!(repo.read_publication_journal().unwrap().is_some());
    assert_eq!(repo.head().unwrap(), Some(candidate));
}

#[test]
fn compact_runtime_fence_rejects_signed_receipt_with_mismatched_frozen_proof() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let table = Uuid::new_v4();
    let intent = [50; 16];
    let plan = compact_plan(
        &repo,
        table,
        intent,
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let pending = repo.publish_compact_repository_boundary(plan).unwrap();
    let mismatched_watermark = [51; 32];
    let signing_bytes = CompactRuntimeReceipt::signing_bytes(
        pending.runtime_intent_id(),
        mismatched_watermark,
        pending.commit(),
        pending.journal_verifier(),
    )
    .unwrap();
    let receipt = CompactRuntimeReceipt::new(
        pending.runtime_intent_id(),
        mismatched_watermark,
        pending.commit().clone(),
        pending.journal_verifier(),
        signing_key.sign(&signing_bytes).to_bytes(),
    )
    .unwrap();

    assert!(matches!(
        repo.finish_compact_with_receipt(&receipt),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert!(matches!(
        repo.recover_compact_publication_boundary().unwrap(),
        Some(orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(ref value))
            if value == &pending
    ));
    assert!(repo.read_publication_journal().unwrap().is_some());
    assert_eq!(repo.head().unwrap(), Some(pending.commit().clone()));
}

#[test]
fn compact_runtime_fence_rejects_ref_drift_before_cleanup() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let signing_key = compact_receipt_signing_key();
    provision_compact_receipt_trust_root(&repo, &signing_key);
    let table = Uuid::new_v4();
    let intent = [51; 16];
    let plan = compact_plan(
        &repo,
        table,
        intent,
        &[compact_segment(table, 1, b"compact object\n".to_vec())],
    );
    let pending = repo.publish_compact_repository_boundary(plan).unwrap();
    let receipt = compact_runtime_receipt(&pending, &signing_key);
    fs::write(root.path().join("ordinary.txt"), "native writer\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "native writer"]);
    let result = repo.finish_compact_with_receipt(&receipt);
    assert!(matches!(
        result,
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert!(repo.read_publication_journal().unwrap().is_some());
    let reconciliation = repo
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap();
    match reconciliation {
        orna_repository_v1::CompactPublicationRecovery::ReconciliationRequired(value) => {
            assert_eq!(value.candidate(), pending.commit());
            assert_ne!(value.current_head(), pending.commit());
            assert_eq!(value.runtime_intent_id(), intent);
            assert_eq!(value.cleanup_watermark(), [51; 32]);
        }
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(_) => {
            panic!("ref drift must require explicit reconciliation")
        }
    }
}
