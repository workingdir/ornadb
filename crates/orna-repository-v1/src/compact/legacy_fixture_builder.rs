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
//! git release. The bundle test pins its SHA-256 and rebuilds the repository
//! from this builder, asserting that the rebuilt bundle equals the committed
//! one byte for byte, so a builder change without a regenerated bundle fails.
//!
//! Two entry points:
//!
//! - `write_legacy_format1_fixture` writes the shell: marker, identity, source
//!   record, the compact tables and the gitlink.
//! - `write_legacy_format1_golden_fixture` writes that shell plus the
//!   editable-placement table, reading the declaring module and its row units
//!   from a directory the caller names. A test fixture may not be included
//!   across crates (`crates/orna-syntax-v1/tests/fixture_audit.rs`), so the
//!   content lives once, in the consuming crate's fixture tree, and this
//!   builder copies it from that path instead of embedding a second copy.
//!
//! Members (Gate F, `implementation/ACCEPTANCE.md:61-68`):
//!
//! - the format marker `.orna/format.orna`, exactly
//!   `{repository_format: 1, storage_profile: "compact-storage-v1"}`; 1.0.0
//!   `19-repository.md` defines it as a legacy-format input while
//!   ORNA-META-002 keeps `database.orna` for the new format, so both records
//!   are present and 1.0.0 `40-upgrade-contract.md` ORNA-UPGRADE-001 still
//!   dispatches the reader by the recorded format;
//! - the database identity record `.orna/database.orna`;
//! - a source record `main.orna`;
//! - key-only compact tables for `Int` (INT64), `Str` (BYTE_ARRAY STRING,
//!   `utf8`) and `Date` (INT32 DATE), each with a committed placement record,
//!   manifest, shard, Parquet segment and page-checksum profile
//!   (1.0.0 `23-storage.md` lines 88-106, 158-218);
//! - one editable-placement table whose rows are loose row units under the
//!   table namespace (1.0.0 `23-storage.md:13` ORNA-STORAGE-003 and :104, and
//!   `07-tables.md` lines 295-397 key-path-v1 plus ORNA-ROW-001..005). The key
//!   components stay inside the readable `key-path-v1` set, so the same path
//!   text is canonical for both the 1.0.0 and the current VFS spelling. Gate F
//!   needs committed loose rows because the legacy read path (query, history,
//!   mount, export) must decode both representations of one format-1 commit;
//! - a committed gitlink `vendor/stdlib` (a dependency pin, not a submodule
//!   clone: ORNA-GIT-003 needs the gitlink record, not the checked-out work).
//!
//! Deliberately absent: `.orna/store`, so a legacy marker and a format-3 store
//! never appear in one snapshot (the relabel guard in `format_context.rs` must
//! report that pairing as `MixedFormats`); and any Blob-bearing row, because
//! the format-1 compact decoder in `compact.rs` admits only Int/Bool/Str/Date
//! (`PhysicalValueKind`) although 1.0.0 `23-storage.md:184` defines
//! `Blob | blob | Unannotated BYTE_ARRAY`, so no legacy Blob value can be
//! decoded yet and ORNA-UPGRADE-004's annotation default has no fixture value.
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
const FORMAT_1: &str = "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n";
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
const PLACEMENT_COMPACT: &str = "compact";
const PLACEMENT_EDITABLE: &str = "editable";
/// The editable-placement table added after the compact tables.
const LOOSE_TABLE_NUMBER: u64 = 4;
/// Table namespace and name, which the loose row path repeats
/// (`contacts/Contact/<key component>.orna`, 1.0.0 `07-tables.md:374-380`).
const LOOSE_NAMESPACE: &str = "contacts";
const LOOSE_TABLE_NAME: &str = "Contact";
/// File names the caller's loose-source directory must supply. The content
/// lives once, in the consuming crate's fixture tree (a test fixture may not be
/// included across crates); only the names are fixture shape.
const LOOSE_MODULE_FILE: &str = "legacy-loose-contacts-main.orna";
/// One editable row: the encoded `Str` key component and the row-unit file.
/// Row units hold non-key fields only (ORNA-ROW-001); the key is reconstructed
/// from the path (ORNA-ROW-002).
const LOOSE_ROWS: [(&str, &str); 2] = [
    ("alice-smith", "legacy-loose-contact-alice-smith.orna"),
    ("bob-jones", "legacy-loose-contact-bob-jones.orna"),
];

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
            Self::Str(values) => values.iter().map(|value| Key::Str(value)).collect(),
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
            KeyValue::new("orna.profile".to_owned(), Some(PROFILE.to_owned())),
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

