//! Immutable semantic catalogue snapshots.
//!
//! A snapshot contains resolved definitions for one active catalogue revision.
//! It does not contain source syntax, physical storage state, or backend types.

use std::{collections::HashMap, error::Error, fmt};

use crate::{CatalogueRevisionId, ExpressionId, FieldId, TypeId, types::ResolvedType};

/// A resolved, qualified semantic name.
///
/// Name resolution establishes identifier case and quoted-name semantics before
/// it creates this value. This type compares its parts exactly.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QualifiedSemanticName {
    parts: Vec<String>,
}

impl QualifiedSemanticName {
    /// Creates a semantic name from resolved name parts.
    pub fn new(
        parts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, SemanticNameError> {
        let parts = parts.into_iter().map(Into::into).collect::<Vec<_>>();

        if parts.is_empty() {
            return Err(SemanticNameError::EmptyName);
        }

        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() {
                return Err(SemanticNameError::EmptyPart { index });
            }
        }

        Ok(Self { parts })
    }

    /// Returns the resolved name parts in qualification order.
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
}

impl fmt::Display for QualifiedSemanticName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.parts.join("."))
    }
}

/// An error returned when a semantic name cannot represent qualified parts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticNameError {
    /// A qualified name must contain at least one part.
    EmptyName,
    /// A qualified name cannot contain an empty part.
    EmptyPart {
        /// The zero-based position of the invalid part.
        index: usize,
    },
}

impl fmt::Display for SemanticNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => {
                formatter.write_str("a semantic name must contain at least one part")
            }
            Self::EmptyPart { index } => write!(formatter, "semantic name part {index} is empty"),
        }
    }
}

impl Error for SemanticNameError {}

/// The action to take when a referenced object is deleted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OnDeleteAction {
    /// Reject deletion while a referencing value exists.
    Restrict,
    /// Set the reference field to null when its target is deleted.
    SetNull,
    /// Delete the object that contains the reference.
    Cascade,
}

/// One resolved field of an object type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldDefinition {
    id: FieldId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
    unique: bool,
    default_expression: Option<ExpressionId>,
    on_delete: Option<OnDeleteAction>,
}

impl FieldDefinition {
    /// Creates a field definition from resolved semantic data.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FieldId,
        name: impl Into<String>,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
        unique: bool,
        default_expression: Option<ExpressionId>,
        on_delete: Option<OnDeleteAction>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
        }
    }

    /// Returns this field's stable identity.
    pub const fn id(&self) -> FieldId {
        self.id
    }

    /// Returns this field's resolved semantic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns this field's zero-based declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns this field's resolved type descriptor.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    /// Reports whether this field can contain null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether this field has a uniqueness constraint.
    pub const fn unique(&self) -> bool {
        self.unique
    }

    /// Returns the identity of the resolved default expression, when present.
    pub const fn default_expression(&self) -> Option<ExpressionId> {
        self.default_expression
    }

    /// Returns the delete policy for a typed reference, when present.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
}

/// One resolved durable object type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectTypeDefinition {
    id: TypeId,
    name: QualifiedSemanticName,
    fields: Vec<FieldDefinition>,
}

impl ObjectTypeDefinition {
    /// Creates an object type definition from resolved semantic data.
    ///
    /// [`CatalogueSnapshot::new`] validates the type's field invariants before
    /// it accepts this definition into an immutable snapshot.
    pub fn new(id: TypeId, name: QualifiedSemanticName, fields: Vec<FieldDefinition>) -> Self {
        Self { id, name, fields }
    }

    /// Returns this type's stable identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns this type's resolved qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns fields in declaration ordinal order.
    pub fn fields(&self) -> &[FieldDefinition] {
        &self.fields
    }

    /// Finds a field by its exact resolved semantic name.
    pub fn field_by_name(&self, name: &str) -> Option<&FieldDefinition> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Finds a field by its stable identity.
    pub fn field_by_id(&self, id: FieldId) -> Option<&FieldDefinition> {
        self.fields.iter().find(|field| field.id == id)
    }
}

/// An immutable set of resolved definitions for one catalogue revision.
#[derive(Clone, Debug)]
pub struct CatalogueSnapshot {
    revision: CatalogueRevisionId,
    object_types: Vec<ObjectTypeDefinition>,
    object_type_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    object_type_indices_by_id: HashMap<TypeId, usize>,
}

impl CatalogueSnapshot {
    /// Validates and creates an immutable catalogue snapshot.
    pub fn new(
        revision: CatalogueRevisionId,
        object_types: Vec<ObjectTypeDefinition>,
    ) -> Result<Self, CatalogueSnapshotError> {
        let mut object_type_indices_by_name = HashMap::with_capacity(object_types.len());
        let mut object_type_indices_by_id = HashMap::with_capacity(object_types.len());

        for (type_index, object_type) in object_types.iter().enumerate() {
            if object_type_indices_by_name
                .insert(object_type.name.clone(), type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateObjectTypeName {
                    name: object_type.name.clone(),
                });
            }

            if object_type_indices_by_id
                .insert(object_type.id, type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateObjectTypeId { id: object_type.id });
            }

