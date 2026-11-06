//! Versioned reader for format-1/2 (legacy) snapshots: one table's rows, and
//! one row's revisions.
//!
//! ORNA-UPGRADE-001 dispatches readers by the recorded repository format rather
//! than by the CLI's own version, ORNA-UPGRADE-002 keeps a format-1/2 commit
//! readable under its original decoders, and ORNA-ROW-013 requires format-1/2
//! histories to retain versioned readers. `orna query` and `orna history` read
//! a pin whose format context reports a read-only compatibility input through
//! this module; a format-3 pin keeps the native store path.
//!
//! A 1.0.0 table is stored in one of two ways, and one commit may hold both:
//!
//! - **compact**: `.orna/storage/<table>/` carries a manifest, shards and
//!   Parquet segments. The repository's own verified reader returns the
//!   authoritative rows (`Repository::read_legacy_authoritative_tables`,
//!   `compact.rs`).
//! - **editable**: the rows are loose row units at
//!   `<table path>/<key component>.orna` holding non-key fields only (1.0.0
//!   `23-storage.md:104`; `07-tables.md` key-path-v1; ORNA-ROW-001). The key is
//!   reconstructed from the path (ORNA-ROW-002).
//!
//! The two sources have different committed addresses, and this module keeps
//! them rather than inventing a shared one. A compact table is addressed by its
//! storage table id, the uuid that names its `.orna/storage` root. An editable
//! table has no storage table id in the snapshot: 1.0.0 recorded table object
//! ids in committed identity metadata, and a legacy snapshot carries no such
//! inventory (the 1.0.0 metadata records only the database identity), so it is
//! addressed by the key-path directory its row units live under — the table's
//! committed address in the 1.0.0 layout.
//!
//! Both sources are read from the committed tree alone. 1.0.0 discovered a
//! module by its path, not by an import graph, so a row unit's table need not be
//! reachable from the root module; the units are therefore enumerated from the
//! commit's own tree entries rather than from a loaded project.
//!
//! Nothing here evaluates source: a row unit is parsed as one record expression
//! and lowered as data ([`orna_semantic::lower_data_expression`]), and no
//! blob payload is fetched.

use orna_project::ProjectLimits;
use orna_repository::{
    CommittedRevisionSnapshot, CommittedTreeEntryKind, GitCommitRef, Repository, RepositoryError,
    TypedKey, Uuid,
};
use orna_semantic::lower_data_expression;
use orna_syntax::parse_row;
use orna_value::Raw;

/// The 1.0.0 record tag that wraps one stored row value as
/// `[null, [[field, value], ...]]` (the compact reader's row record).
const LEGACY_ROW_RECORD_TAG: u64 = 60009;
/// The tag a field name carries inside a legacy row record, holding the field's
/// 16-byte object id.
const LEGACY_FIELD_NAME_TAG: u64 = 37;
/// The suffix a loose row unit file carries (ORNA-NS-002 row units).
const ROW_UNIT_SUFFIX: &str = ".orna";
/// The directory 1.0.0 kept repository metadata and storage in, which never
/// holds a row unit's table directory.
const METADATA_DIRECTORY: &str = ".orna";

/// How one caller names a table in a legacy snapshot.
///
/// The two forms are the two committed addresses the format has (see the module
/// docs), not two spellings of one identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyTableAddress {
    /// A compact table's storage table id: the uuid naming its
    /// `.orna/storage` root.
    Relation([u8; 16]),
    /// An editable table's key-path directory, the parent of its loose row
    /// units (`contacts/Contact` for the 1.0.0 fixture).
    TablePath(String),
}

/// Which representation answered a legacy table read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyRowSource {
    /// Authoritative rows of a compact table, decoded by the repository's
    /// verified 1.0.0 segment reader.
    Compact,
    /// Committed loose row units under an editable table's key-path directory.
    LooseRowUnit,
}

