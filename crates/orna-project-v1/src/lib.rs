//! Bounded source-project loading for a Git-backed Orna 1.0 worktree.
//!
//! This crate resolves ordinary source-module imports and discovers bounded,
//! opaque editable loose-row candidates. `sys` and `std` remain catalogue
//! dependencies; semantic schema admission, execution, and runtime state
//! remain outside this boundary.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt, fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use orna_repository_v1::{
    CommittedTableBinding, CommittedTreeEntry, CommittedTreeEntryKind, FormatContextError,
    GitCommitRef, OrpBlobBinding, PrivateCommit, ProtectedContentPin, Repository, RepositoryError,
    RepositoryFormatContext, TypedKey,
};
use orna_runtime_v1::vfs::{VfsComponent, project_key_component, unproject_component};
use orna_runtime_v1::{TableMutation, TableObjectId};
use orna_semantic_v1::{
    Analysis, Catalogue, ModuleInput, Namespace, RowKeySpelling, RowUnitInput, StandardCatalogueError,
    StandardDependencyProfile, SymbolKind, analyze, analyze_with_catalogue,
    lower_data_expression, lower_row_unit_with_key_spelling,
};
use orna_syntax_v1::{
    Declaration, Expr, FieldInitializer, TableMember, parse_module, parse_module_with_file, parse_row,
    parse_row_with_file,
};

use orna_value_v1::{
    ContextValue, Format3Context, Raw, SchemaDescriptor, Snapshot, Value, ValueFormat, ROV3_BLOB_TAG,
};
use unicode_normalization::UnicodeNormalization;

mod attachments;
mod unicode16;
mod object_identity;
mod typed_row;
mod table_storage;

pub use object_identity::{
    ObjectIdentity, ObjectIdentityError, ObjectIdentityManifest, ObjectIdentityRename,
    ObjectKind, SourceTableSchemaArtifact, SourceTableSchemaError,
    OBJECT_IDENTITY_MANIFEST_PATH,
};
pub use typed_row::{DescriptorField, StoredSchema, TypedRowError};
pub use table_storage::{
    CommittedReadError, CommittedReadLimits, CommittedTable, TableStorageError,
    admitted_table_schemas, blob_placeholder, committed_tables, private_candidate_tables,
    evaluator_rows, evaluator_rows_from_stored, has_text_primary_key, holds_blob,
    restore_stored_blobs, runtime_key_from_evaluator, storage_row_from_record, stored_field_names,
    stored_rows_hold_blob,
};

pub use attachments::{
    AttachedDatabaseSession, AttachedRelationSource, AttachmentError, PACKAGE_PIN_MANIFEST_PATH,
    NestedPairDepthLabel, PackagePin, PackagePinManifest, PackagePinSpec, PackageResolver,
    PinnedDatabase, ReboundPathCheckpoint, ReboundPathResolution, SiblingRebindResolution,
};


/// Bounded resource limits applied before source contents are read.
///
/// These are implementation resource limits: a loader may configure them, and
/// each one is reported by its own error when exceeded (ORNA-CLOSURE-004).
///
/// The committed source tree walk also runs under fixed bounds that are not
/// configurable here. A breach is reported as
/// [`ProjectLoadError::SourceTreeLimit`] before git expands the tree: at most
/// 2^20 visited entries (every interior tree and every listed entry, counting
/// each occurrence of a shared subtree), at most 2^27 listed path bytes, and
/// nesting depth at most 256.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectLimits {
    /// Maximum reachable modules loaded and parsed.
    pub max_modules: usize,
    pub max_source_bytes: usize,
    /// Maximum source module files whose paths are checked for namespace
    /// ownership (ORNA-NS-004, ORNA-NS-005, ORNA-NS-008) before any source is
    /// read. Unreachable modules are checked without being admitted
    /// (ORNA-REPO-002, ORNA-REPO-003), so they count too. Nothing else does:
    /// other files are not modules, and the format-3 store at `.orna/store` is
    /// database content that the loader never walks (19-repository: a "native
    /// Git tree, normally sparsely excluded").
    pub max_source_modules: usize,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            max_modules: 256,
            max_source_bytes: 4 * 1024 * 1024,
            max_source_modules: 4_096,
        }
    }
}

/// One loaded, repository-relative module identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleIdentity {
    logical_path: String,
    namespace: Vec<String>,
}

impl ModuleIdentity {
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub fn namespace(&self) -> &[String] {
        &self.namespace
    }
}

/// One reachable editable loose-row source unit.
///
/// The loader owns only repository identity and bounded source discovery.  The
/// table path and key path remain opaque strings until semantic admission binds
/// them to a declared schema and decodes their types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseRowCandidate {
    logical_path: String,
    table_path: String,
    key_path: Vec<String>,
    source: String,
}

impl LooseRowCandidate {
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Canonical repository-relative directory owning this row's table.
    pub fn table_path(&self) -> &str {
        &self.table_path
    }

    /// Canonical, encoded path components after the table directory.  The
    /// final component retains its `.orna` suffix; semantic admission owns
    /// decoding and key-arity/type checks.
    pub fn key_path(&self) -> &[String] {
        &self.key_path
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn source_bytes(&self) -> &[u8] {
        self.source.as_bytes()
    }

    /// The downstream semantic/conformance boundary's stable unit tag.
    pub const fn parse_as(&self) -> &'static str {
        "row_unit"
    }
}

/// A source-analysis schema bound to the exact committed snapshot that issued
/// its table and field identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTableSchema {
    source_commit: GitCommitRef,
    snapshot_id: [u8; 32],
    database_id: [u8; 16],
    table: SourceTableSchemaArtifact,
    binding: CommittedTableBinding,
}

impl ResolvedTableSchema {
    pub const fn source_commit(&self) -> &GitCommitRef {
        &self.source_commit
    }

    pub const fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub fn declaration_name(&self) -> &str {
        self.table.declaration_name()
    }

    /// The table's qualified name in its module namespace (root tables stay
    /// bare): the identity runtime mutations and the identity manifest use,
    /// so same-named tables in different modules never alias.
    pub fn qualified_name(&self) -> String {
        match self.table.module_name() {
            "" => self.table.declaration_name().to_owned(),
            module => format!("{module}.{}", self.table.declaration_name()),
        }
    }

    pub fn table_object_id(&self) -> TableObjectId {
        TableObjectId::new(*self.table.table_object_id())
    }

    pub fn descriptor(&self) -> &SchemaDescriptor {
        self.table.descriptor()
    }

    pub fn binding(&self) -> &CommittedTableBinding {
        &self.binding
    }

    /// The stored values the table's required fields are introduced with, keyed by field
    /// ObjectId, as `project` frozen them (ORNA-EVOL-009). Used when a row written under an
    /// earlier schema generation is admitted to this one.
    pub fn introduced_values(&self, project: &LoadedProject) -> BTreeMap<[u8; 16], Raw> {
        project.constant_default_stored_values(&self.table)
    }
}

/// One exact accepted ORP Blob value paired with its field identity and pin.
#[derive(Debug)]
pub struct SourceBlobFieldBinding {
    field_object_id: [u8; 16],
    encoded_value: Vec<u8>,
    pin: ProtectedContentPin,
}

impl SourceBlobFieldBinding {
    pub const fn field_object_id(&self) -> &[u8; 16] {
        &self.field_object_id
    }

    pub fn encoded_value(&self) -> &[u8] {
        &self.encoded_value
    }

    pub fn pin(&self) -> &ProtectedContentPin {
        &self.pin
    }

    /// Moves the exact source field identity, encoded value and pin into the
    /// owner-fenced publication path.
    pub fn into_parts(self) -> ([u8; 16], Vec<u8>, ProtectedContentPin) {
        (self.field_object_id, self.encoded_value, self.pin)
    }
}

/// An admitted source row and its exact validated, committed schema authority.
/// Publication retains this artifact with the canonical mutation and Blob pins.
#[derive(Debug)]
pub struct AdmittedSourceRow {
    resolved_schema: ResolvedTableSchema,
    canonical_key: Vec<u8>,
    canonical_fields: Vec<u8>,
    blob_bindings: Vec<SourceBlobFieldBinding>,
    mutation: TableMutation,
}

impl AdmittedSourceRow {
    pub fn table_name(&self) -> &str {
        self.resolved_schema.declaration_name()
    }
    pub fn table_object_id(&self) -> TableObjectId {
        self.resolved_schema.table_object_id()
    }
    pub const fn database_id(&self) -> &[u8; 16] {
        self.resolved_schema.database_id()
    }
    pub fn source_commit(&self) -> &GitCommitRef {
        self.resolved_schema.source_commit()
    }
    pub const fn snapshot_id(&self) -> &[u8; 32] {
        self.resolved_schema.snapshot_id()
    }
    pub fn schema(&self) -> &SchemaDescriptor {
        self.resolved_schema.descriptor()
    }
    pub fn resolved_schema(&self) -> &ResolvedTableSchema {
        &self.resolved_schema
    }
    pub fn canonical_key(&self) -> &[u8] {
        &self.canonical_key
    }
    pub fn canonical_fields(&self) -> &[u8] {
        &self.canonical_fields
    }
    pub fn blob_bindings(&self) -> &[SourceBlobFieldBinding] {
        &self.blob_bindings
    }
    pub fn mutation(&self) -> &TableMutation {
        &self.mutation
    }

    /// Moves the canonical mutation, owned Blob pins, and schema artifact together.
    pub fn into_mutation_and_bindings(
        self,
    ) -> (
        TableMutation,
        Vec<SourceBlobFieldBinding>,
        ResolvedTableSchema,
    ) {
        (self.mutation, self.blob_bindings, self.resolved_schema)
    }
}

/// The deterministic source inputs suitable for `analyze_with_catalogue`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProject {
    modules: Vec<ModuleInput>,
    identities: Vec<ModuleIdentity>,
    loose_rows: Vec<LooseRowCandidate>,
    standard_profile: Option<StandardDependencyProfile>,
    standard_sources: Vec<(String, String)>,
    standard_runtime_modules: BTreeSet<String>,
    standard_imports: bool,
    standard_modules: BTreeSet<String>,
    source_commit: Option<GitCommitRef>,
}

impl LoadedProject {
    pub fn modules(&self) -> &[ModuleInput] {
        &self.modules
    }

    /// The verified commit from which these source units were read, when
    /// loaded through a committed-snapshot or private-candidate entrypoint.
    pub fn source_commit(&self) -> Option<&GitCommitRef> {
        self.source_commit.as_ref()
    }

    /// Canonical repository snapshot identity for the exact source commit.
    pub fn source_snapshot_id(&self) -> Option<[u8; 32]> {
        self.source_commit.as_ref().map(GitCommitRef::snapshot_id)
    }

    pub fn identities(&self) -> &[ModuleIdentity] {
        &self.identities
    }
    /// Reachable editable row units, in canonical logical-path order.
    pub fn loose_rows(&self) -> &[LooseRowCandidate] {
        &self.loose_rows
    }


    pub fn into_loose_rows(self) -> Vec<LooseRowCandidate> {
        self.loose_rows
    }


    pub fn into_modules(self) -> Vec<ModuleInput> {
        self.modules
    }

