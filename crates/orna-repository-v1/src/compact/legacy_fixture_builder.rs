//! Builds the committed legacy format-1 compact golden repository.
//!
//! Compiled only under the `legacy-fixture-writer` feature; production builds
//! never contain it (ORNA-UPGRADE-003).
//!
//! Independence: this builder does NOT call `Repository::prepare_compact_publication`,
//! `CompactManifest`, `CompactSegment` or `canonical_key_extrema`, and it does not
//! reset `HEAD` to a candidate commit. It writes the 1.0.0 layout
//! (`.orna/storage/T/{policy.orna,manifest.orna,shards/,data/xx/seg.parquet}`,
//! 1.0.0 `source/23-storage.md` lines 88-106) directly with plain files and
//! `git add`/`git commit`, computing segment checksums, git blob IDs and key
//! bounds itself from the typed row values.
//!
//! Shared with production and therefore NOT independent: the canonical OVB
//! encoder (`orna_foundation_v1::CanonicalValue`), the base64 helper, the
//! date-key mapping (`compact_date_value`) and the Parquet page-checksum helper
//! (`legacy_fixture_writer::with_page_checksums`). The record text shapes follow
//! the record grammar the production reader admits, because that grammar is
//! what the reader must parse.
//!
//! Fixed commit dates make the committed bundle bytes reproducible for a given
//! git release; the bundle test pins its SHA-256.
//!
//! The fixture covers key-only compact tables for `Int` (INT64), `Str`
//! (BYTE_ARRAY STRING, `utf8`) and `Date` (INT32 DATE), each with a committed
//! Parquet page-checksum profile, plus a committed gitlink.
//!
//! Known remaining gap (not fixed in this slice): the manifest `min_key`/`max_key`
//! and shard entry bounds are written as hex text of the canonical key bytes, exactly
//! as the production codec writes and parses them. 1.0.0 `23-storage.md:214,216`
//! says key values use the table key schema (typed values), so this fixture does
//! not match the spec on those fields.

use std::{fs, path::Path, process::Command, sync::Arc};

