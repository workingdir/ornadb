//! Persisted source identities for table declarations, their fields and the
//! nominal types, variants and units that stored fields use.
//!
//! Reconciliation is an explicit authoring operation. Loading source never
//! allocates identities, and retired identities are never reused.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use num_bigint::BigInt;
use orna_repository_v1::{
    GitCommitRef, PrivateCommit, Repository, RepositoryError, Uuid,
    relation_admission::{RelationAdmissionError, StagedCandidate, StoredObjectIdentity},
};
use orna_semantic_v1::{
    Analysis, ModuleInput, Namespace, SourceUnitIdentity, Symbol, SymbolKind, TableAdmission,
    TableSchema, Type, declared_type,
};
use orna_syntax_v1::{
    Argument, AssignmentTarget, Declaration, EnumVariant, Expr, FieldInitializer, ImplMember,
    LiteralKind, Pattern, RecordField, Statement, StringSegment, SyntaxTree, TableMember, Token,
    TokenKind, TypeExpr, TypeMember, TypeRepresentation, UseTail, lex, parse_module,
};
use orna_value_v1::{Raw, SchemaDescriptor, Value, domain_digest};

/// Repository-relative location of the legacy identity sidecar. Format 3
/// commits keep the inventory in the store (see [`ObjectIdentityManifest::read_committed`]);
/// the worktree copy remains the input of explicit rename tooling.
pub const OBJECT_IDENTITY_MANIFEST_PATH: &str =
    orna_repository_v1::relation_admission::LEGACY_OBJECT_IDENTITY_SIDECAR_PATH;
const MAX_OBJECT_IDENTITY_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
/// Sidecar encoding version. Version 2 stores each kind's `sys.ObjectKind`
/// ordinal (see [`ObjectKind::spec_ordinal`]); version 1 is the legacy
/// internal code (see [`ObjectKind::legacy_code`]) and is read only for old commits.
const MANIFEST_VERSION: u64 = 2;
const LEGACY_MANIFEST_VERSION: u64 = 1;
/// `sys.ObjectKind` ordinals (api/sys.json order) shared by several kinds.
const SPEC_FIELD_ORDINAL: u64 = 9;
const SPEC_VARIANT_ORDINAL: u64 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ObjectKind {
    Table,
    /// A table column, including primary-key columns (`Table.column`).
    Field,
    Unit,
    /// A nominal user type, refined nominal type or enum (`Type`).
    Type,
    /// One enum variant (`Enum.variant`).
    Variant,
    /// One field of a nominal record type (`Type.field`).
    TypeField,
    /// One payload field of an enum variant (`Enum.variant.field`).
    VariantField,
}

impl ObjectKind {
    /// The `sys.ObjectKind` value this kind is stored under. Both nominal
    /// fields and variant payload fields are `field`; they are told apart by
    /// their owner when read (see [`records_from_spec_ordinals`]).
    const fn spec_ordinal(self) -> u64 {
        match self {
            Self::Table => 2,
            Self::Field => 3,
            Self::Unit => 15,
            Self::Type => 8,
            Self::Variant => SPEC_VARIANT_ORDINAL,
            Self::TypeField | Self::VariantField => SPEC_FIELD_ORDINAL,
        }
    }

    /// The internal code of sidecar version 1.
    const fn legacy_code(self) -> u64 {
        match self {
            Self::Table => 1,
            Self::Field => 2,
            Self::Unit => 3,
            Self::Type => 4,
            Self::Variant => 5,
            Self::TypeField => 6,
            Self::VariantField => 7,
        }
    }

    fn from_legacy_code(code: u64) -> Option<Self> {
        [
            Self::Table,
            Self::Field,
            Self::Unit,
            Self::Type,
            Self::Variant,
            Self::TypeField,
            Self::VariantField,
        ]
        .into_iter()
        .find(|kind| kind.legacy_code() == code)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectIdentity {
    kind: ObjectKind,
    qualified_name: String,
    id: [u8; 16],
    retired: bool,
}

impl ObjectIdentity {
    pub const fn kind(&self) -> ObjectKind {
        self.kind
    }
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
    pub const fn id(&self) -> &[u8; 16] {
        &self.id
    }
    pub const fn is_retired(&self) -> bool {
        self.retired
    }
}

/// An explicit move of an existing object identity to a new qualified name.
///
/// Rename tooling supplies these mappings from semantic intent; reconciliation
/// never infers identity continuity from similar source text or names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectIdentityRename {
    kind: ObjectKind,
    old_qualified_name: String,
    new_qualified_name: String,
}

impl ObjectIdentityRename {
    pub fn new(
        kind: ObjectKind,
        old_qualified_name: impl Into<String>,
        new_qualified_name: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            old_qualified_name: old_qualified_name.into(),
            new_qualified_name: new_qualified_name.into(),
        }
    }

    pub const fn kind(&self) -> ObjectKind {
        self.kind
    }

    pub fn old_qualified_name(&self) -> &str {
        &self.old_qualified_name
    }

    pub fn new_qualified_name(&self) -> &str {
        &self.new_qualified_name
    }
}

/// Canonical, persistent table/field identity inventory for a source project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObjectIdentityManifest {
    records: Vec<ObjectIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectIdentityError {
    InvalidEncoding,
    InvalidSource,
    IdentityCollision,
    /// A rename names an object with no active committed or staged identity.
    MissingIdentity,
    /// The Git index or `HEAD` changed while identities were being staged.
    StaleIndex,
    Io,
}

impl std::fmt::Display for ObjectIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEncoding => "the object identity inventory is not canonical",
            Self::InvalidSource => {
                "the source does not declare the renamed object under its new name"
            }
            Self::IdentityCollision => "the new name already has an object identity",
            Self::MissingIdentity => "the object has no committed or staged identity to keep",
            Self::StaleIndex => {
                "the Git index or HEAD changed while the identity was staged; retry"
            }
            Self::Io => "the object identity inventory could not be read or staged",
        })
    }
}

impl std::error::Error for ObjectIdentityError {}

impl From<RepositoryError> for ObjectIdentityError {
    fn from(_: RepositoryError) -> Self {
        Self::Io
    }
}

impl From<RelationAdmissionError> for ObjectIdentityError {
    fn from(error: RelationAdmissionError) -> Self {
        match error {
            RelationAdmissionError::StaleIndex => Self::StaleIndex,
            RelationAdmissionError::InvalidIdentity => Self::InvalidEncoding,
            _ => Self::Io,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceTableSchemaError {
    InvalidAnalysis,
    InvalidModulePath,
    ModuleNotFound,
    AmbiguousModule,
    DeclarationNotFound,
    NotTable,
    MissingSchema,
    MissingAdmission,
    MissingIdentity,
    /// A stored table reference names its database, which the caller did not
    /// supply (29-formats.md type node 6).
    MissingDatabaseIdentity,
    InvalidSourceSchema,
    UnsupportedType,
    InvalidDescriptor,
}

/// Source-qualified table identity and schema derived from checked declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceTableSchemaArtifact {
    module_name: String,
    source_path: String,
    declaration_name: String,
    table_object_id: [u8; 16],
    field_object_ids: BTreeMap<String, [u8; 16]>,
    descriptor: SchemaDescriptor,
}

impl SourceTableSchemaArtifact {
    pub fn module_name(&self) -> &str {
        &self.module_name
    }
    pub fn source_path(&self) -> &str {
        &self.source_path
    }
    pub fn declaration_name(&self) -> &str {
        &self.declaration_name
    }
    pub const fn table_object_id(&self) -> &[u8; 16] {
        &self.table_object_id
    }
    pub fn field_object_ids(&self) -> &BTreeMap<String, [u8; 16]> {
        &self.field_object_ids
    }
    pub fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }
}

impl ObjectIdentityManifest {
    /// Derives a complete storage descriptor from one analyzed source table and
    /// this persisted identity inventory. No graph or row metadata is consulted.
    ///
    /// `modules` are the exact source units analysed into `analysis`; their
    /// declarations supply computed selectors, nominal definitions and import
    /// scopes. A stored table reference records the database that owns its
    /// target, so a table storing one requires `database_id`.
    pub fn source_table_schema(
        &self,
        modules: &[ModuleInput],
        analysis: &Analysis,
        database_id: Option<[u8; 16]>,
        module_name: &str,
        source_path: &str,
        declaration_name: &str,
    ) -> Result<SourceTableSchemaArtifact, SourceTableSchemaError> {
        let namespace =
            module_namespace(source_path).ok_or(SourceTableSchemaError::InvalidModulePath)?;
        if !analysis.is_ok() || namespace != module_name {
            return Err(SourceTableSchemaError::InvalidAnalysis);
        }
        let mut selected = None;
        for (candidate_namespace, module) in &analysis.modules {
            if candidate_namespace.display() != module_name {
                continue;
            }
            if selected.replace((candidate_namespace, module)).is_some() {
                return Err(SourceTableSchemaError::AmbiguousModule);
            }
        }
        let (table_namespace, module) = selected.ok_or(SourceTableSchemaError::ModuleNotFound)?;
        let symbol = module
            .symbols
            .get(declaration_name)
            .ok_or(SourceTableSchemaError::DeclarationNotFound)?;
        if symbol.kind != SymbolKind::Table {
            return Err(SourceTableSchemaError::NotTable);
        }
        let schema = symbol
            .table_schema
            .as_ref()
            .ok_or(SourceTableSchemaError::MissingSchema)?;
        let admission = schema
            .admission
            .as_ref()
            .ok_or(SourceTableSchemaError::MissingAdmission)?;
        let qualified_table = qualify(module_name, declaration_name);
        let table_object_id = *self
            .id(ObjectKind::Table, &qualified_table)
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        let field_object_ids = schema
            .fields
            .keys()
            .map(|name| {
                let qualified_field = format!("{qualified_table}.{name}");
                self.id(ObjectKind::Field, &qualified_field)
                    .copied()
                    .map(|id| (name.clone(), id))
                    .ok_or(SourceTableSchemaError::MissingIdentity)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let expected_identities = std::iter::once((ObjectKind::Table, qualified_table.clone()))
            .chain(
                field_object_ids
                    .keys()
                    .map(|name| (ObjectKind::Field, format!("{qualified_table}.{name}"))),
            )
            .collect::<BTreeSet<_>>();
        let field_prefix = format!("{qualified_table}.");
        let active_identities = self
            .records
            .iter()
            .filter(|record| {
                !record.retired
                    && ((record.kind == ObjectKind::Table
                        && record.qualified_name == qualified_table)
                        || (record.kind == ObjectKind::Field
                            && record
                                .qualified_name
                                .strip_prefix(&field_prefix)
                                .is_some_and(|name| !name.is_empty() && !name.contains('.'))))
            })
            .map(|record| (record.kind, record.qualified_name.clone()))
            .collect::<BTreeSet<_>>();
        if active_identities != expected_identities {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let sources = parse_source_modules(modules)?;
        if sources
            .get(module_name)
            .is_none_or(|source| source.module.logical_path != source_path)
        {
            return Err(SourceTableSchemaError::InvalidModulePath);
        }
        let descriptor = DescriptorBuilder::new(self, analysis, &sources, database_id)
            .table_descriptor(
                table_namespace,
                declaration_name,
                table_object_id,
                &field_object_ids,
                schema,
                admission,
            )?;
        Ok(SourceTableSchemaArtifact {
            module_name: module_name.to_owned(),
            source_path: source_path.to_owned(),
            declaration_name: declaration_name.to_owned(),
            table_object_id,
            field_object_ids,
            descriptor,
        })
    }
}
impl ObjectIdentityManifest {
    pub fn records(&self) -> &[ObjectIdentity] {
        &self.records
    }

    pub fn id(&self, kind: ObjectKind, qualified_name: &str) -> Option<&[u8; 16]> {
        self.records
            .iter()
            .find(|record| {
                !record.retired && record.kind == kind && record.qualified_name == qualified_name
            })
            .map(|record| &record.id)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ObjectIdentityError> {
        self.encode_as(MANIFEST_VERSION, ObjectKind::spec_ordinal)
    }

    /// Encodes the inventory under `version`, storing each kind as `code` gives it.
    fn encode_as(
        &self,
        version: u64,
        code: fn(ObjectKind) -> u64,
    ) -> Result<Vec<u8>, ObjectIdentityError> {
        validate_records(&self.records)?;
        let raw = Raw::Array(vec![
            Raw::Int(version.into()),
            Raw::Array(
                self.records
                    .iter()
                    .map(|record| {
                        Raw::Array(vec![
                            Raw::Int(code(record.kind).into()),
                            Raw::Text(record.qualified_name.clone()),
                            Raw::Bytes(record.id.to_vec()),
                            Raw::Bool(record.retired),
                        ])
                    })
                    .collect(),
            ),
        ]);
        Value::new(raw)
            .and_then(|value| value.encode())
            .map_err(|_| ObjectIdentityError::InvalidEncoding)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ObjectIdentityError> {
        if bytes.len() > MAX_OBJECT_IDENTITY_MANIFEST_BYTES {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        let value = Value::decode(bytes).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        let Raw::Array(root) = value.raw() else {
            return Err(ObjectIdentityError::InvalidEncoding);
        };
        let version = match root.first().and_then(raw_int) {
            Some(version @ (LEGACY_MANIFEST_VERSION | MANIFEST_VERSION)) if root.len() == 2 => {
                version
            }
            _ => return Err(ObjectIdentityError::InvalidEncoding),
        };
        let Raw::Array(entries) = &root[1] else {
            return Err(ObjectIdentityError::InvalidEncoding);
        };
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries {
            let Raw::Array(fields) = entry else {
                return Err(ObjectIdentityError::InvalidEncoding);
            };
            if fields.len() != 4 {
                return Err(ObjectIdentityError::InvalidEncoding);
            }
            let code = raw_int(&fields[0]).ok_or(ObjectIdentityError::InvalidEncoding)?;
            let Raw::Text(qualified_name) = &fields[1] else {
                return Err(ObjectIdentityError::InvalidEncoding);
            };
            let Raw::Bytes(id) = &fields[2] else {
                return Err(ObjectIdentityError::InvalidEncoding);
            };
            let Raw::Bool(retired) = &fields[3] else {
                return Err(ObjectIdentityError::InvalidEncoding);
            };
            let id: [u8; 16] = id
                .as_slice()
                .try_into()
                .map_err(|_| ObjectIdentityError::InvalidEncoding)?;
            rows.push((code, qualified_name.clone(), id, *retired));
        }
        let records = if version == LEGACY_MANIFEST_VERSION {
            rows.into_iter()
                .map(|(code, qualified_name, id, retired)| {
                    Ok(ObjectIdentity {
                        kind: ObjectKind::from_legacy_code(code)
                            .ok_or(ObjectIdentityError::InvalidEncoding)?,
                        qualified_name,
                        id,
                        retired,
                    })
                })
                .collect::<Result<Vec<_>, ObjectIdentityError>>()?
        } else {
            records_from_spec_ordinals(rows)?
        };
        let manifest = Self { records };
        validate_records(&manifest.records).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        let canonical = if version == LEGACY_MANIFEST_VERSION {
            manifest.encode_as(LEGACY_MANIFEST_VERSION, ObjectKind::legacy_code)?
        } else {
            manifest.encode()?
        };
        if canonical.as_slice() != bytes {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        Ok(manifest)
    }

    /// Writer-only conservative reconciliation. Existing IDs survive source edits
    /// at their current qualified names; removed names retire and new names get
    /// random UUIDs. Use `reconcile_with_renames` for explicit semantic renames.
    pub fn reconcile(
        modules: &[ModuleInput],
        previous: Option<&Self>,
    ) -> Result<Self, ObjectIdentityError> {
        Self::reconcile_with_renames(modules, previous, &[])
    }

    /// Reconciles explicit semantic identity moves with current source.
    ///
    /// Each rename must identify one active identity from the previous manifest
    /// and a matching new source name. Similarity or a textual name
    /// match is never used to infer a rename.
    pub fn reconcile_with_renames(
        modules: &[ModuleInput],
        previous: Option<&Self>,
        renames: &[ObjectIdentityRename],
    ) -> Result<Self, ObjectIdentityError> {
        let active = source_identity_keys(modules)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut records = previous.map_or_else(Vec::new, |manifest| manifest.records.clone());
        apply_explicit_renames(&mut records, &active, renames)?;
        for record in &mut records {
            if !active.contains(&(record.kind, record.qualified_name.clone())) {
                record.retired = true;
            }
        }
        let mut used_ids = records
            .iter()
            .map(|record| record.id)
            .collect::<HashSet<_>>();
        for (kind, qualified_name) in active {
            if records.iter().any(|record| {
                record.kind == kind && record.qualified_name == qualified_name && !record.retired
            }) {
                continue;
            }
            let id = loop {
                let candidate = *Uuid::new_v4().as_bytes();

                if used_ids.insert(candidate) {
                    break candidate;
                }
            };
            records.push(ObjectIdentity {
                kind,
                qualified_name,
                id,
                retired: false,
            });
        }
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str())
                .cmp(&(right.kind, right.qualified_name.as_str()))
        });
        validate_records(&records)?;
        Ok(Self { records })
    }

    /// Applies only explicit semantic renames to this inventory, as
    /// `orna mv` does before the renamed source is staged. Every other
    /// record keeps its name, ID and retirement, so unstaged source edits
    /// neither retire nor allocate identities; the next `orna commit`
    /// reconciles the staged source as usual. Each rename must name an
    /// active identity and a declaration `modules` contain, and a target
    /// may not collide with another active identity.
    pub fn with_renames(
        &self,
        modules: &[ModuleInput],
        renames: &[ObjectIdentityRename],
    ) -> Result<Self, ObjectIdentityError> {
        let active = source_identity_keys(modules)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut records = self.records.clone();
        apply_explicit_renames(&mut records, &active, renames)?;
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str())
                .cmp(&(right.kind, right.qualified_name.as_str()))
        });
        validate_records(&records)?;
        Ok(Self { records })
    }

    /// Reads and validates the committed identity inventory of one exact
    /// commit.
    ///
    /// Format 3 keeps the inventory as a sealed system relation of
    /// `.orna/store`, because tracked `.orna/` holds only `database.orna` and
    /// `store` (ORNA-META-001) and stable IDs are committed metadata
    /// (ORNA-OBJECT-002). The tracked sidecar is read only for a legacy
    /// commit whose store has no inventory relation; when a commit has both,
    /// the store is the authority.
    pub fn read_committed(
        repository: &Repository,
        commit: &GitCommitRef,
    ) -> Result<Self, ObjectIdentityError> {
        Self::read_committed_if_present(repository, commit)?.ok_or(ObjectIdentityError::Io)
    }

    /// Reads the committed identity inventory of one exact commit, or
    /// `Ok(None)` when the commit carries no inventory at all: neither a store
    /// identity relation nor a legacy sidecar. Such a commit never allocated
    /// object identities, so none of its tables has a committed identity.
    /// Unreadable state is still an error.
    pub fn read_committed_if_present(
        repository: &Repository,
        commit: &GitCommitRef,
    ) -> Result<Option<Self>, ObjectIdentityError> {
        if let Some(stored) = repository
            .read_committed_object_identities(commit)
            .map_err(|_| ObjectIdentityError::Io)?
        {
            return Self::from_stored(&stored).map(Some);
        }
        repository
            .read_legacy_identity_sidecar(commit, MAX_OBJECT_IDENTITY_MANIFEST_BYTES)
            .map_err(|_| ObjectIdentityError::Io)?
            .map(|bytes| Self::decode(&bytes))
            .transpose()
    }

    /// Reads the inventory the staged candidate of the ordinary index
    /// carries: its staged store's inventory relation, or else the legacy
    /// sidecar its staged tree tracks. `Ok(None)` before any identity was
    /// allocated, as for the first commit of a database.
    pub fn read_staged(
        repository: &Repository,
        staged: &StagedCandidate,
    ) -> Result<Option<Self>, ObjectIdentityError> {
        if let Some(stored) = repository
            .read_staged_object_identities(staged)
            .map_err(|_| ObjectIdentityError::Io)?
        {
            return Self::from_stored(&stored).map(Some);
        }
        repository
            .read_staged_legacy_identity_sidecar(staged, MAX_OBJECT_IDENTITY_MANIFEST_BYTES)
            .map_err(|_| ObjectIdentityError::Io)?
            .map(|bytes| Self::decode(&bytes))
            .transpose()
    }

    /// Reads the inventory a private publication candidate's tree carries: its
    /// candidate store's inventory relation, or else the legacy sidecar its tree
    /// tracks. `Ok(None)` before any identity was allocated.
    pub fn read_private_candidate(
        repository: &Repository,
        candidate: &PrivateCommit,
    ) -> Result<Option<Self>, ObjectIdentityError> {
        if let Some(stored) = repository
            .read_private_candidate_object_identities(candidate)
            .map_err(|_| ObjectIdentityError::Io)?
        {
            return Self::from_stored(&stored).map(Some);
        }
        repository
            .read_private_candidate_legacy_identity_sidecar(
                candidate,
                MAX_OBJECT_IDENTITY_MANIFEST_BYTES,
            )
            .map_err(|_| ObjectIdentityError::Io)?
            .map(|bytes| Self::decode(&bytes))
            .transpose()
    }

    /// Reads the inventory the ordinary Git index stages: the identity
    /// relation of its `.orna/store`, or the legacy sidecar of an index that
    /// predates that relation. `Ok(None)` while nothing is staged or
    /// committed, when no object has an identity yet.
    ///
    /// This is the inventory the next `orna commit` reconciles, so it answers
    /// whether a rename can keep an object's ObjectId (ORNA-OBJECT-003).
    pub fn read_index(repository: &Repository) -> Result<Option<Self>, ObjectIdentityError> {
        let Some(staged) = Self::capture_index_inventory(repository)? else {
            return Ok(None);
        };
        Self::read_staged(repository, &staged)
    }

    /// Captures the ordinary index when it stages a store or the legacy
    /// sidecar, the only places an identity inventory can be staged.
    fn capture_index_inventory(
        repository: &Repository,
    ) -> Result<Option<StagedCandidate>, ObjectIdentityError> {
        if !repository.index_stages_object_identities()? {
            return Ok(None);
        }
        Ok(Some(repository.capture_staged_candidate()?))
    }

    /// Moves each renamed identity to its new name in the inventory the
    /// ordinary index stages, and stages the resulting `.orna/store`
    /// (ORNA-OBJECT-003). Nothing is committed: the change is reviewable with
    /// `git status`, and the next `orna commit` keeps the ObjectId.
    ///
    /// `modules` is the source after the rename, which must declare every
    /// new name. Each old name must have an active identity and each new
    /// name must have none. A table carries its fields and a type carries its
    /// fields, variants and variant fields, as in
    /// [`Self::reconcile_with_renames`]. No other identity is allocated or
    /// retired, so declarations staged and unstaged differently keep theirs.
    /// `Ok(None)` when the index stages no inventory: no object has an
    /// identity to keep yet, so the rename is plain source text.
    ///
    /// `staged_sources` replaces the staged bytes of the source files the
    /// rename edits, so the renamed declaration is staged with the inventory
    /// that names it, as `git mv` stages a move. Other paths are kept.
    pub fn stage_renames(
        repository: &Repository,
        modules: &[ModuleInput],
        renames: &[ObjectIdentityRename],
        staged_sources: &[(&str, &[u8])],
    ) -> Result<Option<Self>, ObjectIdentityError> {
        let Some(staged) = Self::capture_index_inventory(repository)? else {
            return Ok(None);
        };
        let Some(previous) = Self::read_staged(repository, &staged)? else {
            return Ok(None);
        };
        for rename in renames {
            if previous
                .id(rename.kind, &rename.old_qualified_name)
                .is_none()
            {
                return Err(ObjectIdentityError::MissingIdentity);
            }
            if previous
                .id(rename.kind, &rename.new_qualified_name)
                .is_some()
            {
                return Err(ObjectIdentityError::IdentityCollision);
            }
        }
        let declared = source_identity_keys(modules)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut records = previous.records;
        apply_explicit_renames(&mut records, &declared, renames)?;
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str())
                .cmp(&(right.kind, right.qualified_name.as_str()))
        });
        validate_records(&records)?;
        let next = Self { records };
        if staged.store_root().is_none() {
            // A legacy index with only the sidecar: the store and its
            // inventory relation are created by the next `orna commit`.
            return Err(ObjectIdentityError::Io);
        }
        repository.stage_object_identities(&staged, &next.to_stored(), staged_sources)?;
        Ok(Some(next))
    }

    /// The inventory as store rows for the sealed identity relation.
    pub fn to_stored(&self) -> Vec<StoredObjectIdentity> {
        self.records
            .iter()
            .map(|record| {
                StoredObjectIdentity::new(
                    record.id,
                    record.kind.spec_ordinal(),
                    record.qualified_name.clone(),
                    record.retired,
                )
            })
            .collect()
    }

    /// Rebuilds the inventory from the store rows of the sealed identity
    /// relation, in the same canonical record order reconciliation uses.
    fn from_stored(stored: &[StoredObjectIdentity]) -> Result<Self, ObjectIdentityError> {
        let rows = stored
            .iter()
            .map(|identity| {
                (
                    identity.kind(),
                    identity.qualified_name().to_owned(),
                    *identity.id(),
                    identity.retired(),
                )
            })
            .collect();
        let mut records = records_from_spec_ordinals(rows)?;
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str())
                .cmp(&(right.kind, right.qualified_name.as_str()))
        });
        validate_records(&records).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        Ok(Self { records })
    }
    /// Reads the manifest from the current worktree without allocating IDs.
    pub fn read_worktree(repository: &Repository) -> Result<Option<Self>, ObjectIdentityError> {
        let root = repository.worktree();
        let path = root.join(OBJECT_IDENTITY_MANIFEST_PATH);
        crate::ensure_no_symlink_ancestors(root, &path).map_err(|_| ObjectIdentityError::Io)?;
        let Some(bytes) = Self::read_bounded_manifest(&path)? else {
            return Ok(None);
        };
        Self::decode(&bytes).map(Some)
    }

    fn read_bounded_manifest(path: &Path) -> Result<Option<Vec<u8>>, ObjectIdentityError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ObjectIdentityError::Io),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ObjectIdentityError::Io);
        }
        let length =
            usize::try_from(metadata.len()).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        if length > MAX_OBJECT_IDENTITY_MANIFEST_BYTES {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        let mut bytes = Vec::with_capacity(length);
        fs::File::open(path)
            .map_err(|_| ObjectIdentityError::Io)?
            .take((MAX_OBJECT_IDENTITY_MANIFEST_BYTES as u64) + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ObjectIdentityError::Io)?;
        if bytes.len() > MAX_OBJECT_IDENTITY_MANIFEST_BYTES {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        Ok(Some(bytes))
    }

    /// Reconciles an explicit source snapshot with the current worktree
    /// manifest and atomically writes the result. Loading never calls this.
    pub fn reconcile_and_write_worktree(
        repository: &Repository,
        modules: &[ModuleInput],
    ) -> Result<Self, ObjectIdentityError> {
        Self::reconcile_and_write_worktree_with_renames(repository, modules, &[])
    }

    /// Reconciles explicit semantic renames against the worktree manifest and
    /// atomically writes the result. Loading never allocates or renames identities.
    pub fn reconcile_and_write_worktree_with_renames(
        repository: &Repository,
        modules: &[ModuleInput],
        renames: &[ObjectIdentityRename],
    ) -> Result<Self, ObjectIdentityError> {
        repository.with_coordination_lock(|| {
            let previous = Self::read_worktree(repository)?;
            let next = Self::reconcile_with_renames(modules, previous.as_ref(), renames)?;
            next.write_worktree(repository)?;
            Ok(next)
        })
    }

    /// Atomically replaces the worktree manifest after explicit reconciliation.
    /// The caller remains responsible for reviewing and committing the file.
    pub fn write_worktree(&self, repository: &Repository) -> Result<PathBuf, ObjectIdentityError> {
        let root = repository.worktree();
        let path = root.join(OBJECT_IDENTITY_MANIFEST_PATH);
        crate::ensure_no_symlink_ancestors(root, &path).map_err(|_| ObjectIdentityError::Io)?;
        let parent = path.parent().ok_or(ObjectIdentityError::Io)?;
        fs::create_dir_all(parent).map_err(|_| ObjectIdentityError::Io)?;
        crate::ensure_no_symlink_ancestors(root, &path).map_err(|_| ObjectIdentityError::Io)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ObjectIdentityError::Io);
            }
        }
        let bytes = self.encode()?;
        let temp = temporary_path(parent, &path);
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| ObjectIdentityError::Io)?;
            file.write_all(&bytes)
                .map_err(|_| ObjectIdentityError::Io)?;
            file.sync_all().map_err(|_| ObjectIdentityError::Io)?;
            fs::rename(&temp, &path).map_err(|_| ObjectIdentityError::Io)?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ObjectIdentityError::Io)?;
            Ok(path.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}