    /// Returns the explicitly supplied immutable standard dependency profile,
    /// if this project was loaded with one.
    pub fn standard_profile(&self) -> Option<&StandardDependencyProfile> {
        self.standard_profile.as_ref()
    }

    /// Source units read from the exact Git commit attached as `stdlib/std`,
    /// rewritten under the reserved `std/` logical namespace.
    pub fn standard_sources(&self) -> &[(String, String)] {
        &self.standard_sources
    }

    /// Pinned standard modules reachable from this program's explicit imports.
    pub fn standard_runtime_modules(&self) -> &BTreeSet<String> {
        &self.standard_runtime_modules
    }

    /// Reports whether reachable project source requested the reserved
    /// standard namespace. The loader does not treat that request as proof
    /// that a verified standard profile was supplied.
    pub const fn has_standard_imports(&self) -> bool {
        self.standard_imports
    }

    /// Returns the logical standard modules requested by reachable source.
    pub fn standard_modules(&self) -> &BTreeSet<String> {
        &self.standard_modules
    }

    /// Derives the semantic catalogue for this project's explicitly pinned
    /// standard dependency. Source bytes remain caller-supplied and are
    /// verified against the profile; ordinary project loading never discovers
    /// a host standard library.
    pub fn standard_catalogue(
        &self,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Option<Catalogue>, StandardCatalogueError> {
        self.standard_profile
            .as_ref()
            .map(|profile| Catalogue::from_standard_sources(profile, sources))
            .transpose()
    }

    /// The production catalogue of this project, used by `orna check`, `orna run`
    /// (table and stream roots) and VFS admission: the authoritative core,
    /// extended by the pinned standard profile and its verified sources when
    /// this project captured one.
    pub fn production_catalogue(&self) -> Result<Catalogue, StandardCatalogueError> {
        let catalogue = Catalogue::authoritative_core();
        let Some(profile) = self.standard_profile.as_ref() else {
            return Ok(catalogue);
        };
        catalogue.with_standard_sources(profile, self.standard_sources.iter().cloned())
    }

    /// Whether reachable source imports a standard module while this project
    /// captured no standard dependency. Such a project has no production
    /// catalogue: every standard import is unavailable.
    pub fn has_uncaptured_standard_imports(&self) -> bool {
        !self.standard_modules.is_empty() && self.standard_profile.is_none()
    }

    /// Builds a source-only schema from this project's committed identity
    /// manifest. A table that stores a table reference records its database,
    /// so it fails here; use [`Self::source_table_schema_in_database`].
    pub fn source_table_schema(
        &self,
        manifest: &ObjectIdentityManifest,
        module_name: &str,
        declaration_name: &str,
    ) -> Result<SourceTableSchemaArtifact, ProjectLoadError> {
        self.build_source_table_schema(manifest, None, module_name, declaration_name)
    }

    /// Builds a source-only schema whose stored table references bind
    /// `database_id`, the database that owns this project's tables.
    pub fn source_table_schema_in_database(
        &self,
        manifest: &ObjectIdentityManifest,
        database_id: [u8; 16],
        module_name: &str,
        declaration_name: &str,
    ) -> Result<SourceTableSchemaArtifact, ProjectLoadError> {
        self.build_source_table_schema(manifest, Some(database_id), module_name, declaration_name)
    }

    fn build_source_table_schema(
        &self,
        manifest: &ObjectIdentityManifest,
        database_id: Option<[u8; 16]>,
        module_name: &str,
        declaration_name: &str,
    ) -> Result<SourceTableSchemaArtifact, ProjectLoadError> {
        let source_path = self
            .identities
            .iter()
            .find(|identity| Namespace(identity.namespace().to_vec()).display() == module_name)
            .map(ModuleIdentity::logical_path)
            .ok_or(ProjectLoadError::InvalidTableSchema)?;
        let catalogue = self
            .standard_catalogue(self.standard_sources.iter().cloned())
            .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
        let analysis = match catalogue.as_ref() {
            Some(catalogue) => analyze_with_catalogue(&self.modules, catalogue),
            None => analyze(&self.modules),
        };
        manifest
            .source_table_schema(
                &self.modules,
                &analysis,
                database_id,
                module_name,
                source_path,
                declaration_name,
            )
            .map_err(|_| ProjectLoadError::InvalidTableSchema)
    }

    /// The stored value of each closed constant insert-time default of the
    /// table `artifact` describes, keyed by field ObjectId. When schema
    /// evolution introduces such a required field, each committed row
    /// receives this value, frozen at the introducing commit
    /// (ORNA-FIELD-003, ORNA-EVOL-009). A default that needs evaluation has
    /// no entry (ORNA-FIELD-005).
    pub fn constant_default_stored_values(
        &self,
        artifact: &SourceTableSchemaArtifact,
    ) -> BTreeMap<[u8; 16], Raw> {
        let Ok(schema) = StoredSchema::new(artifact.descriptor()) else {
            return BTreeMap::new();
        };
        table_constant_defaults(self, artifact.module_name(), artifact.declaration_name())
            .into_iter()
            .filter_map(|(name, value)| {
                let field = schema.field(&name).filter(|field| field.role() == 1)?;
                let stored = schema.lower_field(field, &value).ok()?;
                Some((*field.id(), stored))
            })
            .collect()
    }
}
/// Admit a completed format-3 row by its typed key and value, without deriving
/// a loose-row filename that has no authority in the native row store.
pub fn admit_format3_row(
    project: &LoadedProject,
    canonical_key: &[u8],
    row: &Raw,
    schema: &ResolvedTableSchema,
    context: &Format3Context,
    blob_bindings: Vec<(String, OrpBlobBinding)>,
    mutation_id: [u8; 16],
) -> Result<AdmittedSourceRow, ProjectLoadError> {
    if project.source_commit() != Some(schema.source_commit())
        || project.source_snapshot_id().as_ref() != Some(schema.snapshot_id())
        || !format_context_matches_source_schema(
            context,
            schema.database_id(),
            schema.source_commit().as_str(),
        )
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    if blob_bindings
        .iter()
        .any(|(_, binding)| binding.transfer_record().database_id() != schema.database_id())
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }

