//! Backend-neutral planning for supported durable object storage changes.

use std::{error::Error, fmt};

use crate::{
    FieldId, TypeId,
    catalogue::{
        FieldDefinition, ObjectTypeDefinition, OnDeleteAction, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    types::StandardScalar,
};

/// Plans the physical changes supported by the initial durable-object slice.
///
/// The result contains no backend names, types, or statements. Unsupported
/// changes fail closed before a storage adapter receives a plan.
pub fn plan_physical_changes(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<PhysicalPlan, PhysicalPlanError> {
    if candidate.expected_base() != active.pair() {
        return Err(PhysicalPlanError::ExpectedBaseMismatch {
            expected: candidate.expected_base(),
            active: active.pair(),
        });
    }

    for active_object in active.catalogue().object_types() {
        if candidate
            .candidate()
            .object_type_by_id(active_object.id())
            .is_none()
        {
            return Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: active_object.id(),
            });
        }
    }

    let active_revision = PhysicalRevision::Active(active);
    let candidate_revision = PhysicalRevision::Deployable(candidate);

    let mut add_field = None;
    for active_object in active.catalogue().object_types() {
        let candidate_object = candidate
            .candidate()
            .object_type_by_id(active_object.id())
            .ok_or(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: active_object.id(),
            })?;
        let appended_value_type = candidate_object
            .fields()
            .get(active_object.fields().len()..)
            .and_then(|added| {
                let [field] = added else {
                    return None;
                };
                field.resolved_type().value_type()
            });
        let active_projection = project_physical_object(active_revision, active_object)?;
        let candidate_projection = project_physical_object(candidate_revision, candidate_object)?;
        if active_projection != candidate_projection {
            if add_field.is_some() {
                return Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: active_object.id(),
                });
            }
            let field = appended_nullable_value_scalar_field(
                &active_projection,
                &candidate_projection,
                appended_value_type,
            )
            .ok_or(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: active_object.id(),
            })?;
            add_field = Some(AddField {
                object_type: active_object.id(),
                field,
            });
        }
    }

    let create_objects = candidate
        .candidate()
        .object_types()
        .iter()
        .filter(|object_type| {
            active
                .catalogue()
                .object_type_by_id(object_type.id())
                .is_none()
        })
        .map(|object_type| project_physical_object(candidate_revision, object_type))
        .collect::<Result<_, _>>()?;

    Ok(PhysicalPlan {
        create_objects,
        add_field,
    })
}

fn appended_nullable_value_scalar_field(
    active: &CreateObject,
    candidate: &CreateObject,
    appended_value_type: Option<TypeId>,
) -> Option<CreateField> {
    let added = candidate.fields.strip_prefix(active.fields.as_slice())?;
    let [field] = added else {
        return None;
    };
    if appended_value_type.is_none()
        || !matches!(
            field.field_type,
            PhysicalFieldType::Scalar(
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            )
        )
        || !field.nullable
        || field.unique
    {
        return None;
    }
    Some(field.clone())
}

/// Projects an active catalogue into physical object storage facts.
pub fn active_physical_catalogue(
    active: &ActiveDatabaseRevision,
) -> Result<PhysicalCatalogue, PhysicalPlanError> {
    let revision = PhysicalRevision::Active(active);
    let objects = active
        .catalogue()
        .object_types()
        .iter()
        .map(|object_type| project_physical_object(revision, object_type))
        .collect::<Result<_, _>>()?;
    Ok(PhysicalCatalogue { objects })
}

/// One complete ordered physical catalogue projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalCatalogue {
    objects: Vec<CreateObject>,
}

impl PhysicalCatalogue {
    /// Returns physical objects in catalogue snapshot order.
    pub fn objects(&self) -> &[CreateObject] {
        &self.objects
    }
}

/// One complete ordered set of supported physical changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalPlan {
    create_objects: Vec<CreateObject>,
    add_field: Option<AddField>,
}

impl PhysicalPlan {
    /// Returns new durable object relations in candidate catalogue order.
    pub fn create_objects(&self) -> &[CreateObject] {
        &self.create_objects
    }

    /// Returns the one admitted existing-object field addition, when present.
    pub const fn add_field(&self) -> Option<&AddField> {
        self.add_field.as_ref()
    }
}

