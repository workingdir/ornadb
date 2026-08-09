//! Backend-neutral planning for supported durable object storage changes.

use std::{error::Error, fmt};

use crate::{
    FieldId, TypeId,
    catalogue::{FieldDefinition, ObjectTypeDefinition, OnDeleteAction},
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    types::{ResolvedType, StandardScalar},
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

    for existing in active.catalogue().object_types() {
        let Some(revised) = candidate.candidate().object_type_by_id(existing.id()) else {
            return Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: existing.id(),
            });
        };
        if !same_storage_projection(existing, revised) {
            return Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: existing.id(),
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
        .map(|object_type| plan_new_object(object_type, candidate))
        .collect::<Result<_, _>>()?;

    Ok(PhysicalPlan { create_objects })
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

/// One new durable object relation.
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

fn same_storage_projection(
    existing: &ObjectTypeDefinition,
    revised: &ObjectTypeDefinition,
) -> bool {
    existing.id() == revised.id()
        && existing.fields().len() == revised.fields().len()
        && existing
            .fields()
            .iter()
            .zip(revised.fields())
            .all(|(existing, revised)| {
                existing.id() == revised.id()
                    && existing.ordinal() == revised.ordinal()
                    && existing.resolved_type() == revised.resolved_type()
                    && existing.nullable() == revised.nullable()
                    && existing.unique() == revised.unique()
                    && existing.default_expression() == revised.default_expression()
                    && existing.on_delete() == revised.on_delete()
            })
}

fn plan_new_object(
    object_type: &ObjectTypeDefinition,
    candidate: &DeployableRevision,
) -> Result<CreateObject, PhysicalPlanError> {
    let fields = object_type
        .fields()
        .iter()
        .map(|field| plan_new_field(object_type.id(), field, candidate))
        .collect::<Result<_, _>>()?;
    Ok(CreateObject {
        type_id: object_type.id(),
        fields,
    })
}

fn plan_new_field(
    object_type: TypeId,
    field: &FieldDefinition,
    candidate: &DeployableRevision,
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

    let field_type = match field.resolved_type() {
        ResolvedType::Scalar(StandardScalar::Void) => {
            return Err(PhysicalPlanError::UnsupportedVoidField {
                object_type,
                field: field.id(),
            });
        }
        ResolvedType::Scalar(scalar) => {
            if field.on_delete().is_some() {
                return Err(PhysicalPlanError::InvalidDeleteAction {
                    object_type,
                    field: field.id(),
                });
            }
            PhysicalFieldType::Scalar(scalar)
        }
        ResolvedType::Named(_) => {
            return Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type,
                field: field.id(),
            });
        }
        ResolvedType::Reference { target } => {
            if candidate.candidate().object_type_by_id(target).is_none() {
                return Err(PhysicalPlanError::UnknownReferenceTarget {
                    object_type,
                    field: field.id(),
                    target,
                });
            }
            if field.on_delete() == Some(OnDeleteAction::SetNull) && !field.nullable() {
                return Err(PhysicalPlanError::InvalidDeleteAction {
                    object_type,
                    field: field.id(),
                });
            }
            PhysicalFieldType::Reference {
                target,
                on_delete: field.on_delete(),
            }
        }
    };

    Ok(CreateField {
        field_id: field.id(),
        field_type,
        nullable: field.nullable(),
        unique: field.unique(),
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
        SourceUnitId, TypeId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            SchemaDefinition,
        },
        revision::{
            ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
            RevisionPair, Sha256Digest, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
        },
    };

    use super::*;

    const SCHEMA_ID: SchemaId = SchemaId::from_bytes([1; 16]);
    const FIRST_TYPE: TypeId = TypeId::from_bytes([10; 16]);
    const SECOND_TYPE: TypeId = TypeId::from_bytes([11; 16]);
    const FIRST_FIELD: FieldId = FieldId::from_bytes([20; 16]);
    const SECOND_FIELD: FieldId = FieldId::from_bytes([21; 16]);

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
        let active = active(vec![baseline.clone()], 1);
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
            let candidate = candidate(&active, vec![variant], 10 + index as u8);
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                })
            );
        }
    }

    #[test]
    fn storage_projection_compares_type_and_field_ordinals() {
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
        let different_type = object(SECOND_TYPE, "first", baseline.fields().to_vec());
        let different_ordinal = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "first",
                1,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            )],
        );

        assert!(!same_storage_projection(&baseline, &different_type));
        assert!(!same_storage_projection(&baseline, &different_ordinal));
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