use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use parquet::{
    basic::{Compression, Encoding},
    data_type::{ByteArray, ByteArrayType, Int32Type, Int64Type},
    file::{
        metadata::KeyValue,
        properties::{WriterProperties, WriterVersion},
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{compact_date_value, legacy_fixture_writer::with_page_checksums};

const TABLE_NAMESPACE: u64 = 0x018f_0000_0000_7100;
const FIELD_NAMESPACE: u64 = 0x018f_0000_0000_7200;
const SEGMENT_NAMESPACE: u64 = 0x018f_0000_0000_7000;
const FORMAT_1: &str =
    "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n";
const GITLINK_PATH: &str = "vendor/stdlib";
const AUTHOR_NAME: &str = "orna-fixture";
const AUTHOR_EMAIL: &str = "orna-fixture@example.invalid";
/// Fixed author and committer time (`1767225600` = 2026-01-01T00:00:00Z).
const FIXED_TIME: &str = "1767225600 +0000";
const PROFILE: &str = "compact-storage-v1";
const ENCODER: &str = "test-encoder-v1";
const GENERATION: u64 = 1;
const NEXT_GENERATION: u64 = 2;
const SHARD_NUMBER: u64 = 0;
const SHARD_FILE: &str = "shards/00000000.orna";

/// One physical segment of a fixture table. A table holds one variant only.
enum Column {
    Int(Vec<i64>),
    Date(Vec<i32>),
    Str(Vec<&'static str>),
}

/// Typed primary key of one row. Ordering is the value ordering of the key
/// type; all rows of one table share one variant.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Key {
    Int(i64),
    Date(i32),
    Str(&'static str),
}

impl Key {
    /// Canonical logical key bytes, as the compact reader stores bounds.
    fn canonical(&self) -> Vec<u8> {
        let raw = match self {
            Self::Int(value) => OvbRaw::Int((*value).into()),
            Self::Date(days) => compact_date_value(*days).expect("date key"),
            Self::Str(value) => OvbRaw::Text((*value).to_owned()),
        };
        CanonicalValue::new(raw)
            .expect("canonical key")
            .encode()
            .expect("canonical key encoding")
    }
}

impl Column {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "Int",
            Self::Date(_) => "Date",
            Self::Str(_) => "Str",
        }
    }

    fn encoding(&self) -> &'static str {
        match self {
            Self::Int(_) => "int64",
            Self::Date(_) => "date",
            Self::Str(_) => "utf8",
        }
    }

    /// Physical Parquet type and logical annotation of the key column.
    fn physical(&self) -> (&'static str, &'static str) {
        match self {
            Self::Int(_) => ("INT64", ""),
            Self::Date(_) => ("INT32", " (DATE)"),
            Self::Str(_) => ("BYTE_ARRAY", " (STRING)"),
        }
    }

    fn keys(&self) -> Vec<Key> {
        match self {
            Self::Int(values) => values.iter().map(|value| Key::Int(*value)).collect(),
            Self::Date(values) => values.iter().map(|value| Key::Date(*value)).collect(),
            Self::Str(values) => values.iter().map(|value| Key::Str(*value)).collect(),
        }
    }

    fn parquet(
        &self,
        table: Uuid,
        schema: &SchemaDescriptor,
        field: Uuid,
        columns: &[u8],
    ) -> Vec<u8> {
        let (physical, annotation) = self.physical();
        let message = format!(
            "message schema {{ REQUIRED {physical} f_{}{annotation}; }}",
            field.simple()
        );
        let schema_descriptor =
            Arc::new(parse_message_type(&message).expect("fixture message type"));
        let metadata = vec![
            KeyValue::new(
                "orna.profile".to_owned(),
                Some(PROFILE.to_owned()),
            ),
            KeyValue::new("orna.table".to_owned(), Some(table.to_string())),
            KeyValue::new(
                "orna.schema.sha256".to_owned(),
                Some(hex_digest(&schema_fingerprint(schema))),
            ),
            KeyValue::new(
                "orna.schema.ovb".to_owned(),
                Some(orna_value_v1::base64_encode(
                    &schema.encode().expect("schema encoding"),
                )),
            ),
            KeyValue::new(
                "orna.columns.ovb".to_owned(),
                Some(orna_value_v1::base64_encode(columns)),
            ),
            KeyValue::new("orna.encoder".to_owned(), Some(ENCODER.to_owned())),
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
        let mut writer =
            SerializedFileWriter::new(&mut bytes, schema_descriptor, properties).unwrap();
        let mut row_group = writer.next_row_group().unwrap();
        let mut column = row_group.next_column().unwrap().unwrap();
        match self {
            Self::Int(values) => {
                column
                    .typed::<Int64Type>()
                    .write_batch(values.as_slice(), None, None)
                    .unwrap();
            }
            Self::Date(values) => {
                column
                    .typed::<Int32Type>()
                    .write_batch(values.as_slice(), None, None)
                    .unwrap();
            }
            Self::Str(values) => {
                let values = values
                    .iter()
                    .map(|value| ByteArray::from(value.as_bytes()))
                    .collect::<Vec<_>>();
                column
                    .typed::<ByteArrayType>()
                    .write_batch(&values, None, None)
                    .unwrap();
            }
        }
        column.close().unwrap();
        row_group.close().unwrap();
        writer.close().unwrap();

        // parquet-rs writes FileMetaData.version from its writer setting; the
        // Orna profile requires V2 data pages while keeping file version 1.
        let footer_length =
            u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap())
                as usize;
        let footer = bytes.len() - 8 - footer_length;
        assert_eq!(&bytes[footer..footer + 2], &[0x15, 0x04]);
        bytes[footer + 1] = 0x02;
        with_page_checksums(bytes)
    }
}