            Self::validate_fields(object_type)?;
        }

        Ok(Self {
            revision,
            object_types,
            object_type_indices_by_name,
            object_type_indices_by_id,
        })
    }

    /// Returns the stable identity of this catalogue revision.
    pub const fn revision(&self) -> CatalogueRevisionId {
        self.revision
    }

    /// Returns the object type definitions in their snapshot order.
    pub fn object_types(&self) -> &[ObjectTypeDefinition] {
        &self.object_types
    }

    /// Finds an object type by its exact resolved qualified name.
    pub fn object_type_by_name(
        &self,
        name: &QualifiedSemanticName,
    ) -> Option<&ObjectTypeDefinition> {
        self.object_type_indices_by_name
            .get(name)
            .map(|index| &self.object_types[*index])
    }

    /// Finds an object type by its stable identity.
    pub fn object_type_by_id(&self, id: TypeId) -> Option<&ObjectTypeDefinition> {
        self.object_type_indices_by_id
            .get(&id)
            .map(|index| &self.object_types[*index])
    }

    fn validate_fields(object_type: &ObjectTypeDefinition) -> Result<(), CatalogueSnapshotError> {
        let mut field_ids = HashMap::with_capacity(object_type.fields.len());
        let mut field_names = HashMap::with_capacity(object_type.fields.len());
        let mut ordinals = HashMap::with_capacity(object_type.fields.len());

        for (index, field) in object_type.fields.iter().enumerate() {
            if field.name.is_empty() {
                return Err(CatalogueSnapshotError::EmptyFieldName {
                    owner: object_type.id,
                    field: field.id,
                });
            }

            if field_names.insert(field.name.as_str(), index).is_some() {
                return Err(CatalogueSnapshotError::DuplicateFieldName {
                    owner: object_type.id,
                    name: field.name.clone(),
                });
            }

            if field_ids.insert(field.id, index).is_some() {
                return Err(CatalogueSnapshotError::DuplicateFieldId {
                    owner: object_type.id,
                    id: field.id,
                });
            }

            if ordinals.insert(field.ordinal, index).is_some() {
                return Err(CatalogueSnapshotError::DuplicateFieldOrdinal {
                    owner: object_type.id,
                    ordinal: field.ordinal,
                });
            }

            let expected = u32::try_from(index).map_err(|_| {
                CatalogueSnapshotError::FieldOrdinalOutOfRange {
                    owner: object_type.id,
                    field: field.id,
                }
            })?;
            if field.ordinal != expected {
                return Err(CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                    owner: object_type.id,
                    field: field.id,
                    expected,
                    actual: field.ordinal,
                });
            }
        }

        Ok(())
    }
}

/// An error returned when definitions cannot form a coherent snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueSnapshotError {
    /// More than one object type has the same resolved qualified name.
    DuplicateObjectTypeName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one object type has the same stable identity.
    DuplicateObjectTypeId {
        /// The repeated identity.
        id: TypeId,
    },
    /// An object field has no semantic name.
    EmptyFieldName {
        /// The type that owns the invalid field.
        owner: TypeId,
        /// The invalid field identity.
        field: FieldId,
    },
    /// More than one field in an object type has the same semantic name.
    DuplicateFieldName {
        /// The owning object type.
        owner: TypeId,
        /// The repeated name.
        name: String,
    },
    /// More than one field in an object type has the same stable identity.
    DuplicateFieldId {
        /// The owning object type.
        owner: TypeId,
        /// The repeated identity.
        id: FieldId,
    },
    /// More than one field in an object type has the same ordinal.
    DuplicateFieldOrdinal {
        /// The owning object type.
        owner: TypeId,
        /// The repeated ordinal.
        ordinal: u32,
    },
    /// An object type has more fields than the ordinal representation allows.
    FieldOrdinalOutOfRange {
        /// The owning object type.
        owner: TypeId,
        /// The field without a representable ordinal.
        field: FieldId,
    },
    /// Fields must be contiguous and stored in declaration ordinal order.
    FieldOrdinalOutOfSequence {
        /// The owning object type.
        owner: TypeId,
        /// The field that has the invalid ordinal.
        field: FieldId,
        /// The expected zero-based ordinal.
        expected: u32,
        /// The actual ordinal.
        actual: u32,
    },
}

impl fmt::Display for CatalogueSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateObjectTypeName { name } => {
                write!(formatter, "duplicate object type name {name}")
            }
            Self::DuplicateObjectTypeId { id } => {
                write!(formatter, "duplicate object type identity {id}")
            }
            Self::EmptyFieldName { owner, field } => {
                write!(
                    formatter,
                    "field {field} in object type {owner} has an empty name"
                )
            }
            Self::DuplicateFieldName { owner, name } => {
                write!(
                    formatter,
                    "duplicate field name {name} in object type {owner}"
                )
            }
            Self::DuplicateFieldId { owner, id } => {
                write!(
                    formatter,
                    "duplicate field identity {id} in object type {owner}"
                )
            }
            Self::DuplicateFieldOrdinal { owner, ordinal } => {
                write!(
                    formatter,
                    "duplicate field ordinal {ordinal} in object type {owner}"
                )
            }
            Self::FieldOrdinalOutOfRange { owner, field } => {
                write!(
                    formatter,
                    "field {field} in object type {owner} has no representable ordinal"
                )
            }
            Self::FieldOrdinalOutOfSequence {
                owner,
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} in object type {owner} has ordinal {actual}, expected {expected}"
            ),
        }
    }
}