    let (descriptor_table_id, fields) = source_schema_fields(schema.descriptor())?;
    if &descriptor_table_id != schema.table.table_object_id() {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let key_ids = match raw_map_value(schema.descriptor().raw(), "2") {
        Some(Raw::Array(ids)) => ids
            .iter()
            .map(|id| committed_uuid(id).ok_or(ProjectLoadError::InvalidSourceRow))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(ProjectLoadError::InvalidSourceRow),
    };
    if key_ids.is_empty() {
        return Err(ProjectLoadError::InvalidSourceRow);
    }

    let Raw::Map(entries) = row else {
        return Err(ProjectLoadError::InvalidSourceRow);
    };
    let mut row_fields = BTreeMap::new();
    for (key, value) in entries {
        let Raw::Text(name) = key else {
            return Err(ProjectLoadError::InvalidSourceRow);
        };
        if row_fields.insert(name.as_str(), value).is_some() {
            return Err(ProjectLoadError::InvalidSourceRow);
        }
    }

    let key_fields = key_ids
        .iter()
        .map(|id| {
            fields
                .iter()
                .find(|field| field.id == *id && field.role == 0)
                .ok_or(ProjectLoadError::InvalidSourceRow)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let typed_key = Value::decode(canonical_key).map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    let key_matches = if let [field] = key_fields.as_slice() {
        row_fields
            .get(field.name.as_str())
            .is_some_and(|value| *value == typed_key.raw())
    } else {
        match typed_key.raw() {
            Raw::Array(components) if components.len() == key_fields.len() => key_fields
                .iter()
                .zip(components)
                .all(|(field, component)| {
                    row_fields
                        .get(field.name.as_str())
                        .is_some_and(|value| *value == component)
                }),
            _ => false,
        }
    };
    if !key_matches {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let key_field_names = key_fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    let stored_names = fields
        .iter()
        .filter(|field| field.role == 1)
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    if row_fields.keys().any(|name| {
        !key_field_names.contains(*name) && !stored_names.contains(*name)
    }) {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let stored_row_fields = fields
        .iter()
        .filter(|field| field.role == 1)
        .filter_map(|field| {
            row_fields
                .get(field.name.as_str())
                .map(|value| (field.name.as_str(), *value))
        })
        .collect::<BTreeMap<_, _>>();

    // The evaluator already evaluated every omitted default once
    // (ORNA-FIELD-001), so the completed row supplies every stored field.
    admit_stored_source_row(
        schema,
        context,
        canonical_key.to_vec(),
        &stored_row_fields,
        &BTreeMap::new(),
        blob_bindings,
        Vec::new(),
        mutation_id,
    )
}

/// Admit a source row expression and accepted captured Blob bindings as one
/// canonical mutation. This variant accepts no source-owner scalar fields.
///
/// The logical key path must use the VFS-1 projection (profiles/vfs-1.md:44-56).
/// The legacy `key-path-v1` spelling (`~xx`, `~ff`) is refused with
/// `InvalidSourceRow`: legacy loose rows are read only through their recorded
/// import context (Orna-1.1.0.md, "File extension"), which this path does not admit.
pub fn admit_source_row(
    project: &LoadedProject,
    logical_path: &str,
    source: &str,
    schema: &ResolvedTableSchema,
    context: &Format3Context,
    blob_bindings: Vec<(String, OrpBlobBinding)>,
    mutation_id: [u8; 16],
) -> Result<AdmittedSourceRow, ProjectLoadError> {
    admit_source_row_with_owner_fields_inner(
        project,
        logical_path,
        source,
        schema,
        context,
        blob_bindings,
        Vec::new(),
        &[],
        mutation_id,
    )
}

/// Admit a source row while filling additional stored scalar fields supplied
/// by the trusted source owner (for example, payload size derived from a Blob).
pub fn admit_source_row_with_owner_fields(
    project: &LoadedProject,
    logical_path: &str,
    source: &str,
    schema: &ResolvedTableSchema,
    context: &Format3Context,
    blob_bindings: Vec<(String, OrpBlobBinding)>,
    owner_field_values: Vec<(String, Raw)>,
    key_components: &[KeyComponentText],
    mutation_id: [u8; 16],
) -> Result<AdmittedSourceRow, ProjectLoadError> {
    admit_source_row_with_owner_fields_inner(
        project,
        logical_path,
        source,
        schema,
        context,
        blob_bindings,
        owner_field_values,
        key_components,
        mutation_id,
    )
}

fn admit_source_row_with_owner_fields_inner(
    project: &LoadedProject,
    logical_path: &str,
    source: &str,
    schema: &ResolvedTableSchema,
    context: &Format3Context,
    blob_bindings: Vec<(String, OrpBlobBinding)>,
    owner_field_values: Vec<(String, Raw)>,
    key_components: &[KeyComponentText],
    mutation_id: [u8; 16],
) -> Result<AdmittedSourceRow, ProjectLoadError> {
    if project.source_commit() != Some(schema.source_commit())
        || project.source_snapshot_id().as_ref() != Some(schema.snapshot_id())
        || !format_context_matches_source_schema(
            context,
            schema.database_id(),
            schema.source_commit().as_str(),
        )
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    if blob_bindings
        .iter()
        .any(|(_, binding)| binding.transfer_record().database_id() != schema.database_id())
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let catalogue = project
        .standard_catalogue(project.standard_sources().iter().cloned())
        .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    let analysis = match catalogue.as_ref() {
        Some(catalogue) => analyze_with_catalogue(project.modules(), catalogue),
        None => analyze(project.modules()),
    };
    if !analysis.is_ok() {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let module_name = source_table_module_name(project, &analysis, schema.declaration_name())?;
    let table_path = if module_name.is_empty() {
        schema.declaration_name().to_owned()
    } else {
        format!(
            "{}/{}",
            module_name.replace('.', "/"),
            schema.declaration_name()
        )
    };
    let row_prefix = format!("{table_path}/");
    let key_path = logical_path
        .strip_prefix(&row_prefix)
        .map(|key| key.split('/').map(str::to_owned).collect::<Vec<_>>())
        .filter(|key| !key.is_empty())
        .ok_or(ProjectLoadError::InvalidSourceRow)?;
    let input = RowUnitInput {
        logical_path,
        table_path: &table_path,
        key_path: &key_path,
        source,
        source_bytes: source.as_bytes(),
        parse_as: "row_unit",
    };
    let mut owner_field_names = BTreeSet::new();
    if owner_field_values
        .iter()
        .any(|(name, _)| !owner_field_names.insert(name.clone()))
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let owner_field_names = owner_field_values.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
    let parsed_row = parse_row(source);
    if !parsed_row.diagnostics.is_empty() {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let Expr::Record {
        fields: parsed_fields, ..
    } = parsed_row.value
    else {
        return Err(ProjectLoadError::InvalidSourceRow);
    };
    let source_field_names = parsed_fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut reserved_field_names = blob_bindings
        .iter()
        .filter(|(name, _)| !source_field_names.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    reserved_field_names.extend(owner_field_names.iter().cloned());
    let decoded_key = decode_source_key_path(&key_path, key_components)
        .ok_or(ProjectLoadError::InvalidSourceRow)?;
    let lowered = lower_row_unit_with_key_spelling(
        &analysis,
        &input,
        &reserved_field_names,
        RowKeySpelling::VerifiedVfs1(&decoded_key),
    )
    .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    if lowered.owner != table_path {
        return Err(ProjectLoadError::InvalidSourceRow);
    }

    let row_fields = lowered
        .fields
        .iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect::<BTreeMap<_, _>>();
    let defaults = table_constant_defaults(project, &module_name, schema.declaration_name());
    admit_stored_source_row(
        schema,
        context,
        lowered.canonical_key,
        &row_fields,
        &defaults,
        blob_bindings,
        owner_field_values,
        mutation_id,
    )
}

/// One key component of a source row: its canonical text and its canonical
/// OVB-1 encoding (`TypedKey::canonical_bytes`). A `~key-` long alias digests
/// the encoding and cannot be reversed to its text, so admission needs the pair
/// to name the row's key (profiles/vfs-1.md:52-56, ORNA-VFS-016).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyComponentText {
    pub text: String,
    pub canonical_encoding: Vec<u8>,
}

/// Decodes the VFS-1 projected key components of a logical source row path to
/// canonical key text. Each component must re-project to exactly its spelling
/// (profiles/vfs-1.md:44-50); a `~key-` alias resolves only through a supplied
/// pair whose projection is that alias and whose text is the canonical text of
/// its encoding (profiles/vfs-1.md:52-56).
fn decode_source_key_path(
    key_path: &[String],
    key_components: &[KeyComponentText],
) -> Option<Vec<String>> {
    let (last, parents) = key_path.split_last()?;
    let last = last.strip_suffix(".orna")?;
    parents
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(last))
        .map(|component| match unproject_component(component).ok()? {
            VfsComponent::Canonical(text) => Some(text),
            VfsComponent::LongAlias { .. } => key_components
                .iter()
                .find(|key| {
                    project_key_component(&key.text, &key.canonical_encoding) == component
                        && key_pair_round_trips(key)
                })
                .map(|key| key.text.clone()),
        })
        .collect()
}

/// A supplied pair names a key only when its encoding decodes to a typed key
/// whose canonical component text is exactly the supplied text. Without this
/// check a caller could pair a digest-matching encoding with any text.
fn key_pair_round_trips(key: &KeyComponentText) -> bool {
    TypedKey::decode_canonical(&key.canonical_encoding)
        .ok()
        .and_then(|typed| typed.component_text())
        .is_some_and(|text| text == key.text)
}

/// The closed constant insert-time defaults of `table` in `module_name`, as
/// schema-free data values (ORNA-FIELD-001, ORNA-FIELD-004). A row document
/// that omits such a field stores the default's value. A default that needs
/// evaluation is absent here, so a document must supply that field.
fn table_constant_defaults(
    project: &LoadedProject,
    module_name: &str,
    table: &str,
) -> BTreeMap<String, Raw> {
    let mut defaults = BTreeMap::new();
    for module in project.modules() {
        if object_identity::module_namespace(&module.logical_path).as_deref() != Some(module_name) {
            continue;
        }
        let parsed = parse_module(&module.source);
        for item in &parsed.value.items {
            let Declaration::Table { name, members, .. } = &item.declaration else {
                continue;
            };
            if name != table {
                continue;
            }
            for member in members {
                if let TableMember::Field {
                    name,
                    initializer: Some(FieldInitializer::Default(expression)),
                    ..
                } = member
                {
                    if let Some(value) = lower_data_expression(expression) {
                        defaults.insert(name.clone(), value);
                    }
                }
            }
        }
    }
    defaults
}

/// Lowers a source row's fields against its committed descriptor and binds
/// its Blob fields. Every stored field comes from exactly one of the row,
/// an owner-supplied value, a Blob binding or a constant default; each value
/// is lowered to its stored type (ORNA-FORMAT-004).
#[allow(clippy::too_many_arguments)]
fn admit_stored_source_row(
    schema: &ResolvedTableSchema,
    context: &Format3Context,
    canonical_key: Vec<u8>,
    row_fields: &BTreeMap<&str, &Raw>,
    defaults: &BTreeMap<String, Raw>,
    blob_bindings: Vec<(String, OrpBlobBinding)>,
    owner_field_values: Vec<(String, Raw)>,
    mutation_id: [u8; 16],
) -> Result<AdmittedSourceRow, ProjectLoadError> {
    let stored_schema =
        StoredSchema::new(schema.descriptor()).map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    let (descriptor_table_id, fields) = source_schema_fields(schema.descriptor())?;
    if &descriptor_table_id != schema.table.table_object_id() {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    schema
        .descriptor()
        .validate_stored_key(*schema.table.table_object_id(), &canonical_key)
        .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    let mut key_fields = fields.iter().filter(|field| field.role == 0);
    let is_text_primary_key = match (key_fields.next(), key_fields.next()) {
        (Some(field), None) => field.is_text,
        _ => false,
    };
    let mutation_key = if is_text_primary_key {
        let key = Value::decode(&canonical_key)
            .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
        let Raw::Text(key) = key.raw() else {
            return Err(ProjectLoadError::InvalidSourceRow);
        };
        key.as_bytes().to_vec()
    } else {
        canonical_key.clone()
    };
    let mut pending_owner_fields = BTreeMap::new();
    for (name, value) in owner_field_values {
        if pending_owner_fields.insert(name, value).is_some() {
            return Err(ProjectLoadError::InvalidSourceRow);
        }
    }
    if pending_owner_fields.keys().any(|name| {
        !fields
            .iter()
            .any(|field| field.name == *name && field.role == 1 && !field.is_blob)
    }) {
        return Err(ProjectLoadError::InvalidSourceRow);
    }
    let mut pending_bindings = BTreeMap::new();
    for (field_name, binding) in blob_bindings {
        let field = fields
            .iter()
            .find(|field| field.name == field_name && field.role == 1 && field.is_blob)
            .ok_or(ProjectLoadError::InvalidSourceRow)?;
        if pending_bindings.insert(field.id, binding).is_some() {
            return Err(ProjectLoadError::InvalidSourceRow);
        }
    }

    let mut stored_fields = Vec::new();
    let mut retained_bindings = Vec::new();
    for field in fields.iter().filter(|field| field.role == 1) {
        if field.is_blob {
            if pending_owner_fields.contains_key(&field.name) {
                return Err(ProjectLoadError::InvalidSourceRow);
            }
            let Some(binding) = pending_bindings.remove(&field.id) else {
                if field.is_optional_blob
                    && row_fields
                        .get(field.name.as_str())
                        .is_some_and(|value| **value == Raw::Null)
                {
                    stored_fields.push(Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))));
                    continue;
                }
                return Err(ProjectLoadError::InvalidSourceRow);
            };
            if row_fields.contains_key(field.name.as_str()) {
                return Err(ProjectLoadError::InvalidSourceRow);
            }
            let encoded_value = binding.encoded_value().to_vec();
            let decoded = ContextValue::decode_in_context(&encoded_value, ValueFormat::Rov3, context)
                .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
            if !matches!(decoded.raw(), Raw::Tag(ROV3_BLOB_TAG, _)) {
                return Err(ProjectLoadError::InvalidSourceRow);
            }
            let encoded_blob = decoded.into_raw();
            stored_fields.push(if field.is_optional_blob {
                Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(1.into()), encoded_blob])))
            } else {
                encoded_blob
            });
            retained_bindings.push(SourceBlobFieldBinding {
                field_object_id: field.id,
                encoded_value,
                pin: binding.into_pin(),
            });
        } else {
            if pending_bindings.contains_key(&field.id) {
                return Err(ProjectLoadError::InvalidSourceRow);
            }
            let value = match (
                pending_owner_fields.remove(&field.name),
                row_fields.get(field.name.as_str()),
            ) {
                (Some(_), Some(_)) => return Err(ProjectLoadError::InvalidSourceRow),
                (Some(owner_value), None) => owner_value,
                (None, Some(value)) => (**value).clone(),
                // ORNA-FIELD-001: an omitted defaulted field stores the
                // default's value, evaluated once for this insertion.
                (None, None) => defaults
                    .get(&field.name)
                    .cloned()
                    .ok_or(ProjectLoadError::InvalidSourceRow)?,
            };
            let descriptor_field = stored_schema
                .field(&field.name)
                .ok_or(ProjectLoadError::InvalidSourceRow)?;
            stored_fields.push(
                stored_schema
                    .lower_field(descriptor_field, &value)
                    .map_err(|_| ProjectLoadError::InvalidSourceRow)?,
            );
        }
    }
    if !pending_bindings.is_empty()
        || !pending_owner_fields.is_empty()
        || row_fields.keys().any(|name| {
            !fields
                .iter()
                .any(|field| field.name == *name && field.role == 1)
        })
    {
        return Err(ProjectLoadError::InvalidSourceRow);
    }

    let canonical_fields =
        ContextValue::new_in_context(context, ValueFormat::Rov3, Raw::Array(stored_fields))
            .and_then(|value| value.encode())
            .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    schema
        .descriptor()
        .validate_stored_row(
            *schema.table.table_object_id(),
            &canonical_key,
            &canonical_fields,
        )
        .map_err(|_| ProjectLoadError::InvalidSourceRow)?;