/// Implementation-owned digest domains for source expressions recorded by a
/// closed storage descriptor: a computed selector's `[2, expression_digest]`
/// and a refined type's `assertion_semantic_digests` (29-formats.md). Neither
/// is one of the fixed 29-formats.md domains; work ADR 0118 documents both.
///
/// The payload is an OVB-1 array with one entry per lexical token of the
/// expression, so whitespace and comments never change a fingerprint
/// (ORNA-SYS-034). An identifier that is a free reference is replaced by what
/// it resolves to, `["declaration", namespace, name]` or `["module",
/// namespace]`, so identical tokens bound to a different declaration give a
/// different fingerprint. Row fields, local bindings and core names keep
/// their `["identifier", name]` entry.
const COMPUTED_SELECTOR_DIGEST_DOMAIN: &str = "ornadb.computed-selector.v2";
const REFINEMENT_ASSERTION_DIGEST_DOMAIN: &str = "ornadb.refinement-assertion.v2";

const RECORD_DEFINITION: u64 = 0;
const ENUM_DEFINITION: u64 = 1;
const REFINED_DEFINITION: u64 = 2;
const UNIT_DEFINITION: u64 = 3;
const CURRENCY_DEFINITION: u64 = 4;

/// One parsed source unit, keyed by its namespace display name.
struct SourceModule<'a> {
    module: &'a ModuleInput,
    tree: SyntaxTree,
}

fn parse_source_modules(
    modules: &[ModuleInput],
) -> Result<BTreeMap<String, SourceModule<'_>>, SourceTableSchemaError> {
    let mut sources = BTreeMap::new();
    for module in modules {
        let namespace = module_namespace(&module.logical_path)
            .ok_or(SourceTableSchemaError::InvalidModulePath)?;
        let parsed = parse_module(&module.source);
        if !parsed.is_ok() {
            return Err(SourceTableSchemaError::InvalidAnalysis);
        }
        if sources
            .insert(
                namespace,
                SourceModule {
                    module,
                    tree: parsed.value,
                },
            )
            .is_some()
        {
            return Err(SourceTableSchemaError::AmbiguousModule);
        }
    }
    Ok(sources)
}

/// The definition body one nominal declaration contributes to key 4.
enum NominalBody<'a> {
    Record(&'a [TypeMember]),
    Enum(&'a [EnumVariant]),
    Refined(&'a TypeExpr, &'a [TypeMember]),
}

/// Builds one closed storage schema descriptor (29-formats.md and
/// profiles/store-format-3.md §6) from checked semantics, parsed declarations
/// and persisted identities. Every nominal, unit and currency definition that
/// a type node reaches is emitted once under its ObjectId.
struct DescriptorBuilder<'a> {
    manifest: &'a ObjectIdentityManifest,
    analysis: &'a Analysis,
    sources: &'a BTreeMap<String, SourceModule<'a>>,
    database_id: Option<[u8; 16]>,
    definitions: BTreeMap<[u8; 16], (u64, Raw)>,
    pending: BTreeSet<[u8; 16]>,
    units: BTreeMap<SourceUnitIdentity, [u8; 16]>,
    /// Transparent aliases and stored-reference keys being expanded; reaching
    /// one again has no finite representation.
    expanding: BTreeSet<String>,
}

impl<'a> DescriptorBuilder<'a> {
    fn new(
        manifest: &'a ObjectIdentityManifest,
        analysis: &'a Analysis,
        sources: &'a BTreeMap<String, SourceModule<'a>>,
        database_id: Option<[u8; 16]>,
    ) -> Self {
        Self {
            manifest,
            analysis,
            sources,
            database_id,
            definitions: BTreeMap::new(),
            pending: BTreeSet::new(),
            units: BTreeMap::new(),
            expanding: BTreeSet::new(),
        }
    }

    fn table_descriptor(
        mut self,
        namespace: &Namespace,
        declaration_name: &str,
        table_id: [u8; 16],
        field_ids: &BTreeMap<String, [u8; 16]>,
        schema: &TableSchema,
        admission: &TableAdmission,
    ) -> Result<SchemaDescriptor, SourceTableSchemaError> {
        if !schema.conversions.is_empty()
            || admission.private_required
            || admission.keys.is_empty()
            || field_ids.len() != schema.fields.len()
            || admission
                .required
                .iter()
                .chain(&admission.computed)
                .any(|name| !schema.fields.contains_key(name))
            || admission
                .computed
                .iter()
                .any(|name| admission.required.contains(name))
        {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let mut ids = BTreeSet::from([table_id]);
        if field_ids.values().any(|id| !ids.insert(*id)) {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let mut key_names = BTreeSet::new();
        let mut key_ids = Vec::with_capacity(admission.keys.len());
        for (name, key_type) in &admission.keys {
            // A computed field is never a primary-key field (29-formats.md).
            if !key_names.insert(name.as_str())
                || schema.fields.get(name) != Some(key_type)
                || admission.computed.contains(name)
            {
                return Err(SourceTableSchemaError::InvalidSourceSchema);
            }
            key_ids.push(uuid_raw(
                field_ids
                    .get(name)
                    .ok_or(SourceTableSchemaError::MissingIdentity)?,
            ));
        }
        if admission.automatic_key
            && (admission.keys.len() != 1
                || admission.keys[0].0 != "id"
                || admission.keys[0].1 != Type::Int)
        {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let computed = self.computed_selector_digests(
            namespace,
            declaration_name,
            &admission.computed,
            &schema.fields,
        )?;
        let mut encoded_fields = Vec::with_capacity(schema.fields.len());
        for (name, ty) in &schema.fields {
            let id = field_ids
                .get(name)
                .ok_or(SourceTableSchemaError::MissingIdentity)?;
            // Required and defaulted fields are both stored (role 1): an
            // insert-time default is code in the schema revision, never a
            // `[1, value]` read fallback. A computed selector (role 2) stores
            // no value and records its expression digest (ORNA-FIELD-001..011).
            let (role, fallback) = if key_names.contains(name.as_str()) {
                (0_u64, no_fallback())
            } else if let Some(digest) = computed.get(name) {
                (
                    2,
                    Raw::Array(vec![Raw::Int(2.into()), Raw::Bytes(digest.to_vec())]),
                )
            } else {
                (1, no_fallback())
            };
            encoded_fields.push((
                *id,
                Raw::Array(vec![
                    uuid_raw(id),
                    Raw::Text(name.clone()),
                    self.type_node(ty, namespace)?,
                    Raw::Int(role.into()),
                    fallback,
                ]),
            ));
        }
        let fields = sorted_by_id(encoded_fields);
        let definitions = self.finish_definitions()?;
        let raw = Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid_raw(&table_id)),
            (Raw::Int(2.into()), Raw::Array(key_ids)),
            (Raw::Int(3.into()), fields),
            (Raw::Int(4.into()), Raw::Array(definitions)),
        ]);
        SchemaDescriptor::new(raw).map_err(|_| SourceTableSchemaError::InvalidDescriptor)
    }