/// The name of one stored field of a legacy row.
///
/// A compact row names its fields by 16-byte object id, because the compact
/// runtime record carries the schema coordinate rather than a name. A loose row
/// unit names its fields as the source declaration does, because the unit is a
/// record expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyFieldName {
    /// A compact row's field object id.
    ObjectId([u8; 16]),
    /// A loose row unit's declared field name.
    Declared(String),
}

/// One stored field of one legacy row, in the order the row stores it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyField {
    name: LegacyFieldName,
    value: Raw,
}

impl LegacyField {
    /// The field's name in the representation that answered.
    pub const fn name(&self) -> &LegacyFieldName {
        &self.name
    }

    /// The field's stored value, decoded as data.
    pub const fn value(&self) -> &Raw {
        &self.value
    }
}

/// One row of a legacy table: its key and its stored fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyRow {
    key: TypedKey,
    fields: Vec<LegacyField>,
}

impl LegacyRow {
    /// The row's primary key.
    pub const fn key(&self) -> &TypedKey {
        &self.key
    }

    /// The row's stored fields, in stored order. A loose row unit holds
    /// non-key fields only (ORNA-ROW-001), and a compact row's record holds the
    /// live projection the segment carried.
    pub fn fields(&self) -> &[LegacyField] {
        &self.fields
    }
}

/// One legacy table's rows, in key order, with the representation that answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyTableRows {
    source: LegacyRowSource,
    rows: Vec<LegacyRow>,
}

impl LegacyTableRows {
    /// Which representation answered this read.
    pub const fn source(&self) -> LegacyRowSource {
        self.source
    }

    /// The table's rows, in the language's logical key order (ORNA-ROW-002).
    pub fn rows(&self) -> &[LegacyRow] {
        &self.rows
    }
}

/// One revision of a legacy row: the commit, and whether its tree carries the
/// row's unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyRowRevision {
    snapshot: CommittedRevisionSnapshot,
    present: bool,
}

impl LegacyRowRevision {
    /// The commit this revision is.
    pub const fn snapshot(&self) -> &CommittedRevisionSnapshot {
        &self.snapshot
    }

    /// Whether the row's unit is committed at this revision.
    pub const fn present(&self) -> bool {
        self.present
    }
}

/// Why a legacy read could not answer.
#[derive(Debug)]
pub enum LegacyReadError {
    /// The repository could not read a committed object.
    Repository(RepositoryError),
    /// No compact table of the snapshot carries this storage table id. An
    /// editable table has no compact state; its rows are addressed by their
    /// key-path directory.
    UnknownRelation([u8; 16]),
    /// No committed loose row units live under this key-path directory.
    MissingTablePath(String),
    /// The table holds more row units than one read admits, under the
    /// project loader's own unit bound.
    RowUnitLimit(usize),
    /// One committed row unit is not valid UTF-8, is not a single record
    /// expression, or lowers to a value that is not a record.
    UndecodableRowUnit(String),
    /// One compact row's stored value is not the legacy row record shape this
    /// reader decodes.
    UndecodableCompactRow,
    /// A row's key path component is escaped (`~`) or a long alias (`~key-`),
    /// which needs the typed key decoder this reader does not carry.
    UnreadableKeyPath(String),
    /// Two rows of one table name the same key, so the table has no single
    /// authoritative row for it (UPGRADE-3 step 3). The read refuses rather
    /// than choosing one.
    DuplicateRowKey { table: String, key: String },
}