    let table_name = schema.declaration_name().to_owned();
    let mut mutation = TableMutation::insert(
        mutation_id,
        table_name.clone(),
        mutation_key,
        canonical_fields.clone(),
    )
    .map_err(|_| ProjectLoadError::InvalidSourceRow)?
    .with_table_object_id(schema.table_object_id());
    for binding in &retained_bindings {
        mutation = mutation
            .with_protected_content_pin(&binding.pin)
            .map_err(|_| ProjectLoadError::InvalidSourceRow)?;
    }
    Ok(AdmittedSourceRow {
        resolved_schema: schema.clone(),
        canonical_key,
        canonical_fields,
        blob_bindings: retained_bindings,
        mutation,
    })
}

#[derive(Clone, Debug)]
struct SourceSchemaField {
    id: [u8; 16],
    name: String,
    role: u64,
    is_blob: bool,
    is_optional_blob: bool,
    is_text: bool,
}

fn source_schema_blob_optional(ty: &Raw) -> Option<bool> {
    let Raw::Array(parts) = ty else { return None; };
    match parts.as_slice() {
        [Raw::Int(kind), Raw::Text(name)]
            if kind.to_string() == "0"
                && matches!(name.as_str(), "Blob" | "std.BINARY_LARGE_OBJECT") =>
        {
            Some(false)
        }
        [Raw::Int(kind), inner] if kind.to_string() == "2" => {
            source_schema_blob_optional(inner).map(|_| true)
        }
        _ => None,
    }
}

fn source_schema_fields(
    descriptor: &SchemaDescriptor,
) -> Result<([u8; 16], Vec<SourceSchemaField>), ProjectLoadError> {
    let table_id = raw_map_value(descriptor.raw(), "1")
        .and_then(committed_uuid)
        .ok_or(ProjectLoadError::InvalidSourceRow)?;
    let Raw::Array(entries) = raw_map_value(descriptor.raw(), "3")
        .ok_or(ProjectLoadError::InvalidSourceRow)?
    else {
        return Err(ProjectLoadError::InvalidSourceRow);
    };
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut fields = Vec::with_capacity(entries.len());
    for entry in entries {
        let Raw::Array(parts) = entry else { return Err(ProjectLoadError::InvalidSourceRow); };
        let [id, Raw::Text(name), ty, Raw::Int(role), _] = parts.as_slice() else {
            return Err(ProjectLoadError::InvalidSourceRow);
        };
        let id = committed_uuid(id).ok_or(ProjectLoadError::InvalidSourceRow)?;
        if !ids.insert(id) || !names.insert(name.clone()) {
            return Err(ProjectLoadError::InvalidSourceRow);
        }
        let blob_optional = source_schema_blob_optional(ty);
        let role = role.to_string().parse::<u64>().map_err(|_| ProjectLoadError::InvalidSourceRow)?;
        // Role 2 is a computed selector: never a stored column (ORNA-FIELD-006).
        if role > 2 { return Err(ProjectLoadError::InvalidSourceRow); }
        fields.push(SourceSchemaField {
            id,
            name: name.clone(),
            role,
            is_blob: blob_optional.is_some(),
            is_optional_blob: blob_optional == Some(true),
            is_text: matches!(
                ty,
                Raw::Array(parts)
                    if matches!(parts.as_slice(), [Raw::Int(kind), Raw::Text(name)]
                        if kind.to_string() == "0" && name == "Str")
            ),
        });
    }
    Ok((table_id, fields))
}

fn format_context_matches_source_schema(
    context: &Format3Context,
    expected_database_id: &[u8; 16],
    commit: &str,
) -> bool {
    context.database_uuid() == *expected_database_id && format_context_matches_commit(context, commit)
}

fn format_context_matches_commit(context: &Format3Context, commit: &str) -> bool {
    let (algorithm, oid) = match commit.len() {
        40 => (orna_value_v1::GitHash::Sha1, decode_source_oid(commit)),
        64 => (orna_value_v1::GitHash::Sha256, decode_source_oid(commit)),
        _ => return false,
    };
    let Some(oid) = oid else { return false; };
    matches!(context.pin(), Snapshot::Commit { database, algorithm: actual_algorithm, oid: actual_oid }
        if *database == context.database_uuid()
            && *actual_algorithm == algorithm
            && actual_oid == &oid)
}

fn decode_source_oid(value: &str) -> Option<Vec<u8>> {
    if !value.is_ascii() || value.len() % 2 != 0 { return None; }
    value.as_bytes().chunks_exact(2).map(|pair| {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        Some(((high << 4) | low) as u8)
    }).collect()
}

/// Loads the root module and its reachable ordinary source imports.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProjectLoader {
    limits: ProjectLimits,
}

impl ProjectLoader {
    pub fn new(limits: ProjectLimits) -> Self {
        Self { limits }
    }

    pub fn load(&self, repository: &Repository) -> Result<LoadedProject, ProjectLoadError> {
        let project = self.load_with_standard_profile(repository, None)?;
        let commit = repository.head().map_err(ProjectLoadError::Repository)?;
        self.bind_attached_standard_module(repository, commit.as_ref(), project)
    }

    /// Loads a project while carrying an explicitly selected standard
    /// dependency profile. The profile is metadata only at this boundary:
    /// standard modules are never discovered from the worktree and are not
    /// silently replaced with the current host library.
    pub fn load_with_standard_profile(
        &self,
        repository: &Repository,
        standard_profile: Option<StandardDependencyProfile>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        self.load_worktree(repository, standard_profile, &BTreeMap::new())
    }

    /// Loads the worktree project exactly as [`Self::load`] does, except that
    /// a module with an entry in `overlays` (logical path to source text) is
    /// read from that entry instead of the worktree, and an overlay path is
    /// importable before it exists on disk. Editors use this to check unsaved
    /// buffers the way `check` checks the saved files.
    pub fn load_with_overlays(
        &self,
        repository: &Repository,
        overlays: &BTreeMap<String, String>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let project = self.load_worktree(repository, None, overlays)?;
        let commit = repository.head().map_err(ProjectLoadError::Repository)?;
        self.bind_attached_standard_module(repository, commit.as_ref(), project)
    }

    fn load_worktree(
        &self,
        repository: &Repository,
        standard_profile: Option<StandardDependencyProfile>,
        overlays: &BTreeMap<String, String>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let root = canonical_worktree(repository)?;
        validate_repository_paths(&root, self.limits)?;
        load_reachable_project(
            self.limits,
            standard_profile,
            vec![String::from("main.orna")],
            false,
            |logical_path, total_bytes| match overlays.get(logical_path) {
                Some(source) => read_overlay(source, total_bytes, self.limits),
                None => read_module(&root, logical_path, total_bytes, self.limits),
            },
            |_, segments| resolve_import(&root, segments, overlays),
            |tables, _, total_bytes| {
                discover_worktree_rows(&root, tables, total_bytes, self.limits)
            },
        )
    }

    /// Loads the root module and reachable ordinary imports from an already
    /// verified, reachable Git commit. The committed tree is read directly:
    /// this does not consult or modify the worktree, index, or refs.
    pub fn load_committed_snapshot(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let project = self.load_committed_snapshot_with_standard_profile(repository, commit, None)?;
        self.bind_attached_standard_module(repository, Some(commit), project)
    }

    /// Snapshot variant of [`Self::load_with_standard_profile`]. Standard
    /// dependencies remain explicitly caller supplied; committed project
    /// loading never discovers host standard-library files.
    pub fn load_committed_snapshot_with_standard_profile(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
        standard_profile: Option<StandardDependencyProfile>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        self.load_committed_project(
            repository,
            commit,
            standard_profile,
            vec![String::from("main.orna")],
            false,
        )
    }

    /// Loads the project reachable from `roots` at one committed snapshot.
    /// `std_snapshot` selects attached standard-library import rules, where
    /// imports inside the library are resolved relative to its own tree.
    fn load_committed_project(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
        standard_profile: Option<StandardDependencyProfile>,
        roots: Vec<String>,
        std_snapshot: bool,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let snapshot = validate_committed_tree(repository, commit, self.limits)?;
        if snapshot.has_standard_gitlink
            && let Some(profile) = &standard_profile
            && repository
                .committed_submodule_commit(commit, "stdlib/std")
                .map_err(ProjectLoadError::Repository)?
                .as_str()
                != profile.snapshot()
        {
            return Err(ProjectLoadError::StandardSnapshotMismatch);
        }
        let has_format3_database = snapshot.has_database_metadata
            && repository
                .open_format_context_at_selector(commit.as_str())
                .map_err(ProjectLoadError::FormatContext)?
                .repository_format_number()
                == 3;

        let mut project = load_reachable_project(
            self.limits,
            standard_profile,
            roots,
            std_snapshot,
            |logical_path, total_bytes| {
                read_committed_module(
                    repository,
                    commit,
                    &snapshot.source_paths,
                    logical_path,
                    total_bytes,
                    self.limits,
                )
            },
            |importer, segments| {
                if std_snapshot {
                    resolve_std_snapshot_import(&snapshot.source_paths, importer, segments)
                } else {
                    resolve_committed_import(&snapshot.source_paths, segments)
                }
            },
            |tables, modules, total_bytes| {
                if has_format3_database {
                    refuse_format3_loose_rows(&snapshot.row_paths, tables, modules)?;
                    Ok(Vec::new())
                } else {
                    discover_committed_rows(
                        repository,
                        commit,
                        &snapshot.row_paths,
                        tables,
                        total_bytes,
                        self.limits,
                    )
                }
            },
        )?;
        project.source_commit = Some(commit.clone());
        Ok(project)
    }

    /// Resolves a table UUID from the identity manifest and source declaration at
    /// one exact committed snapshot, then issues source-qualified binding authority.
    pub fn resolve_committed_source_table_schema(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
        table_object_id: TableObjectId,
    ) -> Result<ResolvedTableSchema, ProjectLoadError> {
        let project = self.load_committed_snapshot(repository, commit)?;
        resolve_committed_source_table_schema_for_project(
            repository,
            commit,
            &project,
            table_object_id,
        )
    }

    /// Like [`Self::resolve_committed_source_table_schema`], and also returns the stored
    /// constant default of each defaulted field of the table as frozen by the source at
    /// `commit` (ORNA-FIELD-003, ORNA-EVOL-009). A later source never changes these values.
    pub fn resolve_committed_source_table_schema_with_introductions(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
        table_object_id: TableObjectId,
    ) -> Result<(ResolvedTableSchema, BTreeMap<[u8; 16], Raw>), ProjectLoadError> {
        let project = self.load_committed_snapshot(repository, commit)?;
        let schema = resolve_committed_source_table_schema_for_project(
            repository,
            commit,
            &project,
            table_object_id,
        )?;
        let introductions = schema.introduced_values(&project);
        Ok((schema, introductions))
    }