    /// Digests every computed selector of the table declaration, which must
    /// declare exactly the computed fields that semantic analysis admitted.
    fn computed_selector_digests(
        &self,
        namespace: &Namespace,
        declaration_name: &str,
        computed: &BTreeSet<String>,
        fields: &BTreeMap<String, Type>,
    ) -> Result<BTreeMap<String, [u8; 32]>, SourceTableSchemaError> {
        let source = self.source(namespace)?;
        let mut tables = source
            .tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, members, .. } if name == declaration_name => {
                    Some(members)
                }
                _ => None,
            });
        let (Some(members), None) = (tables.next(), tables.next()) else {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        };
        let mut digests = BTreeMap::new();
        for member in members {
            if let TableMember::Field {
                name,
                initializer: Some(FieldInitializer::Computed(expression)),
                ..
            } = member
            {
                let digest = self.expression_digest(
                    COMPUTED_SELECTOR_DIGEST_DOMAIN,
                    namespace,
                    expression,
                    Some(fields),
                )?;
                if digests.insert(name.clone(), digest).is_some() {
                    return Err(SourceTableSchemaError::InvalidSourceSchema);
                }
            }
        }
        if !digests.keys().eq(computed.iter()) {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        Ok(digests)
    }

    /// Digests one expression declared in `namespace` under `domain`.
    /// `row_fields` are the table fields a computed selector reads as lexical
    /// selectors (ORNA-FIELD-009); refinement assertions have none.
    fn expression_digest(
        &self,
        domain: &str,
        namespace: &Namespace,
        expression: &Expr,
        row_fields: Option<&BTreeMap<String, Type>>,
    ) -> Result<[u8; 32], SourceTableSchemaError> {
        let mut references = ReferenceResolver {
            builder: self,
            namespace,
            row_fields,
            scopes: Vec::new(),
            resolved: BTreeMap::new(),
        };
        references.expr(expression)?;
        let source = self.source(namespace)?;
        let span = expression.span();
        let tokens = source
            .module
            .source
            .get(span.start..span.end)
            .and_then(|text| lex(text).ok())
            .ok_or(SourceTableSchemaError::InvalidSourceSchema)?;
        let mut replaced = 0_usize;
        let payload = Raw::Array(
            tokens
                .iter()
                .filter_map(|token| {
                    let reference = references.resolved.get(&(span.start + token.span.start));
                    match (&token.kind, reference) {
                        (TokenKind::Identifier { .. }, Some(reference)) => {
                            replaced += 1;
                            Some(reference.clone())
                        }
                        _ => token_raw(token),
                    }
                })
                .collect(),
        );
        // Every resolved reference must stand in for exactly one identifier.
        if replaced != references.resolved.len() {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        domain_digest(domain, &payload).map_err(|_| SourceTableSchemaError::InvalidDescriptor)
    }

    /// Encodes one declared type, spelled in `namespace`, as a closed type node.
    fn type_node(
        &mut self,
        ty: &Type,
        namespace: &Namespace,
    ) -> Result<Raw, SourceTableSchemaError> {
        let primitive = match ty {
            Type::Int => Some("Int"),
            Type::Decimal => Some("Decimal"),
            Type::Float => Some("Float"),
            Type::Date => Some("Date"),
            Type::Instant => Some("Instant"),
            Type::Text => Some("Str"),
            Type::Bool => Some("Bool"),
            Type::Unit => Some("Unit"),
            Type::Named(name) => match name.as_str() {
                "Blob" | "std.BINARY_LARGE_OBJECT" => Some("Blob"),
                "std.UUID" => Some("Uuid"),
                "std.TIME" => Some("TimeOfDay"),
                "std.DURATION" => Some("Duration"),
                _ => None,
            },
            _ => None,
        };
        if let Some(name) = primitive {
            return Ok(Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.into())]));
        }
        Ok(match ty {
            Type::List(element) => Raw::Array(vec![
                Raw::Int(1.into()),
                self.type_node(element, namespace)?,
            ]),
            Type::Optional(element) => Raw::Array(vec![
                Raw::Int(2.into()),
                self.type_node(element, namespace)?,
            ]),
            Type::Tuple(elements) => Raw::Array(vec![
                Raw::Int(3.into()),
                Raw::Array(
                    elements
                        .iter()
                        .map(|element| self.type_node(element, namespace))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            ]),
            // Descriptor order is declaration order and is NOT the stored order: stored record
            // fields are sorted by encoded field key (Orna-1.1.0.md:4015; 29-formats.md:46).
            // The text-rendering order at 29-formats.md:123 is a separate rule and does not
            // govern stored entries.
            Type::Record(fields) => Raw::Array(vec![
                Raw::Int(4.into()),
                Raw::Array(
                    fields
                        .iter()
                        .map(|(name, field)| {
                            Ok(Raw::Array(vec![
                                Raw::Text(name.clone()),
                                self.type_node(field, namespace)?,
                            ]))
                        })
                        .collect::<Result<Vec<_>, SourceTableSchemaError>>()?,
                ),
            ]),
            Type::Range(endpoint) => Raw::Array(vec![
                Raw::Int(9.into()),
                self.type_node(endpoint, namespace)?,
            ]),
            Type::Applied { base, arguments } => match (base.as_str(), arguments.as_slice()) {
                ("Float" | "Decimal", [Type::Named(unit)]) => Raw::Array(vec![
                    Raw::Int(7.into()),
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text(base.clone())]),
                    uuid_raw(&self.unit(namespace, unit)?),
                ]),
                ("Money", [Type::Named(currency)]) => self.currency_node(namespace, currency)?,
                _ => return Err(SourceTableSchemaError::UnsupportedType),
            },
            Type::Named(name) => self.named_node(namespace, name)?,
            _ => return Err(SourceTableSchemaError::UnsupportedType),
        })
    }

    /// Resolves a unit spelling exactly as semantic analysis does: a qualified
    /// path names its declaring namespace; a bare name prefers the current
    /// module, then the one source unit with that name.
    fn unit(
        &mut self,
        namespace: &Namespace,
        spelling: &str,
    ) -> Result<[u8; 16], SourceTableSchemaError> {
        let analysis = self.analysis;
        let identity = match spelling.rsplit_once('.') {
            Some((unit_namespace, name)) => SourceUnitIdentity {
                namespace: Namespace(unit_namespace.split('.').map(str::to_owned).collect()),
                name: name.to_owned(),
            },
            None => {
                let local = SourceUnitIdentity {
                    namespace: namespace.clone(),
                    name: spelling.to_owned(),
                };
                if analysis.source_units.contains_key(&local) {
                    local
                } else {
                    let mut candidates = analysis
                        .source_units
                        .keys()
                        .filter(|identity| identity.name == spelling);
                    match (candidates.next(), candidates.next()) {
                        (Some(identity), None) => identity.clone(),
                        _ => return Err(SourceTableSchemaError::UnsupportedType),
                    }
                }
            }
        };
        if !analysis.source_units.contains_key(&identity) {
            return Err(SourceTableSchemaError::UnsupportedType);
        }
        let id = *self
            .manifest
            .id(
                ObjectKind::Unit,
                &qualify(&identity.namespace.display(), &identity.name),
            )
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        self.units.insert(identity, id);
        Ok(id)
    }

    fn named_node(
        &mut self,
        namespace: &Namespace,
        spelling: &str,
    ) -> Result<Raw, SourceTableSchemaError> {
        let (target, name) = self.resolve(namespace, spelling)?;
        let symbol = self.symbol(&target, &name)?;
        match symbol.kind {
            SymbolKind::Table => self.reference_node(&target, &name, symbol),
            SymbolKind::Type | SymbolKind::Enum => self.declared_type_node(&target, &name),
            _ => Err(SourceTableSchemaError::UnsupportedType),
        }
    }

    /// A table type used as a field is a stored reference: the owning
    /// database, the table ObjectId and the target's complete key shape, never
    /// an embedded row (ORNA-REF-001, ORNA-KEY-005).
    fn reference_node(
        &mut self,
        namespace: &Namespace,
        name: &str,
        symbol: &'a Symbol,
    ) -> Result<Raw, SourceTableSchemaError> {
        let database_id = self
            .database_id
            .ok_or(SourceTableSchemaError::MissingDatabaseIdentity)?;
        let qualified = qualify(&namespace.display(), name);
        let table_id = *self
            .manifest
            .id(ObjectKind::Table, &qualified)
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        let keys = symbol
            .table_schema
            .as_ref()
            .and_then(|schema| schema.admission.as_ref())
            .map(|admission| &admission.keys)
            .filter(|keys| !keys.is_empty())
            .ok_or(SourceTableSchemaError::InvalidSourceSchema)?;
        if !self.expanding.insert(qualified.clone()) {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let key_nodes = keys
            .iter()
            .map(|(_, key_type)| self.type_node(key_type, namespace))
            .collect::<Result<Vec<_>, _>>();
        self.expanding.remove(&qualified);
        Ok(Raw::Array(vec![
            Raw::Int(6.into()),
            uuid_raw(&database_id),
            uuid_raw(&table_id),
            Raw::Array(key_nodes?),
        ]))
    }

    fn declared_type_node(
        &mut self,
        namespace: &Namespace,
        name: &str,
    ) -> Result<Raw, SourceTableSchemaError> {
        match self.declaration(namespace, name)? {
            // An uninstantiated generic declaration has no closed type node.
            Declaration::Type { generics, .. } | Declaration::Enum { generics, .. }
                if !generics.is_empty() =>
            {
                Err(SourceTableSchemaError::UnsupportedType)
            }
            Declaration::Type {
                representation: TypeRepresentation::Alias { ty, refinements },
                ..
            } if refinements.is_empty() => {
                // A transparent alias lowers to its target (ORNA-NOMINAL-001).
                let qualified = qualify(&namespace.display(), name);
                if !self.expanding.insert(qualified.clone()) {
                    return Err(SourceTableSchemaError::InvalidSourceSchema);
                }
                let node = self.type_node(&declared_type(ty), namespace);
                self.expanding.remove(&qualified);
                node
            }
            Declaration::Type {
                representation: TypeRepresentation::Alias { ty, refinements },
                ..
            } => self.nominal_node(namespace, name, NominalBody::Refined(ty, refinements)),
            Declaration::Type {
                representation: TypeRepresentation::Nominal { members },
                ..
            } => self.nominal_node(namespace, name, NominalBody::Record(members)),
            Declaration::Enum { variants, .. } => {
                self.nominal_node(namespace, name, NominalBody::Enum(variants))
            }
            _ => Err(SourceTableSchemaError::UnsupportedType),
        }
    }

    fn nominal_node(
        &mut self,
        namespace: &Namespace,
        name: &str,
        body: NominalBody<'a>,
    ) -> Result<Raw, SourceTableSchemaError> {
        let qualified = qualify(&namespace.display(), name);
        let id = *self
            .manifest
            .id(ObjectKind::Type, &qualified)
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        let kind = match body {
            NominalBody::Record(_) => RECORD_DEFINITION,
            NominalBody::Enum(_) => ENUM_DEFINITION,
            NominalBody::Refined(..) => REFINED_DEFINITION,
        };
        let node = Raw::Array(vec![Raw::Int(5.into()), uuid_raw(&id)]);
        match self.definitions.get(&id) {
            Some((existing, _)) if *existing == kind => return Ok(node),
            Some(_) => return Err(SourceTableSchemaError::InvalidSourceSchema),
            None => {}
        }
        // A recursive use while this definition is being built is a reference
        // by ID; the descriptor validator checks finite constructibility.
        if !self.pending.insert(id) {
            return Ok(node);
        }
        let encoded = match body {
            NominalBody::Record(members) => self.record_body(namespace, &qualified, members)?,
            NominalBody::Enum(variants) => self.enum_body(namespace, &qualified, variants)?,
            NominalBody::Refined(base, members) => self.refined_body(namespace, base, members)?,
        };
        self.pending.remove(&id);
        let definition = Raw::Array(vec![uuid_raw(&id), Raw::Int(kind.into()), encoded]);
        if self.definitions.insert(id, (kind, definition)).is_some() {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        Ok(node)
    }

    /// Every declared field, public or private, belongs to the stored
    /// representation of a nominal record (29-formats.md definition kind 0).
    fn record_body(
        &mut self,
        namespace: &Namespace,
        owner: &str,
        members: &'a [TypeMember],
    ) -> Result<Raw, SourceTableSchemaError> {
        let mut fields = Vec::new();
        for member in members {
            let TypeMember::Field { name, ty, .. } = member else {
                continue;
            };
            let id = *self
                .manifest
                .id(ObjectKind::TypeField, &format!("{owner}.{name}"))
                .ok_or(SourceTableSchemaError::MissingIdentity)?;
            let node = self.type_node(&declared_type(ty), namespace)?;
            fields.push((id, member_descriptor(&id, name, node)));
        }
        Ok(sorted_by_id(fields))
    }

    /// Variants `[variant_id, name, payload_fields]` in variant-ID order
    /// (29-formats.md definition kind 1).
    fn enum_body(
        &mut self,
        namespace: &Namespace,
        owner: &str,
        variants: &'a [EnumVariant],
    ) -> Result<Raw, SourceTableSchemaError> {
        let mut encoded = Vec::with_capacity(variants.len());
        for variant in variants {
            let qualified = format!("{owner}.{}", variant.name);
            let id = *self
                .manifest
                .id(ObjectKind::Variant, &qualified)
                .ok_or(SourceTableSchemaError::MissingIdentity)?;
            let mut payload = Vec::with_capacity(variant.fields.len());
            for field in &variant.fields {
                let field_id = *self
                    .manifest
                    .id(
                        ObjectKind::VariantField,
                        &format!("{qualified}.{}", field.name),
                    )
                    .ok_or(SourceTableSchemaError::MissingIdentity)?;
                let node = self.type_node(&declared_type(&field.ty), namespace)?;
                payload.push((field_id, member_descriptor(&field_id, &field.name, node)));
            }
            encoded.push((
                id,
                Raw::Array(vec![
                    uuid_raw(&id),
                    Raw::Text(variant.name.clone()),
                    sorted_by_id(payload),
                ]),
            ));
        }
        Ok(sorted_by_id(encoded))
    }

    /// A refined nominal type is its base representation plus the digests of
    /// its declaration assertions in source order (ORNA-NOMINAL-003,
    /// ORNA-REFINE-001).
    fn refined_body(
        &mut self,
        namespace: &Namespace,
        base: &TypeExpr,
        members: &'a [TypeMember],
    ) -> Result<Raw, SourceTableSchemaError> {
        let base = self.type_node(&declared_type(base), namespace)?;
        let digests = members
            .iter()
            .filter_map(|member| match member {
                TypeMember::Assertion { value, .. } => Some(value),
                _ => None,
            })
            .map(|value| {
                self.expression_digest(REFINEMENT_ASSERTION_DIGEST_DOMAIN, namespace, value, None)
                    .map(|digest| Raw::Bytes(digest.to_vec()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Raw::Array(vec![base, Raw::Array(digests)]))
    }

    /// `Money<C>` names a nominal type with one nested `Currency`
    /// implementation whose static code and minor digits are literals
    /// (ORNA-MONEY-004); its definition is currency kind 4.
    fn currency_node(
        &mut self,
        namespace: &Namespace,
        spelling: &str,
    ) -> Result<Raw, SourceTableSchemaError> {
        let (target, name) = self.resolve(namespace, spelling)?;
        if self.symbol(&target, &name)?.kind != SymbolKind::Type {
            return Err(SourceTableSchemaError::UnsupportedType);
        }
        let Declaration::Type {
            generics,
            representation: TypeRepresentation::Nominal { members },
            ..
        } = self.declaration(&target, &name)?
        else {
            return Err(SourceTableSchemaError::UnsupportedType);
        };
        if !generics.is_empty() {
            return Err(SourceTableSchemaError::UnsupportedType);
        }
        let mut implementations = members.iter().filter_map(|member| match member {
            TypeMember::Implementation { implementation, .. }
                if matches!(
                    &implementation.protocol,
                    TypeExpr::Name { path, arguments, .. }
                        if path.as_slice() == ["Currency"] && arguments.is_empty()
                ) =>
            {
                Some(implementation.members.as_slice())
            }
            _ => None,
        });
        let (Some(statics), None) = (implementations.next(), implementations.next()) else {
            return Err(SourceTableSchemaError::UnsupportedType);
        };
        let code = currency_code(currency_static(statics, "code")?)?;
        let minor_digits = currency_minor_digits(currency_static(statics, "minor_digits")?)?;
        let id = *self
            .manifest
            .id(ObjectKind::Type, &qualify(&target.display(), &name))
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        let definition = Raw::Array(vec![
            uuid_raw(&id),
            Raw::Int(CURRENCY_DEFINITION.into()),
            Raw::Array(vec![Raw::Text(code), Raw::Int(minor_digits)]),
        ]);
        if self.pending.contains(&id)
            || self
                .definitions
                .get(&id)
                .is_some_and(|(_, existing)| existing != &definition)
        {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        self.definitions
            .insert(id, (CURRENCY_DEFINITION, definition));
        Ok(Raw::Array(vec![Raw::Int(8.into()), uuid_raw(&id)]))
    }

    /// Resolves a declared spelling in `namespace` to its defining module and
    /// declaration through the module's own declarations and imports
    /// (ORNA-IMPORT-001..005). A qualified spelling is either the canonical
    /// module-qualified identity semantic analysis gives a nominal
    /// declaration (`shop.Price`), or starts at an imported namespace binding
    /// and names a public declaration. Analysis already rejected ambiguity,
    /// so any remaining doubt fails instead of choosing.
    fn resolve(
        &self,
        namespace: &Namespace,
        spelling: &str,
    ) -> Result<(Namespace, String), SourceTableSchemaError> {
        let analysis = self.analysis;
        let declares = |module: &Namespace, name: &str| {
            analysis
                .modules
                .get(module)
                .is_some_and(|header| header.symbols.contains_key(name))
        };
        if let Some((binding, member)) = spelling.split_once('.') {
            if let Some((qualifier, name)) = spelling.rsplit_once('.') {
                let target = Namespace(qualifier.split('.').map(str::to_owned).collect());
                if analysis
                    .modules
                    .get(&target)
                    .and_then(|header| header.symbols.get(name))
                    .is_some_and(|symbol| symbol.ty == Type::Named(spelling.to_owned()))
                {
                    return Ok((target, name.to_owned()));
                }
            }
            let mut target = self.module_binding(namespace, binding)?;
            let name = match member.rsplit_once('.') {
                Some((nested, name)) => {
                    target.0.extend(nested.split('.').map(str::to_owned));
                    name
                }
                None => member,
            };
            return if analysis
                .modules
                .get(&target)
                .is_some_and(|header| header.exports.contains_key(name))
            {
                Ok((target, name.to_owned()))
            } else {
                Err(SourceTableSchemaError::UnsupportedType)
            };
        }
        if declares(namespace, spelling) {
            return Ok((namespace.clone(), spelling.to_owned()));
        }
        let mut explicit = BTreeSet::new();
        let mut glob = BTreeSet::new();
        for (target, tail) in self.imports(namespace) {
            let Some(header) = analysis.modules.get(&target) else {
                continue;
            };
            if !header.exports.contains_key(spelling) {
                continue;
            }
            match tail {
                UseTail::Names(names) if names.iter().any(|import| import.name == spelling) => {
                    explicit.insert(target);
                }
                UseTail::Alias { name, .. }
                    if name == "_" && header.prelude_exports.contains(spelling) =>
                {
                    explicit.insert(target);
                }
                UseTail::Glob { .. } => {
                    glob.insert(target);
                }
                _ => {}
            }
        }
        let mut candidates = if explicit.is_empty() { glob } else { explicit }.into_iter();
        match (candidates.next(), candidates.next()) {
            (Some(target), None) => Ok((target, spelling.to_owned())),
            (Some(_), Some(_)) => Err(SourceTableSchemaError::InvalidSourceSchema),
            (None, _) => Err(SourceTableSchemaError::UnsupportedType),
        }
    }

    /// The namespace bound to `binding` by `use a.b;` or `use a.b as x;`.
    fn module_binding(
        &self,
        namespace: &Namespace,
        binding: &str,
    ) -> Result<Namespace, SourceTableSchemaError> {
        let mut targets = self
            .imports(namespace)
            .into_iter()
            .filter_map(|(target, tail)| {
                let bound = match tail {
                    UseTail::None => target.0.last().is_some_and(|last| last == binding),
                    UseTail::Alias { name, .. } => name != "_" && name == binding,
                    UseTail::Glob { .. } | UseTail::Names(_) => false,
                };
                bound.then_some(target)
            })
            .collect::<BTreeSet<_>>()
            .into_iter();
        match (targets.next(), targets.next()) {
            (Some(target), None) => Ok(target),
            (Some(_), Some(_)) => Err(SourceTableSchemaError::InvalidSourceSchema),
            (None, _) => Err(SourceTableSchemaError::UnsupportedType),
        }
    }

    fn imports(&self, namespace: &Namespace) -> Vec<(Namespace, &'a UseTail)> {
        let sources = self.sources;
        sources
            .get(&namespace.display())
            .map_or_else(Vec::new, |source| {
                source
                    .tree
                    .items
                    .iter()
                    .filter_map(|item| match &item.declaration {
                        Declaration::Use { path, tail } => Some((
                            Namespace(path.iter().map(|segment| segment.name.clone()).collect()),
                            tail,
                        )),
                        _ => None,
                    })
                    .collect()
            })
    }

    fn declaration(
        &self,
        namespace: &Namespace,
        name: &str,
    ) -> Result<&'a Declaration, SourceTableSchemaError> {
        let source = self.source(namespace)?;
        let mut matches = source
            .tree
            .items
            .iter()
            .map(|item| &item.declaration)
            .filter(|declaration| {
                matches!(
                    declaration,
                    Declaration::Type { name: candidate, .. }
                        | Declaration::Enum { name: candidate, .. }
                        if candidate == name
                )
            });
        match (matches.next(), matches.next()) {
            (Some(declaration), None) => Ok(declaration),
            _ => Err(SourceTableSchemaError::InvalidSourceSchema),
        }
    }

    fn source(
        &self,
        namespace: &Namespace,
    ) -> Result<&'a SourceModule<'a>, SourceTableSchemaError> {
        let sources = self.sources;
        sources
            .get(&namespace.display())
            .ok_or(SourceTableSchemaError::ModuleNotFound)
    }

    fn symbol(
        &self,
        namespace: &Namespace,
        name: &str,
    ) -> Result<&'a Symbol, SourceTableSchemaError> {
        let analysis = self.analysis;
        analysis
            .modules
            .get(namespace)
            .and_then(|module| module.symbols.get(name))
            .ok_or(SourceTableSchemaError::UnsupportedType)
    }

    /// Emits unit definitions over the dimension basis shared by every unit
    /// this descriptor reached, then every definition in type-ID order.
    fn finish_definitions(mut self) -> Result<Vec<Raw>, SourceTableSchemaError> {
        if !self.pending.is_empty() {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let analysis = self.analysis;
        let units = self
            .units
            .iter()
            .map(|(identity, id)| {
                analysis
                    .source_units
                    .get(identity)
                    .map(|unit| (unit, *id))
                    .ok_or(SourceTableSchemaError::InvalidSourceSchema)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let dimension_basis = units
            .iter()
            .flat_map(|(unit, _)| unit.dimension_basis.keys())
            .collect::<BTreeSet<_>>();
        for (unit, id) in units {
            let exponents = dimension_basis
                .iter()
                .map(|dimension| {
                    Raw::Int(BigInt::from(
                        unit.dimension_basis
                            .get(*dimension)
                            .copied()
                            .unwrap_or_default(),
                    ))
                })
                .collect();
            let body = Raw::Array(vec![
                Raw::Array(exponents),
                source_unit_integer(&unit.scale.numerator)?,
                source_unit_integer(&unit.scale.denominator)?,
                source_unit_integer(&unit.offset.numerator)?,
                source_unit_integer(&unit.offset.denominator)?,
                Raw::Bool(unit.affine),
            ]);
            let definition =
                Raw::Array(vec![uuid_raw(&id), Raw::Int(UNIT_DEFINITION.into()), body]);
            if self
                .definitions
                .insert(id, (UNIT_DEFINITION, definition))
                .is_some()
            {
                return Err(SourceTableSchemaError::InvalidSourceSchema);
            }
        }
        Ok(self
            .definitions
            .into_values()
            .map(|(_, definition)| definition)
            .collect())
    }
}

/// Finds the free references of one expression and what they resolve to,
/// keyed by the source offset of their identifier token. Local bindings shadow
/// row fields, which shadow module declarations and imports (ORNA-FIELD-009);
/// a name that resolves to none of these is a core name and stays spelled.
struct ReferenceResolver<'r, 'a> {
    builder: &'r DescriptorBuilder<'a>,
    namespace: &'r Namespace,
    row_fields: Option<&'r BTreeMap<String, Type>>,
    scopes: Vec<BTreeSet<String>>,
    resolved: BTreeMap<usize, Raw>,
}

impl ReferenceResolver<'_, '_> {
    fn bound(&self, name: &str) -> bool {
        name == "self"
            || self.scopes.iter().any(|scope| scope.contains(name))
            || self
                .row_fields
                .is_some_and(|fields| fields.contains_key(name))
    }

    fn value_name(&mut self, name: &str, start: usize) -> Result<(), SourceTableSchemaError> {
        if self.bound(name) {
            return Ok(());
        }
        self.declared_name(name, start)
    }

    /// Records the declaration or module a free spelling resolves to.
    fn declared_name(&mut self, name: &str, start: usize) -> Result<(), SourceTableSchemaError> {
        let reference = match self.builder.resolve(self.namespace, name) {
            Ok((namespace, name)) => Raw::Array(vec![
                Raw::Text("declaration".into()),
                Raw::Text(namespace.display()),
                Raw::Text(name),
            ]),
            Err(SourceTableSchemaError::UnsupportedType) => {
                match self.builder.module_binding(self.namespace, name) {
                    Ok(namespace) => Raw::Array(vec![
                        Raw::Text("module".into()),
                        Raw::Text(namespace.display()),
                    ]),
                    Err(SourceTableSchemaError::UnsupportedType) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        self.resolved.insert(start, reference);
        Ok(())
    }

    fn scoped(
        &mut self,
        names: BTreeSet<String>,
        body: impl FnOnce(&mut Self) -> Result<(), SourceTableSchemaError>,
    ) -> Result<(), SourceTableSchemaError> {
        self.scopes.push(names);
        let result = body(self);
        self.scopes.pop();
        result
    }

    fn expr(&mut self, expression: &Expr) -> Result<(), SourceTableSchemaError> {
        match expression {
            Expr::Name { text, span } => self.value_name(text, span.start),
            Expr::Literal { .. } | Expr::ReplBinding { .. } => Ok(()),
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let StringSegment::Expression { value, .. } = segment {
                        self.expr(value)?;
                    }
                }
                Ok(())
            }
            Expr::Unary { rhs, .. } => self.expr(rhs),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index {
                base: lhs,
                index: rhs,
                ..
            } => {
                self.expr(lhs)?;
                self.expr(rhs)
            }
            Expr::Range { lower, upper, .. } => {
                for bound in [lower, upper].into_iter().flatten() {
                    self.expr(bound)?;
                }
                Ok(())
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                self.expr(callee)?;
                self.arguments(arguments)
            }
            Expr::GenericCall {
                callee,
                type_arguments,
                arguments,
                ..
            } => {
                self.expr(callee)?;
                for argument in type_arguments {
                    self.type_expr(argument)?;
                }
                self.arguments(arguments)
            }
            // A member name after `.` is resolved by the base value's type,
            // not by scope; only the base is a reference.
            Expr::Field { base, .. } => self.expr(base),
            Expr::Group { inner, .. } => self.expr(inner),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    self.expr(element)?;
                }
                Ok(())
            }
            Expr::Record { fields, .. } => self.record_fields(fields),
            Expr::Nominal { path, fields, .. } => {
                if let Some(first) = path.first() {
                    self.declared_name(&first.text, first.span.start)?;
                }
                self.record_fields(fields)
            }
            Expr::Lambda {
                parameters, body, ..
            } => {
                let mut names = BTreeSet::new();
                for parameter in parameters {
                    if let Some(annotation) = &parameter.annotation {
                        self.type_expr(annotation)?;
                    }
                    self.pattern(&parameter.pattern, &mut names)?;
                }
                self.scoped(names, |resolver| resolver.expr(body))
            }
            Expr::Block {
                statements, tail, ..
            } => self.scoped(BTreeSet::new(), |resolver| {
                for statement in statements {
                    resolver.statement(statement)?;
                }
                tail.as_deref().map_or(Ok(()), |tail| resolver.expr(tail))
            }),
            Expr::Control {
                binding,
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                if let Some(condition) = condition {
                    self.expr(condition)?;
                }
                let mut names = BTreeSet::new();
                if let Some(binding) = binding {
                    self.pattern(binding, &mut names)?;
                }
                if let Some(body) = body {
                    self.scoped(names, |resolver| resolver.expr(body))?;
                }
                for arm in arms {
                    let mut names = BTreeSet::new();
                    self.pattern(&arm.pattern, &mut names)?;
                    self.scoped(names, |resolver| {
                        if let Some(guard) = &arm.guard {
                            resolver.expr(guard)?;
                        }
                        resolver.expr(&arm.body)
                    })?;
                }
                alternate
                    .as_deref()
                    .map_or(Ok(()), |alternate| self.expr(alternate))
            }
        }
    }

    fn statement(&mut self, statement: &Statement) -> Result<(), SourceTableSchemaError> {
        match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                ..
            } => {
                if let Some(annotation) = annotation {
                    self.type_expr(annotation)?;
                }
                self.expr(value)?;
                let mut names = BTreeSet::new();
                self.pattern(pattern, &mut names)?;
                if let Some(scope) = self.scopes.last_mut() {
                    scope.extend(names);
                }
                Ok(())
            }
            Statement::Assert { value, .. }
            | Statement::Expression { value, .. }
            | Statement::Control { value, .. } => self.expr(value),
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                value.as_ref().map_or(Ok(()), |value| self.expr(value))
            }
            Statement::Continue { .. } => Ok(()),
            Statement::Assignment { target, value, .. } => {
                self.assignment_target(target)?;
                self.expr(value)
            }
        }
    }

    fn assignment_target(
        &mut self,
        target: &AssignmentTarget,
    ) -> Result<(), SourceTableSchemaError> {
        match target {
            AssignmentTarget::Name { name, span } => self.value_name(name, span.start),
            AssignmentTarget::Field { base, .. } => self.assignment_target(base),
            AssignmentTarget::Index { base, index, .. } => {
                self.assignment_target(base)?;
                self.expr(index)
            }
        }
    }

    fn arguments(&mut self, arguments: &[Argument]) -> Result<(), SourceTableSchemaError> {
        for argument in arguments {
            self.expr(&argument.value)?;
        }
        Ok(())
    }

    fn record_fields(&mut self, fields: &[RecordField]) -> Result<(), SourceTableSchemaError> {
        for field in fields {
            self.expr(&field.value)?;
        }
        Ok(())
    }

    /// Collects the names a pattern binds and resolves its constructor paths.
    fn pattern(
        &mut self,
        pattern: &Pattern,
        names: &mut BTreeSet<String>,
    ) -> Result<(), SourceTableSchemaError> {
        match pattern {
            Pattern::Name(name, _) => {
                names.insert(name.clone());
                Ok(())
            }
            Pattern::Wildcard(_) | Pattern::Literal { .. } => Ok(()),
            Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
                for element in elements {
                    self.pattern(element, names)?;
                }
                Ok(())
            }
            Pattern::Record { fields, .. } => {
                for (name, pattern, _) in fields {
                    match pattern {
                        Some(pattern) => self.pattern(pattern, names)?,
                        None => {
                            names.insert(name.clone());
                        }
                    }
                }
                Ok(())
            }
            Pattern::Constructor {
                path,
                arguments,
                fields,
                ..
            } => {
                if let Some(first) = path.first() {
                    self.declared_name(&first.text, first.span.start)?;
                }
                for argument in arguments {
                    self.pattern(argument, names)?;
                }
                for field in fields {
                    match &field.pattern {
                        Some(pattern) => self.pattern(pattern, names)?,
                        None => {
                            names.insert(field.name.clone());
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Type names are never row fields or local bindings.
    fn type_expr(&mut self, ty: &TypeExpr) -> Result<(), SourceTableSchemaError> {
        match ty {
            TypeExpr::Name {
                path,
                arguments,
                span,
            } => {
                if let Some(first) = path.first() {
                    self.declared_name(first, span.start)?;
                }
                for argument in arguments {
                    self.type_expr(argument)?;
                }
                Ok(())
            }
            TypeExpr::Optional { inner, .. } | TypeExpr::List { inner, .. } => {
                self.type_expr(inner)
            }
            TypeExpr::Product { lhs, rhs, .. } => {
                self.type_expr(lhs)?;
                self.type_expr(rhs)
            }
            TypeExpr::Record { fields, .. } => {
                for (_, field, _) in fields {
                    self.type_expr(field)?;
                }
                Ok(())
            }
            TypeExpr::Tuple { elements, .. } => {
                for element in elements {
                    self.type_expr(element)?;
                }
                Ok(())
            }
            TypeExpr::Function {
                parameters, result, ..
            } => {
                for parameter in parameters {
                    self.type_expr(parameter)?;
                }
                self.type_expr(result)
            }
        }
    }
}

fn token_raw(token: &Token) -> Option<Raw> {
    let (class, spelling) = match &token.kind {
        TokenKind::Identifier { normalized } => ("identifier", normalized.as_str()),
        TokenKind::Keyword(keyword) => ("keyword", keyword.spelling()),
        TokenKind::Integer => ("integer", token.text.as_str()),
        TokenKind::Decimal => ("decimal", token.text.as_str()),
        TokenKind::Float => ("float", token.text.as_str()),
        TokenKind::Date => ("date", token.text.as_str()),
        TokenKind::Instant => ("instant", token.text.as_str()),
        TokenKind::String => ("string", token.text.as_str()),
        TokenKind::StringStart => ("string-start", token.text.as_str()),
        TokenKind::StringText => ("string-text", token.text.as_str()),
        TokenKind::InterpolationStart => ("interpolation-start", token.text.as_str()),
        TokenKind::InterpolationEnd => ("interpolation-end", token.text.as_str()),
        TokenKind::StringEnd => ("string-end", token.text.as_str()),
        TokenKind::ReplBinding => ("repl-binding", token.text.as_str()),
        TokenKind::Punct(punctuation) => ("punctuation", *punctuation),
        TokenKind::Eof => return None,
    };
    Some(Raw::Array(vec![
        Raw::Text(class.into()),
        Raw::Text(spelling.into()),
    ]))
}

fn currency_static<'e>(
    members: &'e [ImplMember],
    name: &str,
) -> Result<&'e Expr, SourceTableSchemaError> {
    let mut values = members.iter().filter_map(|member| match member {
        ImplMember::Static {
            name: candidate,
            value,
            ..
        } if candidate == name => Some(value),
        _ => None,
    });
    match (values.next(), values.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(SourceTableSchemaError::UnsupportedType),
    }
}

/// The descriptor never evaluates source, so a currency code must be an
/// escape-free string literal.
fn currency_code(value: &Expr) -> Result<String, SourceTableSchemaError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = value
    else {
        return Err(SourceTableSchemaError::UnsupportedType);
    };
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .filter(|code| !code.contains(['\\', '"']))
        .map(str::to_owned)
        .ok_or(SourceTableSchemaError::UnsupportedType)
}

fn currency_minor_digits(value: &Expr) -> Result<BigInt, SourceTableSchemaError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = value
    else {
        return Err(SourceTableSchemaError::UnsupportedType);
    };
    let digits = text.replace('_', "");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SourceTableSchemaError::UnsupportedType);
    }
    BigInt::parse_bytes(digits.as_bytes(), 10).ok_or(SourceTableSchemaError::UnsupportedType)
}

fn member_descriptor(id: &[u8; 16], name: &str, type_node: Raw) -> Raw {
    Raw::Array(vec![
        uuid_raw(id),
        Raw::Text(name.to_owned()),
        type_node,
        Raw::Int(1.into()),
        no_fallback(),
    ])
}

fn no_fallback() -> Raw {
    Raw::Array(vec![Raw::Int(0.into())])
}

fn sorted_by_id(mut entries: Vec<([u8; 16], Raw)>) -> Raw {
    entries.sort_by_key(|(id, _)| *id);
    Raw::Array(entries.into_iter().map(|(_, entry)| entry).collect())
}

fn source_unit_integer(value: &str) -> Result<Raw, SourceTableSchemaError> {
    BigInt::parse_bytes(value.as_bytes(), 10)
        .map(Raw::Int)
        .ok_or(SourceTableSchemaError::InvalidSourceSchema)
}

fn uuid_raw(id: &[u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
}

fn temporary_path(parent: &Path, manifest: &Path) -> PathBuf {
    let name = manifest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("object-identities.cbor");
    parent.join(format!(".{name}.{}.tmp", Uuid::new_v4()))
}

fn apply_explicit_renames(
    records: &mut [ObjectIdentity],
    active: &BTreeSet<(ObjectKind, String)>,
    renames: &[ObjectIdentityRename],
) -> Result<(), ObjectIdentityError> {
    let mut moves = BTreeMap::new();
    let mut targets = BTreeSet::new();
    for rename in renames {
        let from = rename.old_qualified_name.clone();
        let to = rename.new_qualified_name.clone();
        if from.is_empty() || to.is_empty() || from == to {
            return Err(ObjectIdentityError::InvalidSource);
        }
        let source = (rename.kind, from);
        let target = (rename.kind, to);
        if !records.iter().any(|record| {
            !record.retired && record.kind == source.0 && record.qualified_name == source.1
        }) || !active.contains(&target)
        {
            return Err(ObjectIdentityError::InvalidSource);
        }
        if moves.insert(source, target.1.clone()).is_some() || !targets.insert(target) {
            return Err(ObjectIdentityError::IdentityCollision);
        }
    }

    for record in records.iter_mut().filter(|record| !record.retired) {
        if let Some(new_name) = moves.get(&(record.kind, record.qualified_name.clone())) {
            record.qualified_name.clone_from(new_name);
        }
    }

    // An explicitly renamed owner carries its member identities with it unless
    // a member has its own explicit rename (ORNA-OBJECT-003). `nesting` is the
    // number of dots a member name has below its owner.
    for rename in renames {
        let members: &[(ObjectKind, usize)] = match rename.kind {
            ObjectKind::Table => &[(ObjectKind::Field, 0)],
            ObjectKind::Type => &[
                (ObjectKind::TypeField, 0),
                (ObjectKind::Variant, 0),
                (ObjectKind::VariantField, 1),
            ],
            ObjectKind::Variant => &[(ObjectKind::VariantField, 0)],
            ObjectKind::Field
            | ObjectKind::Unit
            | ObjectKind::TypeField
            | ObjectKind::VariantField => &[],
        };
        let old_prefix = format!("{}.", rename.old_qualified_name);
        let new_prefix = format!("{}.", rename.new_qualified_name);
        for &(kind, nesting) in members {
            let explicit_targets = renames
                .iter()
                .filter(|member| member.kind == kind)
                .map(|member| member.new_qualified_name.as_str())
                .collect::<BTreeSet<_>>();
            for record in records
                .iter_mut()
                .filter(|record| !record.retired && record.kind == kind)
            {
                let Some(member) = record.qualified_name.strip_prefix(&old_prefix) else {
                    continue;
                };
                if member.matches('.').count() != nesting {
                    continue;
                }
                let target = format!("{new_prefix}{member}");
                if !explicit_targets.contains(target.as_str())
                    && active.contains(&(kind, target.clone()))
                {
                    record.qualified_name = target;
                }
            }
        }
    }
    Ok(())
}

fn source_identity_keys(
    modules: &[ModuleInput],
) -> Result<Vec<(ObjectKind, String)>, ObjectIdentityError> {
    let mut keys = Vec::new();
    for module in modules {
        let namespace =
            module_namespace(&module.logical_path).ok_or(ObjectIdentityError::InvalidSource)?;
        let parsed = parse_module(&module.source);
        if !parsed.is_ok() {
            return Err(ObjectIdentityError::InvalidSource);
        }
        for item in &parsed.value.items {
            match &item.declaration {
                Declaration::Unit { name, .. } => {
                    keys.push((ObjectKind::Unit, qualify(&namespace, name)));
                }
                Declaration::Table {
                    name,
                    keys: table_keys,
                    members,
                    ..
                } => {
                    let table = qualify(&namespace, name);
                    keys.push((ObjectKind::Table, table.clone()));
                    if table_keys.is_empty() {
                        keys.push((ObjectKind::Field, format!("{table}.id")));
                    } else {
                        for key in table_keys {
                            if let Pattern::Name(name, _) = &key.pattern {
                                keys.push((ObjectKind::Field, format!("{table}.{name}")));
                            } else {
                                return Err(ObjectIdentityError::InvalidSource);
                            }
                        }
                    }
                    for member in members {
                        if let TableMember::Field { name, .. } = member {
                            keys.push((ObjectKind::Field, format!("{table}.{name}")));
                        }
                    }
                }
                Declaration::Type {
                    name,
                    representation,
                    ..
                } => {
                    let owner = qualify(&namespace, name);
                    match representation {
                        TypeRepresentation::Nominal { members } => {
                            for member in members {
                                if let TypeMember::Field { name, .. } = member {
                                    keys.push((ObjectKind::TypeField, format!("{owner}.{name}")));
                                }
                            }
                            keys.push((ObjectKind::Type, owner));
                        }
                        // A transparent alias has no runtime or equality
                        // identity; a refined alias is a nominal type
                        // (ORNA-NOMINAL-001, ORNA-NOMINAL-003).
                        TypeRepresentation::Alias { refinements, .. }
                            if !refinements.is_empty() =>
                        {
                            keys.push((ObjectKind::Type, owner));
                        }
                        TypeRepresentation::Alias { .. } => {}
                    }
                }
                Declaration::Enum { name, variants, .. } => {
                    let owner = qualify(&namespace, name);
                    for variant in variants {
                        let variant_name = format!("{owner}.{}", variant.name);
                        for field in &variant.fields {
                            keys.push((
                                ObjectKind::VariantField,
                                format!("{variant_name}.{}", field.name),
                            ));
                        }
                        keys.push((ObjectKind::Variant, variant_name));
                    }
                    keys.push((ObjectKind::Type, owner));
                }
                _ => {}
            }
        }
    }
    keys.sort();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ObjectIdentityError::IdentityCollision);
    }
    Ok(keys)
}