impl std::fmt::Display for LegacyReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Repository(error) => write!(formatter, "{error}"),
            Self::UnknownRelation(relation) => write!(
                formatter,
                "the snapshot holds no compact table {}; an editable table's rows are loose row units addressed by their key-path directory",
                Uuid::from_bytes(*relation).simple()
            ),
            Self::MissingTablePath(path) => {
                write!(formatter, "no committed row units end {path:?}/<key>.orna")
            }
            Self::RowUnitLimit(limit) => write!(
                formatter,
                "the table holds more loose row units than one read admits ({limit})"
            ),
            Self::UndecodableRowUnit(path) => {
                write!(
                    formatter,
                    "committed row unit {path:?} is not one record expression"
                )
            }
            Self::UndecodableCompactRow => {
                formatter.write_str("a compact row is not the legacy row record shape")
            }
            Self::UnreadableKeyPath(path) => write!(
                formatter,
                "row unit {path:?} addresses its key with an escaped or aliased path component"
            ),
            Self::DuplicateRowKey { table, key } => write!(
                formatter,
                "table {table} holds more than one authoritative row for key {key:?}"
            ),
        }
    }
}

impl std::error::Error for LegacyReadError {}

impl From<RepositoryError> for LegacyReadError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

/// Reads one legacy table's rows at one committed format-1/2 snapshot.
pub fn read_legacy_table_rows(
    repository: &Repository,
    commit: &GitCommitRef,
    address: &LegacyTableAddress,
) -> Result<LegacyTableRows, LegacyReadError> {
    match address {
        LegacyTableAddress::Relation(relation) => {
            read_compact_table_rows(repository, commit, *relation)
        }
        LegacyTableAddress::TablePath(path) => read_loose_table_rows(repository, commit, path),
    }
}

fn read_compact_table_rows(
    repository: &Repository,
    commit: &GitCommitRef,
    relation: [u8; 16],
) -> Result<LegacyTableRows, LegacyReadError> {
    let tables = repository.read_legacy_authoritative_tables(commit)?;
    let wanted = Uuid::from_bytes(relation);
    let Some(table) = tables.iter().find(|table| table.table() == wanted) else {
        return Err(LegacyReadError::UnknownRelation(relation));
    };
    let rows = table
        .rows()
        .iter()
        .map(|row| {
            Ok(LegacyRow {
                key: TypedKey::from_value(row.key())
                    .map_err(|_| LegacyReadError::UndecodableCompactRow)?,
                fields: legacy_compact_fields(row.value().raw())?,
            })
        })
        .collect::<Result<Vec<_>, LegacyReadError>>()?;
    let rows = one_row_per_key(wanted.simple().to_string(), rows)?;
    Ok(LegacyTableRows {
        source: LegacyRowSource::Compact,
        rows,
    })
}

/// The stored fields of one compact row: the 1.0.0 row record is
/// `[null, [[field object id, value], ...]]` (tag 60009, field name tag 37).
fn legacy_compact_fields(value: &Raw) -> Result<Vec<LegacyField>, LegacyReadError> {
    let Raw::Tag(LEGACY_ROW_RECORD_TAG, record) = value else {
        return Err(LegacyReadError::UndecodableCompactRow);
    };
    let Raw::Array(parts) = record.as_ref() else {
        return Err(LegacyReadError::UndecodableCompactRow);
    };
    let [Raw::Null, Raw::Array(fields)] = parts.as_slice() else {
        return Err(LegacyReadError::UndecodableCompactRow);
    };
    fields
        .iter()
        .map(|field| {
            let Raw::Array(field) = field else {
                return Err(LegacyReadError::UndecodableCompactRow);
            };
            let [Raw::Tag(LEGACY_FIELD_NAME_TAG, name), value] = field.as_slice() else {
                return Err(LegacyReadError::UndecodableCompactRow);
            };
            let Raw::Bytes(name) = name.as_ref() else {
                return Err(LegacyReadError::UndecodableCompactRow);
            };
            let name: [u8; 16] = name
                .as_slice()
                .try_into()
                .map_err(|_| LegacyReadError::UndecodableCompactRow)?;
            Ok(LegacyField {
                name: LegacyFieldName::ObjectId(name),
                value: value.clone(),
            })
        })
        .collect()
}

/// One committed loose row unit: its repository-relative path and the key-path
/// components after the table directory.
struct LooseRowUnit {
    path: String,
    key_path: Vec<String>,
}