    /// Private-candidate variant of [`Self::resolve_committed_source_table_schema`]:
    /// resolves the table from the identity manifest, source declaration and format
    /// context of one private publication candidate, before any ref names it.
    pub fn resolve_private_candidate_source_table_schema(
        &self,
        repository: &Repository,
        candidate: &PrivateCommit,
        table_object_id: TableObjectId,
    ) -> Result<ResolvedTableSchema, ProjectLoadError> {
        let project = self.load_private_candidate(repository, candidate)?;
        let manifest = ObjectIdentityManifest::read_private_candidate(repository, candidate)
            .map_err(|_| ProjectLoadError::InvalidTableSchema)?
            .ok_or(ProjectLoadError::InvalidTableSchema)?;
        let context = repository
            .open_private_candidate_format_context(candidate)
            .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
        resolve_source_table_schema(
            candidate.commit(),
            &project,
            &manifest,
            &context,
            table_object_id,
        )
    }

    /// Loads the root module and reachable imports from the immutable tree of
    /// a private publication candidate. The candidate capability, rather than
    /// a ref name or worktree path, is the source authority.
    pub fn load_private_candidate(
        &self,
        repository: &Repository,
        candidate: &PrivateCommit,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let project = self.load_private_candidate_with_standard_profile(repository, candidate, None)?;
        self.bind_private_standard_module(repository, candidate, project)
    }

    /// Private-candidate variant of
    /// [`Self::load_committed_snapshot_with_standard_profile`].
    pub fn load_private_candidate_with_standard_profile(
        &self,
        repository: &Repository,
        candidate: &PrivateCommit,
        standard_profile: Option<StandardDependencyProfile>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let snapshot = validate_private_candidate_tree(repository, candidate, self.limits)?;
        if snapshot.has_standard_gitlink
            && let Some(profile) = &standard_profile
            && repository
                .private_candidate_submodule_commit(candidate, "stdlib/std")
                .map_err(ProjectLoadError::Repository)?
                .as_str()
                != profile.snapshot()
        {
            return Err(ProjectLoadError::StandardSnapshotMismatch);
        }
        let has_format3_database = snapshot.has_database_metadata
            && repository
                .private_candidate_repository_format_number(candidate)
                .map_err(ProjectLoadError::FormatContext)?
                == 3;

        let mut project = load_reachable_project(
            self.limits,
            standard_profile,
            vec![String::from("main.orna")],
            false,
            |logical_path, total_bytes| {
                read_private_candidate_module(
                    repository,
                    candidate,
                    &snapshot.source_paths,
                    logical_path,
                    total_bytes,
                    self.limits,
                )
            },
            |_, segments| resolve_committed_import(&snapshot.source_paths, segments),
            |tables, modules, total_bytes| {
                if has_format3_database {
                    refuse_format3_loose_rows(&snapshot.row_paths, tables, modules)?;
                    Ok(Vec::new())
                } else {
                    discover_private_candidate_rows(
                        repository,
                        candidate,
                        &snapshot.row_paths,
                        tables,
                        total_bytes,
                        self.limits,
                    )
                }
            },
        )?;
        project.source_commit = Some(candidate.commit().clone());
        Ok(project)
    }

    /// Resolves `std` only through the gitlink captured in the owning
    /// database snapshot. `stdlib/std` is the attached repository path; no
    /// installed library or mutable worktree source can replace that commit.
    /// `.orna/packages` can pin read-only attached databases (including a
    /// `std` alias for an attached REPL session); `PackageResolver` requires
    /// that alias to agree with this gitlink whenever both are present.
    fn bind_attached_standard_module(
        &self,
        repository: &Repository,
        owner_commit: Option<&GitCommitRef>,
        mut project: LoadedProject,
    ) -> Result<LoadedProject, ProjectLoadError> {
        if project.standard_modules.is_empty() {
            return Ok(project);
        }
        let Some(owner_commit) = owner_commit else {
            return Ok(project);
        };
        let Ok(module_commit) = repository.committed_submodule_commit(owner_commit, "stdlib/std")
        else {
            return Ok(project);
        };
        self.load_attached_standard_commit(repository, &module_commit, &mut project);
        Ok(project)
    }

    fn bind_private_standard_module(
        &self,
        repository: &Repository,
        candidate: &PrivateCommit,
        mut project: LoadedProject,
    ) -> Result<LoadedProject, ProjectLoadError> {
        if project.standard_modules.is_empty() {
            return Ok(project);
        }
        let Ok(module_commit) =
            repository.private_candidate_submodule_commit(candidate, "stdlib/std")
        else {
            return Ok(project);
        };
        self.load_attached_standard_commit(repository, &module_commit, &mut project);
        Ok(project)
    }

    fn load_attached_standard_commit(
        &self,
        owner: &Repository,
        module_commit: &GitCommitRef,
        project: &mut LoadedProject,
    ) {
        let module_path = owner.worktree().join("stdlib/std");
        let Ok(metadata) = fs::symlink_metadata(&module_path) else {
            return;
        };
        if !metadata.is_dir() {
            return;
        }
        let Ok(module_root) = fs::canonicalize(&module_path) else {
            return;
        };
        let Ok(module_repository) = Repository::discover(&module_path) else {
            return;
        };
        if module_repository.worktree() != module_root {
            return;
        }
        let Ok(module_commit) = module_repository.resolve_snapshot(module_commit.as_str()) else {
            return;
        };
        let Ok(snapshot) = validate_committed_tree(&module_repository, &module_commit, self.limits)
        else {
            return;
        };
        let available = snapshot
            .source_paths
            .iter()
            .map(|path| format!("std/{path}"))
            .collect::<BTreeSet<_>>();
        let Some(requested) = project
            .standard_modules
            .iter()
            .map(|module| {
                canonical_standard_module(module, |candidate| available.contains(candidate))
            })
            .collect::<Option<BTreeSet<String>>>()
        else {
            return;
        };
        let mut roots = requested
            .iter()
            .map(|module| module["std/".len()..].to_owned())
            .collect::<Vec<_>>();
        if available.contains("std/main.orna") {
            roots.push(String::from("main.orna"));
        }
        let Ok(module_project) = self.load_committed_project(
            &module_repository,
            &module_commit,
            None,
            roots,
            true,
        ) else {
            return;
        };
        let sources = module_project
            .modules()
            .iter()
            .map(|module| {
                (
                    format!("std/{}", module.logical_path),
                    module.source.clone(),
                )
            })
            .collect::<Vec<_>>();
        let loaded = sources
            .iter()
            .map(|(logical_path, _)| logical_path.as_str())
            .collect::<BTreeSet<_>>();
        if !requested
            .iter()
            .all(|logical_path| loaded.contains(logical_path.as_str()))
        {
            return;
        }
        let Ok(profile) = StandardDependencyProfile::from_sources(
            module_commit.as_str().to_owned(),
            sources.clone(),
        ) else {
            return;
        };
        project.standard_runtime_modules = standard_runtime_closure(&requested, &sources);
        project.standard_modules = requested;
        project.standard_profile = Some(profile);
        project.standard_sources = sources;
    }
}

fn resolve_committed_source_table_schema_for_project(
    repository: &Repository,
    commit: &GitCommitRef,
    project: &LoadedProject,
    table_object_id: TableObjectId,
) -> Result<ResolvedTableSchema, ProjectLoadError> {
    let manifest = ObjectIdentityManifest::read_committed(repository, commit)
        .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
    let context = repository
        .open_pinned_format_context(commit.as_str())
        .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
    resolve_source_table_schema(commit, project, &manifest, &context, table_object_id)
}

/// Resolves one table against the identity `manifest` and format `context` of the
/// exact snapshot `commit`, which must be the snapshot `project` was loaded from.
fn resolve_source_table_schema(
    commit: &GitCommitRef,
    project: &LoadedProject,
    manifest: &ObjectIdentityManifest,
    context: &RepositoryFormatContext,
    table_object_id: TableObjectId,
) -> Result<ResolvedTableSchema, ProjectLoadError> {
    let table_identity = manifest
        .records()
        .iter()
        .find(|record| {
            record.kind() == ObjectKind::Table
                && !record.is_retired()
                && *record.id() == table_object_id.bytes()
        })
        .ok_or(ProjectLoadError::InvalidTableSchema)?;
    let qualified_name = table_identity.qualified_name();
    let (module_name, declaration_name) = match qualified_name.rsplit_once('.') {
        Some((module_name, declaration_name)) => (module_name, declaration_name),
        None => ("", qualified_name),
    };
    let source_commit = project
        .source_commit()
        .cloned()
        .ok_or(ProjectLoadError::InvalidTableSchema)?;
    if &source_commit != commit {
        return Err(ProjectLoadError::InvalidTableSchema);
    }
    let snapshot_id = project
        .source_snapshot_id()
        .ok_or(ProjectLoadError::InvalidTableSchema)?;
    if context.snapshot_pin().snapshot_id() != snapshot_id {
        return Err(ProjectLoadError::InvalidTableSchema);
    }
    let expected_database_id = context
        .require_database_id()
        .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
    let table = project.source_table_schema_in_database(
        manifest,
        *expected_database_id.as_bytes(),
        module_name,
        declaration_name,
    )?;
    if *table.table_object_id() != table_object_id.bytes() {
        return Err(ProjectLoadError::InvalidTableSchema);
    }
    let binding = context
        .bind_committed_table(
            table.module_name(),
            table.source_path(),
            table.declaration_name(),
            *table.table_object_id(),
            expected_database_id,
            &snapshot_id,
            table.descriptor(),
        )
        .map_err(|_| ProjectLoadError::InvalidTableSchema)?;
    if binding.database_id() != expected_database_id
        || binding.snapshot_id() != &snapshot_id
        || binding.declaration_name() != Some(table.declaration_name())
        || binding.module_name() != Some(table.module_name())
        || binding.source_path() != Some(table.source_path())
        || binding.table_object_id() != table.table_object_id()
        || binding.descriptor() != table.descriptor()
    {
        return Err(ProjectLoadError::InvalidTableSchema);
    }
    Ok(ResolvedTableSchema {
        source_commit,
        snapshot_id,
        database_id: *expected_database_id.as_bytes(),
        table,
        binding,
    })
}

fn source_table_module_name(
    project: &LoadedProject,
    analysis: &Analysis,
    declaration_name: &str,
) -> Result<String, ProjectLoadError> {
    let mut resolved = None;
    for identity in project.identities() {
        let namespace = Namespace(identity.namespace().to_vec());
        let Some(module) = analysis.modules.get(&namespace) else {
            continue;
        };
        if !module
            .symbols
            .get(declaration_name)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Table)
        {
            continue;
        }
        if resolved.replace(namespace.display()).is_some() {
            return Err(ProjectLoadError::InvalidTableSchema);
        }
    }
    resolved.ok_or(ProjectLoadError::InvalidTableSchema)
}


fn raw_map_value<'a>(raw: &'a Raw, key: &str) -> Option<&'a Raw> {
    let Raw::Map(entries) = raw else {
        return None;
    };
    entries.iter().find_map(|(candidate, value)| match candidate {
        Raw::Int(integer) if integer.to_string() == key => Some(value),
        _ => None,
    })
}

