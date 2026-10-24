//! Builds the committed legacy format-1 compact golden repository.
//!
//! Compiled only under the `legacy-fixture-writer` feature; production builds
//! never contain it (ORNA-UPGRADE-003). Format-1 snapshots are produced solely
//! by this test-only writer, through the same
//! `Repository::prepare_compact_publication` path that the compact reader
//! admits, and then made `HEAD` the way an older build left them.
//!
//! The fixture covers key-only compact tables for `Int` (INT64), `Str`
//! (BYTE_ARRAY STRING, `utf8`) and `Date` (INT32 DATE), each with a committed
//! Parquet page-checksum profile, plus a committed gitlink.

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

use super::{
    CompactManifest, CompactSegment, CompactSegmentRole, canonical_key_extrema,
    compact_date_value, legacy_fixture_writer::with_page_checksums,
};
use crate::{ManagedPath, Repository};

const TABLE_NAMESPACE: u64 = 0x018f_0000_0000_7100;
const FIELD_NAMESPACE: u64 = 0x018f_0000_0000_7200;
const SEGMENT_NAMESPACE: u64 = 0x018f_0000_0000_7000;
const FORMAT_1: &str =
    "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n";
const GITLINK_PATH: &str = "vendor/stdlib";
const AUTHOR_NAME: &str = "orna-fixture";
const AUTHOR_EMAIL: &str = "orna-fixture@example.invalid";

/// One physical segment of a fixture table. A table holds one variant only.
enum Column {
    Int(Vec<i64>),
    Date(Vec<i32>),
    Str(Vec<&'static str>),
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

    fn row_count(&self) -> usize {
        match self {
            Self::Int(values) => values.len(),
            Self::Date(values) => values.len(),
            Self::Str(values) => values.len(),
        }
    }

    /// Canonical logical key of row `index`, as the compact reader compares it.
    fn key(&self, index: usize) -> Vec<u8> {
        let raw = match self {
            Self::Int(values) => OvbRaw::Int(values[index].into()),
            Self::Date(values) => compact_date_value(values[index]).expect("date key"),
            Self::Str(values) => OvbRaw::Text(values[index].to_owned()),
        };
        CanonicalValue::new(raw)
            .expect("canonical key")
            .encode()
            .expect("canonical key encoding")
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
                Some("compact-storage-v1".to_owned()),
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

    fn segment(
        &self,
        table: Uuid,
        field: Uuid,
        schema: &SchemaDescriptor,
        segment_id: Uuid,
    ) -> CompactSegment {
        let path = ManagedPath::new(format!(
            ".orna/storage/{table}/data/{}/{segment_id}.parquet",
            &segment_id.to_string()[..2]
        ))
        .expect("segment path");
        let columns = columns_for(field, self.type_name(), self.encoding());
        let keys = (0..self.row_count())
            .map(|index| self.key(index))
            .collect::<Vec<_>>();
        let (min_key, max_key) =
            canonical_key_extrema(keys.iter().map(Vec::as_slice)).expect("key extrema");
        let bytes = self.parquet(table, schema, field, &columns);
        CompactSegment::new(
            segment_id,
            CompactSegmentRole::Data,
            schema_fingerprint(schema),
            "test-encoder-v1",
            path,
            bytes,
            min_key.to_vec(),
            max_key.to_vec(),
            u64::try_from(self.row_count()).expect("row count"),
            columns,
            true,
            false,
        )
        .expect("compact segment")
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
        publish_table(
            root,
            table_id(number),
            field_id(number),
            number,
            segments,
        );
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

fn publish_table(root: &Path, table: Uuid, field: Uuid, number: u64, segments: &[Column]) {
    let type_name = segments[0].type_name();
    let schema = table_schema(table, field, type_name);
    let schema_digest = schema_fingerprint(&schema);
    let compact_segments = segments
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let ordinal = (number << 16) | u64::try_from(index + 1).expect("segment number");
            let segment_id =
                Uuid::from_u64_pair(SEGMENT_NAMESPACE, ordinal | 0x8000_0000_0000_0000);
            column.segment(table, field, &schema, segment_id)
        })
        .collect::<Vec<_>>();

    let repository = Repository::discover(root).expect("repository");
    let head = repository.head().expect("head").expect("committed head");
    let base = repository
        .read_compact_manifest(&head, table)
        .expect("manifest")
        .unwrap_or_else(|| CompactManifest::empty(table, schema_digest));
    let intent = *table.as_bytes();
    let plan = repository
        .prepare_compact_publication(
            &head,
            repository.index_generation().expect("index generation"),
            base,
            intent,
            [intent[0]; 32],
            &compact_segments,
            "orna: publish compact runtime data",
        )
        .expect("compact publication");
    // The fixture is a committed older-build publication: make the candidate
    // the committed HEAD exactly as an older build left it.
    git(
        root,
        &[
            "reset",
            "--quiet",
            "--hard",
            plan.candidate_commit().as_str(),
        ],
    );
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

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output").trim().to_owned()
}