/// One appended field on one existing durable object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddField {
    object_type: TypeId,
    field: CreateField,
}

impl AddField {
    /// Returns the stable identity of the existing object type.
    pub const fn object_type(&self) -> TypeId {
        self.object_type
    }

    /// Returns the appended field's backend-neutral physical projection.
    pub const fn field(&self) -> &CreateField {
        &self.field
    }
}

/// One physical object projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateObject {
    type_id: TypeId,
    fields: Vec<CreateField>,
}

impl CreateObject {
    /// Returns the stable object-type identity.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns physical fields in declaration ordinal order.
    pub fn fields(&self) -> &[CreateField] {
        &self.fields
    }
}

/// One backend-neutral physical field projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateField {
    field_id: FieldId,
    field_type: PhysicalFieldType,
    nullable: bool,
    unique: bool,
}

impl CreateField {
    /// Returns the stable field identity.
    pub const fn field_id(&self) -> FieldId {
        self.field_id
    }

    /// Returns the backend-neutral storage type.
    pub const fn field_type(&self) -> PhysicalFieldType {
        self.field_type
    }

    /// Reports whether the physical field can contain null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether the physical field requires one-column uniqueness.
    pub const fn unique(&self) -> bool {
        self.unique
    }
}

/// The closed field types supported by initial physical creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalFieldType {
    /// A standard scalar with a backend storage encoding.
    Scalar(StandardScalar),
    /// An application enum stored through its stable catalogue identity.
    Enum(TypeId),
    /// A nominal record value stored as canonical Orna value bytes.
    Record(TypeId),
    /// A typed object reference with its delete action.
    Reference {
        /// The referenced durable object type.
        target: TypeId,
        /// The selected delete action, or the language default.
        on_delete: Option<OnDeleteAction>,
    },
}

/// A fail-closed error returned for an unsupported physical change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhysicalPlanError {
    /// The deployable candidate does not target the supplied active pair.
    ExpectedBaseMismatch {
        expected: RevisionPair,
        active: RevisionPair,
    },
    /// An active durable object type is absent from the complete candidate.
    UnsupportedObjectDrop { object_type: TypeId },
    /// An active durable object type has an unsupported storage change.
    UnsupportedExistingObjectChange { object_type: TypeId },
    /// A new field uses a named value type without a storage contract.
    UnsupportedNamedFieldType { object_type: TypeId, field: FieldId },
    /// A record field is nullable before nullable record values are defined.
    UnsupportedNullableRecordField { object_type: TypeId, field: FieldId },
    /// A resolved value type is absent from the pinned standard library.
    MissingValueTypeDefinition {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
    },
    /// A resolved value type does not have a supported physical contract.
    UnsupportedValueTypeContract {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
        contract: String,
    },
    /// A resolved value type is valid only in transient positions.
    TransientValueType {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
    },
    /// A new field uses the non-storable VOID scalar.
    UnsupportedVoidField { object_type: TypeId, field: FieldId },
    /// The field requests uniqueness outside the Text or required-Reference shapes.
    UnsupportedUniqueField { object_type: TypeId, field: FieldId },
    /// Initial physical creation does not yet install field defaults.
    UnsupportedFieldDefault { object_type: TypeId, field: FieldId },
    /// A typed reference names no object in the complete candidate catalogue.
    UnknownReferenceTarget {
        object_type: TypeId,
        field: FieldId,
        target: TypeId,
    },
    /// A delete action is incompatible with the field type or nullability.
    InvalidDeleteAction { object_type: TypeId, field: FieldId },
}

