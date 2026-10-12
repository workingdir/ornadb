//! Canonical committed ObjectId metadata and source identity issuance.
//!
//! Identities are persisted inputs to authoring, never derived from names or
//! source hashes. Reconciliation is an authoring operation; readers only decode
//! the committed manifest.

use std::collections::BTreeSet;

use orna_syntax_v1::{Declaration, TypeMember, TypeRepresentation, parse_module_with_file};
use orna_value_v1::{Raw, Value};
use uuid::Uuid;

use crate::ModuleInput;

pub const OBJECT_IDENTITY_METADATA_PATH: &str = ".orna/objects";
const MANIFEST_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u64)]
pub enum ObjectKind {
    Module = 0,
    Table = 1,
    Column = 2,
    Function = 3,
    Nominal = 4,
    Variant = 5,
    Dimension = 6,
    Unit = 7,
    Protocol = 8,
}

impl ObjectKind {
    fn from_raw(raw: &Raw) -> Option<Self> {
        match raw {
            Raw::Int(value) => match value.to_u64()? {
                0 => Some(Self::Module),
                1 => Some(Self::Table),
                2 => Some(Self::Column),
                3 => Some(Self::Function),
                4 => Some(Self::Nominal),
                5 => Some(Self::Variant),
                6 => Some(Self::Dimension),
                7 => Some(Self::Unit),
                8 => Some(Self::Protocol),
                _ => None,
            },
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
    pub fn kind(&self) -> ObjectKind { self.kind }
    pub fn qualified_name(&self) -> &str { &self.qualified_name }
    pub fn id(&self) -> &[u8; 16] { &self.id }
    pub const fn is_retired(&self) -> bool { self.retired }
}

/// Immutable committed identity inventory. IDs survive source revisions and
/// only explicit authoring continuity may move an ID to another name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObjectIdentityManifest {
    records: Vec<ObjectIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectIdentityError {
    InvalidEncoding,
    InvalidSource,
    IdentityCollision,
    AllocationFailed,
}

impl ObjectIdentityManifest {
    pub fn records(&self) -> &[ObjectIdentity] { &self.records }

    pub fn id(&self, kind: ObjectKind, qualified_name: &str) -> Option<&[u8; 16]> {
        self.records.iter().find(|record| {
            !record.retired && record.kind == kind && record.qualified_name == qualified_name
        }).map(|record| &record.id)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ObjectIdentityError> {
        let mut records = self.records.clone();
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str(), left.id)
                .cmp(&(right.kind, right.qualified_name.as_str(), right.id))
        });
        validate_records(&records)?;
        let raw = Raw::Array(vec![
            Raw::Int(MANIFEST_VERSION.into()),
            Raw::Array(records.into_iter().map(|record| Raw::Array(vec![
                Raw::Int((record.kind as u64).into()),
                Raw::Text(record.qualified_name),
                Raw::Bytes(record.id.to_vec()),
                Raw::Bool(record.retired),
            ])).collect()),
        ]);
        Value::new(raw).and_then(|value| value.encode()).map_err(|_| ObjectIdentityError::InvalidEncoding)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ObjectIdentityError> {
        let value = Value::decode(bytes).map_err(|_| ObjectIdentityError::InvalidEncoding)?;
        let Raw::Array(root) = value.raw() else { return Err(ObjectIdentityError::InvalidEncoding) };
        if root.len() != 2 || int_u64(&root[0]) != Some(MANIFEST_VERSION) {
            return Err(ObjectIdentityError::InvalidEncoding);
        }
        let Raw::Array(entries) = &root[1] else { return Err(ObjectIdentityError::InvalidEncoding) };
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            let Raw::Array(fields) = entry else { return Err(ObjectIdentityError::InvalidEncoding) };
            if fields.len() != 4 { return Err(ObjectIdentityError::InvalidEncoding) }
            let kind = ObjectKind::from_raw(&fields[0]).ok_or(ObjectIdentityError::InvalidEncoding)?;
            let Raw::Text(qualified_name) = &fields[1] else { return Err(ObjectIdentityError::InvalidEncoding) };
            let Raw::Bytes(id) = &fields[2] else { return Err(ObjectIdentityError::InvalidEncoding) };
            let Raw::Bool(retired) = &fields[3] else { return Err(ObjectIdentityError::InvalidEncoding) };
            let id: [u8; 16] = id.as_slice().try_into().map_err(|_| ObjectIdentityError::InvalidEncoding)?;
            records.push(ObjectIdentity { kind, qualified_name: qualified_name.clone(), id, retired: *retired });
        }
        validate_records(&records)?;
        let manifest = Self { records };
        if manifest.encode()?.as_slice() != bytes { return Err(ObjectIdentityError::InvalidEncoding) }
        Ok(manifest)
    }

