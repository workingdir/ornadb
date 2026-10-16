//! Persisted source identities for table declarations and their fields.
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
use orna_repository_v1::{GitCommitRef, Repository, Uuid};
use orna_semantic_v1::{
    Analysis, ModuleInput, SourceUnitIdentity, SourceUnitMetadata, SymbolKind, TableAdmission,
    TableSchema, Type,
};
use orna_syntax_v1::{Declaration, Pattern, TableMember, parse_module};
use orna_value_v1::{Raw, SchemaDescriptor, Value};

/// Repository-relative committed location of the object identity inventory.
pub const OBJECT_IDENTITY_MANIFEST_PATH: &str = ".orna/object-identities.cbor";
const MAX_OBJECT_IDENTITY_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MANIFEST_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u64)]
pub enum ObjectKind {
    Table = 1,
    Field = 2,
    Unit = 3,
}

impl ObjectKind {
    fn from_raw(raw: &Raw) -> Option<Self> {
        match raw {
            Raw::Int(value) if value.to_string() == "1" => Some(Self::Table),
            Raw::Int(value) if value.to_string() == "2" => Some(Self::Field),
            Raw::Int(value) if value.to_string() == "3" => Some(Self::Unit),
            _ => None,
        }
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
    Io,
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
    pub fn source_table_schema(
        &self,
        analysis: &Analysis,
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
        let (_, module) = selected.ok_or(SourceTableSchemaError::ModuleNotFound)?;
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
        let descriptor = build_source_descriptor(
            table_object_id,
            &field_object_ids,
            schema,
            admission,
            analysis,
            module_name,
            self,
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
        validate_records(&self.records)?;
        let raw = Raw::Array(vec![
            Raw::Int(MANIFEST_VERSION.into()),
            Raw::Array(
                self.records
                    .iter()
                    .map(|record| {
                        Raw::Array(vec![
                            Raw::Int((record.kind as u64).into()),
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
        if root.len() != 2 || raw_int(&root[0]) != Some(MANIFEST_VERSION) {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        let Raw::Array(entries) = &root[1] else {
            return Err(ObjectIdentityError::InvalidEncoding);
        };
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            let Raw::Array(fields) = entry else {
                return Err(ObjectIdentityError::InvalidEncoding);
            };
            if fields.len() != 4 {
                return Err(ObjectIdentityError::InvalidEncoding);
            }
            let kind =
                ObjectKind::from_raw(&fields[0]).ok_or(ObjectIdentityError::InvalidEncoding)?;
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
            records.push(ObjectIdentity {
                kind,
                qualified_name: qualified_name.clone(),
                id,
                retired: *retired,
            });
        }
        let manifest = Self { records };
        validate_records(&manifest.records).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        if manifest.encode()?.as_slice() != bytes {
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

    /// Reads and validates the source identity inventory from one exact commit.
    pub fn read_committed(
        repository: &Repository,
        commit: &GitCommitRef,
    ) -> Result<Self, ObjectIdentityError> {
        let bytes = repository
            .read_committed_file(
                commit,
                OBJECT_IDENTITY_MANIFEST_PATH,
                MAX_OBJECT_IDENTITY_MANIFEST_BYTES,
            )
            .map_err(|_| ObjectIdentityError::Io)?;
        Self::decode(&bytes)
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
        let previous = Self::read_worktree(repository)?;
        let next = Self::reconcile_with_renames(modules, previous.as_ref(), renames)?;
        next.write_worktree(repository)?;
        Ok(next)
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
fn build_source_descriptor(
    table_id: [u8; 16],
    field_ids: &BTreeMap<String, [u8; 16]>,
    schema: &TableSchema,
    admission: &TableAdmission,
    analysis: &Analysis,
    module_name: &str,
    manifest: &ObjectIdentityManifest,
) -> Result<SchemaDescriptor, SourceTableSchemaError> {
    if !schema.conversions.is_empty()
        || !admission.computed.is_empty()
        || admission.private_required
        || admission.keys.is_empty()
        || field_ids.len() != schema.fields.len()
        || admission
            .required
            .iter()
            .any(|name| !schema.fields.contains_key(name))
    {
        return Err(SourceTableSchemaError::InvalidSourceSchema);
    }
    let mut source_units = BTreeMap::<SourceUnitIdentity, (SourceUnitMetadata, [u8; 16])>::new();
    for ty in schema.fields.values() {
        collect_source_units(ty, analysis, module_name, manifest, &mut source_units)?;
    }
    let dimension_basis = source_units
        .values()
        .flat_map(|(unit, _)| unit.dimension_basis.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut unit_ids = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    for (identity, (unit, id)) in &source_units {
        let exponents = dimension_basis
            .iter()
            .map(|dimension| {
                Raw::Int(BigInt::from(
                    unit.dimension_basis
                        .get(dimension)
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
        definitions.insert(
            *id,
            Raw::Array(vec![uuid_raw(id), Raw::Int(3.into()), body]),
        );
        unit_ids.insert(identity.clone(), *id);
    }
    let mut ids = BTreeSet::from([table_id]);
    if field_ids.values().any(|id| !ids.insert(*id)) {
        return Err(SourceTableSchemaError::InvalidSourceSchema);
    }
    let mut key_names = BTreeSet::new();
    let mut key_ids = Vec::with_capacity(admission.keys.len());
    for (name, key_type) in &admission.keys {
        if !key_names.insert(name.as_str()) || schema.fields.get(name) != Some(key_type) {
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
    let mut encoded_fields = Vec::with_capacity(schema.fields.len());
    for (name, ty) in &schema.fields {
        if !admission.required.contains(name) && !(admission.automatic_key && name == "id") {
            return Err(SourceTableSchemaError::InvalidSourceSchema);
        }
        let id = field_ids
            .get(name)
            .ok_or(SourceTableSchemaError::MissingIdentity)?;
        let role = u64::from(!key_names.contains(name.as_str()));
        encoded_fields.push((
            *id,
            Raw::Array(vec![
                uuid_raw(id),
                Raw::Text(name.clone()),
                source_type_node(ty, analysis, module_name, &unit_ids)?,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ]),
        ));
    }
    encoded_fields.sort_by_key(|(id, _)| *id);
    let raw = Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid_raw(&table_id)),
        (Raw::Int(2.into()), Raw::Array(key_ids)),
        (
            Raw::Int(3.into()),
            Raw::Array(encoded_fields.into_iter().map(|(_, field)| field).collect()),
        ),
        (
            Raw::Int(4.into()),
            Raw::Array(definitions.into_values().collect()),
        ),
    ]);
    SchemaDescriptor::new(raw).map_err(|_| SourceTableSchemaError::InvalidDescriptor)
}

fn source_unit_integer(value: &str) -> Result<Raw, SourceTableSchemaError> {
    BigInt::parse_bytes(value.as_bytes(), 10)
        .map(Raw::Int)
        .ok_or(SourceTableSchemaError::InvalidSourceSchema)
}

fn uuid_raw(id: &[u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
}

fn source_unit_metadata<'a>(
    analysis: &'a Analysis,
    module_name: &str,
    name: &str,
) -> Result<&'a SourceUnitMetadata, SourceTableSchemaError> {
    let (namespace, unit_name) = name.rsplit_once('.').unwrap_or((module_name, name));
    let mut matches = analysis.source_units.values().filter(|unit| {
        unit.identity.namespace.display() == namespace && unit.identity.name == unit_name
    });
    let unit = matches
        .next()
        .ok_or(SourceTableSchemaError::UnsupportedType)?;
    if matches.next().is_some() || analysis.source_units.get(&unit.identity) != Some(unit) {
        return Err(SourceTableSchemaError::InvalidSourceSchema);
    }
    Ok(unit)
}

fn collect_source_units(
    ty: &Type,
    analysis: &Analysis,
    module_name: &str,
    manifest: &ObjectIdentityManifest,
    output: &mut BTreeMap<SourceUnitIdentity, (SourceUnitMetadata, [u8; 16])>,
) -> Result<(), SourceTableSchemaError> {
    match ty {
        Type::Applied { base, arguments }
            if matches!(base.as_str(), "Float" | "Decimal") && arguments.len() == 1 =>
        {
            let Type::Named(name) = &arguments[0] else {
                return Err(SourceTableSchemaError::UnsupportedType);
            };
            let unit = source_unit_metadata(analysis, module_name, name)?;
            let qualified_name = qualify(&unit.identity.namespace.display(), &unit.identity.name);
            let id = *manifest
                .id(ObjectKind::Unit, &qualified_name)
                .ok_or(SourceTableSchemaError::MissingIdentity)?;
            output.insert(unit.identity.clone(), (unit.clone(), id));
            Ok(())
        }
        Type::List(element) | Type::Optional(element) => {
            collect_source_units(element, analysis, module_name, manifest, output)
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_source_units(element, analysis, module_name, manifest, output)?;
            }
            Ok(())
        }
        Type::Record(fields) => {
            for field in fields.values() {
                collect_source_units(field, analysis, module_name, manifest, output)?;
            }
            Ok(())
        }
        Type::Applied { .. } => Err(SourceTableSchemaError::UnsupportedType),
        _ => Ok(()),
    }
}

fn source_type_node(
    ty: &Type,
    analysis: &Analysis,
    module_name: &str,
    unit_ids: &BTreeMap<SourceUnitIdentity, [u8; 16]>,
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
        _ => None,
    };
    if let Some(name) = primitive {
        return Ok(Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.into())]));
    }
    match ty {
        Type::List(element) => Ok(Raw::Array(vec![
            Raw::Int(1.into()),
            source_type_node(element, analysis, module_name, unit_ids)?,
        ])),
        Type::Optional(element) => Ok(Raw::Array(vec![
            Raw::Int(2.into()),
            source_type_node(element, analysis, module_name, unit_ids)?,
        ])),
        Type::Tuple(elements) => Ok(Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Array(
                elements
                    .iter()
                    .map(|element| source_type_node(element, analysis, module_name, unit_ids))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        ])),
        Type::Record(fields) => Ok(Raw::Array(vec![
            Raw::Int(4.into()),
            Raw::Array(
                fields
                    .iter()
                    .map(|(name, ty)| {
                        Ok(Raw::Array(vec![
                            Raw::Text(name.clone()),
                            source_type_node(ty, analysis, module_name, unit_ids)?,
                        ]))
                    })
                    .collect::<Result<Vec<_>, SourceTableSchemaError>>()?,
            ),
        ])),
        Type::Applied { base, arguments }
            if matches!(base.as_str(), "Float" | "Decimal") && arguments.len() == 1 =>
        {
            let Type::Named(name) = &arguments[0] else {
                return Err(SourceTableSchemaError::UnsupportedType);
            };
            let unit = source_unit_metadata(analysis, module_name, name)?;
            let unit_id = unit_ids
                .get(&unit.identity)
                .ok_or(SourceTableSchemaError::MissingIdentity)?;
            Ok(Raw::Array(vec![
                Raw::Int(7.into()),
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text(base.clone())]),
                uuid_raw(unit_id),
            ]))
        }
        Type::Named(name) => {
            let name = match name.as_str() {
                "Blob" | "std.BINARY_LARGE_OBJECT" => "Blob",
                "std.UUID" => "Uuid",
                "std.TIME" => "TimeOfDay",
                "std.DURATION" => "Duration",
                _ => return Err(SourceTableSchemaError::UnsupportedType),
            };
            Ok(Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.into())]))
        }
        _ => Err(SourceTableSchemaError::UnsupportedType),
    }
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

fn module_namespace(path: &str) -> Option<String> {
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
        assert_eq!(renamed.id(ObjectKind::Field, "Archive.size"), Some(&size_id));
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
        assert_eq!(unchanged.id(ObjectKind::Field, "Archive.key"), Some(&key_id));

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
        let new_field_id = reintroduced.id(ObjectKind::Field, "Archive.payload").unwrap();
        assert_ne!(new_field_id, &removed_field_id);
        assert!(reintroduced.records().iter().any(|record| {
            record.id() == &removed_field_id && record.is_retired()
        }));
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
            .source_table_schema(&analysis, "", "main.orna", "Media")
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
}