pub(crate) fn module_namespace(path: &str) -> Option<String> {
    let path = path.strip_suffix(".orna")?;
    if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return None;
    }
    let mut parts = path.split('/').collect::<Vec<_>>();
    let file = parts.pop()?;
    if !crate::valid_component(file) || parts.iter().any(|part| !crate::valid_component(part)) {
        return None;
    }
    if file != "main" {
        parts.push(file);
    }
    Some(parts.join("."))
}

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}.{name}")
    }
}

fn raw_int(raw: &Raw) -> Option<u64> {
    match raw {
        Raw::Int(value) => value.to_string().parse().ok(),
        _ => None,
    }
}

/// Rebuilds records from `(sys.ObjectKind ordinal, qualified name, id, retired)`
/// rows. A `field` row is a variant payload field exactly when its owner (the
/// name without its last segment) is a variant row; otherwise it is a type field.
fn records_from_spec_ordinals(
    rows: Vec<(u64, String, [u8; 16], bool)>,
) -> Result<Vec<ObjectIdentity>, ObjectIdentityError> {
    let variants: BTreeSet<&str> = rows
        .iter()
        .filter(|(code, ..)| *code == SPEC_VARIANT_ORDINAL)
        .map(|(_, qualified_name, ..)| qualified_name.as_str())
        .collect();
    rows.iter()
        .map(|(code, qualified_name, id, retired)| {
            let kind = if *code == SPEC_FIELD_ORDINAL {
                let (owner, _) = qualified_name
                    .rsplit_once('.')
                    .ok_or(ObjectIdentityError::InvalidEncoding)?;
                if variants.contains(owner) {
                    ObjectKind::VariantField
                } else {
                    ObjectKind::TypeField
                }
            } else {
                [
                    ObjectKind::Table,
                    ObjectKind::Field,
                    ObjectKind::Unit,
                    ObjectKind::Type,
                    ObjectKind::Variant,
                ]
                .into_iter()
                .find(|kind| kind.spec_ordinal() == *code)
                .ok_or(ObjectIdentityError::InvalidEncoding)?
            };
            Ok(ObjectIdentity {
                kind,
                qualified_name: qualified_name.clone(),
                id: *id,
                retired: *retired,
            })
        })
        .collect()
}