fn committed_uuid(raw: &Raw) -> Option<[u8; 16]> {
    let Raw::Tag(37, value) = raw else {
        return None;
    };
    let Raw::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    let bytes: [u8; 16] = bytes.as_slice().try_into().ok()?;
    Some(bytes)
}

fn load_reachable_project(
    limits: ProjectLimits,
    standard_profile: Option<StandardDependencyProfile>,
    roots: Vec<String>,
    std_snapshot: bool,
    mut read_module: impl FnMut(&str, &mut usize) -> Result<String, ProjectLoadError>,
    mut resolve_import: impl FnMut(&str, &[&str]) -> Result<String, ProjectLoadError>,
    mut discover_rows: impl FnMut(
        &BTreeSet<String>,
        &BTreeSet<&str>,
        &mut usize,
    ) -> Result<Vec<LooseRowCandidate>, ProjectLoadError>,
) -> Result<LoadedProject, ProjectLoadError> {
    let mut pending = VecDeque::from(roots);
    let mut loaded = BTreeMap::<String, LoadedModule>::new();
    let mut namespaces = BTreeMap::<Vec<String>, String>::new();
    let mut total_bytes = 0usize;
    let mut standard_imports = false;
    let mut standard_modules = BTreeSet::new();
    let mut table_paths = BTreeSet::new();

    while let Some(logical_path) = pending.pop_front() {
        if loaded.contains_key(&logical_path) {
            continue;
        }
        if loaded.len() == limits.max_modules {
            return Err(ProjectLoadError::ModuleLimit);
        }
        let source = read_module(&logical_path, &mut total_bytes)?;
        let parsed = parse_module(&source);
        if !parsed.is_ok() {
            // Re-parse with the logical path so the reported spans carry the
            // file and 1-based line/column coordinates. Accepted modules keep
            // the unannotated tree, so the happy path is unchanged.
            let located = parse_module_with_file(&source, logical_path.as_str());
            return Err(ProjectLoadError::InvalidModule {
                logical_path,
                diagnostics: located.diagnostics,
            });
        }
        let namespace = namespace_for_path(&logical_path)?;
        if let Some(previous) = namespaces.insert(namespace.clone(), logical_path.clone())
            && previous != logical_path
        {
            return Err(ProjectLoadError::DuplicateNamespace);
        }

        let mut imports = BTreeSet::new();
        for item in &parsed.value.items {
            let Declaration::Use { path, .. } = &item.declaration else {
                continue;
            };
            let segments = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>();
            if segments.is_empty() {
                return Err(ProjectLoadError::UnsupportedImport);
            }
            if segments[0] == "sys" {
                continue;
            }
            if segments[0] == "std" && std_snapshot {
                imports.insert(resolve_import(&logical_path, &segments[1..])?);
                continue;
            }
            if segments[0] == "std" {
                standard_imports = true;
                let mut requested = segments.join("/");
                if segments.len() == 1 {
                    requested.push_str("/main");
                }
                if !requested.ends_with(".orna") {
                    requested.push_str(".orna");
                }
                standard_modules.insert(requested);
                continue;
            }
            imports.insert(resolve_import(&logical_path, &segments)?);
        }
        pending.extend(imports);
        for item in &parsed.value.items {
            if let Declaration::Table { name, .. } = &item.declaration {
                let mut table_path = namespace.clone();
                table_path.push(name.clone());
                table_paths.insert(table_path.join("/"));
            }
        }
        loaded.insert(logical_path, LoadedModule { source, namespace });
    }

    let mut modules = Vec::with_capacity(loaded.len());
    let mut identities = Vec::with_capacity(loaded.len());
    for (logical_path, module) in loaded {
        identities.push(ModuleIdentity {
            logical_path: logical_path.clone(),
            namespace: module.namespace,
        });
        modules.push(ModuleInput::new(logical_path, module.source));
    }
    let module_paths = identities
        .iter()
        .map(|identity| identity.logical_path.as_str())
        .collect::<BTreeSet<_>>();
    let loose_rows = discover_rows(&table_paths, &module_paths, &mut total_bytes)?;
    for row in &loose_rows {
        // Every loose row file is a row unit (ORNA-SOURCE-003). Reject one that
        // is not a single record expression here, with its parser code, as for
        // module units; admission would otherwise report only a loader code.
        if !parse_row(row.source()).is_ok() {
            return Err(ProjectLoadError::InvalidRowUnit {
                logical_path: row.logical_path().to_owned(),
                diagnostics: parse_row_with_file(row.source(), row.logical_path()).diagnostics,
            });
        }
    }
    let standard_modules = match &standard_profile {
        Some(profile) => standard_modules
            .into_iter()
            .map(|requested| {
                canonical_standard_module(&requested, |candidate| {
                    profile.module_digests().contains_key(candidate)
                })
                .unwrap_or(requested)
            })
            .collect(),
        None => standard_modules,
    };
    Ok(LoadedProject {
        modules,
        identities,
        loose_rows,
        standard_profile,
        standard_sources: Vec::new(),
        standard_runtime_modules: BTreeSet::new(),
        standard_imports,
        standard_modules,
        source_commit: None,
    })
}

fn standard_runtime_closure(
    requested: &BTreeSet<String>,
    sources: &[(String, String)],
) -> BTreeSet<String> {
    let source_by_path = sources
        .iter()
        .map(|(logical_path, source)| (logical_path.as_str(), source.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut pending = requested.iter().cloned().collect::<VecDeque<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(logical_path) = pending.pop_front() {
        if !reachable.insert(logical_path.clone()) {
            continue;
        }
        let Some(source) = source_by_path.get(logical_path.as_str()) else {
            continue;
        };
        let parsed = parse_module(source);
        if !parsed.is_ok() {
            continue;
        }
        for item in &parsed.value.items {
            let Declaration::Use { path, .. } = &item.declaration else {
                continue;
            };
            let segments = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>();
            if segments.is_empty() || segments[0] == "sys" {
                continue;
            }
            let joined = segments.join("/");
            let bases = if segments[0] == "std" {
                vec![joined]
            } else {
                let importer_directory = logical_path
                    .rsplit_once('/')
                    .map_or("std", |(directory, _)| directory);
                vec![
                    format!("std/{joined}"),
                    format!("{importer_directory}/{joined}"),
                ]
            };
            for base in bases {
                let file = format!("{base}.orna");
                let directory = format!("{base}/main.orna");
                if source_by_path.contains_key(file.as_str()) {
                    pending.push_back(file);
                    break;
                }
                if source_by_path.contains_key(directory.as_str()) {
                    pending.push_back(directory);
                    break;
                }
            }
        }
    }
    reachable
}

#[derive(Debug)]
pub enum ProjectLoadError {
    Repository(RepositoryError),
    FormatContext(FormatContextError),
    RootUnavailable,
    UnsafePath,
    Symlink,
    SourceUnavailable,
    SourceTooLarge,
    RepositoryLimit,
    SourceTreeLimit,
    ModuleLimit,
    InvalidModule {
        logical_path: String,
        diagnostics: Vec<orna_syntax_v1::SyntaxDiagnostic>,
    },
    InvalidRowUnit {
        logical_path: String,
        diagnostics: Vec<orna_syntax_v1::SyntaxDiagnostic>,
    },
    UnsupportedImport,
    ImportUnavailable,
    AmbiguousImport,
    DuplicateNamespace,
    SiblingCollision,
    NonPortablePath,
    DuplicateModuleNamespace,
    ReservedNamespace,
    StandardSnapshotMismatch,
    InvalidTableSchema,
    InvalidSourceRow,
}

impl fmt::Display for ProjectLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository(_) => "could not discover the Git worktree",
            // The admission error says which metadata or path failed, such as
            // the entry or row file that mixes formats.
            Self::FormatContext(error) => return write!(formatter, "{error}"),
            Self::RootUnavailable => "could not resolve the Git worktree",
            Self::UnsafePath => "project contains an unsafe module path",
            Self::Symlink => "project module path contains a symbolic link",
            Self::SourceUnavailable => "reachable source module is unavailable",
            Self::SourceTooLarge => "project source exceeds the configured limit",
            Self::RepositoryLimit => "project exceeds the configured source-module limit",
            Self::SourceTreeLimit => "committed source tree exceeds the fixed walk limit",
            Self::ModuleLimit => "project exceeds the configured module limit",
            Self::InvalidModule { .. } => "reachable source module is not a valid module unit",
            Self::InvalidRowUnit { .. } => "reachable row unit is not a valid row unit",
            Self::UnsupportedImport => "source import cannot be resolved by this loader",
            Self::ImportUnavailable => "imported source module is unavailable",
            Self::AmbiguousImport => "imported source module has ambiguous ownership",
            Self::DuplicateNamespace => "reachable source modules define the same namespace",
            Self::SiblingCollision => "repository contains colliding sibling path components",
            Self::NonPortablePath => "repository contains a non-portable path component",
            Self::DuplicateModuleNamespace => {
                "repository contains multiple source files for one module namespace"
            }
            Self::ReservedNamespace => "repository source module shadows a reserved namespace",
            Self::StandardSnapshotMismatch => {
                "standard dependency profile does not match the committed std submodule pin"
            }
            Self::InvalidTableSchema => {
                "committed table schema could not be resolved from source and identity metadata"
            }
            Self::InvalidSourceRow => "source row could not be admitted against its committed schema",
        })
    }
}

impl Error for ProjectLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::FormatContext(error) => Some(error),
            _ => None,
        }
    }
}

struct LoadedModule {
    source: String,
    namespace: Vec<String>,
}

fn canonical_worktree(repository: &Repository) -> Result<PathBuf, ProjectLoadError> {
    fs::canonicalize(repository.worktree()).map_err(|_| ProjectLoadError::RootUnavailable)
}

/// The materialised format-3 store. It is database content, not source.
const NATIVE_STORE_PATH: &str = ".orna/store";

/// Validates path portability without reading or parsing repository file bodies.
/// Git administrative directories are outside repository content and are skipped,
/// and so is the materialised store at `.orna/store`. Only module sources count
/// against [`ProjectLimits::max_source_modules`].
fn validate_repository_paths(root: &Path, limits: ProjectLimits) -> Result<(), ProjectLoadError> {
    let mut pending = VecDeque::from([root.to_path_buf()]);
    let mut module_owners = BTreeMap::<Vec<String>, String>::new();
    let store = root.join(NATIVE_STORE_PATH);
    while let Some(directory) = pending.pop_front() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|_| ProjectLoadError::SourceUnavailable)?
            .map(|entry| entry.map_err(|_| ProjectLoadError::SourceUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut siblings = BTreeMap::<String, String>::new();
        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ProjectLoadError::UnsafePath)?;
            if orna_foundation_v1::is_git_metadata_component(name.as_bytes()) {
                continue;
            }
            if !portable_component(&name) {
                return Err(ProjectLoadError::NonPortablePath);
            }
            let key = unicode_sibling_key(&name);
            if let Some(existing) = siblings.insert(key, name.clone())
                && existing != name
            {
                return Err(ProjectLoadError::SiblingCollision);
            }

            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if metadata.file_type().is_symlink() {
                return Err(ProjectLoadError::Symlink);
            }
            if metadata.is_dir() {
                let subdirectory = entry.path();
                if subdirectory != store {
                    pending.push_back(subdirectory);
                }
            } else if !metadata.is_file() {
                return Err(ProjectLoadError::UnsafePath);
            } else if name.ends_with(".orna")
                && !is_committed_metadata_path(root, &entry.path())
                && is_module_source_path(root, &entry.path())
            {
                let logical_path = logical_path(root, &entry.path())?;
                let namespace = namespace_for_path(&logical_path)?;
                if namespace
                    .first()
                    .is_some_and(|component| matches!(component.as_str(), "sys" | "std"))
                {
                    return Err(ProjectLoadError::ReservedNamespace);
                }
                if let Some(existing) = module_owners.insert(namespace, logical_path.clone())
                    && existing != logical_path
                {
                    return Err(ProjectLoadError::DuplicateModuleNamespace);
                }
                if module_owners.len() > limits.max_source_modules {
                    return Err(ProjectLoadError::RepositoryLimit);
                }
            }
        }
    }
    Ok(())
}