/// Writes the complete legacy format-1 golden repository into `root`.
///
/// `root` must be an existing empty directory. The function panics on any
/// failure: it is a fixture builder, not a repository operation.
pub fn write_legacy_format1_fixture(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", AUTHOR_NAME]);
    git(root, &["config", "user.email", AUTHOR_EMAIL]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("main.orna"),
        include_str!("../../tests/fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(root.join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(root.join(".orna/format.orna"), FORMAT_1).unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("../../tests/fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);

    for (ordinal, segments) in fixture_tables().iter().enumerate() {
        let number = u64::try_from(ordinal + 1).expect("table number");
        publish_table(root, table_id(number), field_id(number), number, segments);
    }

    let head = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},{GITLINK_PATH}"),
        ],
    );
    git(root, &["commit", "-m", "orna: record stdlib gitlink"]);
}

/// Three tables: two `Int` segments, one `Str` segment, one `Date` segment.
fn fixture_tables() -> Vec<Vec<Column>> {
    vec![
        vec![Column::Int(vec![1, 2]), Column::Int(vec![3, 4])],
        vec![Column::Str(vec!["alpha", "beta", "gamma"])],
        vec![Column::Date(vec![-1, 0, 1])],
    ]
}

/// Writes one table's committed 1.0.0 compact layout as one commit.
fn publish_table(root: &Path, table: Uuid, field: Uuid, number: u64, segments: &[Column]) {
    let type_name = segments[0].type_name();
    let schema = table_schema(table, field, type_name);
    let schema_digest = schema_fingerprint(&schema);
    let table_root = format!(".orna/storage/{table}");
    let table_dir = root.join(&table_root);
    fs::create_dir_all(table_dir.join("shards")).unwrap();
    fs::write(table_dir.join("policy.orna"), policy_record(table)).unwrap();

    let mut entries = Vec::new();
    let mut all_keys = Vec::new();
    for (index, column) in segments.iter().enumerate() {
        let ordinal = (number << 16) | u64::try_from(index + 1).expect("segment number");
        let segment_id = Uuid::from_u64_pair(SEGMENT_NAMESPACE, ordinal | 0x8000_0000_0000_0000);
        let relative = format!(
            "{table_root}/data/{}/{segment_id}.parquet",
            &segment_id.to_string()[..2]
        );
        let file = root.join(&relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let columns = columns_for(field, type_name, column.encoding());
        let bytes = column.parquet(table, &schema, field, &columns);
        fs::write(&file, &bytes).unwrap();
        let keys = column.keys();
        entries.push(EntryRecord {
            segment_id,
            relative_path: relative.clone(),
            git_object_id: git(root, &["hash-object", "-w", &relative]),
            sha256: Sha256::digest(&bytes).into(),
            min_key: keys.iter().min().expect("non-empty segment").canonical(),
            max_key: keys.iter().max().expect("non-empty segment").canonical(),
            row_count: keys.len(),
            compressed_bytes: bytes.len(),
            columns,
        });
        all_keys.extend(keys);
    }

    let shard = shard_record(&schema_digest, &entries).into_bytes();
    fs::write(table_dir.join(SHARD_FILE), &shard).unwrap();
    let manifest = manifest_record(
        table,
        &schema_digest,
        &all_keys.iter().min().expect("keys").canonical(),
        &all_keys.iter().max().expect("keys").canonical(),
        entries.len(),
        &Sha256::digest(&shard).into(),
    );
    fs::write(table_dir.join("manifest.orna"), manifest).unwrap();

    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "orna: publish compact runtime data"]);
}

/// One shard entry, before it is rendered into `shards/00000000.orna`.
struct EntryRecord {
    segment_id: Uuid,
    relative_path: String,
    git_object_id: String,
    sha256: [u8; 32],
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    row_count: usize,
    compressed_bytes: usize,
    columns: Vec<u8>,
}