fn validate_records(records: &[ObjectIdentity]) -> Result<(), ObjectIdentityError> {
    let mut active = BTreeSet::new();
    let mut ids = HashSet::new();
    for record in records {
        if record.qualified_name.is_empty()
            || record.qualified_name.contains('\0')
            || !ids.insert(record.id)
            || (!record.retired && !active.insert((record.kind, record.qualified_name.as_str())))
        {
            return Err(ObjectIdentityError::IdentityCollision);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_identity_reconciliation_is_locked_across_read_and_write() {
        let directory = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        let repository = Repository::discover(directory.path()).unwrap();
        let original = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, size: Int, }",
        );
        let initial = ObjectIdentityManifest::reconcile_and_write_worktree(
            &repository,
            std::slice::from_ref(&original),
        )
        .unwrap();
        let table_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let payload_id = *initial.id(ObjectKind::Field, "Media.payload").unwrap();
        let renamed = ModuleInput::new(
            "main.orna",
            "pub table Archive(id: Str) { payload: Blob, size: Int, }",
        );
        let renames = [ObjectIdentityRename::new(
            ObjectKind::Table,
            "Media",
            "Archive",
        )];

        let (locked_sender, locked_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let lock_repository = repository.clone();
        let lock_holder = std::thread::spawn(move || {
            lock_repository.with_coordination_lock(|| {
                locked_sender
                    .send(())
                    .map_err(|_| ObjectIdentityError::Io)?;
                release_receiver
                    .recv()
                    .map_err(|_| ObjectIdentityError::Io)?;
                Ok::<(), ObjectIdentityError>(())
            })
        });
        locked_receiver.recv().expect("coordination lock acquired");

        let blocked = ObjectIdentityManifest::reconcile_and_write_worktree_with_renames(
            &repository,
            std::slice::from_ref(&renamed),
            &renames,
        );
        release_sender.send(()).unwrap();
        lock_holder
            .join()
            .expect("lock holder thread")
            .expect("coordination lock callback");

        assert_eq!(blocked, Err(ObjectIdentityError::Io));
        assert_eq!(
            ObjectIdentityManifest::read_worktree(&repository).unwrap(),
            Some(initial)
        );

        let next = ObjectIdentityManifest::reconcile_and_write_worktree_with_renames(
            &repository,
            std::slice::from_ref(&renamed),
            &renames,
        )
        .unwrap();
        assert_eq!(next.id(ObjectKind::Table, "Archive"), Some(&table_id));
        assert_eq!(
            next.id(ObjectKind::Field, "Archive.payload"),
            Some(&payload_id)
        );
        assert_eq!(
            ObjectIdentityManifest::read_worktree(&repository).unwrap(),
            Some(next)
        );
    }

    /// `orna mv` moves only the renamed table and its fields: a table the
    /// worktree dropped but the staged source still declares stays active
    /// under its ID, and a new worktree declaration gets no ID yet.
    #[test]
    fn rename_only_updates_move_named_identities_and_leave_the_rest() {
        let staged = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, }\npub table Clip(id: Str) { take: Int, }",
        );
        let previous = ObjectIdentityManifest::reconcile(std::slice::from_ref(&staged), None)
            .expect("initial identities");
        let worktree = ModuleInput::new(
            "main.orna",
            "pub table Archive(id: Str) { payload: Blob, }\npub table Note(id: Str) { body: Str, }",
        );
        let rename = ObjectIdentityRename::new(ObjectKind::Table, "Media", "Archive");

        let renamed = previous
            .with_renames(
                std::slice::from_ref(&worktree),
                std::slice::from_ref(&rename),
            )
            .expect("explicit rename");

        assert_eq!(
            renamed.id(ObjectKind::Table, "Archive"),
            previous.id(ObjectKind::Table, "Media")
        );
        assert_eq!(
            renamed.id(ObjectKind::Field, "Archive.payload"),
            previous.id(ObjectKind::Field, "Media.payload")
        );
        assert_eq!(
            renamed.id(ObjectKind::Field, "Archive.id"),
            previous.id(ObjectKind::Field, "Media.id")
        );
        assert_eq!(renamed.id(ObjectKind::Table, "Media"), None);
        assert_eq!(
            renamed.id(ObjectKind::Table, "Clip"),
            previous.id(ObjectKind::Table, "Clip")
        );
        assert_eq!(renamed.id(ObjectKind::Table, "Note"), None);
        assert_eq!(renamed.records().len(), previous.records().len());
        assert!(renamed.records().iter().all(|record| !record.is_retired()));

        let colliding = ModuleInput::new("main.orna", "pub table Clip(id: Str) { payload: Blob, }");
        assert_eq!(
            previous.with_renames(
                std::slice::from_ref(&colliding),
                &[ObjectIdentityRename::new(
                    ObjectKind::Table,
                    "Media",
                    "Clip"
                )],
            ),
            Err(ObjectIdentityError::IdentityCollision),
        );
        assert_eq!(
            previous.with_renames(
                std::slice::from_ref(&staged),
                &[ObjectIdentityRename::new(
                    ObjectKind::Table,
                    "Media",
                    "Archive"
                )],
            ),
            Err(ObjectIdentityError::InvalidSource),
        );
    }

    #[test]
    fn explicit_rename_mappings_reject_invalid_sources_and_targets() {
        let media = ModuleInput::new("main.orna", "pub table Media(id: Str) { payload: Blob, }");
        let archive =
            ModuleInput::new("main.orna", "pub table Archive(id: Str) { payload: Blob, }");
        let previous = ObjectIdentityManifest::reconcile(std::slice::from_ref(&media), None)
            .expect("initial identities");

        for rename in [
            ObjectIdentityRename::new(ObjectKind::Table, "", "Archive"),
            ObjectIdentityRename::new(ObjectKind::Table, "Media", ""),
            ObjectIdentityRename::new(ObjectKind::Table, "Media", "Media"),
            ObjectIdentityRename::new(ObjectKind::Table, "Missing", "Archive"),
            ObjectIdentityRename::new(ObjectKind::Table, "Media", "Missing"),
            ObjectIdentityRename::new(ObjectKind::Unit, "Media", "Archive"),
        ] {
            assert_eq!(
                ObjectIdentityManifest::reconcile_with_renames(
                    std::slice::from_ref(&media),
                    Some(&previous),
                    &[rename],
                ),
                Err(ObjectIdentityError::InvalidSource),
            );
        }

        let retired_source =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&archive), Some(&previous))
                .expect("old identity is retired after source deletion");
        assert_eq!(
            ObjectIdentityManifest::reconcile_with_renames(
                std::slice::from_ref(&archive),
                Some(&retired_source),
                &[ObjectIdentityRename::new(
                    ObjectKind::Table,
                    "Media",
                    "Archive"
                )],
            ),
            Err(ObjectIdentityError::InvalidSource),
        );
    }

    #[test]
    fn explicit_rename_mappings_reject_source_target_and_existing_identity_collisions() {
        let media = ModuleInput::new("main.orna", "pub table Media(id: Str) { payload: Blob, }");
        let media_previous = ObjectIdentityManifest::reconcile(std::slice::from_ref(&media), None)
            .expect("initial identities");
        let destinations = ModuleInput::new(
            "main.orna",
            "pub table Archive(id: Str) { payload: Blob, }
             pub table Backup(id: Str) { payload: Blob, }",
        );

        assert_eq!(
            ObjectIdentityManifest::reconcile_with_renames(
                std::slice::from_ref(&destinations),
                Some(&media_previous),
                &[
                    ObjectIdentityRename::new(ObjectKind::Table, "Media", "Archive"),
                    ObjectIdentityRename::new(ObjectKind::Table, "Media", "Backup"),
                ],
            ),
            Err(ObjectIdentityError::IdentityCollision),
        );

        let sources = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, }
             pub table Clip(id: Str) { payload: Blob, }",
        );
        let sources_previous =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&sources), None)
                .expect("source identities");
        let archive =
            ModuleInput::new("main.orna", "pub table Archive(id: Str) { payload: Blob, }");
        assert_eq!(
            ObjectIdentityManifest::reconcile_with_renames(
                std::slice::from_ref(&archive),
                Some(&sources_previous),
                &[
                    ObjectIdentityRename::new(ObjectKind::Table, "Media", "Archive"),
                    ObjectIdentityRename::new(ObjectKind::Table, "Clip", "Archive"),
                ],
            ),
            Err(ObjectIdentityError::IdentityCollision),
        );

        let existing_destinations = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, }
             pub table Archive(id: Str) { payload: Blob, }",
        );
        let existing_previous =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&existing_destinations), None)
                .expect("existing destination identity");
        assert_eq!(
            ObjectIdentityManifest::reconcile_with_renames(
                std::slice::from_ref(&archive),
                Some(&existing_previous),
                &[ObjectIdentityRename::new(
                    ObjectKind::Table,
                    "Media",
                    "Archive"
                )],
            ),
            Err(ObjectIdentityError::IdentityCollision),
        );
    }

    #[test]
    fn manifest_reconciliation_persists_ids_and_retires_removed_declarations() {
        let source = ModuleInput::new(
            "main.orna",
            include_str!("../tests/fixtures/vfs-blob-mib.orna"),
        );
        let first = ObjectIdentityManifest::reconcile(std::slice::from_ref(&source), None).unwrap();
        let encoded = first.encode().unwrap();
        assert_eq!(ObjectIdentityManifest::decode(&encoded).unwrap(), first);
        let payload_id = *first.id(ObjectKind::Field, "Media.payload").unwrap();
        let unit_id = *first.id(ObjectKind::Unit, "MiB").unwrap();
        let stable =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&source), Some(&first)).unwrap();
        assert_eq!(
            stable.id(ObjectKind::Field, "Media.payload"),
            Some(&payload_id)
        );
        assert_eq!(stable.id(ObjectKind::Unit, "MiB"), Some(&unit_id));
        let removed = ObjectIdentityManifest::reconcile(&[], Some(&stable)).unwrap();
        assert!(removed.id(ObjectKind::Field, "Media.payload").is_none());
        assert!(removed.id(ObjectKind::Unit, "MiB").is_none());
        assert!(removed.records().iter().any(|record| {
            record.kind() == ObjectKind::Unit
                && record.qualified_name() == "MiB"
                && record.is_retired()
        }));
        assert_eq!(
            ObjectIdentityManifest::decode(&removed.encode().unwrap()).unwrap(),
            removed
        );
    }

    // Restores the retired-id invariant deleted with orna-semantic-v1 (commit
    // 0e26aa9a4, PR #9168). Spec: ORNA-OBJECT-005 (retired IDs MUST NOT be
    // reused for an unrelated definition) and ORNA-AUTOID-002 (allocated IDs
    // are never reused after deletion).
    #[test]
    fn committed_ids_survive_edits_and_retirement_prevents_reuse() {
        let first_source = "pub table Note(id: Str) { title: Str, }";
        let first = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", first_source)],
            None,
        )
        .expect("initial authoring allocates committed identities");
        let table_id = *first.id(ObjectKind::Table, "Note").unwrap();
        let title_id = *first.id(ObjectKind::Field, "Note.title").unwrap();
        let encoded = first.encode().unwrap();
        assert_eq!(ObjectIdentityManifest::decode(&encoded).unwrap(), first);

        let edited = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new(
                "main.orna",
                "pub table Note(id: Str) { title: Str, body: Str, }",
            )],
            Some(&first),
        )
        .expect("compatible edit retains prior identities");
        assert_eq!(edited.id(ObjectKind::Table, "Note"), Some(&table_id));
        assert_eq!(edited.id(ObjectKind::Field, "Note.title"), Some(&title_id));

        let removed = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", "")],
            Some(&edited),
        )
        .expect("removal retires identities");
        assert!(removed.records().iter().any(|record| {
            record.kind() == ObjectKind::Table
                && record.qualified_name() == "Note"
                && record.is_retired()
        }));
        let restored = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", first_source)],
            Some(&removed),
        )
        .expect("unassisted recreation receives new identities");
        assert_ne!(restored.id(ObjectKind::Table, "Note"), Some(&table_id));
        assert!(restored.records().iter().any(|record| {
            record.kind() == ObjectKind::Table
                && record.qualified_name() == "Note"
                && record.id() == &table_id
                && record.is_retired()
        }));
        let removed_again = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", "")],
            Some(&restored),
        )
        .expect("later removal can retire the new identity generation");
        let retired_table_count = removed_again
            .records()
            .iter()
            .filter(|record| {
                record.kind() == ObjectKind::Table
                    && record.qualified_name() == "Note"
                    && record.is_retired()
            })
            .count();
        assert_eq!(retired_table_count, 2);
        assert_eq!(
            ObjectIdentityManifest::decode(&removed_again.encode().unwrap()).unwrap(),
            removed_again,
        );
    }
    #[test]
    fn table_rename_cascades_child_field_identities() {
        let original = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, size: Int, }",
        );
        let previous = ObjectIdentityManifest::reconcile(std::slice::from_ref(&original), None)
            .expect("initial identities");
        let payload_id = *previous.id(ObjectKind::Field, "Media.payload").unwrap();
        let size_id = *previous.id(ObjectKind::Field, "Media.size").unwrap();
        let renamed = ModuleInput::new(
            "main.orna",
            "pub table Archive(id: Str) { payload: Blob, size: Int, }",
        );

        let next = ObjectIdentityManifest::reconcile_with_renames(
            std::slice::from_ref(&renamed),
            Some(&previous),
            &[ObjectIdentityRename::new(
                ObjectKind::Table,
                "Media",
                "Archive",
            )],
        )
        .expect("table rename identities");

        assert_eq!(
            next.id(ObjectKind::Field, "Archive.payload"),
            Some(&payload_id)
        );
        assert_eq!(next.id(ObjectKind::Field, "Archive.size"), Some(&size_id));
        assert!(next.id(ObjectKind::Field, "Media.payload").is_none());
        assert!(next.id(ObjectKind::Field, "Media.size").is_none());
    }

    #[test]
    fn explicit_field_rename_overrides_table_field_cascade() {
        let original = ModuleInput::new(
            "main.orna",
            "pub table Media(id: Str) { payload: Blob, size: Int, }",
        );
        let previous = ObjectIdentityManifest::reconcile(std::slice::from_ref(&original), None)
            .expect("initial identities");
        let payload_id = *previous.id(ObjectKind::Field, "Media.payload").unwrap();
        let size_id = *previous.id(ObjectKind::Field, "Media.size").unwrap();
        let renamed = ModuleInput::new(
            "main.orna",
            "pub table Archive(id: Str) { content: Blob, size: Int, }",
        );

        let next = ObjectIdentityManifest::reconcile_with_renames(
            std::slice::from_ref(&renamed),
            Some(&previous),
            &[
                ObjectIdentityRename::new(ObjectKind::Table, "Media", "Archive"),
                ObjectIdentityRename::new(ObjectKind::Field, "Media.payload", "Archive.content"),
            ],
        )
        .expect("table and explicit field rename identities");

        assert_eq!(
            next.id(ObjectKind::Field, "Archive.content"),
            Some(&payload_id)
        );
        assert_eq!(next.id(ObjectKind::Field, "Archive.size"), Some(&size_id));
        assert!(next.id(ObjectKind::Field, "Media.payload").is_none());
    }

    #[test]
    fn object_identity_rename_rekey_roundtrip() {
        let original = ModuleInput::new(
            "main.orna",
            "pub dim Information;\npub unit MiB : Information = 1048576.B;\n\
             pub table Media(id: Str) { payload: Blob, size: Float<MiB>, }",
        );
        let initial = ObjectIdentityManifest::reconcile(std::slice::from_ref(&original), None)
            .expect("initial identities");
        let table_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let key_id = *initial.id(ObjectKind::Field, "Media.id").unwrap();
        let removed_field_id = *initial.id(ObjectKind::Field, "Media.payload").unwrap();
        let size_id = *initial.id(ObjectKind::Field, "Media.size").unwrap();
        let unit_id = *initial.id(ObjectKind::Unit, "MiB").unwrap();

        let renamed_source = ModuleInput::new(
            "main.orna",
            "pub dim Information;\npub unit MiB : Information = 1048576.B;\n\
             pub table Archive(key: Str) { size: Float<MiB>, }",
        );
        let unassisted = ObjectIdentityManifest::reconcile(
            std::slice::from_ref(&renamed_source),
            Some(&initial),
        )
        .expect("unassisted source rename is treated as delete plus create");
        assert_ne!(unassisted.id(ObjectKind::Table, "Archive"), Some(&table_id));
        assert!(unassisted.records().iter().any(|record| {
            record.kind() == ObjectKind::Table
                && record.qualified_name() == "Media"
                && record.id() == &table_id
                && record.is_retired()
        }));

        let renames = [
            ObjectIdentityRename::new(ObjectKind::Table, "Media", "Archive"),
            ObjectIdentityRename::new(ObjectKind::Field, "Media.id", "Archive.key"),
            ObjectIdentityRename::new(ObjectKind::Field, "Media.size", "Archive.size"),
        ];
        let renamed = ObjectIdentityManifest::reconcile_with_renames(
            std::slice::from_ref(&renamed_source),
            Some(&initial),
            &renames,
        )
        .expect("explicit rename and key-field rekey");

        assert_eq!(renamed.id(ObjectKind::Table, "Archive"), Some(&table_id));
        assert_eq!(renamed.id(ObjectKind::Field, "Archive.key"), Some(&key_id));
        assert_eq!(
            renamed.id(ObjectKind::Field, "Archive.size"),
            Some(&size_id)
        );
        assert_eq!(renamed.id(ObjectKind::Unit, "MiB"), Some(&unit_id));
        assert!(renamed.records().iter().any(|record| {
            record.kind() == ObjectKind::Field
                && record.qualified_name() == "Media.payload"
                && record.id() == &removed_field_id
                && record.is_retired()
        }));

        let roundtrip = ObjectIdentityManifest::decode(&renamed.encode().unwrap()).unwrap();
        assert_eq!(roundtrip, renamed);
        let unchanged = ObjectIdentityManifest::reconcile(
            std::slice::from_ref(&renamed_source),
            Some(&roundtrip),
        )
        .expect("identity survives a normal subsequent reconciliation");
        assert_eq!(unchanged.id(ObjectKind::Table, "Archive"), Some(&table_id));
        assert_eq!(
            unchanged.id(ObjectKind::Field, "Archive.key"),
            Some(&key_id)
        );

        let field_reintroduced = ModuleInput::new(
            "main.orna",
            "pub dim Information;\npub unit MiB : Information = 1048576.B;\n\
             pub table Archive(key: Str) { payload: Blob, size: Float<MiB>, }",
        );
        let reintroduced = ObjectIdentityManifest::reconcile(
            std::slice::from_ref(&field_reintroduced),
            Some(&roundtrip),
        )
        .expect("reintroduced field receives a fresh identity");
        let new_field_id = reintroduced
            .id(ObjectKind::Field, "Archive.payload")
            .unwrap();
        assert_ne!(new_field_id, &removed_field_id);
        assert!(
            reintroduced
                .records()
                .iter()
                .any(|record| { record.id() == &removed_field_id && record.is_retired() })
        );
    }

    #[test]
    fn source_quantity_schema_persists_basis_and_exact_ratio() {
        let source = ModuleInput::new(
            "main.orna",
            include_str!("../tests/fixtures/vfs-blob-mib.orna"),
        );
        let analysis = orna_semantic_v1::analyze(std::slice::from_ref(&source));
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let unit = analysis
            .source_units
            .values()
            .find(|unit| unit.identity.name == "MiB")
            .unwrap();
        assert_eq!(
            unit.dimension_basis.values().copied().collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(unit.scale.numerator, "1048576");
        assert_eq!(unit.scale.denominator, "1");
        assert_eq!(unit.offset.numerator, "0");
        assert_eq!(unit.offset.denominator, "1");
        assert!(!unit.affine);
        assert_eq!(unit.provenance.logical_path, "main.orna");

        let manifest =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&source), None).unwrap();
        let schema = manifest
            .source_table_schema(
                std::slice::from_ref(&source),
                &analysis,
                None,
                "",
                "main.orna",
                "Media",
            )
            .unwrap();
        let unit_id = *manifest.id(ObjectKind::Unit, "MiB").unwrap();
        let Raw::Map(entries) = schema.descriptor.raw() else {
            panic!("schema descriptor is a map");
        };
        let entry = |key: u64| {
            entries
                .iter()
                .find(|(raw_key, _)| {
                    matches!(raw_key, Raw::Int(value) if value == &BigInt::from(key))
                })
                .map(|(_, value)| value)
                .unwrap()
        };
        let Raw::Array(fields) = entry(3) else {
            panic!("schema fields are an array");
        };
        let size_field = fields
            .iter()
            .find(|field| {
                matches!(field, Raw::Array(parts) if parts.get(1) == Some(&Raw::Text("size".into())))
            })
            .unwrap();
        let Raw::Array(size_field) = size_field else {
            panic!("size field is an array");
        };
        assert_eq!(
            size_field[2],
            Raw::Array(vec![
                Raw::Int(7.into()),
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Float".into())]),
                uuid_raw(&unit_id),
            ])
        );
        let Raw::Array(definitions) = entry(4) else {
            panic!("schema definitions are an array");
        };
        assert_eq!(
            definitions,
            &[Raw::Array(vec![
                uuid_raw(&unit_id),
                Raw::Int(3.into()),
                Raw::Array(vec![
                    Raw::Array(vec![Raw::Int(1.into())]),
                    Raw::Int(1_048_576.into()),
                    Raw::Int(1.into()),
                    Raw::Int(0.into()),
                    Raw::Int(1.into()),
                    Raw::Bool(false),
                ]),
            ])]
        );
        SchemaDescriptor::decode(&schema.descriptor.encode().unwrap()).unwrap();
    }

    #[test]
    fn legacy_sidecar_codes_decode_to_spec_kinds_and_reencode_as_v2() {
        let manifest = ObjectIdentityManifest {
            records: vec![
                ObjectIdentity {
                    kind: ObjectKind::Type,
                    qualified_name: "Shape".into(),
                    id: [1; 16],
                    retired: false,
                },
                ObjectIdentity {
                    kind: ObjectKind::Variant,
                    qualified_name: "Shape.circle".into(),
                    id: [2; 16],
                    retired: false,
                },
                ObjectIdentity {
                    kind: ObjectKind::VariantField,
                    qualified_name: "Shape.circle.radius".into(),
                    id: [3; 16],
                    retired: false,
                },
            ],
        };
        let legacy = manifest
            .encode_as(LEGACY_MANIFEST_VERSION, ObjectKind::legacy_code)
            .unwrap();
        let decoded = ObjectIdentityManifest::decode(&legacy).unwrap();
        assert_eq!(decoded, manifest);
        let current = decoded.encode().unwrap();
        assert_ne!(current, legacy);
        assert_eq!(ObjectIdentityManifest::decode(&current).unwrap(), manifest);
    }

    #[test]
    fn oversized_worktree_manifest_is_rejected_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("object-identities.cbor");
        let file = fs::File::create(&path).unwrap();
        file.set_len((MAX_OBJECT_IDENTITY_MANIFEST_BYTES as u64) + 1)
            .unwrap();

        assert_eq!(
            ObjectIdentityManifest::read_bounded_manifest(&path),
            Err(ObjectIdentityError::InvalidEncoding)
        );
    }

    // 07-tables.md "Stored, defaulted and computed fields".
    const SPEC_CONTACT: &str = "pub table Contact(id: Str) {
    first: Str,
    last: Str,
    country: Str = \"GB\",
    full_name: Str => \"{first} {last}\",
}
";

    // 07-tables.md "Automatic keys" and "Key defaults".
    const SPEC_KEY_DEFAULTS: &str = "pub table Event(id: Uuid = uuid7()) {
    time: Instant,
    value: Str,
}

