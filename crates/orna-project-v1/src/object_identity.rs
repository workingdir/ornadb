//! Persisted source identities for table declarations and their fields.
//!
//! Reconciliation is an explicit authoring operation. Loading source never
//! allocates identities, and retired identities are never reused.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use orna_repository_v1::{GitCommitRef, Repository, Uuid};
use orna_semantic_v1::{Analysis, ModuleInput, SymbolKind, TableAdmission, TableSchema, Type};
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
}

impl ObjectKind {
    fn from_raw(raw: &Raw) -> Option<Self> {
        match raw {
            Raw::Int(value) if value.to_string() == "1" => Some(Self::Table),
            Raw::Int(value) if value.to_string() == "2" => Some(Self::Field),
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
        let descriptor =
            build_source_descriptor(table_object_id, &field_object_ids, schema, admission)?;
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

    /// Writer-only reconciliation. Existing IDs survive source edits; removed
    /// names become permanently retired; new names receive random UUIDs.
    pub fn reconcile(
        modules: &[ModuleInput],
        previous: Option<&Self>,
    ) -> Result<Self, ObjectIdentityError> {
        let active = source_identity_keys(modules)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut records = previous.map_or_else(Vec::new, |manifest| manifest.records.clone());
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
        match fs::read(path) {
            Ok(bytes) => Self::decode(&bytes).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(ObjectIdentityError::Io),
        }
    }

    /// Reconciles an explicit source snapshot with the current worktree
    /// manifest and atomically writes the result. Loading never calls this.
    pub fn reconcile_and_write_worktree(
        repository: &Repository,
        modules: &[ModuleInput],
    ) -> Result<Self, ObjectIdentityError> {
        let previous = Self::read_worktree(repository)?;
        let next = Self::reconcile(modules, previous.as_ref())?;
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
                source_type_node(ty)?,
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
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]);
    SchemaDescriptor::new(raw).map_err(|_| SourceTableSchemaError::InvalidDescriptor)
}

fn uuid_raw(id: &[u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
}

fn source_type_node(ty: &Type) -> Result<Raw, SourceTableSchemaError> {
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
            source_type_node(element)?,
        ])),
        Type::Optional(element) => Ok(Raw::Array(vec![
            Raw::Int(2.into()),
            source_type_node(element)?,
        ])),
        Type::Tuple(elements) => Ok(Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Array(
                elements
                    .iter()
                    .map(source_type_node)
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
                            source_type_node(ty)?,
                        ]))
                    })
                    .collect::<Result<Vec<_>, SourceTableSchemaError>>()?,
            ),
        ])),
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
            if let Declaration::Table {
                name,
                keys: table_keys,
                members,
                ..
            } = &item.declaration
            {
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
            "pub table Object(id: Str) { payload: Blob, size: Float, }",
        );
        let first = ObjectIdentityManifest::reconcile(std::slice::from_ref(&source), None).unwrap();
        let encoded = first.encode().unwrap();
        assert_eq!(ObjectIdentityManifest::decode(&encoded).unwrap(), first);
        let payload_id = *first.id(ObjectKind::Field, "Object.payload").unwrap();
        let stable =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&source), Some(&first)).unwrap();
        assert_eq!(
            stable.id(ObjectKind::Field, "Object.payload"),
            Some(&payload_id)
        );
        let removed = ObjectIdentityManifest::reconcile(&[], Some(&stable)).unwrap();
        assert!(removed.id(ObjectKind::Field, "Object.payload").is_none());
        assert!(
            removed
                .records()
                .iter()
                .any(|record| record.qualified_name() == "Object.payload" && record.is_retired())
        );
        assert_eq!(
            ObjectIdentityManifest::decode(&removed.encode().unwrap()).unwrap(),
            removed
        );
    }
}