impl fmt::Display for PhysicalPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedBaseMismatch { .. } => {
                formatter.write_str("physical candidate base is not active")
            }
            Self::UnsupportedObjectDrop { .. } => {
                formatter.write_str("durable object drops are not supported")
            }
            Self::UnsupportedExistingObjectChange { .. } => {
                formatter.write_str("changes to existing object storage are not supported")
            }
            Self::UnsupportedNamedFieldType { .. } => {
                formatter.write_str("named field storage is not supported")
            }
            Self::UnsupportedNullableRecordField { .. } => {
                formatter.write_str("nullable record fields are not supported")
            }
            Self::MissingValueTypeDefinition { .. } => formatter
                .write_str("physical value type is absent from the pinned standard library"),
            Self::UnsupportedValueTypeContract { .. } => {
                formatter.write_str("physical value type contract is not supported")
            }
            Self::TransientValueType { .. } => {
                formatter.write_str("transient value types cannot be stored")
            }
            Self::UnsupportedVoidField { .. } => {
                formatter.write_str("VOID fields cannot be stored")
            }
            Self::UnsupportedUniqueField { .. } => formatter
                .write_str("UNIQUE is supported only for TEXT fields or required REF fields"),
            Self::UnsupportedFieldDefault { .. } => {
                formatter.write_str("physical field defaults are not supported")
            }
            Self::UnknownReferenceTarget { .. } => {
                formatter.write_str("physical REF target is absent from the candidate")
            }
            Self::InvalidDeleteAction { .. } => {
                formatter.write_str("physical field delete action is invalid")
            }
        }
    }
}

impl Error for PhysicalPlanError {}

#[derive(Clone, Copy)]
enum PhysicalRevision<'a> {
    Active(&'a ActiveDatabaseRevision),
    Deployable(&'a DeployableRevision),
}

impl<'a> PhysicalRevision<'a> {
    fn catalogue(self) -> &'a crate::catalogue::CatalogueSnapshot {
        match self {
            Self::Active(active) => active.catalogue(),
            Self::Deployable(candidate) => candidate.candidate(),
        }
    }

    fn standard_catalogue(self) -> Option<&'a crate::catalogue::CatalogueSnapshot> {
        match self {
            Self::Active(active) => active.catalogue_hash_context().standard(),
            Self::Deployable(candidate) => candidate.catalogue_hash_context().standard(),
        }
        .map(crate::revision::VerifiedStandardLibrarySnapshot::catalogue)
    }
}

fn project_physical_object(
    revision: PhysicalRevision<'_>,
    object_type: &ObjectTypeDefinition,
) -> Result<CreateObject, PhysicalPlanError> {
    let fields = object_type
        .fields()
        .iter()
        .map(|field| project_physical_field(revision, object_type.id(), field))
        .collect::<Result<_, _>>()?;
    Ok(CreateObject {
        type_id: object_type.id(),
        fields,
    })
}

fn project_physical_field(
    revision: PhysicalRevision<'_>,
    object_type: TypeId,
    field: &FieldDefinition,
) -> Result<CreateField, PhysicalPlanError> {
    if field.default_expression().is_some() && !field.unique() {
        return Err(PhysicalPlanError::UnsupportedFieldDefault {
            object_type,
            field: field.id(),
        });
    }
    let resolved_type = field.resolved_type();
    let legacy_scalar = resolved_type.legacy_scalar();
    let named_type = resolved_type.named_type();
    let reference_target = resolved_type.reference_target();
    let value_type = resolved_type.value_type();

    let projected = (|| {
        if let Some(scalar) = legacy_scalar {
            if field.unique() && revision.standard_catalogue().is_some() {
                Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type,
                    field: field.id(),
                })
            } else {
                Ok(PhysicalFieldType::Scalar(scalar))
            }
        } else if let Some(named_type) = named_type {
            if revision.catalogue().enum_type_by_id(named_type).is_some() {
                Ok(PhysicalFieldType::Enum(named_type))
            } else if revision
                .catalogue()
                .record_value_type_by_id(named_type)
                .is_some()
            {
                if field.nullable() {
                    return Err(PhysicalPlanError::UnsupportedNullableRecordField {
                        object_type,
                        field: field.id(),
                    });
                }
                Ok(PhysicalFieldType::Record(named_type))
            } else {
                Err(PhysicalPlanError::UnsupportedNamedFieldType {
                    object_type,
                    field: field.id(),
                })
            }
        } else if let Some(target) = reference_target {
            if revision.catalogue().object_type_by_id(target).is_none() {
                return Err(PhysicalPlanError::UnknownReferenceTarget {
                    object_type,
                    field: field.id(),
                    target,
                });
            }
            Ok(PhysicalFieldType::Reference {
                target,
                on_delete: field.on_delete(),
            })
        } else if let Some(value_type) = value_type {
            project_value_type(revision, object_type, field.id(), value_type)
        } else {
            // Unknown resolved-type projections must fail closed.
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type,
                field: field.id(),
            })
        }
    })();
    let field_type = if field.unique() {
        // The unique-shape error remains authoritative for every closed type.
        // Projection errors stay exact for fields that do not request UNIQUE.
        match projected {
            Ok(field_type @ PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)) => {
                field_type
            }
            Ok(field_type @ PhysicalFieldType::Reference { .. }) if !field.nullable() => field_type,
            Ok(_) | Err(_) => {
                return Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type,
                    field: field.id(),
                });
            }
        }
    } else {
        projected?
    };

    if field_type == PhysicalFieldType::Scalar(StandardScalar::Void) {
        return Err(PhysicalPlanError::UnsupportedVoidField {
            object_type,
            field: field.id(),
        });
    }
    if field.default_expression().is_some() {
        return Err(PhysicalPlanError::UnsupportedFieldDefault {
            object_type,
            field: field.id(),
        });
    }
    if let PhysicalFieldType::Reference { .. } = field_type
        && field.on_delete() == Some(OnDeleteAction::SetNull)
        && !field.nullable()
    {
        return Err(PhysicalPlanError::InvalidDeleteAction {
            object_type,
            field: field.id(),
        });
    }
    if !matches!(field_type, PhysicalFieldType::Reference { .. }) && field.on_delete().is_some() {
        return Err(PhysicalPlanError::InvalidDeleteAction {
            object_type,
            field: field.id(),
        });
    }

    Ok(CreateField {
        field_id: field.id(),
        field_type,
        nullable: field.nullable(),
        unique: field.unique(),
    })
}