pub table Contact(
    id: Uuid = uuid7()
) {
    name: Str,
    emails: [Str],
}
";

    fn source_descriptor(
        modules: &[ModuleInput],
        database_id: Option<[u8; 16]>,
        source_path: &str,
        table: &str,
    ) -> (
        ObjectIdentityManifest,
        Result<SourceTableSchemaArtifact, SourceTableSchemaError>,
    ) {
        let analysis = orna_semantic_v1::analyze(modules);
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let manifest = ObjectIdentityManifest::reconcile(modules, None).unwrap();
        let module_name = module_namespace(source_path).unwrap();
        let schema = manifest.source_table_schema(
            modules,
            &analysis,
            database_id,
            &module_name,
            source_path,
            table,
        );
        (manifest, schema)
    }

    fn descriptor_entry(descriptor: &SchemaDescriptor, key: u64) -> &Raw {
        let Raw::Map(entries) = descriptor.raw() else {
            panic!("schema descriptor is a map");
        };
        entries
            .iter()
            .find(|(raw_key, _)| matches!(raw_key, Raw::Int(value) if value == &BigInt::from(key)))
            .map(|(_, value)| value)
            .expect("schema descriptor key")
    }

    fn descriptor_field<'a>(descriptor: &'a SchemaDescriptor, name: &str) -> &'a [Raw] {
        let Raw::Array(fields) = descriptor_entry(descriptor, 3) else {
            panic!("schema fields are an array");
        };
        fields
            .iter()
            .find_map(|field| match field {
                Raw::Array(parts) if parts.get(1) == Some(&Raw::Text(name.into())) => {
                    Some(parts.as_slice())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("schema field {name}"))
    }

    fn descriptor_definition(descriptor: &SchemaDescriptor, id: &[u8; 16]) -> Raw {
        let Raw::Array(definitions) = descriptor_entry(descriptor, 4) else {
            panic!("schema definitions are an array");
        };
        definitions
            .iter()
            .find(|definition| {
                matches!(definition, Raw::Array(parts) if parts.first() == Some(&uuid_raw(id)))
            })
            .cloned()
            .expect("schema definition")
    }

    fn primitive(name: &str) -> Raw {
        Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.into())])
    }

    fn canonical(raw: Raw) -> Vec<u8> {
        Value::new(raw).unwrap().encode().unwrap()
    }

    /// Canonical stored values for every role-1 field, in descriptor order.
    fn stored_row(descriptor: &SchemaDescriptor, values: &[(&str, Raw)]) -> Vec<u8> {
        let Raw::Array(fields) = descriptor_entry(descriptor, 3) else {
            panic!("schema fields are an array");
        };
        let stored = fields
            .iter()
            .filter_map(|field| {
                let Raw::Array(parts) = field else {
                    panic!("schema field is an array");
                };
                (parts[3] == Raw::Int(1.into())).then(|| {
                    values
                        .iter()
                        .find(|(name, _)| parts[1] == Raw::Text((*name).into()))
                        .map(|(_, value)| value.clone())
                        .expect("stored field value")
                })
            })
            .collect();
        canonical(Raw::Array(stored))
    }

    #[test]
    fn spec_contact_stores_defaulted_fields_and_omits_the_computed_selector() {
        let modules = [ModuleInput::new("main.orna", SPEC_CONTACT)];
        let (manifest, schema) = source_descriptor(&modules, None, "main.orna", "Contact");
        let schema = schema.expect("defaulted and computed fields produce a schema node");
        let descriptor = schema.descriptor();
        let field_id = |name: &str| {
            *manifest
                .id(ObjectKind::Field, &format!("Contact.{name}"))
                .unwrap()
        };
        assert_eq!(
            descriptor_entry(descriptor, 2),
            &Raw::Array(vec![uuid_raw(&field_id("id"))])
        );
        for (name, role) in [("id", 0), ("first", 1), ("last", 1), ("country", 1)] {
            let field = descriptor_field(descriptor, name);
            assert_eq!(field[0], uuid_raw(&field_id(name)));
            assert_eq!(field[2], primitive("Str"));
            assert_eq!(field[3], Raw::Int(role.into()), "{name}");
            // An insert-time default is code, never a `[1, value]` fallback.
            assert_eq!(field[4], Raw::Array(vec![Raw::Int(0.into())]), "{name}");
        }
        let full_name = descriptor_field(descriptor, "full_name");
        assert_eq!(full_name[0], uuid_raw(&field_id("full_name")));
        assert_eq!(full_name[2], primitive("Str"));
        assert_eq!(full_name[3], Raw::Int(2.into()));
        assert!(matches!(
            &full_name[4],
            Raw::Array(fallback) if matches!(
                fallback.as_slice(),
                [Raw::Int(kind), Raw::Bytes(digest)] if kind == &BigInt::from(2) && digest.len() == 32
            )
        ));
        assert_eq!(descriptor_entry(descriptor, 4), &Raw::Array(Vec::new()));
        assert_eq!(
            &SchemaDescriptor::decode(&descriptor.encode().unwrap()).unwrap(),
            descriptor
        );

        // Stored columns are exactly the role-1 fields; the computed selector
        // has no stored value (ORNA-FIELD-006, ORNA-FIELD-011).
        let key = canonical(Raw::Text("ada".into()));
        let row = [
            ("first", Raw::Text("Ada".into())),
            ("last", Raw::Text("Lovelace".into())),
            ("country", Raw::Text("GB".into())),
        ];
        descriptor
            .validate_stored_row(
                *schema.table_object_id(),
                &key,
                &stored_row(descriptor, &row),
            )
            .expect("a row without the computed field is admitted");
        let with_computed = canonical(Raw::Array(vec![
            Raw::Text("Ada".into()),
            Raw::Text("Lovelace".into()),
            Raw::Text("GB".into()),
            Raw::Text("Ada Lovelace".into()),
        ]));
        assert!(
            descriptor
                .validate_stored_row(*schema.table_object_id(), &key, &with_computed)
                .is_err()
        );
    }

    #[test]
    fn spec_key_defaults_encode_generated_keys_as_primary_keys() {
        let modules = [ModuleInput::new("main.orna", SPEC_KEY_DEFAULTS)];
        let tables = [
            (
                "Event",
                vec![("time", primitive("Instant")), ("value", primitive("Str"))],
            ),
            (
                "Contact",
                vec![
                    ("name", primitive("Str")),
                    (
                        "emails",
                        Raw::Array(vec![Raw::Int(1.into()), primitive("Str")]),
                    ),
                ],
            ),
        ];
        for (table, stored) in tables {
            let (manifest, schema) = source_descriptor(&modules, None, "main.orna", table);
            let schema = schema.unwrap_or_else(|error| panic!("{table}: {error:?}"));
            let descriptor = schema.descriptor();
            let key_id = *manifest
                .id(ObjectKind::Field, &format!("{table}.id"))
                .unwrap();
            assert_eq!(
                descriptor_entry(descriptor, 2),
                &Raw::Array(vec![uuid_raw(&key_id)])
            );
            let key = descriptor_field(descriptor, "id");
            assert_eq!(key[2], primitive("Uuid"));
            assert_eq!(key[3], Raw::Int(0.into()));
            assert_eq!(key[4], Raw::Array(vec![Raw::Int(0.into())]));
            for (name, node) in stored {
                let field = descriptor_field(descriptor, name);
                assert_eq!(field[2], node, "{table}.{name}");
                assert_eq!(field[3], Raw::Int(1.into()), "{table}.{name}");
            }
            let generated = canonical(Raw::Tag(37, Box::new(Raw::Bytes(vec![7; 16]))));
            descriptor
                .validate_stored_key(*schema.table_object_id(), &generated)
                .expect("a generated Uuid key is the stored row identity");
        }
    }

    #[test]
    fn computed_selector_digest_ignores_formatting_and_binds_the_expression() {
        let digest = |source: &str| {
            let modules = [ModuleInput::new("main.orna", source)];
            let (_, schema) = source_descriptor(&modules, None, "main.orna", "Contact");
            descriptor_field(schema.unwrap().descriptor(), "full_name")[4].clone()
        };
        let original = digest(SPEC_CONTACT);
        let reformatted = SPEC_CONTACT.replace(
            "=> \"{first} {last}\"",
            "=>\n        // display name\n        \"{first} {last}\"",
        );
        assert_eq!(digest(&reformatted), original);
        let changed = SPEC_CONTACT.replace("\"{first} {last}\"", "\"{last}, {first}\"");
        assert_ne!(digest(&changed), original);
    }

    #[test]
    fn computed_selector_digest_binds_the_resolved_helper_declaration() {
        // Identical tokens bound to a different declaration are a different
        // selector: semantic identity includes resolved meaning (15-system.md).
        let digest = |module: &str| {
            let modules = [
                ModuleInput::new(
                    "labels/plain.orna",
                    "pub fn label(value: Str): Str = value;\n",
                ),
                ModuleInput::new(
                    "labels/loud.orna",
                    "pub fn label(value: Str): Str = \"{value}!\";\n",
                ),
                ModuleInput::new(
                    "main.orna",
                    format!(
                        "use {module}.{{label}};\n\npub table Contact(id: Str) {{\n    name: Str,\n    display: Str => label(name),\n}}\n"
                    ),
                ),
            ];
            let (_, schema) = source_descriptor(&modules, None, "main.orna", "Contact");
            descriptor_field(schema.unwrap().descriptor(), "display")[4].clone()
        };
        assert_ne!(digest("labels.plain"), digest("labels.loud"));
    }

    #[test]
    fn computed_selector_digest_reads_row_fields_before_module_declarations() {
        // A table field is a lexical selector inside a computed expression
        // (ORNA-FIELD-009), so a module declaration with the same spelling
        // does not change what the selector reads.
        let digest = |extra: &str| {
            let modules = [ModuleInput::new(
                "main.orna",
                format!(
                    "{extra}pub table Contact(id: Str) {{\n    name: Str,\n    display: Str => name,\n}}\n"
                ),
            )];
            let (_, schema) = source_descriptor(&modules, None, "main.orna", "Contact");
            descriptor_field(schema.unwrap().descriptor(), "display")[4].clone()
        };
        assert_eq!(digest(""), digest("pub fn name(): Str = \"module\";\n\n"));
    }

    #[test]
    fn refinement_digest_tracks_assertion_meaning_not_formatting() {
        let digests = |refined: &str| {
            let modules = [ModuleInput::new(
                "main.orna",
                format!("{refined}\npub table Player(id: Str) {{\n    score: Score,\n}}\n"),
            )];
            let (manifest, schema) = source_descriptor(&modules, None, "main.orna", "Player");
            let score = *manifest.id(ObjectKind::Type, "Score").unwrap();
            let Raw::Array(definition) =
                descriptor_definition(schema.unwrap().descriptor(), &score)
            else {
                panic!("refined definition is an array");
            };
            let Raw::Array(body) = &definition[2] else {
                panic!("refined body is an array");
            };
            body[1].clone()
        };
        let original = digests("pub type Score = Int {\n    assert >= 0;\n}\n");
        assert_eq!(
            digests("pub type Score = Int {\n    // never negative\n    assert >=   0;\n}\n"),
            original
        );
        assert_ne!(
            digests("pub type Score = Int {\n    assert >= 1;\n}\n"),
            original
        );
    }

    #[test]
    fn stored_reference_records_database_table_and_target_key_shape() {
        let database_id = [0x42; 16];
        let modules = [
            ModuleInput::new(
                "directory/main.orna",
                "pub table Contact(id: Uuid) {\n    name: Str,\n}\n",
            ),
            // 07-tables.md "Stored table references", plus a reference key
            // component (ORNA-KEY-005).
            ModuleInput::new(
                "main.orna",
                "use directory;\n\npub table Vehicle(id: Uuid) {\n    owner: directory.Contact,\n}\n\npub table Registration(vehicle: Vehicle) {\n    plate: Str,\n}\n",
            ),
        ];
        let uuid = |byte: u8| Raw::Tag(37, Box::new(Raw::Bytes(vec![byte; 16])));
        let reference = |table: &[u8; 16], key: Raw| {
            Raw::Tag(
                60021,
                Box::new(Raw::Array(vec![
                    uuid_raw(&database_id),
                    uuid_raw(table),
                    key,
                ])),
            )
        };

        let (manifest, vehicle) =
            source_descriptor(&modules, Some(database_id), "main.orna", "Vehicle");
        let vehicle = vehicle.expect("a stored reference produces a schema node");
        let contact_id = *manifest.id(ObjectKind::Table, "directory.Contact").unwrap();
        let owner = descriptor_field(vehicle.descriptor(), "owner");
        assert_eq!(
            owner[2],
            Raw::Array(vec![
                Raw::Int(6.into()),
                uuid_raw(&database_id),
                uuid_raw(&contact_id),
                Raw::Array(vec![primitive("Uuid")]),
            ])
        );
        assert_eq!(owner[3], Raw::Int(1.into()));
        vehicle
            .descriptor()
            .validate_stored_row(
                *vehicle.table_object_id(),
                &canonical(uuid(1)),
                &canonical(Raw::Array(vec![reference(&contact_id, uuid(9))])),
            )
            .expect("the row stores the referenced key, not a row");

        let (manifest, registration) =
            source_descriptor(&modules, Some(database_id), "main.orna", "Registration");
        let registration = registration.expect("a reference key produces a schema node");
        let vehicle_id = *manifest.id(ObjectKind::Table, "Vehicle").unwrap();
        let key = descriptor_field(registration.descriptor(), "vehicle");
        assert_eq!(
            key[2],
            Raw::Array(vec![
                Raw::Int(6.into()),
                uuid_raw(&database_id),
                uuid_raw(&vehicle_id),
                Raw::Array(vec![primitive("Uuid")]),
            ])
        );
        assert_eq!(key[3], Raw::Int(0.into()));
        registration
            .descriptor()
            .validate_stored_key(
                *registration.table_object_id(),
                &canonical(reference(&vehicle_id, uuid(1))),
            )
            .expect("a reference key component");

        let (_, without_database) = source_descriptor(&modules, None, "main.orna", "Vehicle");
        assert_eq!(
            without_database,
            Err(SourceTableSchemaError::MissingDatabaseIdentity)
        );
    }

    #[test]
    fn qualified_field_types_resolve_through_imported_namespaces() {
        let database_id = [0x24; 16];
        let modules = [
            ModuleInput::new(
                "sensors/main.orna",
                "pub table Sensor(id: Str) {\n    label: Str,\n}\n",
            ),
            ModuleInput::new(
                "sensors/greenhouse.orna",
                "pub dim Illuminance;\npub unit lx : Illuminance base;\n\npub enum Mode {\n    auto,\n    manual,\n}\n",
            ),
            // ORNA-IMPORT-001: `use sensors;` binds the namespace by its final
            // component; nested modules are reached through that binding.
            ModuleInput::new(
                "main.orna",
                "use sensors;\n\npub table Reading(id: Str) {\n    sensor: sensors.Sensor,\n    mode: sensors.greenhouse.Mode,\n    level: Float<sensors.greenhouse.lx>,\n}\n",
            ),
        ];
        let (manifest, schema) =
            source_descriptor(&modules, Some(database_id), "main.orna", "Reading");
        let schema = schema.expect("qualified field types produce a schema node");
        let descriptor = schema.descriptor();
        let sensor = *manifest.id(ObjectKind::Table, "sensors.Sensor").unwrap();
        let mode = *manifest
            .id(ObjectKind::Type, "sensors.greenhouse.Mode")
            .unwrap();
        let lux = *manifest
            .id(ObjectKind::Unit, "sensors.greenhouse.lx")
            .unwrap();
        assert_eq!(
            descriptor_field(descriptor, "sensor")[2],
            Raw::Array(vec![
                Raw::Int(6.into()),
                uuid_raw(&database_id),
                uuid_raw(&sensor),
                Raw::Array(vec![primitive("Str")]),
            ])
        );
        assert_eq!(
            descriptor_field(descriptor, "mode")[2],
            Raw::Array(vec![Raw::Int(5.into()), uuid_raw(&mode)])
        );
        assert_eq!(
            descriptor_field(descriptor, "level")[2],
            Raw::Array(vec![Raw::Int(7.into()), primitive("Float"), uuid_raw(&lux),])
        );
        let Raw::Array(definition) = descriptor_definition(descriptor, &mode) else {
            panic!("enum definition is an array");
        };
        assert_eq!(definition[1], Raw::Int(1.into()));
        let Raw::Array(definition) = descriptor_definition(descriptor, &lux) else {
            panic!("unit definition is an array");
        };
        assert_eq!(definition[1], Raw::Int(3.into()));
    }

    #[test]
    fn canonical_nominal_identities_resolve_in_declaring_and_importing_modules() {
        // Semantic analysis canonicalizes a nominal field type to its
        // module-qualified identity, so `price: Price` declared in module
        // `shop` and a named import of it both reach the builder as
        // `shop.Price`.
        let modules = [
            ModuleInput::new(
                "shop.orna",
                "pub type Price {\n    pub amount: Int,\n}\n\npub table Order(id: Str) {\n    price: Price,\n}\n",
            ),
            ModuleInput::new(
                "main.orna",
                "use shop.{Price};\n\npub table Quote(id: Str) {\n    price: Price,\n}\n",
            ),
        ];
        for (source_path, table) in [("shop.orna", "Order"), ("main.orna", "Quote")] {
            let (manifest, schema) = source_descriptor(&modules, None, source_path, table);
            let schema = schema.unwrap_or_else(|error| panic!("{table}: {error:?}"));
            let price = *manifest.id(ObjectKind::Type, "shop.Price").unwrap();
            assert_eq!(
                descriptor_field(schema.descriptor(), "price")[2],
                Raw::Array(vec![Raw::Int(5.into()), uuid_raw(&price)])
            );
            let Raw::Array(definition) = descriptor_definition(schema.descriptor(), &price) else {
                panic!("nominal definition is an array");
            };
            assert_eq!(definition[1], Raw::Int(0.into()));
        }
    }

    #[test]
    fn nominal_enum_and_refined_field_types_emit_closed_definitions() {
        let source = "pub type Username = Str {
    assert != \"\";
}