impl Error for CatalogueSnapshotError {}

#[cfg(test)]
mod tests {
    use super::{
        CatalogueSnapshot, CatalogueSnapshotError, FieldDefinition, ObjectTypeDefinition,
        OnDeleteAction, QualifiedSemanticName, SemanticNameError,
    };
    use crate::{
        CatalogueRevisionId, ExpressionId, FieldId, TypeId,
        types::{ResolvedType, StandardScalar},
    };

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn field(id: u8, name: &str, ordinal: u32) -> FieldDefinition {
        FieldDefinition::new(
            FieldId::from_bytes([id; 16]),
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
            false,
            None,
            None,
        )
    }

    fn object(id: u8, name_parts: &[&str], fields: Vec<FieldDefinition>) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(TypeId::from_bytes([id; 16]), name(name_parts), fields)
    }

    fn snapshot(types: Vec<ObjectTypeDefinition>) -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([7; 16]), types).unwrap()
    }

    #[test]
    fn snapshot_resolves_object_types_by_exact_name_and_stable_id() {
        let contact_id = TypeId::from_bytes([1; 16]);
        let person_id = TypeId::from_bytes([2; 16]);
        let contact = ObjectTypeDefinition::new(
            contact_id,
            name(&["crm", "contact"]),
            vec![FieldDefinition::new(
                FieldId::from_bytes([3; 16]),
                "person",
                0,
                ResolvedType::reference(person_id),
                false,
                true,
                Some(ExpressionId::from_bytes([4; 16])),
                Some(OnDeleteAction::Restrict),
            )],
        );
        let catalogue = snapshot(vec![contact]);

        let contact = catalogue
            .object_type_by_name(&name(&["crm", "contact"]))
            .unwrap();
        assert_eq!(
            catalogue.revision(),
            CatalogueRevisionId::from_bytes([7; 16])
        );
        assert_eq!(catalogue.object_type_by_id(contact_id), Some(contact));
        assert!(
            catalogue
                .object_type_by_name(&name(&["CRM", "contact"]))
                .is_none()
        );

        let person = contact.field_by_name("person").unwrap();
        assert_eq!(person.resolved_type(), ResolvedType::reference(person_id));
        assert!(!person.nullable());
        assert!(person.unique());
        assert_eq!(
            person.default_expression(),
            Some(ExpressionId::from_bytes([4; 16]))
        );
        assert_eq!(person.on_delete(), Some(OnDeleteAction::Restrict));
    }

    #[test]
    fn semantic_names_require_nonempty_parts() {
        assert_eq!(
            QualifiedSemanticName::new(Vec::<String>::new()),
            Err(SemanticNameError::EmptyName)
        );
        assert_eq!(
            QualifiedSemanticName::new(["crm", ""]),
            Err(SemanticNameError::EmptyPart { index: 1 })
        );
        assert_eq!(
            QualifiedSemanticName::new(["crm.contact"]),
            Ok(name(&["crm.contact"]))
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_type_names_and_ids() {
        let first = object(1, &["crm", "contact"], vec![]);
        let same_name = object(2, &["crm", "contact"], vec![]);
        let same_id = object(1, &["crm", "person"], vec![]);

        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![first.clone(), same_name]),
            Err(CatalogueSnapshotError::DuplicateObjectTypeName { name: duplicate_name })
                if duplicate_name == name(&["crm", "contact"])
        ));
        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![first, same_id]),
            Err(CatalogueSnapshotError::DuplicateObjectTypeId { id }) if id == TypeId::from_bytes([1; 16])
        ));
    }

    #[test]
    fn snapshot_rejects_duplicate_fields_per_object_type() {
        let duplicate_name = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(2, "name", 1)],
        );
        let duplicate_id = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(1, "email", 1)],
        );
        let duplicate_ordinal = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(2, "email", 0)],
        );

        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![duplicate_name]),
            Err(CatalogueSnapshotError::DuplicateFieldName { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![duplicate_id]),
            Err(CatalogueSnapshotError::DuplicateFieldId { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![duplicate_ordinal]),
            Err(CatalogueSnapshotError::DuplicateFieldOrdinal { .. })
        ));
    }

    #[test]
    fn snapshot_requires_contiguous_fields_in_ordinal_order() {
        let missing_first_ordinal = object(1, &["crm", "contact"], vec![field(1, "name", 1)]);
        let out_of_order = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 1), field(2, "email", 0)],
        );

        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![missing_first_ordinal]),
            Err(CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                expected: 0,
                actual: 1,
                ..
            })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![out_of_order]),
            Err(CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                expected: 0,
                actual: 1,
                ..
            })
        ));
    }
}