    /// Reconciles reachable parsed source during a writer-owned authoring
    /// transaction. Existing IDs are retained; removed identities are retired
    /// permanently; newly declared objects receive random UUIDs, never hashes.
    pub fn reconcile(modules: &[ModuleInput], previous: Option<&Self>) -> Result<Self, ObjectIdentityError> {
        let declarations = source_identity_keys(modules)?;
        let active = declarations.into_iter().collect::<BTreeSet<_>>();
        let mut records = previous.map_or_else(Vec::new, |manifest| manifest.records.clone());
        for record in &mut records {
            if !active.contains(&(record.kind, record.qualified_name.clone())) {
                record.retired = true;
            }
        }
        let mut used_ids = records.iter().map(|record| record.id).collect::<BTreeSet<_>>();
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
            records.push(ObjectIdentity { kind, qualified_name, id, retired: false });
        }
        records.sort_by(|left, right| {
            (left.kind, left.qualified_name.as_str(), left.id)
                .cmp(&(right.kind, right.qualified_name.as_str(), right.id))
        });
        validate_records(&records)?;
        Ok(Self { records })
    }
}

fn source_identity_keys(modules: &[ModuleInput]) -> Result<Vec<(ObjectKind, String)>, ObjectIdentityError> {
    let mut keys = Vec::new();
    for module in modules {
        let namespace = namespace_from_path(&module.logical_path).ok_or(ObjectIdentityError::InvalidSource)?;
        keys.push((ObjectKind::Module, namespace.clone()));
        let parsed = parse_module_with_file(&module.source, module.logical_path.clone());
        if !parsed.is_ok() { return Err(ObjectIdentityError::InvalidSource) }
        for item in &parsed.value.items {
            match &item.declaration {
                Declaration::Table { name, members, .. } => {
                    let table = qualify(&namespace, name);
                    keys.push((ObjectKind::Table, table.clone()));
                    for member in members {
                        if let orna_syntax_v1::TableMember::Field { name, .. } = member {
                            keys.push((ObjectKind::Column, format!("{table}.{name}")));
                        }
                    }
                }
                Declaration::Function { signature, .. } => keys.push((ObjectKind::Function, qualify(&namespace, &signature.name))),
                Declaration::Protocol { name, .. } => keys.push((ObjectKind::Protocol, qualify(&namespace, name))),
                Declaration::Enum { name, variants, .. } => {
                    let owner = qualify(&namespace, name);
                    keys.push((ObjectKind::Nominal, owner.clone()));
                    for variant in variants {
                        keys.push((ObjectKind::Variant, format!("{owner}::{}", variant.name)));
                        for field in &variant.fields {
                            keys.push((ObjectKind::Column, format!("{owner}::{}.{}", variant.name, field.name)));
                        }
                    }
                }
                Declaration::Dimension { name, .. } => keys.push((ObjectKind::Dimension, qualify(&namespace, name))),
                Declaration::Unit { name, .. } => keys.push((ObjectKind::Unit, qualify(&namespace, name))),
                Declaration::Type { name, representation, .. } => {
                    let owner = qualify(&namespace, name);
                    if let TypeRepresentation::Nominal { members } = representation {
                        keys.push((ObjectKind::Nominal, owner.clone()));
                        for member in members {
                            if let TypeMember::Field { name, .. } = member {
                                keys.push((ObjectKind::Column, format!("{owner}.{name}")));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    keys.sort();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) { return Err(ObjectIdentityError::IdentityCollision) }
    Ok(keys)
}

fn namespace_from_path(path: &str) -> Option<String> {
    let mut parts = path.strip_suffix(".orna")?.split('/').collect::<Vec<_>>();
    let file = parts.pop()?;
    if file != "main" { parts.push(file); }
    if parts.iter().any(|part| part.is_empty() || matches!(*part, "." | "..")) { return None }
    let namespace = parts.join(".");
    Some(if namespace.is_empty() { "main".to_owned() } else { namespace })
}

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() { name.to_owned() } else { format!("{namespace}.{name}") }
}

fn int_u64(raw: &Raw) -> Option<u64> {
    if let Raw::Int(value) = raw { value.to_string().parse().ok() } else { None }
}

fn validate_records(records: &[ObjectIdentity]) -> Result<(), ObjectIdentityError> {
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for record in records {
        if record.qualified_name.is_empty() || record.qualified_name.contains('\0')
            || !keys.insert((record.kind, record.qualified_name.as_str(), record.retired))
            || !ids.insert(record.id)
        { return Err(ObjectIdentityError::IdentityCollision) }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ObjectIdentityManifest, ObjectKind};
    use crate::ModuleInput;

    #[test]
    fn committed_ids_survive_edits_and_retirement_prevents_reuse() {
        let first_source = "pub table Note(id: Str) { title: Str, }";
        let first = ObjectIdentityManifest::reconcile(
            &[ModuleInput::new("main.orna", first_source)],
            None,
        )
        .expect("initial authoring allocates committed identities");
        let table_id = *first.id(ObjectKind::Table, "Note").unwrap();
        let title_id = *first.id(ObjectKind::Column, "Note.title").unwrap();
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
        assert_eq!(edited.id(ObjectKind::Column, "Note.title"), Some(&title_id));

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
    }
}