fn project_value_type(
    revision: PhysicalRevision<'_>,
    object_type: TypeId,
    field: FieldId,
    value_type: TypeId,
) -> Result<PhysicalFieldType, PhysicalPlanError> {
    let definition = revision
        .standard_catalogue()
        .and_then(|catalogue| catalogue.value_type_by_id(value_type));
    project_value_type_definition(definition, object_type, field, value_type)
}

fn project_value_type_definition(
    definition: Option<&ValueTypeDefinition>,
    object_type: TypeId,
    field: FieldId,
    value_type: TypeId,
) -> Result<PhysicalFieldType, PhysicalPlanError> {
    let Some(definition) = definition else {
        return Err(PhysicalPlanError::MissingValueTypeDefinition {
            object_type,
            field,
            value_type,
        });
    };
    let contract = definition.representation_contract();
    if definition.kind() != ValueTypeKind::Primitive
        || definition.mutability() != ValueTypeMutability::Immutable
    {
        return Err(PhysicalPlanError::UnsupportedValueTypeContract {
            object_type,
            field,
            value_type,
            contract: contract.to_owned(),
        });
    }
    let scalar = match contract {
        "orna.kernel.value.boolean@1" => StandardScalar::Boolean,
        "orna.kernel.value.integer@1" => StandardScalar::Integer,
        "orna.kernel.value.bigint@1" => StandardScalar::BigInt,
        "orna.kernel.value.float@1" => StandardScalar::Float,
        "orna.kernel.value.decimal@1" => StandardScalar::Decimal,
        "orna.kernel.value.character-large-object@1" => StandardScalar::CharacterLargeObject,
        "orna.kernel.value.binary-large-object@1" => StandardScalar::BinaryLargeObject,
        "orna.kernel.value.uuid@1" => StandardScalar::Uuid,
        "orna.kernel.value.date@1" => StandardScalar::Date,
        "orna.kernel.value.time@1" => StandardScalar::Time,
        "orna.kernel.value.timestamp@1" => StandardScalar::Timestamp,
        "orna.kernel.value.duration@1" => StandardScalar::Duration,
        "orna.kernel.value.void@1" => StandardScalar::Void,
        _ => {
            return Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type,
                field,
                value_type,
                contract: contract.to_owned(),
            });
        }
    };
    match definition.persistence() {
        ValueTypePersistence::Persistable => {}
        ValueTypePersistence::Transient if scalar != StandardScalar::Void => {
            return Err(PhysicalPlanError::TransientValueType {
                object_type,
                field,
                value_type,
            });
        }
        ValueTypePersistence::Transient => {}
    }
    Ok(PhysicalFieldType::Scalar(scalar))
}

#[cfg(test)]
mod tests;