/// `policy.orna` content is spec-silent in 1.0.0 (`23-storage.md` names the
/// file in the layout tree only); this minimal record is a fixture choice.
fn policy_record(table: Uuid) -> String {
    format!("{{profile: \"{PROFILE}\", table: \"{table}\"}}\n")
}

fn shard_record(schema: &[u8; 32], entries: &[EntryRecord]) -> String {
    let mut output = String::from("{entries: [");
    for (index, entry) in entries.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str(&format!(
            "{{segment_id: \"{}\", role: \"data\", generation: {GENERATION}, schema_id: \"{}\", profile_version: 1, encoder_version: \"{ENCODER}\", relative_path: \"{}\", git_object_id: \"{}\", sha256: \"{}\", min_key: \"{}\", max_key: \"{}\", min_event_time: null, max_event_time: null, row_count: {}, compressed_bytes: {}, columns: \"{}\", row_group_index: true, bloom: false}}",
            entry.segment_id,
            hex_digest(schema),
            entry.relative_path,
            entry.git_object_id,
            hex_digest(&entry.sha256),
            hex_digest(&entry.min_key),
            hex_digest(&entry.max_key),
            entry.row_count,
            entry.compressed_bytes,
            orna_value_v1::base64_encode(&entry.columns),
        ));
    }
    output.push_str("]}\n");
    output
}

fn manifest_record(
    table: Uuid,
    schema: &[u8; 32],
    min_key: &[u8],
    max_key: &[u8],
    entries: usize,
    shard_sha256: &[u8; 32],
) -> String {
    format!(
        "{{profile: \"{PROFILE}\", table: \"{table}\", schema: \"{}\", next_generation: {NEXT_GENERATION}, shards: [{{number: {SHARD_NUMBER}, min_key: \"{}\", max_key: \"{}\", entries: {entries}, file: \"{SHARD_FILE}\", hash: \"{}\"}}]}}\n",
        hex_digest(schema),
        hex_digest(min_key),
        hex_digest(max_key),
        hex_digest(shard_sha256),
    )
}

fn table_id(number: u64) -> Uuid {
    Uuid::from_u64_pair(TABLE_NAMESPACE, number)
}

fn field_id(number: u64) -> Uuid {
    Uuid::from_u64_pair(FIELD_NAMESPACE, number)
}

fn table_schema(table: Uuid, field: Uuid, type_name: &str) -> SchemaDescriptor {
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
    .expect("table schema")
}

fn columns_for(field: Uuid, type_name: &str, encoding: &str) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Array(vec![OvbRaw::Array(vec![
        OvbRaw::Array(vec![OvbRaw::Tag(
            37,
            Box::new(OvbRaw::Bytes(field.as_bytes().to_vec())),
        )]),
        OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", field.simple()))]),
        OvbRaw::Array(vec![
            OvbRaw::Int(0.into()),
            OvbRaw::Text(type_name.to_owned()),
        ]),
        OvbRaw::Text(encoding.to_owned()),
        OvbRaw::Array(Vec::new()),
    ])]))
    .expect("columns")
    .encode()
    .expect("columns encoding")
}

fn schema_fingerprint(schema: &SchemaDescriptor) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.schema.v1\0");
    digest.update(schema.encode().expect("schema encoding"));
    digest.finalize().into()
}

fn hex_digest(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs git with fixed author/committer times so bundle bytes are reproducible.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_AUTHOR_DATE", FIXED_TIME)
        .env("GIT_COMMITTER_DATE", FIXED_TIME)
        .args(arguments)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use super::*;

    #[test]
    fn key_order_is_value_order_for_negative_dates_and_ints() {
        assert_eq!(Key::Int(-3).cmp(&Key::Int(2)), Ordering::Less);
        assert_eq!(Key::Date(-1).cmp(&Key::Date(0)), Ordering::Less);
    }
}