/// Every loose row unit the commit tracks under `table_path`, in path order.
///
/// The units are the commit's own tree entries under that directory that end in
/// `.orna`; a directory under the metadata root is never one. The unit count is
/// bounded by the project loader's own module bound, so one read cannot walk an
/// unbounded directory.
fn loose_row_units(
    repository: &Repository,
    commit: &GitCommitRef,
    table_path: &str,
) -> Result<Vec<LooseRowUnit>, LegacyReadError> {
    let limit = ProjectLimits::default().max_source_modules;
    let prefix = format!("{table_path}/");
    let mut units = Vec::new();
    for entry in repository.list_committed_source_tree(commit)? {
        if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
            continue;
        }
        let Some(path) = entry.path().as_path().to_str() else {
            return Err(LegacyReadError::UndecodableRowUnit(
                entry.path().as_path().display().to_string(),
            ));
        };
        if path.starts_with(METADATA_DIRECTORY) {
            continue;
        }
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        if !rest.ends_with(ROW_UNIT_SUFFIX) || rest == ROW_UNIT_SUFFIX {
            continue;
        }
        units.push(LooseRowUnit {
            path: path.to_owned(),
            key_path: rest.split('/').map(str::to_owned).collect(),
        });
        if units.len() > limit {
            return Err(LegacyReadError::RowUnitLimit(limit));
        }
    }
    if units.is_empty() {
        return Err(LegacyReadError::MissingTablePath(table_path.to_owned()));
    }
    Ok(units)
}

fn read_loose_table_rows(
    repository: &Repository,
    commit: &GitCommitRef,
    table_path: &str,
) -> Result<LegacyTableRows, LegacyReadError> {
    let mut total_bytes = 0usize;
    let budget = ProjectLimits::default().max_source_bytes;
    let mut rows = Vec::new();
    for unit in loose_row_units(repository, commit, table_path)? {
        let maximum = budget.saturating_sub(total_bytes);
        let bytes = repository.read_committed_file(commit, &unit.path, maximum)?;
        total_bytes += bytes.len();
        let source = String::from_utf8(bytes)
            .map_err(|_| LegacyReadError::UndecodableRowUnit(unit.path.clone()))?;
        let parsed = parse_row(&source);
        if !parsed.diagnostics.is_empty() {
            return Err(LegacyReadError::UndecodableRowUnit(unit.path));
        }
        let Some(Raw::Map(entries)) = lower_data_expression(&parsed.value) else {
            return Err(LegacyReadError::UndecodableRowUnit(unit.path));
        };
        let Some(key) = legacy_row_key(&unit.key_path, &unit.path)? else {
            continue;
        };
        let fields = entries
            .into_iter()
            .filter_map(|(name, value)| match name {
                Raw::Text(name) => Some(LegacyField {
                    name: LegacyFieldName::Declared(name),
                    value,
                }),
                _ => None,
            })
            .collect();
        rows.push(LegacyRow { key, fields });
    }
    let rows = one_row_per_key(table_path.to_owned(), rows)?;
    Ok(LegacyTableRows {
        source: LegacyRowSource::LooseRowUnit,
        rows,
    })
}

/// Decodes one table's rows into exactly one authoritative row per key
/// (UPGRADE-3 step 3, `reference/Orna-1.1.0/source/40-upgrade-contract.md`).
///
/// The rows are put in key order. Two rows that name one key fail closed with
/// [`LegacyReadError::DuplicateRowKey`] rather than one being chosen: the
/// compact source already folds its generations and deletions in the
/// repository reader, so a repeated key there or in one loose table is
/// corruption, and no precedence rule between rows of one source is specified.
/// This step reads no source and writes nothing.
fn one_row_per_key(
    table: String,
    mut rows: Vec<LegacyRow>,
) -> Result<Vec<LegacyRow>, LegacyReadError> {
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    if let Some(pair) = rows.windows(2).find(|pair| pair[0].key == pair[1].key) {
        let key = pair[0]
            .key
            .component_text()
            .unwrap_or_else(|| format!("{:?}", pair[0].key));
        return Err(LegacyReadError::DuplicateRowKey { table, key });
    }
    Ok(rows)
}