/// Validates the immutable source tree before any reachable source body is
/// read. The listing excludes only the native store at `.orna/store`. Git emits
/// only leaf entries, so sibling ownership is reconstructed from every
/// component rather than inferred from the mutable worktree.
struct ValidatedSnapshotTree {
    source_paths: BTreeSet<String>,
    row_paths: BTreeSet<String>,
    has_standard_gitlink: bool,
    has_database_metadata: bool,
}

fn validate_committed_tree(
    repository: &Repository,
    commit: &GitCommitRef,
    limits: ProjectLimits,
) -> Result<ValidatedSnapshotTree, ProjectLoadError> {
    let entries = repository
        .list_committed_source_tree(commit)
        .map_err(source_tree_load_error)?;
    validate_snapshot_tree(entries, limits)
}

fn validate_private_candidate_tree(
    repository: &Repository,
    candidate: &PrivateCommit,
    limits: ProjectLimits,
) -> Result<ValidatedSnapshotTree, ProjectLoadError> {
    let entries = repository
        .list_private_candidate_source_tree(candidate)
        .map_err(source_tree_load_error)?;
    validate_snapshot_tree(entries, limits)
}

/// A source tree over the fixed walk limit is reported as
/// [`ProjectLoadError::SourceTreeLimit`], and every other repository failure
/// stays a repository error.
fn source_tree_load_error(error: RepositoryError) -> ProjectLoadError {
    match error {
        RepositoryError::SourceTreeLimit => ProjectLoadError::SourceTreeLimit,
        error => ProjectLoadError::Repository(error),
    }
}

fn validate_snapshot_tree(
    entries: Vec<CommittedTreeEntry>,
    limits: ProjectLimits,
) -> Result<ValidatedSnapshotTree, ProjectLoadError> {
    let has_standard_gitlink = entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("stdlib/std")
            && entry.kind() == CommittedTreeEntryKind::Submodule
    });
    let has_database_metadata = entries.iter().any(|entry| {
        entry.path().as_path() == Path::new(".orna/database.orna")
            && matches!(entry.kind(), CommittedTreeEntryKind::File { .. })
    });
    let mut source_paths = BTreeSet::new();
    let mut row_paths = BTreeSet::new();
    let mut module_owners = BTreeMap::<Vec<String>, String>::new();
    let mut siblings = BTreeMap::<PathBuf, BTreeMap<String, String>>::new();
    for entry in entries {
        validate_committed_tree_entry(
            entry,
            &mut siblings,
            &mut module_owners,
            &mut source_paths,
            &mut row_paths,
        )?;
        if module_owners.len() > limits.max_source_modules {
            return Err(ProjectLoadError::RepositoryLimit);
        }
    }
    Ok(ValidatedSnapshotTree {
        source_paths,
        row_paths,
        has_standard_gitlink,
        has_database_metadata,
    })
}

fn validate_committed_tree_entry(
    entry: CommittedTreeEntry,
    siblings: &mut BTreeMap<PathBuf, BTreeMap<String, String>>,
    module_owners: &mut BTreeMap<Vec<String>, String>,
    source_paths: &mut BTreeSet<String>,
    row_paths: &mut BTreeSet<String>,
) -> Result<(), ProjectLoadError> {
    let path = entry.path().as_path();
    let attached_standard_module = path == Path::new("stdlib/std");
    match entry.kind() {
        CommittedTreeEntryKind::File { .. } => {}
        CommittedTreeEntryKind::Symlink => return Err(ProjectLoadError::Symlink),
        CommittedTreeEntryKind::Submodule if attached_standard_module => {}
        CommittedTreeEntryKind::Submodule => return Err(ProjectLoadError::UnsafePath),
    }

    let mut parent = PathBuf::new();
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ProjectLoadError::UnsafePath);
        };
        let component = component.to_str().ok_or(ProjectLoadError::UnsafePath)?;
        if !portable_component(component) {
            return Err(ProjectLoadError::NonPortablePath);
        }
        let owned = component.to_owned();
        let key = unicode_sibling_key(component);
        if let Some(existing) = siblings
            .entry(parent.clone())
            .or_default()
            .insert(key, owned.clone())
            && existing != owned
        {
            return Err(ProjectLoadError::SiblingCollision);
        }
        parent.push(component);
        components.push(component);
    }
    if components.is_empty() {
        return Err(ProjectLoadError::UnsafePath);
    }
    if components[0] == ".orna" {
        return Ok(());
    }

    let logical_path = path.to_str().ok_or(ProjectLoadError::UnsafePath)?;
    if !logical_path.ends_with(".orna") {
        return Ok(());
    }
    source_paths.insert(logical_path.to_owned());
    if !is_module_source_logical(logical_path) {
        row_paths.insert(logical_path.to_owned());
        return Ok(());
    }
    let namespace = namespace_for_path(logical_path)?;
    if namespace
        .first()
        .is_some_and(|component| matches!(component.as_str(), "sys" | "std"))
    {
        return Err(ProjectLoadError::ReservedNamespace);
    }
    if let Some(existing) = module_owners.insert(namespace, logical_path.to_owned())
        && existing != logical_path
    {
        return Err(ProjectLoadError::DuplicateModuleNamespace);
    }
    Ok(())
}

fn is_committed_metadata_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .is_some_and(|relative| {
            relative
                .components()
                .any(|component| component.as_os_str() == ".orna")
        })
}

fn is_module_source_path(root: &Path, path: &Path) -> bool {
    logical_path(root, path)
        .map(|logical_path| is_module_source_logical(&logical_path))
        .unwrap_or(false)
}

fn is_module_source_logical(logical_path: &str) -> bool {
    let mut components = logical_path.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    components.next().is_none() || logical_path.ends_with("/main.orna") || first == "main.orna"
}

fn resolve_import(
    root: &Path,
    segments: &[&str],
    overlays: &BTreeMap<String, String>,
) -> Result<String, ProjectLoadError> {
    if segments.iter().any(|segment| !valid_component(segment)) {
        return Err(ProjectLoadError::UnsupportedImport);
    }
    let mut base = root.to_path_buf();
    for segment in segments {
        base.push(segment);
    }
    let file = base.with_extension("orna");
    let directory = base.join("main.orna");
    let logical_base = segments.join("/");
    let file_exists =
        overlays.contains_key(&format!("{logical_base}.orna")) || checked_candidate(root, &file)?;
    let directory_exists = overlays.contains_key(&format!("{logical_base}/main.orna"))
        || checked_candidate(root, &directory)?;
    match (file_exists, directory_exists) {
        (false, false) => Err(ProjectLoadError::ImportUnavailable),
        (true, true) => Err(ProjectLoadError::AmbiguousImport),
        (true, false) => logical_path(root, &file),
        (false, true) => logical_path(root, &directory),
    }
}

fn resolve_committed_import(
    source_paths: &BTreeSet<String>,
    segments: &[&str],
) -> Result<String, ProjectLoadError> {
    if segments.iter().any(|segment| !valid_component(segment)) {
        return Err(ProjectLoadError::UnsupportedImport);
    }
    let base = segments.join("/");
    let file = format!("{base}.orna");
    let directory = format!("{base}/main.orna");
    match (
        source_paths.contains(&file),
        source_paths.contains(&directory),
    ) {
        (false, false) => Err(ProjectLoadError::ImportUnavailable),
        (true, true) => Err(ProjectLoadError::AmbiguousImport),
        (true, false) => Ok(file),

        (false, true) => Ok(directory),
    }
}

fn resolve_std_snapshot_import(
    source_paths: &BTreeSet<String>,
    importer: &str,
    segments: &[&str],
) -> Result<String, ProjectLoadError> {
    match resolve_committed_import(source_paths, segments) {
        Err(ProjectLoadError::ImportUnavailable) => {
            let Some((directory, _)) = importer.rsplit_once('/') else {
                return Err(ProjectLoadError::ImportUnavailable);
            };
            let mut sibling = directory.split('/').collect::<Vec<_>>();
            sibling.extend_from_slice(segments);
            resolve_committed_import(source_paths, &sibling)
        }
        result => result,
    }
}

/// Maps a requested `std/<path>.orna` onto the module file that exists:
/// either the file itself or its directory `main.orna`, never both.
fn canonical_standard_module(
    requested: &str,
    available: impl Fn(&str) -> bool,
) -> Option<String> {
    let base = requested.strip_suffix(".orna")?;
    let directory = format!("{base}/main.orna");
    match (available(requested), available(&directory)) {
        (true, false) => Some(requested.to_owned()),
        (false, true) => Some(directory),
        _ => None,
    }
}

