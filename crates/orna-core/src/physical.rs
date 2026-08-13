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

    for active_object in active.catalogue().object_types() {
        let candidate_object = candidate
            .candidate()
            .object_type_by_id(active_object.id())
            .ok_or(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: active_object.id(),
            })?;
        let active_projection = project_physical_object(active_revision, active_object)?;
        let candidate_projection = project_physical_object(candidate_revision, candidate_object)?;
        if active_projection != candidate_projection {
            return Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: active_object.id(),
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

    Ok(PhysicalPlan { create_objects })
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
}

impl PhysicalPlan {
    /// Returns new durable object relations in candidate catalogue order.
    pub fn create_objects(&self) -> &[CreateObject] {
        &self.create_objects
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

/// One field in a new durable object relation.
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
    /// The field requests uniqueness outside the required typed-reference shape.
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
            Self::UnsupportedUniqueField { .. } => {
                formatter.write_str("UNIQUE is supported only for required REF fields")
            }
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
    if field.unique() && !field.is_required_unique_reference() {
        return Err(PhysicalPlanError::UnsupportedUniqueField {
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

    let resolved_type = field.resolved_type();
    let legacy_scalar = resolved_type.legacy_scalar();
    let named_type = resolved_type.named_type();
    let reference_target = resolved_type.reference_target();
    let value_type = resolved_type.value_type();

    let field_type = if let Some(scalar) = legacy_scalar {
        PhysicalFieldType::Scalar(scalar)
    } else if let Some(named_type) = named_type {
        if revision.catalogue().enum_type_by_id(named_type).is_some() {
            PhysicalFieldType::Enum(named_type)
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
            PhysicalFieldType::Record(named_type)
        } else {
            return Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type,
                field: field.id(),
            });
        }
    } else if let Some(target) = reference_target {
        if revision.catalogue().object_type_by_id(target).is_none() {
            return Err(PhysicalPlanError::UnknownReferenceTarget {
                object_type,
                field: field.id(),
                target,
            });
        }
        PhysicalFieldType::Reference {
            target,
            on_delete: field.on_delete(),
        }
    } else if let Some(value_type) = value_type {
        project_value_type(revision, object_type, field.id(), value_type)?
    } else {
        // Unknown resolved-type projections must fail closed.
        return Err(PhysicalPlanError::UnsupportedNamedFieldType {
            object_type,
            field: field.id(),
        });
    };

    if field_type == PhysicalFieldType::Scalar(StandardScalar::Void) {
        return Err(PhysicalPlanError::UnsupportedVoidField {
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
mod tests {
    use crate::canonical_hash::{
        calculate_standard_library_digest_for_test, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot,
    };
    use crate::{
        CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
        SourceUnitId, StandardLibraryRevisionId, TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, ObjectTypeDefinition,
            QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
            SchemaDefinition, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
            DeployableRevisionContent, DeployableRevisionInput, RevisionPair, Sha256Digest,
            SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
            StoredSourceRevision, StoredSourceUnit,
        },
        types::ResolvedType,
    };

    use super::*;

    const SCHEMA_ID: SchemaId = SchemaId::from_bytes([1; 16]);
    const FIRST_TYPE: TypeId = TypeId::from_bytes([10; 16]);
    const SECOND_TYPE: TypeId = TypeId::from_bytes([11; 16]);
    const FIRST_FIELD: FieldId = FieldId::from_bytes([20; 16]);
    const SECOND_FIELD: FieldId = FieldId::from_bytes([21; 16]);
    const STANDARD_SCHEMA_ID: SchemaId = SchemaId::from_bytes([30; 16]);
    const STANDARD_TYPES_SCHEMA_ID: SchemaId = SchemaId::from_bytes([31; 16]);

    #[test]
    fn exact_existing_objects_need_no_physical_change() {
        let object = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            )],
        );
        let active = active(vec![object.clone()], 1);
        let candidate = candidate(&active, vec![object], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate).unwrap(),
            PhysicalPlan {
                create_objects: Vec::new(),
            }
        );
    }

    #[test]
    fn projects_a_verified_value_contract_from_a_candidate() {
        let value_type = TypeId::from_bytes([0xa1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            value_type,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let candidate = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard,
            2,
        );

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Boolean),
                    nullable: false,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn projects_catalogue_enums_as_named_physical_fields() {
        let enum_type = TypeId::from_bytes([0xa2; 16]);
        let standard = verified_standard(Vec::new());
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let source = source(2, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([3; 16]),
            vec![schema()],
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stage",
                    0,
                    ResolvedType::named(enum_type),
                    false,
                )],
            )],
            vec![],
            vec![EnumTypeDefinition::new(
                enum_type,
                name(&["demo", "stage"]),
                ["lead", "qualified"],
            )],
            vec![],
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let candidate = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(2),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap();

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Enum(enum_type),
                    nullable: false,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn projects_record_values_as_canonical_byte_fields() {
        let record_type = TypeId::from_bytes([0xa3; 16]);
        let standard = verified_standard(vec![standard_value_type(
            TypeId::from_bytes([0xa4; 16]),
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let source = source(2, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
            CatalogueRevisionId::from_bytes([3; 16]),
            vec![schema()],
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "status",
                    0,
                    ResolvedType::named(record_type),
                    false,
                )],
            )],
            vec![],
            vec![],
            vec![RecordValueTypeDefinition::new(
                record_type,
                name(&["demo", "status"]),
                vec![
                    RecordValueFieldDefinition::try_new(
                        SECOND_FIELD,
                        "active",
                        0,
                        ResolvedType::value(TypeId::from_bytes([0xa4; 16])),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
            vec![],
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let candidate = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(2),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap();

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects()[0]
                .fields()[0]
                .field_type(),
            PhysicalFieldType::Record(record_type)
        );
        let nullable = field(
            FieldId::from_bytes([22; 16]),
            "optional_status",
            0,
            ResolvedType::named(record_type),
            true,
        );
        assert_eq!(
            project_physical_field(
                PhysicalRevision::Deployable(&candidate),
                FIRST_TYPE,
                &nullable,
            ),
            Err(PhysicalPlanError::UnsupportedNullableRecordField {
                object_type: FIRST_TYPE,
                field: nullable.id(),
            })
        );
    }

    #[test]
    fn projects_every_pinned_kernel_value_contract() {
        let contracts = [
            (0xb0, "orna.kernel.value.boolean@1", StandardScalar::Boolean),
            (0xb1, "orna.kernel.value.integer@1", StandardScalar::Integer),
            (0xb2, "orna.kernel.value.bigint@1", StandardScalar::BigInt),
            (0xb3, "orna.kernel.value.float@1", StandardScalar::Float),
            (0xb4, "orna.kernel.value.decimal@1", StandardScalar::Decimal),
            (
                0xb5,
                "orna.kernel.value.character-large-object@1",
                StandardScalar::CharacterLargeObject,
            ),
            (
                0xb6,
                "orna.kernel.value.binary-large-object@1",
                StandardScalar::BinaryLargeObject,
            ),
            (0xb7, "orna.kernel.value.uuid@1", StandardScalar::Uuid),
            (0xb8, "orna.kernel.value.date@1", StandardScalar::Date),
            (0xb9, "orna.kernel.value.time@1", StandardScalar::Time),
            (
                0xba,
                "orna.kernel.value.timestamp@1",
                StandardScalar::Timestamp,
            ),
            (
                0xbb,
                "orna.kernel.value.duration@1",
                StandardScalar::Duration,
            ),
            (0xbc, "orna.kernel.value.void@1", StandardScalar::Void),
        ];
        let standard = verified_standard(
            contracts
                .iter()
                .map(|(id, contract, _)| {
                    standard_value_type(
                        TypeId::from_bytes([*id; 16]),
                        contract,
                        ValueTypePersistence::Persistable,
                    )
                })
                .collect(),
        );
        let active = active_version_two(Vec::new(), standard.clone(), 1);

        for (index, (id, _, scalar)) in contracts.into_iter().enumerate() {
            let value_type = TypeId::from_bytes([id; 16]);
            let object_type = TypeId::from_bytes([id.wrapping_add(0x20); 16]);
            let candidate = candidate_version_two(
                &active,
                vec![object(
                    object_type,
                    "value_holder",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Value(value_type),
                        false,
                    )],
                )],
                standard.clone(),
                u8::try_from(index + 2).unwrap(),
            );

            let expected = if scalar == StandardScalar::Void {
                Err(PhysicalPlanError::UnsupportedVoidField {
                    object_type,
                    field: FIRST_FIELD,
                })
            } else {
                Ok(PhysicalPlan {
                    create_objects: vec![CreateObject {
                        type_id: object_type,
                        fields: vec![CreateField {
                            field_id: FIRST_FIELD,
                            field_type: PhysicalFieldType::Scalar(scalar),
                            nullable: false,
                            unique: false,
                        }],
                    }],
                })
            };
            assert_eq!(plan_physical_changes(&active, &candidate), expected);
        }
    }

    #[test]
    fn active_value_catalogue_and_legacy_scalar_candidate_storage_are_equal() {
        let value_type = TypeId::from_bytes([0xc1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            value_type,
            "orna.kernel.value.integer@1",
            ValueTypePersistence::Persistable,
        )]);
        let value_active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );

        assert_eq!(
            active_physical_catalogue(&value_active).unwrap().objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: false,
                    unique: false,
                }],
            }]
        );

        let legacy_active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                )],
            )],
            3,
        );
        let value_candidate = candidate_version_two(
            &legacy_active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard,
            4,
        );

        assert_eq!(
            plan_physical_changes(&legacy_active, &value_candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
            })
        );
    }

    #[test]
    fn active_physical_catalogue_rejects_hostile_fields_through_the_shared_projector() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Named(TypeId::from_bytes([0xcf; 16])),
                    false,
                )],
            )],
            1,
        );

        assert_eq!(
            active_physical_catalogue(&active),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn value_contract_errors_retain_identity_contract_and_no_source() {
        let value_type = TypeId::from_bytes([0xc2; 16]);
        let missing =
            project_value_type_definition(None, FIRST_TYPE, FIRST_FIELD, value_type).unwrap_err();
        assert_eq!(
            missing,
            PhysicalPlanError::MissingValueTypeDefinition {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type,
            }
        );
        assert_eq!(
            missing.to_string(),
            "physical value type is absent from the pinned standard library"
        );
        assert!(std::error::Error::source(&missing).is_none());

        let unsupported = project_value_type_definition(
            Some(&standard_value_type(
                value_type,
                "orna.kernel.value.custom@1",
                ValueTypePersistence::Persistable,
            )),
            FIRST_TYPE,
            FIRST_FIELD,
            value_type,
        )
        .unwrap_err();
        assert_eq!(
            unsupported,
            PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type,
                contract: "orna.kernel.value.custom@1".to_owned(),
            }
        );
        assert_eq!(
            unsupported.to_string(),
            "physical value type contract is not supported"
        );
        assert!(std::error::Error::source(&unsupported).is_none());
    }

    #[test]
    fn value_field_gates_preserve_transient_void_and_delete_precedence() {
        let transient_boolean = TypeId::from_bytes([0xc3; 16]);
        let transient_void = TypeId::from_bytes([0xc4; 16]);
        let unknown_contract = TypeId::from_bytes([0xc5; 16]);
        let persistable_boolean = TypeId::from_bytes([0xc6; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                transient_boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Transient,
            ),
            standard_value_type(
                transient_void,
                "orna.kernel.value.void@1",
                ValueTypePersistence::Transient,
            ),
            standard_value_type(
                unknown_contract,
                "orna.kernel.value.custom@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                persistable_boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Persistable,
            ),
        ]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);

        let transient = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(transient_boolean),
                    false,
                )],
            )],
            standard.clone(),
            2,
        );
        let transient_error = plan_physical_changes(&active, &transient).unwrap_err();
        assert_eq!(
            transient_error,
            PhysicalPlanError::TransientValueType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: transient_boolean,
            }
        );
        assert_eq!(
            transient_error.to_string(),
            "transient value types cannot be stored"
        );
        assert!(std::error::Error::source(&transient_error).is_none());

        let void = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(transient_void),
                    false,
                )],
            )],
            standard.clone(),
            3,
        );
        assert_eq!(
            plan_physical_changes(&active, &void),
            Err(PhysicalPlanError::UnsupportedVoidField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let unique_before_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                    true,
                    None,
                    None,
                )],
            )],
            standard.clone(),
            4,
        );
        assert_eq!(
            plan_physical_changes(&active, &unique_before_contract),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let default_before_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                    false,
                    Some(ExpressionId::from_bytes([0xc7; 16])),
                    None,
                )],
            )],
            standard.clone(),
            5,
        );
        assert_eq!(
            plan_physical_changes(&active, &default_before_contract),
            Err(PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let unsupported_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                )],
            )],
            standard.clone(),
            6,
        );
        assert_eq!(
            plan_physical_changes(&active, &unsupported_contract),
            Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: unknown_contract,
                contract: "orna.kernel.value.custom@1".to_owned(),
            })
        );

        let active_unsupported_contract = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                )],
            )],
            standard.clone(),
            7,
        );
        assert_eq!(
            active_physical_catalogue(&active_unsupported_contract),
            Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: unknown_contract,
                contract: "orna.kernel.value.custom@1".to_owned(),
            })
        );

        let delete_after_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(persistable_boolean),
                    false,
                    false,
                    None,
                    Some(OnDeleteAction::Restrict),
                )],
            )],
            standard,
            8,
        );
        assert_eq!(
            plan_physical_changes(&active, &delete_after_contract),
            Err(PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn planner_checks_base_drops_existing_projections_and_new_objects_in_order() {
        let existing = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )],
        );
        let physical_active = active(vec![existing.clone()], 1);
        let stale_candidate = candidate(&physical_active, Vec::new(), 2);
        let other_active = active(Vec::new(), 3);
        assert!(matches!(
            plan_physical_changes(&other_active, &stale_candidate),
            Err(PhysicalPlanError::ExpectedBaseMismatch { .. })
        ));

        let invalid_existing = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::Named(TypeId::from_bytes([0xc8; 16])),
                false,
            )],
        );
        let second = object(SECOND_TYPE, "second", Vec::new());
        let invalid_active = active(vec![invalid_existing.clone(), second], 4);
        assert_eq!(
            active_physical_catalogue(&invalid_active),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
        let dropped_before_projection = candidate(
            &invalid_active,
            vec![
                invalid_existing.clone(),
                object(
                    TypeId::from_bytes([0xc9; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xca; 16])),
                        false,
                    )],
                ),
            ],
            5,
        );
        assert_eq!(
            plan_physical_changes(&invalid_active, &dropped_before_projection),
            Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: SECOND_TYPE,
            })
        );

        let invalid_survivor = candidate(
            &physical_active,
            vec![
                invalid_existing,
                object(
                    TypeId::from_bytes([0xcb; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xcc; 16])),
                        false,
                    )],
                ),
            ],
            6,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &invalid_survivor),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let changed_before_new = candidate(
            &physical_active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::scalar(StandardScalar::BigInt),
                        false,
                    )],
                ),
                object(
                    TypeId::from_bytes([0xcd; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xce; 16])),
                        false,
                    )],
                ),
            ],
            7,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &changed_before_new),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );

        let invalid_new_after_equal = candidate(
            &physical_active,
            vec![
                existing,
                object(
                    TypeId::from_bytes([0xcf; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xd0; 16])),
                        false,
                    )],
                ),
            ],
            8,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &invalid_new_after_equal),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: TypeId::from_bytes([0xcf; 16]),
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn plans_mutual_references_in_candidate_and_field_order() {
        let first = object(
            FIRST_TYPE,
            "first",
            vec![reference_field(
                FIRST_FIELD,
                "second",
                0,
                SECOND_TYPE,
                false,
                Some(OnDeleteAction::Restrict),
            )],
        );
        let second = object(
            SECOND_TYPE,
            "second",
            vec![reference_field(
                SECOND_FIELD,
                "first",
                0,
                FIRST_TYPE,
                true,
                Some(OnDeleteAction::SetNull),
            )],
        );
        let active = active(Vec::new(), 1);
        let candidate = candidate(&active, vec![second, first], 2);

        let plan = plan_physical_changes(&active, &candidate).unwrap();

        assert_eq!(
            plan.create_objects()
                .iter()
                .map(CreateObject::type_id)
                .collect::<Vec<_>>(),
            vec![SECOND_TYPE, FIRST_TYPE]
        );
        assert_eq!(
            plan.create_objects()[0].fields(),
            [CreateField {
                field_id: SECOND_FIELD,
                field_type: PhysicalFieldType::Reference {
                    target: FIRST_TYPE,
                    on_delete: Some(OnDeleteAction::SetNull),
                },
                nullable: true,
                unique: false,
            }]
        );
        assert_eq!(
            plan.create_objects()[1].fields()[0].field_type(),
            PhysicalFieldType::Reference {
                target: SECOND_TYPE,
                on_delete: Some(OnDeleteAction::Restrict),
            }
        );
    }

    #[test]
    fn plans_required_unique_references_against_the_complete_candidate() {
        let owner = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Restrict),
            )],
        );
        let target = object(
            SECOND_TYPE,
            "target",
            vec![reference_field(
                SECOND_FIELD,
                "owner",
                0,
                FIRST_TYPE,
                true,
                None,
            )],
        );
        let active = active(Vec::new(), 1);
        let candidate = candidate(&active, vec![owner, target], 2);

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let unique = &plan.create_objects()[0].fields()[0];
        assert_eq!(unique.field_id(), FIRST_FIELD);
        assert_eq!(
            unique.field_type(),
            PhysicalFieldType::Reference {
                target: SECOND_TYPE,
                on_delete: Some(OnDeleteAction::Restrict),
            }
        );
        assert!(!unique.nullable());
        assert!(unique.unique());
    }

    #[test]
    fn preserves_supported_scalar_field_order_types_and_nullability() {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "count",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        false,
                    ),
                    field(
                        SECOND_FIELD,
                        "title",
                        1,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        true,
                    ),
                ],
            )],
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let fields = plan.create_objects()[0].fields();

        assert_eq!(
            fields,
            [
                CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: false,
                    unique: false,
                },
                CreateField {
                    field_id: SECOND_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject,),
                    nullable: true,
                    unique: false,
                },
            ]
        );
        assert_eq!(fields[0].field_id(), FIRST_FIELD);
        assert_eq!(
            fields[1].field_type(),
            PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)
        );
        assert!(!fields[0].nullable());
        assert!(fields[1].nullable());
    }

    #[test]
    fn rejects_stale_bases_and_object_drops() {
        let object = object(FIRST_TYPE, "first", Vec::new());
        let active_revision = active(vec![object.clone()], 1);
        let deployable = candidate(&active_revision, vec![object], 2);
        let other_active = active(Vec::new(), 3);

        assert!(matches!(
            plan_physical_changes(&other_active, &deployable),
            Err(PhysicalPlanError::ExpectedBaseMismatch { .. })
        ));

        let dropped = candidate(&active_revision, Vec::new(), 4);
        assert_eq!(
            plan_physical_changes(&active_revision, &dropped),
            Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: FIRST_TYPE,
            })
        );
    }

    #[test]
    fn semantic_names_do_not_change_existing_object_storage() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![
                field(
                    FIRST_FIELD,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                field(
                    SECOND_FIELD,
                    "second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
            ],
        );
        let active = active(vec![baseline.clone()], 1);
        let renamed = object(
            FIRST_TYPE,
            "renamed",
            vec![
                field(
                    FIRST_FIELD,
                    "renamed_first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                field(
                    SECOND_FIELD,
                    "renamed_second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
            ],
        );
        let candidate = candidate(&active, vec![renamed], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
            })
        );
    }

    #[test]
    fn semantic_field_names_do_not_change_required_unique_reference_storage() {
        let target = object(SECOND_TYPE, "target", Vec::new());
        let baseline = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Cascade),
            )],
        );
        let active = active(vec![baseline, target.clone()], 1);
        let renamed = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "renamed_target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Cascade),
            )],
        );
        let candidate = candidate(&active, vec![renamed, target], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
            })
        );
    }

    #[test]
    fn rejects_adding_or_removing_existing_reference_uniqueness() {
        let target = object(SECOND_TYPE, "target", Vec::new());
        let owner = |unique| {
            object(
                FIRST_TYPE,
                "owner",
                vec![field_with_options(
                    FIRST_FIELD,
                    "target",
                    0,
                    ResolvedType::reference(SECOND_TYPE),
                    false,
                    unique,
                    None,
                    Some(OnDeleteAction::Restrict),
                )],
            )
        };

        let unique_active = active(vec![owner(true), target.clone()], 1);
        let remove_unique = candidate(&unique_active, vec![owner(false), target.clone()], 2);
        assert_eq!(
            plan_physical_changes(&unique_active, &remove_unique),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );

        let plain_active = active(vec![owner(false), target.clone()], 3);
        let add_unique = candidate(&plain_active, vec![owner(true), target], 4);
        assert_eq!(
            plan_physical_changes(&plain_active, &add_unique),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );
    }

    #[test]
    fn rejects_every_existing_object_storage_change_category() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![
                field(
                    FIRST_FIELD,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                reference_field(
                    SECOND_FIELD,
                    "second",
                    1,
                    SECOND_TYPE,
                    true,
                    Some(OnDeleteAction::Restrict),
                ),
            ],
        );
        let target = object(SECOND_TYPE, "target", Vec::new());
        let active = active(vec![baseline.clone(), target.clone()], 1);
        let variants = vec![
            object(FIRST_TYPE, "first", vec![baseline.fields()[0].clone()]),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    baseline.fields()[0].clone(),
                    baseline.fields()[1].clone(),
                    field(
                        FieldId::from_bytes([22; 16]),
                        "third",
                        2,
                        ResolvedType::reference(SECOND_TYPE),
                        true,
                    ),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FieldId::from_bytes([22; 16]),
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::BigInt),
                        true,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        false,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    reference_field(
                        SECOND_FIELD,
                        "second",
                        1,
                        SECOND_TYPE,
                        true,
                        Some(OnDeleteAction::Cascade),
                    ),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field_with_options(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        true,
                        None,
                        None,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field_with_options(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        false,
                        Some(ExpressionId::from_bytes([30; 16])),
                        None,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        SECOND_FIELD,
                        "second",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    reference_field(
                        FIRST_FIELD,
                        "first",
                        1,
                        SECOND_TYPE,
                        true,
                        Some(OnDeleteAction::Restrict),
                    ),
                ],
            ),
        ];

        for (index, variant) in variants.into_iter().enumerate() {
            let candidate = candidate(&active, vec![variant, target.clone()], 10 + index as u8);
            let expected = match index {
                6 => PhysicalPlanError::UnsupportedUniqueField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
                7 => PhysicalPlanError::UnsupportedFieldDefault {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
                _ => PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                },
            };
            assert_eq!(plan_physical_changes(&active, &candidate), Err(expected));
        }
    }

    #[test]
    fn active_physical_catalogue_preserves_object_and_field_order() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "first",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            )],
        );
        let active = active(vec![baseline], 1);

        assert_eq!(
            active_physical_catalogue(&active).unwrap().objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: true,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn rejects_unsupported_new_field_semantics_fail_closed() {
        let active = active(Vec::new(), 1);
        let cases = [
            (
                field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Named(SECOND_TYPE),
                    true,
                ),
                PhysicalPlanError::UnsupportedNamedFieldType {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
            ),
            (
                field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Void),
                    true,
                ),
                PhysicalPlanError::UnsupportedVoidField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
            ),
        ];
        for (index, (field, expected)) in cases.into_iter().enumerate() {
            let candidate = candidate(
                &active,
                vec![object(FIRST_TYPE, "first", vec![field])],
                20 + index as u8,
            );
            assert_eq!(plan_physical_changes(&active, &candidate), Err(expected));
        }

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                true,
                None,
                None,
            ),
            PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        let target = object(SECOND_TYPE, "second", Vec::new());
        let nullable_unique_reference = field_with_options(
            FIRST_FIELD,
            "second",
            0,
            ResolvedType::reference(SECOND_TYPE),
            true,
            true,
            None,
            None,
        );
        let nullable_candidate = candidate(
            &active,
            vec![
                object(FIRST_TYPE, "first", vec![nullable_unique_reference]),
                target.clone(),
            ],
            29,
        );
        assert_eq!(
            plan_physical_changes(&active, &nullable_candidate),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                false,
                Some(ExpressionId::from_bytes([31; 16])),
                None,
            ),
            PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                false,
                None,
                Some(OnDeleteAction::Cascade),
            ),
            PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        assert_new_field_error(
            &active,
            reference_field(FIRST_FIELD, "missing", 0, SECOND_TYPE, true, None),
            PhysicalPlanError::UnknownReferenceTarget {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                target: SECOND_TYPE,
            },
        );

        let invalid_set_null = reference_field(
            FIRST_FIELD,
            "second",
            0,
            SECOND_TYPE,
            false,
            Some(OnDeleteAction::SetNull),
        );
        let candidate = candidate(
            &active,
            vec![object(FIRST_TYPE, "first", vec![invalid_set_null]), target],
            30,
        );
        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    fn assert_new_field_error(
        active: &ActiveDatabaseRevision,
        field: FieldDefinition,
        expected: PhysicalPlanError,
    ) {
        let candidate = candidate(active, vec![object(FIRST_TYPE, "first", vec![field])], 40);
        assert_eq!(plan_physical_changes(active, &candidate), Err(expected));
    }

    fn active_version_two(
        objects: Vec<ObjectTypeDefinition>,
        standard: crate::revision::VerifiedStandardLibrarySnapshot,
        seed: u8,
    ) -> ActiveDatabaseRevision {
        let source = source(seed, None);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                catalogue,
                digest(seed),
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap()
    }

    fn candidate_version_two(
        active: &ActiveDatabaseRevision,
        objects: Vec<ObjectTypeDefinition>,
        standard: crate::revision::VerifiedStandardLibrarySnapshot,
        seed: u8,
    ) -> DeployableRevision {
        let source = source(seed, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(seed),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap()
    }

    fn verified_standard(
        value_types: Vec<ValueTypeDefinition>,
    ) -> crate::revision::VerifiedStandardLibrarySnapshot {
        let source = standard_source();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xa2; 16]),
            vec![
                SchemaDefinition::new(STANDARD_SCHEMA_ID, name(&["std"])),
                SchemaDefinition::new(STANDARD_TYPES_SCHEMA_ID, name(&["std", "types"])),
            ],
            Vec::new(),
            value_types,
            Vec::new(),
        )
        .unwrap();
        let origins = standard_origins(&source, &catalogue);
        let provisional = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0xa3; 16]),
            StandardLibraryDigestVersion::Version1,
            source.clone(),
            "orna.language/1",
            catalogue.clone(),
            origins.clone(),
            digest(0xa4),
        )
        .unwrap();
        let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
        verify_standard_library_snapshot(
            StandardLibrarySnapshot::new(
                provisional.revision(),
                provisional.digest_version(),
                source,
                provisional.language_version(),
                catalogue,
                origins,
                digest,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn standard_value_type(
        id: TypeId,
        contract: &str,
        persistence: ValueTypePersistence,
    ) -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            id,
            QualifiedSemanticName::new(vec![
                "std".to_owned(),
                "types".to_owned(),
                format!("value_{:02x}", id.to_bytes()[0]),
            ])
            .unwrap(),
            ValueTypeMutability::Immutable,
            persistence,
            contract,
        )
    }

    fn standard_source() -> StoredSourceRevision {
        let bundle = SourceBundleId::from_bytes([0xa5; 16]);
        let revision = SourceRevisionId::from_bytes([0xa6; 16]);
        let content = "CREATE SCHEMA std;";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xa7; 16]),
            0,
            "std/types.orna",
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let revision_hash = source_revision_record_digest(bundle, None, bundle_hash).unwrap();
        StoredSourceRevision::new(
            bundle,
            revision,
            None,
            vec![unit],
            bundle_hash,
            revision_hash,
        )
        .unwrap()
    }

    fn standard_origins(
        source: &StoredSourceRevision,
        catalogue: &CatalogueSnapshot,
    ) -> Vec<DefinitionOrigin> {
        let origin = SourceOrigin::new(
            source.units()[0].id(),
            0,
            u32::try_from(source.units()[0].content().len()).unwrap(),
        )
        .unwrap();
        catalogue
            .schemas()
            .iter()
            .map(|schema| DefinitionIdentity::Schema(schema.id()))
            .chain(
                catalogue
                    .value_types()
                    .iter()
                    .map(|value_type| DefinitionIdentity::ValueType(value_type.id())),
            )
            .map(|identity| DefinitionOrigin::new(identity, origin))
            .collect()
    }

    fn active(objects: Vec<ObjectTypeDefinition>, seed: u8) -> ActiveDatabaseRevision {
        let source = source(seed, None);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest(seed),
            Vec::new(),
            Vec::new(),
            origins,
            Vec::new(),
        )
        .unwrap()
    }

    fn candidate(
        active: &ActiveDatabaseRevision,
        objects: Vec<ObjectTypeDefinition>,
        seed: u8,
    ) -> DeployableRevision {
        let source = source(seed, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        DeployableRevision::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            digest(seed),
            origins,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn source(seed: u8, parent: Option<SourceRevisionId>) -> StoredSourceRevision {
        let bundle = SourceBundleId::from_bytes([seed.wrapping_add(2); 16]);
        let revision = SourceRevisionId::from_bytes([seed.wrapping_add(3); 16]);
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([seed.wrapping_add(4); 16]),
            0,
            "physical.orna",
            "source",
            digest(seed),
        )
        .unwrap();
        StoredSourceRevision::new(
            bundle,
            revision,
            parent,
            vec![unit],
            digest(seed),
            digest(seed),
        )
        .unwrap()
    }

    fn origins(
        source: &StoredSourceRevision,
        catalogue: &CatalogueSnapshot,
    ) -> Vec<DefinitionOrigin> {
        let source_origin = SourceOrigin::new(source.units()[0].id(), 0, 6).unwrap();
        let mut values = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SCHEMA_ID),
            source_origin,
        )];
        for object_type in catalogue.object_types() {
            values.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object_type.id()),
                source_origin,
            ));
            values.extend(object_type.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object_type.id(),
                        field: field.id(),
                    },
                    source_origin,
                )
            }));
        }
        values.extend(catalogue.enum_types().iter().map(|enum_type| {
            DefinitionOrigin::new(DefinitionIdentity::ValueType(enum_type.id()), source_origin)
        }));
        for record_type in catalogue.record_value_types() {
            values.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record_type.id()),
                source_origin,
            ));
            values.extend(record_type.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: record_type.id(),
                        field: field.id(),
                    },
                    source_origin,
                )
            }));
        }
        values
    }

    fn schema() -> SchemaDefinition {
        SchemaDefinition::new(SCHEMA_ID, name(&["demo"]))
    }

    fn object(id: TypeId, object_name: &str, fields: Vec<FieldDefinition>) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(id, name(&["demo", object_name]), fields)
    }

    fn field(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> FieldDefinition {
        field_with_options(
            id,
            field_name,
            ordinal,
            resolved_type,
            nullable,
            false,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn field_with_options(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
        unique: bool,
        default_expression: Option<ExpressionId>,
        on_delete: Option<OnDeleteAction>,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            field_name,
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn reference_field(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        target: TypeId,
        nullable: bool,
        on_delete: Option<OnDeleteAction>,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            field_name,
            ordinal,
            ResolvedType::reference(target),
            nullable,
            false,
            None,
            on_delete,
        )
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    const fn digest(seed: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([seed; 32])
    }
}