/// The key one loose row unit names, reconstructed from its path components
/// after the table directory (ORNA-ROW-002).
///
/// 1.0.0 key-path-v1 writes one component per key component, with the final
/// component keeping the `.orna` row-unit suffix. A component that needs an
/// escape (`~`) or a digest alias (`~key-`) cannot be read back without the
/// table's declared key types, so such a unit is refused rather than guessed.
/// `None` means the entry is not a single-key row of this reader's shape.
fn legacy_row_key(
    key_path: &[String],
    logical_path: &str,
) -> Result<Option<TypedKey>, LegacyReadError> {
    let Some(last) = key_path.last() else {
        return Ok(None);
    };
    let Some(last) = last.strip_suffix(ROW_UNIT_SUFFIX) else {
        return Ok(None);
    };
    if last.is_empty() {
        return Ok(None);
    }
    let mut components = key_path[..key_path.len() - 1].to_vec();
    components.push(last.to_owned());
    if components.iter().any(|component| component.contains('~')) {
        return Err(LegacyReadError::UnreadableKeyPath(logical_path.to_owned()));
    }
    Ok(Some(match components.as_slice() {
        [only] => TypedKey::Text(only.clone()),
        many => TypedKey::Tuple(many.iter().cloned().map(TypedKey::Text).collect()),
    }))
}

/// The committed unit of the loose row `table_path`/`key`, when one row's
/// reconstructed key is the key the caller named.
///
/// A legacy row's key is reconstructed from its path text, so a caller's key
/// spelling is matched by that text: `alice-smith` names the unit
/// `contacts/Contact/alice-smith.orna`. `None` means no committed unit names
/// that key, which is a row that is not committed at this snapshot rather than
/// a failure to read one.
pub fn legacy_loose_row_path(
    repository: &Repository,
    commit: &GitCommitRef,
    table_path: &str,
    key: &TypedKey,
) -> Result<Option<String>, LegacyReadError> {
    let wanted = key.component_text();
    for unit in loose_row_units(repository, commit, table_path)? {
        let Some(row_key) = legacy_row_key(&unit.key_path, &unit.path)? else {
            continue;
        };
        if row_key == *key || row_key.component_text() == wanted {
            return Ok(Some(unit.path));
        }
    }
    Ok(None)
}