pub type EmailAddress {
    value: Str,
}

pub enum Role {
    admin,
    member,
}

pub enum Availability {
    ready,
    waiting { reason: Str },
}

pub table User(id: Uuid) {
    username: Username,
    email: EmailAddress?,
    role: Role,
    availability: Availability,
}
";
        let modules = [ModuleInput::new("main.orna", source)];
        let (manifest, schema) = source_descriptor(&modules, None, "main.orna", "User");
        let schema = schema.expect("nominal, enum and refined fields produce a schema node");
        let descriptor = schema.descriptor();
        let id = |kind: ObjectKind, name: &str| *manifest.id(kind, name).unwrap();
        let username = id(ObjectKind::Type, "Username");
        let email = id(ObjectKind::Type, "EmailAddress");
        let email_value = id(ObjectKind::TypeField, "EmailAddress.value");
        let role = id(ObjectKind::Type, "Role");
        let availability = id(ObjectKind::Type, "Availability");
        let waiting = id(ObjectKind::Variant, "Availability.waiting");
        let reason = id(ObjectKind::VariantField, "Availability.waiting.reason");
        let nominal = |id: &[u8; 16]| Raw::Array(vec![Raw::Int(5.into()), uuid_raw(id)]);
        let member = |id: &[u8; 16], name: &str, node: Raw| {
            Raw::Array(vec![
                uuid_raw(id),
                Raw::Text(name.into()),
                node,
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        let variants = |entries: Vec<([u8; 16], Raw)>| {
            let mut entries = entries;
            entries.sort_by_key(|(id, _)| *id);
            Raw::Array(entries.into_iter().map(|(_, variant)| variant).collect())
        };

        assert_eq!(
            descriptor_field(descriptor, "username")[2],
            nominal(&username)
        );
        assert_eq!(
            descriptor_field(descriptor, "email")[2],
            Raw::Array(vec![Raw::Int(2.into()), nominal(&email)])
        );
        assert_eq!(descriptor_field(descriptor, "role")[2], nominal(&role));
        assert_eq!(
            descriptor_field(descriptor, "availability")[2],
            nominal(&availability)
        );

        let Raw::Array(refined) = descriptor_definition(descriptor, &username) else {
            panic!("refined definition is an array");
        };
        assert_eq!(refined[1], Raw::Int(2.into()));
        assert!(matches!(
            &refined[2],
            Raw::Array(body) if body.len() == 2
                && body[0] == primitive("Str")
                && matches!(&body[1], Raw::Array(digests)
                    if matches!(digests.as_slice(), [Raw::Bytes(digest)] if digest.len() == 32))
        ));
        // A private nominal field is still part of the stored representation.
        assert_eq!(
            descriptor_definition(descriptor, &email),
            Raw::Array(vec![
                uuid_raw(&email),
                Raw::Int(0.into()),
                Raw::Array(vec![member(&email_value, "value", primitive("Str"))]),
            ])
        );
        let admin = id(ObjectKind::Variant, "Role.admin");
        let role_member = id(ObjectKind::Variant, "Role.member");
        assert_eq!(
            descriptor_definition(descriptor, &role),
            Raw::Array(vec![
                uuid_raw(&role),
                Raw::Int(1.into()),
                variants(vec![
                    (
                        admin,
                        Raw::Array(vec![
                            uuid_raw(&admin),
                            Raw::Text("admin".into()),
                            Raw::Array(Vec::new()),
                        ]),
                    ),
                    (
                        role_member,
                        Raw::Array(vec![
                            uuid_raw(&role_member),
                            Raw::Text("member".into()),
                            Raw::Array(Vec::new()),
                        ]),
                    ),
                ]),
            ])
        );
        let ready = id(ObjectKind::Variant, "Availability.ready");
        assert_eq!(
            descriptor_definition(descriptor, &availability),
            Raw::Array(vec![
                uuid_raw(&availability),
                Raw::Int(1.into()),
                variants(vec![
                    (
                        ready,
                        Raw::Array(vec![
                            uuid_raw(&ready),
                            Raw::Text("ready".into()),
                            Raw::Array(Vec::new()),
                        ]),
                    ),
                    (
                        waiting,
                        Raw::Array(vec![
                            uuid_raw(&waiting),
                            Raw::Text("waiting".into()),
                            Raw::Array(vec![member(&reason, "reason", primitive("Str"))]),
                        ]),
                    ),
                ]),
            ])
        );

        let row = [
            // A refined nominal value is tag 60020 `[type_object_id,
            // base_value]` (29-formats.md).
            (
                "username",
                Raw::Tag(
                    60020,
                    Box::new(Raw::Array(vec![
                        uuid_raw(&username),
                        Raw::Text("ada".into()),
                    ])),
                ),
            ),
            (
                "email",
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![
                        Raw::Int(1.into()),
                        Raw::Tag(
                            60009,
                            Box::new(Raw::Array(vec![
                                uuid_raw(&email),
                                Raw::Array(vec![Raw::Array(vec![
                                    uuid_raw(&email_value),
                                    Raw::Text("ada@example.com".into()),
                                ])]),
                            ])),
                        ),
                    ])),
                ),
            ),
            (
                "role",
                Raw::Tag(
                    60008,
                    Box::new(Raw::Array(vec![
                        uuid_raw(&role),
                        uuid_raw(&admin),
                        Raw::Null,
                    ])),
                ),
            ),
            (
                "availability",
                Raw::Tag(
                    60008,
                    Box::new(Raw::Array(vec![
                        uuid_raw(&availability),
                        uuid_raw(&waiting),
                        Raw::Tag(
                            60009,
                            Box::new(Raw::Array(vec![
                                uuid_raw(&waiting),
                                Raw::Array(vec![Raw::Array(vec![
                                    uuid_raw(&reason),
                                    Raw::Text("busy".into()),
                                ])]),
                            ])),
                        ),
                    ])),
                ),
            ),
        ];
        descriptor
            .validate_stored_row(
                *schema.table_object_id(),
                &canonical(Raw::Tag(37, Box::new(Raw::Bytes(vec![3; 16])))),
                &stored_row(descriptor, &row),
            )
            .expect("nominal, refined and enum values validate against their definitions");
    }

    #[test]
    fn transparent_alias_lowers_to_its_target_without_an_identity() {
        let modules = [ModuleInput::new(
            "main.orna",
            "type UserId = Uuid;\n\npub table Session(id: UserId) {\n    owner: UserId?,\n}\n",
        )];
        let (manifest, schema) = source_descriptor(&modules, None, "main.orna", "Session");
        let schema = schema.expect("a transparent alias produces a schema node");
        assert_eq!(
            descriptor_field(schema.descriptor(), "id")[2],
            primitive("Uuid")
        );
        assert_eq!(
            descriptor_field(schema.descriptor(), "owner")[2],
            Raw::Array(vec![Raw::Int(2.into()), primitive("Uuid")])
        );
        assert_eq!(
            descriptor_entry(schema.descriptor(), 4),
            &Raw::Array(Vec::new())
        );
        assert!(manifest.id(ObjectKind::Type, "UserId").is_none());
    }

    #[test]
    fn money_field_records_its_currency_definition() {
        let modules = [ModuleInput::new(
            "main.orna",
            "pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}

pub table Invoice(id: Str) {
    total: Money<GBP>,
}
",
        )];
        let (manifest, schema) = source_descriptor(&modules, None, "main.orna", "Invoice");
        let schema = schema.expect("a Money field produces a schema node");
        let currency = *manifest.id(ObjectKind::Type, "GBP").unwrap();
        assert_eq!(
            descriptor_field(schema.descriptor(), "total")[2],
            Raw::Array(vec![Raw::Int(8.into()), uuid_raw(&currency)])
        );
        assert_eq!(
            descriptor_definition(schema.descriptor(), &currency),
            Raw::Array(vec![
                uuid_raw(&currency),
                Raw::Int(4.into()),
                Raw::Array(vec![Raw::Text("GBP".into()), Raw::Int(2.into())]),
            ])
        );
        let amount = Raw::Tag(
            60007,
            Box::new(Raw::Array(vec![
                Raw::Tag(
                    60000,
                    Box::new(Raw::Array(vec![
                        Raw::Int(1234.into()),
                        Raw::Int((-2).into()),
                    ])),
                ),
                uuid_raw(&currency),
            ])),
        );
        schema
            .descriptor()
            .validate_stored_row(
                *schema.table_object_id(),
                &canonical(Raw::Text("inv-1".into())),
                &canonical(Raw::Array(vec![amount])),
            )
            .expect("a money value in its declared currency");
    }

    #[test]
    fn portable_scalar_spellings_map_to_closed_primitives_and_unsupported_types_fail() {
        let modules = [ModuleInput::new(
            "main.orna",
            "pub table Media(id: Int) { blob: Blob, uuid: UUID, time: TIME, duration: DURATION, bytes: BYTES, }",
        )];
        let (_, schema) = source_descriptor(&modules, None, "main.orna", "Media");
        let schema = schema.expect("portable scalar table descriptor");
        for (field, primitive_name) in [
            ("blob", "Blob"),
            ("uuid", "Uuid"),
            ("time", "TimeOfDay"),
            ("duration", "Duration"),
            ("bytes", "Blob"),
        ] {
            assert_eq!(
                descriptor_field(schema.descriptor(), field)[2],
                primitive(primitive_name)
            );
        }

        let analysis = orna_semantic_v1::analyze(&modules);
        assert_eq!(
            ObjectIdentityManifest::default().source_table_schema(
                &modules,
                &analysis,
                None,
                "",
                "main.orna",
                "Media",
            ),
            Err(SourceTableSchemaError::MissingIdentity)
        );

        // A live relation is outside the serialisable subset (29-formats.md).
        // Semantic analysis rejects it as a stored field type, so
        // `source_table_schema` refuses that analysis, and the descriptor
        // builder has no closed type node for the analysed field type either.
        let relation = [ModuleInput::new(
            "main.orna",
            "pub table Contact(id: Int) { relation: Relation<Int>, }",
        )];
        let analysis = orna_semantic_v1::analyze(&relation);
        assert_eq!(
            analysis
                .diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>(),
            [(
                orna_semantic_v1::DIAG_TYPE,
                "stored table field type must be serialisable"
            )]
        );
        let manifest = ObjectIdentityManifest::reconcile(&relation, None).unwrap();
        assert_eq!(
            manifest.source_table_schema(&relation, &analysis, None, "", "main.orna", "Contact"),
            Err(SourceTableSchemaError::InvalidAnalysis)
        );
        let root = Namespace(Vec::new());
        let field_type = &analysis.modules[&root].symbols["Contact"]
            .table_schema
            .as_ref()
            .expect("Contact table schema")
            .fields["relation"];
        let sources = parse_source_modules(&relation).unwrap();
        let schema = DescriptorBuilder::new(&manifest, &analysis, &sources, None)
            .type_node(field_type, &root);
        assert_eq!(schema, Err(SourceTableSchemaError::UnsupportedType));
    }

    #[test]
    fn nominal_identities_are_issued_and_follow_explicit_type_renames() {
        let original = ModuleInput::new(
            "main.orna",
            "type Alias = Str;\npub type Point {\n    x: Int,\n    y: Int,\n}\npub enum Shape {\n    dot,\n    circle { radius: Float },\n}\n",
        );
        let first = ObjectIdentityManifest::reconcile(std::slice::from_ref(&original), None)
            .expect("initial identities");
        for (kind, name) in [
            (ObjectKind::Type, "Point"),
            (ObjectKind::TypeField, "Point.x"),
            (ObjectKind::TypeField, "Point.y"),
            (ObjectKind::Type, "Shape"),
            (ObjectKind::Variant, "Shape.dot"),
            (ObjectKind::Variant, "Shape.circle"),
            (ObjectKind::VariantField, "Shape.circle.radius"),
        ] {
            assert!(first.id(kind, name).is_some(), "{kind:?} {name}");
        }
        assert!(first.id(ObjectKind::Type, "Alias").is_none());
        assert_eq!(
            ObjectIdentityManifest::decode(&first.encode().unwrap()).unwrap(),
            first
        );

        let renamed = ModuleInput::new(
            "main.orna",
            "type Alias = Str;\npub type Location {\n    x: Int,\n    y: Int,\n}\npub enum Figure {\n    dot,\n    circle { radius: Float },\n}\n",
        );
        let next = ObjectIdentityManifest::reconcile_with_renames(
            std::slice::from_ref(&renamed),
            Some(&first),
            &[
                ObjectIdentityRename::new(ObjectKind::Type, "Point", "Location"),
                ObjectIdentityRename::new(ObjectKind::Type, "Shape", "Figure"),
            ],
        )
        .expect("explicit type renames");
        for (kind, old, new) in [
            (ObjectKind::Type, "Point", "Location"),
            (ObjectKind::TypeField, "Point.x", "Location.x"),
            (ObjectKind::TypeField, "Point.y", "Location.y"),
            (ObjectKind::Type, "Shape", "Figure"),
            (ObjectKind::Variant, "Shape.dot", "Figure.dot"),
            (ObjectKind::Variant, "Shape.circle", "Figure.circle"),
            (
                ObjectKind::VariantField,
                "Shape.circle.radius",
                "Figure.circle.radius",
            ),
        ] {
            assert_eq!(next.id(kind, new), first.id(kind, old), "{kind:?} {new}");
            assert!(next.id(kind, old).is_none(), "{kind:?} {old}");
        }

        // A renamed variant carries its payload field identities too.
        let reshaped = ModuleInput::new(
            "main.orna",
            "type Alias = Str;\npub type Location {\n    x: Int,\n    y: Int,\n}\npub enum Figure {\n    dot,\n    round { radius: Float },\n}\n",
        );
        let last = ObjectIdentityManifest::reconcile_with_renames(
            std::slice::from_ref(&reshaped),
            Some(&next),
            &[ObjectIdentityRename::new(
                ObjectKind::Variant,
                "Figure.circle",
                "Figure.round",
            )],
        )
        .expect("explicit variant rename");
        assert_eq!(
            last.id(ObjectKind::Variant, "Figure.round"),
            first.id(ObjectKind::Variant, "Shape.circle")
        );
        assert_eq!(
            last.id(ObjectKind::VariantField, "Figure.round.radius"),
            first.id(ObjectKind::VariantField, "Shape.circle.radius")
        );
    }

    /// Identity manifest of the `Media` table (Str key, Blob and `Float<MiB>`)
    /// under fixed identities, as the previous builder wrote it: sidecar
    /// version 1 with its private kind codes (table 1, field 2, unit 3).
    const MEDIA_V1_MANIFEST: &str = "8201868401654d656469615011111111111111111111111111111111f48402684d656469612e69645021212121212121212121212121212121f484026d4d656469612e7061796c6f61645022222222222222222222222222222222f484026a4d656469612e73697a655023232323232323232323232323232323f4840361425031313131313131313131313131313131f48403634d69425032323232323232323232323232323232f4";
    /// The same inventory as sidecar version 2, which stores each kind as its
    /// `sys.ObjectKind` ordinal (api/sys.json: table 2, column 3, unit 15).
    const MEDIA_MANIFEST: &str = "8202868402654d656469615011111111111111111111111111111111f48403684d656469612e69645021212121212121212121212121212121f484036d4d656469612e7061796c6f61645022222222222222222222222222222222f484036a4d656469612e73697a655023232323232323232323232323232323f4840f61425031313131313131313131313131313131f4840f634d69425032323232323232323232323232323232f4";
    /// The closed schema descriptor and its `orna.schema.v1` digest that the
    /// previous builder produced for `Media` under those identities.
    const MEDIA_DESCRIPTOR: &str = "a5000101d82550111111111111111111111111111111110281d8255021212121212121212121212121212121038385d825502121212121212121212121212121212162696482006353747200810085d8255022222222222222222222222222222222677061796c6f6164820064426c6f6201810085d82550232323232323232323232323232323236473697a658307820065466c6f6174d8255032323232323232323232323232323232018100048183d8255032323232323232323232323232323232038681011a00100000010001f4";
    const MEDIA_SCHEMA_DIGEST: &str =
        "554f46c098c0604e2177711c207eed49c75deea6c628d4323202dee5554837a9";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn media_identity_manifest() -> ObjectIdentityManifest {
        let record = |kind: ObjectKind, name: &str, byte: u8| ObjectIdentity {
            kind,
            qualified_name: name.to_owned(),
            id: [byte; 16],
            retired: false,
        };
        ObjectIdentityManifest {
            records: vec![
                record(ObjectKind::Table, "Media", 0x11),
                record(ObjectKind::Field, "Media.id", 0x21),
                record(ObjectKind::Field, "Media.payload", 0x22),
                record(ObjectKind::Field, "Media.size", 0x23),
                record(ObjectKind::Unit, "B", 0x31),
                record(ObjectKind::Unit, "MiB", 0x32),
            ],
        }
    }

    /// Builds the `Media` schema from `manifest`'s identities and returns its
    /// descriptor bytes and `orna.schema.v1` digest, both as hex.
    fn media_schema_hex(manifest: &ObjectIdentityManifest) -> (String, String) {
        let source = ModuleInput::new(
            "main.orna",
            include_str!("../tests/fixtures/vfs-blob-mib.orna"),
        );
        let analysis = orna_semantic_v1::analyze(std::slice::from_ref(&source));
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let schema = manifest
            .source_table_schema(
                std::slice::from_ref(&source),
                &analysis,
                None,
                "",
                "main.orna",
                "Media",
            )
            .unwrap();
        (
            hex(&schema.descriptor().encode().unwrap()),
            hex(&orna_value_v1::schema_identity(schema.descriptor()).unwrap()),
        )
    }

    /// Pins the identity manifest bytes, the closed schema descriptor bytes
    /// and the `orna.schema.v1` digest of a table the previous builder could
    /// already encode, so lifecycle support cannot change existing schema
    /// node identities. The descriptor carries object IDs, not `sys.ObjectKind`
    /// values (29-formats.md, closed storage schema descriptor), so the
    /// sidecar's kind encoding cannot reach it.
    #[test]
    fn previously_supported_table_descriptor_bytes_are_stable() {
        let manifest = media_identity_manifest();
        assert_eq!(hex(&manifest.encode().unwrap()), MEDIA_MANIFEST);
        let (descriptor, digest) = media_schema_hex(&manifest);
        assert_eq!(descriptor, MEDIA_DESCRIPTOR);
        assert_eq!(digest, MEDIA_SCHEMA_DIGEST);
    }

    /// ORNA-ROW-013, ORNA-EVOL-003: sidecar version 1 bytes, written before
    /// identity rows stored `sys.ObjectKind` ordinals, keep their versioned
    /// reader and decode to the same identities, so a legacy commit keeps the
    /// exact schema descriptor and digest. The writer emits version 2 only.
    #[test]
    fn legacy_v1_manifest_bytes_decode_to_the_same_table_descriptor() {
        let legacy = (0..MEDIA_V1_MANIFEST.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&MEDIA_V1_MANIFEST[at..at + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let decoded = ObjectIdentityManifest::decode(&legacy).unwrap();
        assert_eq!(decoded, media_identity_manifest());
        assert_eq!(hex(&decoded.encode().unwrap()), MEDIA_MANIFEST);
        let (descriptor, digest) = media_schema_hex(&decoded);
        assert_eq!(descriptor, MEDIA_DESCRIPTOR);
        assert_eq!(digest, MEDIA_SCHEMA_DIGEST);
    }

    fn git_in(root: &Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    const RENAME_SOURCE: &str =
        "pub enum Color { red, green, }\npub table Music(id: Str) { title: Str, hue: Color, }\n";

    /// `orna init`, the source staged with Git, and a staged store holding
    /// only the reconciled identity inventory.
    fn staged_inventory() -> (tempfile::TempDir, Repository, ObjectIdentityManifest) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let repository = orna_repository_v1::initialize_repository(root)
            .unwrap()
            .into_repository();
        fs::write(root.join("main.orna"), RENAME_SOURCE).unwrap();
        git_in(root, &["add", "main.orna", ".orna/database.orna"]);
        let identities = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", RENAME_SOURCE)],
            None,
        )
        .unwrap();
        let staged = repository.capture_staged_candidate().unwrap();
        repository
            .stage_store_admission(
                &staged,
                &[],
                &identities.to_stored(),
                orna_repository_v1::relation_admission::CommittedRelationChanges::Allowed,
            )
            .unwrap();
        (directory, repository, identities)
    }

    #[test]
    fn a_fresh_index_has_no_identity_to_keep() {
        let directory = tempfile::tempdir().unwrap();
        let repository = orna_repository_v1::initialize_repository(directory.path())
            .unwrap()
            .into_repository();
        assert_eq!(ObjectIdentityManifest::read_index(&repository), Ok(None));
        let renames = [ObjectIdentityRename::new(
            ObjectKind::Table,
            "Music",
            "Song",
        )];
        assert_eq!(
            ObjectIdentityManifest::stage_renames(&repository, &[], &renames, &[]),
            Ok(None)
        );
    }

    /// ORNA-OBJECT-003: a staged rename keeps the ObjectId, changes only the
    /// staged store, and allocates or retires nothing else.
    #[test]
    fn staged_renames_move_identities_in_the_index_store() {
        let (directory, repository, identities) = staged_inventory();
        let root = directory.path();
        assert_eq!(
            ObjectIdentityManifest::read_index(&repository).unwrap(),
            Some(identities.clone())
        );
        let renamed_source = "pub enum Color { crimson, green, }\npub table Song(id: Str) { name: Str, hue: Color, }\n";
        let renamed = [ModuleInput::new("main.orna", renamed_source)];
        let renames = [
            ObjectIdentityRename::new(ObjectKind::Table, "Music", "Song"),
            ObjectIdentityRename::new(ObjectKind::Field, "Music.title", "Song.name"),
            ObjectIdentityRename::new(ObjectKind::Variant, "Color.red", "Color.crimson"),
        ];
        let next = ObjectIdentityManifest::stage_renames(&repository, &renamed, &renames, &[])
            .unwrap()
            .expect("the index stages an inventory");
        for (kind, old, new) in [
            (ObjectKind::Table, "Music", "Song"),
            (ObjectKind::Field, "Music.title", "Song.name"),
            (ObjectKind::Field, "Music.id", "Song.id"),
            (ObjectKind::Field, "Music.hue", "Song.hue"),
            (ObjectKind::Variant, "Color.red", "Color.crimson"),
            (ObjectKind::Variant, "Color.green", "Color.green"),
            (ObjectKind::Type, "Color", "Color"),
        ] {
            assert_eq!(
                next.id(kind, new),
                identities.id(kind, old),
                "{kind:?} {new}"
            );
        }
        assert_eq!(next.records().len(), identities.records().len());
        assert!(next.records().iter().all(|record| !record.is_retired()));
        assert_eq!(
            ObjectIdentityManifest::read_index(&repository).unwrap(),
            Some(next.clone())
        );
        // Only the staged store changed: the staged and worktree source are
        // the caller's to edit and stage.
        assert_eq!(git_in(root, &["show", ":main.orna"]), RENAME_SOURCE);
        assert_eq!(
            fs::read_to_string(root.join("main.orna")).unwrap(),
            RENAME_SOURCE
        );
        assert!(repository.head().unwrap().is_none(), "nothing is committed");

        let refused = |renames: &[ObjectIdentityRename]| {
            ObjectIdentityManifest::stage_renames(&repository, &renamed, renames, &[]).unwrap_err()
        };
        assert_eq!(
            refused(&[ObjectIdentityRename::new(
                ObjectKind::Field,
                "Music.title",
                "Song.title"
            )]),
            ObjectIdentityError::MissingIdentity
        );
        assert_eq!(
            refused(&[ObjectIdentityRename::new(
                ObjectKind::Field,
                "Song.id",
                "Song.name"
            )]),
            ObjectIdentityError::IdentityCollision
        );
        assert_eq!(
            refused(&[ObjectIdentityRename::new(
                ObjectKind::Field,
                "Song.name",
                "Song.label"
            )]),
            ObjectIdentityError::InvalidSource
        );
        // A refused rename leaves the staged inventory as it was.
        assert_eq!(
            ObjectIdentityManifest::read_index(&repository).unwrap(),
            Some(next)
        );
    }
}