/// Writes the legacy format-1 shell into `root`: the format marker, the database
/// identity, the source record, the key-only compact tables and the gitlink.
///
/// Callers that need the golden repository, including the committed editable
/// rows, call [`write_legacy_format1_golden_fixture`] instead.
///
/// `root` must be an existing empty directory. The function panics on any
/// failure: it is a fixture builder, not a repository operation.
pub fn write_legacy_format1_fixture(root: &Path) {
    write_repository(root, None);
}

/// Writes the complete legacy format-1 golden repository into `root`.
///
/// `loose_sources` is the directory holding the editable table's declaring
/// module and row units (`LOOSE_MODULE_FILE` and every `LOOSE_ROWS` file). A
/// test fixture may not be included across crates, so the content stays in the
/// consuming crate's fixture tree and this builder copies it from the path the
/// caller names; there is one copy, not one per crate.
///
/// `root` must be an existing empty directory. The function panics on any
/// failure: it is a fixture builder, not a repository operation.
pub fn write_legacy_format1_golden_fixture(root: &Path, loose_sources: &Path) {
    write_repository(root, Some(loose_sources));
}

fn write_repository(root: &Path, loose_sources: Option<&Path>) {
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

    if let Some(sources) = loose_sources {
        publish_loose_table(root, table_id(LOOSE_TABLE_NUMBER), sources);
    }

    record_gitlink(root);
}

/// Records the gitlink pin as the last commit. The pinned commit is the current
/// `HEAD`, so the pin is a real commit object in this repository.
fn record_gitlink(root: &Path) {
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
    fs::write(
        table_dir.join("policy.orna"),
        policy_record(table, PLACEMENT_COMPACT),
    )
    .unwrap();

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
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "orna: publish compact runtime data",
        ],
    );
}

/// Writes one editable-placement table as one commit: the declaring source
/// module, the committed placement record and one loose row unit per key.
///
/// The table root holds only `policy.orna`, which the compact inventory reads
/// as "no compact state" (`compact.rs` `read_compact_manifest_inventory`), so
/// the same commit carries both a table root and loose rows (1.0.0
/// `23-storage.md:104`, "A table may also have editable rows under its
/// ordinary table directory"). No compact key may collide with these keys
/// (ORNA-STORAGE-013); the compact tables use their own table IDs.
fn publish_loose_table(root: &Path, table: Uuid, sources: &Path) {
    let table_dir = root.join(format!(".orna/storage/{table}"));
    fs::create_dir_all(&table_dir).unwrap();
    fs::write(
        table_dir.join("policy.orna"),
        policy_record(table, PLACEMENT_EDITABLE),
    )
    .unwrap();

    let namespace = root.join(LOOSE_NAMESPACE);
    fs::create_dir_all(&namespace).unwrap();
    fs::write(
        namespace.join("main.orna"),
        read_loose_source(sources, LOOSE_MODULE_FILE),
    )
    .unwrap();

    let rows = namespace.join(LOOSE_TABLE_NAME);
    fs::create_dir_all(&rows).unwrap();
    for (key, file) in LOOSE_ROWS {
        fs::write(
            rows.join(format!("{key}.orna")),
            read_loose_source(sources, file),
        )
        .unwrap();
    }

    git(root, &["add", "--all"]);
    git(
        root,
        &["commit", "--quiet", "-m", "orna: publish editable rows"],
    );
}

/// Reads one loose source file from the caller's fixture directory. The path is
/// named in the panic so a caller that points at the wrong directory sees which
/// file the golden layout needs.
fn read_loose_source(sources: &Path, file: &str) -> Vec<u8> {
    let path = sources.join(file);
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "loose fixture source {} is unreadable: {error}",
            path.display()
        )
    })
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

/// One table's committed placement record.
///
/// ORNA-STORAGE-007 (`23-storage.md:69`) requires every table to commit one of
/// `automatic`, `editable` or `compact`; the layout tree at `23-storage.md:93-100`
/// names `policy.orna` but the record shape is spec-silent there, so this
/// minimal record is a fixture choice. The reader ignores the file's contents
/// and treats a root holding only `policy.orna` as a table with no compact
/// state (`compact.rs` `read_compact_manifest_inventory`).
fn policy_record(table: Uuid, placement: &str) -> String {
    format!("{{placement: \"{placement}\", table: \"{table}\"}}\n")
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
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn key_order_is_value_order_for_negative_dates_and_ints() {
        assert_eq!(Key::Int(-3).cmp(&Key::Int(2)), Ordering::Less);
        assert_eq!(Key::Date(-1).cmp(&Key::Date(0)), Ordering::Less);
    }
}