/// Lists up to `max_commits` revisions of one loose row unit, newest first.
///
/// A legacy row is a committed file, so a revision is a commit whose tree does
/// or does not carry that file; the walk reads commit headers and tree listings
/// only (ORNA-UPGRADE-002), the same coordinates the native row walk reports.
pub fn read_legacy_row_revisions(
    repository: &Repository,
    start: &str,
    row_path: &str,
    max_commits: usize,
) -> Result<Vec<LegacyRowRevision>, LegacyReadError> {
    let snapshots = repository.committed_revision_snapshots(start, max_commits)?;
    snapshots
        .into_iter()
        .map(|snapshot| {
            let present = repository.committed_path_present(snapshot.commit(), row_path)?;
            Ok(LegacyRowRevision { snapshot, present })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use orna_repository::{Repository, TypedKey, Uuid};
    use tempfile::TempDir;

    use super::{
        LegacyReadError, LegacyRow, LegacyRowSource, LegacyTableAddress, one_row_per_key,
        read_legacy_table_rows,
    };

    /// The golden format-1 bundle the CLI legacy tests read
    /// (`crates/orna-cli/tests/fixtures/legacy-format1.bundle`).
    const BUNDLE: &str = "../orna-cli/tests/fixtures/legacy-format1.bundle";

    /// The fixture's compact table ids: `Uuid::from_u64_pair(0x018f_0000_0000_7100, n)`.
    /// Table 1 holds Int keys 1..=4, table 2 holds Str keys, table 3 holds Date keys.
    fn compact_table(number: u64) -> [u8; 16] {
        *Uuid::from_u64_pair(0x018f_0000_0000_7100, number).as_bytes()
    }

    /// Clones the golden bundle into a fresh directory and opens it.
    fn golden_repository() -> (TempDir, Repository, orna_repository::GitCommitRef) {
        let directory = TempDir::new().expect("a temporary directory");
        let clone = Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("clone")
            .arg("--quiet")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE))
            .arg(directory.path())
            .output()
            .expect("the git clone process starts");
        assert!(
            clone.status.success(),
            "git clone bundle: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        let repository = Repository::discover(directory.path()).expect("the clone opens");
        let commit = repository
            .head()
            .expect("the clone reads HEAD")
            .expect("the bundle has a commit");
        (directory, repository, commit)
    }

    fn row(key: TypedKey) -> LegacyRow {
        LegacyRow {
            key,
            fields: Vec::new(),
        }
    }

    fn keys(rows: &[LegacyRow]) -> Vec<TypedKey> {
        rows.iter().map(|row| row.key().clone()).collect()
    }

    #[test]
    fn golden_bundle_decodes_exactly_one_row_per_key() {
        let (_directory, repository, commit) = golden_repository();

        // Compact table 1: Int keys 1..=4, one row each.
        let ints = read_legacy_table_rows(
            &repository,
            &commit,
            &LegacyTableAddress::Relation(compact_table(1)),
        )
        .expect("the Int table decodes");
        assert!(matches!(ints.source(), LegacyRowSource::Compact));
        assert_eq!(
            keys(ints.rows()),
            vec![
                TypedKey::Int(1),
                TypedKey::Int(2),
                TypedKey::Int(3),
                TypedKey::Int(4),
            ],
        );

        // Compact tables 2 (Str) and 3 (Date) each hold strictly increasing keys.
        for number in [2, 3] {
            let table = read_legacy_table_rows(
                &repository,
                &commit,
                &LegacyTableAddress::Relation(compact_table(number)),
            )
            .expect("a compact table decodes");
            let decoded = keys(table.rows());
            assert!(!decoded.is_empty(), "table {number} holds rows");
            assert!(
                decoded.windows(2).all(|pair| pair[0] < pair[1]),
                "table {number} keys are strictly increasing, so no key repeats",
            );
        }

        // The editable table is read from its loose row units, one per key.
        let loose = read_legacy_table_rows(
            &repository,
            &commit,
            &LegacyTableAddress::TablePath("contacts/Contact".to_owned()),
        )
        .expect("the loose table decodes");
        assert!(matches!(loose.source(), LegacyRowSource::LooseRowUnit));
        assert_eq!(loose.rows().len(), 2);
        let decoded = keys(loose.rows());
        assert!(decoded.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn one_row_per_key_keeps_distinct_keys_in_key_order() {
        let rows = one_row_per_key(
            "t".to_owned(),
            vec![
                row(TypedKey::Int(3)),
                row(TypedKey::Int(1)),
                row(TypedKey::Int(2)),
            ],
        )
        .expect("distinct keys decode");
        assert_eq!(
            keys(&rows),
            vec![TypedKey::Int(1), TypedKey::Int(2), TypedKey::Int(3)],
        );
    }

    #[test]
    fn duplicate_key_in_one_table_fails_closed_naming_table_and_key() {
        let error = one_row_per_key(
            "contacts/Contact".to_owned(),
            vec![
                row(TypedKey::Int(2)),
                row(TypedKey::Int(1)),
                row(TypedKey::Int(2)),
            ],
        )
        .expect_err("a repeated key has no single authoritative row");
        match error {
            LegacyReadError::DuplicateRowKey { table, key } => {
                assert_eq!(table, "contacts/Contact");
                assert_eq!(key, "2");
            }
            other => panic!("expected DuplicateRowKey, got {other}"),
        }
    }
}