fn discover_worktree_rows(
    root: &Path,
    table_paths: &BTreeSet<String>,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<Vec<LooseRowCandidate>, ProjectLoadError> {
    let mut paths = BTreeSet::new();
    let mut pending = VecDeque::from([root.to_path_buf()]);
    while let Some(directory) = pending.pop_front() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|_| ProjectLoadError::SourceUnavailable)?
            .map(|entry| entry.map_err(|_| ProjectLoadError::SourceUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if directory == root
                && path.file_name().is_some_and(|name| {
                    orna_foundation_v1::is_git_metadata_component(name.as_encoded_bytes())
                })
            {
                continue;
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if metadata.is_dir() {
                // Nothing below a `.orna` directory is a row source (see
                // `is_committed_metadata_path`), and the format-3 store there
                // grows with every row it holds.
                if entry.file_name() != ".orna" {
                    pending.push_back(path);
                }
                continue;
            }
            if !metadata.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("orna")
            {
                continue;
            }
            if is_committed_metadata_path(root, &path) {
                continue;
            }
            let logical_path = logical_path(root, &path)?;
            if table_owner_and_key(&logical_path, table_paths).is_some() {
                paths.insert(logical_path);
            }
        }
    }

    paths
        .into_iter()
        .map(|logical_path| {
            let (table_path, key_path) =
                table_owner_and_key(&logical_path, table_paths).ok_or(ProjectLoadError::UnsafePath)?;
            let source = read_module(root, &logical_path, total_bytes, limits)?;
            Ok(LooseRowCandidate {
                logical_path,
                table_path,
                key_path,
                source,
            })
        })
        .collect()
}

fn discover_committed_rows(
    repository: &Repository,
    commit: &GitCommitRef,
    row_paths: &BTreeSet<String>,
    table_paths: &BTreeSet<String>,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<Vec<LooseRowCandidate>, ProjectLoadError> {
    row_paths
        .iter()
        .filter_map(|logical_path| {
            table_owner_and_key(logical_path, table_paths)
                .map(|(table_path, key_path)| (logical_path, table_path, key_path))
        })
        .map(|(logical_path, table_path, key_path)| {
            let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
            let bytes = repository
                .read_committed_file(commit, logical_path, maximum)
                .map_err(ProjectLoadError::Repository)?;
            let source =
                String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if source.len() > maximum {
                return Err(ProjectLoadError::SourceTooLarge);
            }
            *total_bytes += source.len();
            Ok(LooseRowCandidate {
                logical_path: logical_path.clone(),
                table_path,
                key_path,
                source,
            })
        })
        .collect()
}

fn discover_private_candidate_rows(
    repository: &Repository,
    candidate: &PrivateCommit,
    row_paths: &BTreeSet<String>,
    table_paths: &BTreeSet<String>,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<Vec<LooseRowCandidate>, ProjectLoadError> {
    row_paths
        .iter()
        .filter_map(|logical_path| {
            table_owner_and_key(logical_path, table_paths)
                .map(|(table_path, key_path)| (logical_path, table_path, key_path))
        })
        .map(|(logical_path, table_path, key_path)| {
            let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
            let bytes = repository
                .read_private_candidate_file(candidate, logical_path, maximum)
                .map_err(ProjectLoadError::Repository)?;
            let source =
                String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if source.len() > maximum {
                return Err(ProjectLoadError::SourceTooLarge);
            }
            *total_bytes += source.len();
            Ok(LooseRowCandidate {
                logical_path: logical_path.clone(),
                table_path,
                key_path,
                source,
            })
        })
        .collect()
}

/// Refuses a format-3 snapshot that tracks a loose row file.
///
/// Format 3 keeps rows only in `.orna/store` (ORNA-REPO-004): persisted rows
/// are ORP-1 records, not loose-row files (03-source-modules, "File
/// extension"). So a `.orna` file at a table's row path, `<table>/<key>.orna`,
/// that the module graph did not load is the legacy loose-row layout, and a
/// snapshot that tracks one mixes formats (ORNA-UPGRADE-003; Gate A,
/// "unknown/mixed formats fail early"). A file at such a path that a `use`
/// reached is a module (ORNA-NS-002), never a row. A committed snapshot or
/// commit candidate holding a loose row fails to load with
/// ORNA-REPO-CONTEXT-004 naming the first such path, before its bytes are
/// read, instead of loading with the row silently skipped.
fn refuse_format3_loose_rows(
    row_paths: &BTreeSet<String>,
    table_paths: &BTreeSet<String>,
    module_paths: &BTreeSet<&str>,
) -> Result<(), ProjectLoadError> {
    match row_paths.iter().find(|logical_path| {
        !module_paths.contains(logical_path.as_str())
            && table_owner_and_key(logical_path, table_paths).is_some()
    }) {
        Some(logical_path) => Err(ProjectLoadError::FormatContext(
            FormatContextError::MixedFormats {
                path: logical_path.clone(),
            },
        )),
        None => Ok(()),
    }
}

fn table_owner_and_key(
    logical_path: &str,
    table_paths: &BTreeSet<String>,
) -> Option<(String, Vec<String>)> {
    let mut selected: Option<(String, Vec<String>)> = None;
    for table_path in table_paths {
        let prefix = format!("{table_path}/");
        let Some(suffix) = logical_path.strip_prefix(&prefix) else {
            continue;
        };
        if suffix.is_empty() || !suffix.ends_with(".orna") {
            continue;
        }
        let key_path = suffix.split('/').map(str::to_owned).collect::<Vec<_>>();
        if key_path.iter().any(|component| component.is_empty()) {
            continue;
        }
        let replace = match &selected {
            None => true,
            Some((previous, _)) => table_path.len() > previous.len(),
        };
        if replace {
            selected = Some((table_path.clone(), key_path));
        }
    }
    selected
}

fn checked_candidate(root: &Path, candidate: &Path) -> Result<bool, ProjectLoadError> {
    ensure_no_symlink_ancestors(root, candidate)?;
    match fs::symlink_metadata(candidate) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ProjectLoadError::Symlink);
            }
            Ok(metadata.is_file())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ProjectLoadError::SourceUnavailable),
    }
}

fn read_module(
    root: &Path,
    logical_path: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    let path = root.join(logical_path);
    ensure_no_symlink_ancestors(root, &path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if metadata.file_type().is_symlink() {
        return Err(ProjectLoadError::Symlink);
    }
    if !metadata.is_file() {
        return Err(ProjectLoadError::SourceUnavailable);
    }
    let length = usize::try_from(metadata.len()).map_err(|_| ProjectLoadError::SourceTooLarge)?;
    if length > limits.max_source_bytes.saturating_sub(*total_bytes) {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
    let mut source = String::new();
    fs::File::open(path)
        .map_err(|_| ProjectLoadError::SourceUnavailable)?
        .take(maximum.saturating_add(1) as u64)
        .read_to_string(&mut source)
        .map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if source.len() > maximum {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source)
}

/// Admits one in-memory module under the same source budget as a worktree read.
fn read_overlay(
    source: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    if source.len() > limits.max_source_bytes.saturating_sub(*total_bytes) {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source.to_owned())
}

fn read_committed_module(
    repository: &Repository,
    commit: &GitCommitRef,
    source_paths: &BTreeSet<String>,
    logical_path: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    if !source_paths.contains(logical_path) {
        return Err(ProjectLoadError::SourceUnavailable);
    }
    let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
    let bytes = repository
        .read_committed_file(commit, logical_path, maximum)
        .map_err(ProjectLoadError::Repository)?;
    let source = String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if source.len() > maximum {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source)
}

fn read_private_candidate_module(
    repository: &Repository,
    candidate: &PrivateCommit,
    source_paths: &BTreeSet<String>,
    logical_path: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    if !source_paths.contains(logical_path) {
        return Err(ProjectLoadError::SourceUnavailable);
    }
    let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
    let bytes = repository
        .read_private_candidate_file(candidate, logical_path, maximum)
        .map_err(ProjectLoadError::Repository)?;
    let source = String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if source.len() > maximum {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source)
}

fn logical_path(root: &Path, path: &Path) -> Result<String, ProjectLoadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ProjectLoadError::UnsafePath)?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ProjectLoadError::UnsafePath);
        };
        let component = component.to_str().ok_or(ProjectLoadError::UnsafePath)?;
        components.push(component);
    }
    let logical_path = components.join("/");
    if !logical_path.ends_with(".orna")
        || components.iter().any(|part| !portable_component(part))
    {
        return Err(ProjectLoadError::UnsafePath);
    }
    Ok(logical_path)
}

fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<(), ProjectLoadError> {
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if !ancestor.starts_with(root) {
            return Err(ProjectLoadError::UnsafePath);
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ProjectLoadError::Symlink);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ProjectLoadError::SourceUnavailable),
        }
        if ancestor == root {
            return Ok(());
        }
        current = ancestor.parent();
    }
    Err(ProjectLoadError::UnsafePath)
}

fn namespace_for_path(path: &str) -> Result<Vec<String>, ProjectLoadError> {
    let mut parts = path.split('/').collect::<Vec<_>>();
    let Some(file) = parts.pop() else {
        return Err(ProjectLoadError::UnsafePath);
    };
    let Some(stem) = file.strip_suffix(".orna") else {
        return Err(ProjectLoadError::UnsafePath);
    };
    if !valid_component(stem) {
        return Err(ProjectLoadError::UnsafePath);
    }
    if parts.iter().any(|part| !valid_component(part)) {
        return Err(ProjectLoadError::UnsafePath);
    }
    let mut namespace = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
    if stem != "main" {
        namespace.push(stem.to_owned());
    }
    Ok(namespace)
}

fn valid_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains('.')
        && portable_component(component)
}

fn portable_component(component: &str) -> bool {
    !component.is_empty() && component.nfc().eq(component.chars())
}

/// Host-independent NFKC case-fold key for portable sibling ownership.
fn unicode_sibling_key(component: &str) -> String {
    unicode16::to_nfkc_casefold(component)
}

#[cfg(test)]
mod source_row_admission_tests {
    use super::{Format3Context, format_context_matches_commit, format_context_matches_source_schema};
    use std::sync::Arc;
    use orna_value_v1::{
        AvailabilityState, BlobResolver, ContentReference, Error as ValueError, Format3Quota,
        GitHash, NativeOid, OwnerLifetime, Snapshot,
    };

    struct UnusedResolver;

    impl BlobResolver for UnusedResolver {
        fn read_range(
            &self,
            _reference: &ContentReference,
            _offset: u64,
            _length: u64,
        ) -> orna_value_v1::Result<Vec<u8>> {
            Err(ValueError::ContentUnavailable)
        }
    }

    fn context(database_id: [u8; 16]) -> Format3Context {
        let algorithm = GitHash::Sha1;
        let oid = vec![0x11; algorithm.oid_width()];
        let root = NativeOid::from_bytes(&vec![0x55; algorithm.oid_width()]).unwrap();
        Format3Context::from_persisted(
            database_id,
            Snapshot::Commit {
                database: database_id,
                algorithm,
                oid,
            },
            algorithm,
            root.clone(),
            root,
            AvailabilityState::Available,
            OwnerLifetime::new([0x66; 16]),
            Format3Quota::new(1, 0).unwrap(),
            Arc::new(UnusedResolver),
        )
        .unwrap()
    }

    #[test]
    fn repository_path_validation_skips_git_metadata_in_every_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for name in [".GIT", "git~1", ".git. "] {
            let dir = root.join(name);
            std::fs::create_dir(&dir).unwrap();
            // A decomposed `é` is non-portable, so a validated entry would be refused.
            std::fs::write(dir.join("e\u{301}"), b"").unwrap();
        }
        assert!(crate::validate_repository_paths(root, crate::ProjectLimits::default()).is_ok());
    }

    #[test]
    fn worktree_discovery_skips_git_metadata_in_every_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join(".GIT")).unwrap();
        std::fs::write(root.join(".GIT").join("x.orna"), b"").unwrap();
        let tables = std::collections::BTreeSet::from([".GIT".to_string()]);
        let rows = crate::discover_worktree_rows(
            root,
            &tables,
            &mut 0usize,
            crate::ProjectLimits::default(),
        )
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn source_row_context_rejects_same_commit_from_another_database() {
        const COMMIT: &str = "1111111111111111111111111111111111111111";
        let expected_database_id = [0x22; 16];
        let foreign_context = context([0x33; 16]);

        assert!(format_context_matches_commit(&foreign_context, COMMIT));
        assert!(!format_context_matches_source_schema(
            &foreign_context,
            &expected_database_id,
            COMMIT,
        ));
        assert!(format_context_matches_source_schema(
            &context(expected_database_id),
            &expected_database_id,
            COMMIT,
        ));
    }
}
