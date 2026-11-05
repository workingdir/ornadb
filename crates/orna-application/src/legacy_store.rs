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
    let mut rows = table
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
    rows.sort_by(|left, right| left.key.cmp(&right.key));
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
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(LegacyTableRows {
        source: LegacyRowSource::LooseRowUnit,
        rows,
    })
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
