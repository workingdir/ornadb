//! Semantic catalogue diffs keyed by stable identity (work ADR 0066).
//!
//! [`catalogue_diff`] compares two immutable [`CatalogueSnapshot`]s and
//! reports the definitions added, dropped, and renamed between them. A
//! definition is the same definition — regardless of its display name — when
//! its stable identity matches. Comparison keys are always stable identities
//! (`SchemaId`, `TypeId`, `FunctionId`, `FieldId`, `ParameterId`), never name
//! strings, so a semantic rename keeps its dependent references and is
//! reported as a rename, not as a drop plus an add.
//!
//! The report is a closed, order-stable description of one catalogue
//! transition. It does not execute anything and does not depend on the
//! database; callers recover the two snapshots and render the report.

use crate::{
    FieldId, FunctionId, ParameterId, SchemaId, TypeId,
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, ObjectTypeDefinition,
        ParameterDefinition, RecordValueFieldDefinition, RecordValueTypeDefinition,
        ValueTypeDefinition,
    },
};

/// What happened to one catalogue definition between two catalogues.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticChange {
    /// A schema present in the candidate but absent from the base.
    SchemaAdded { id: SchemaId, name: String },
    /// A schema present in the base but absent from the candidate.
    SchemaDropped { id: SchemaId, name: String },
    /// A schema kept by identity but renamed.
    SchemaRenamed {
        id: SchemaId,
        from: String,
        to: String,
    },
    /// An object type present in the candidate but absent from the base.
    ObjectTypeAdded { id: TypeId, name: String },
    /// An object type present in the base but absent from the candidate.
    ObjectTypeDropped { id: TypeId, name: String },
    /// An object type kept by identity but renamed.
    ObjectTypeRenamed {
        id: TypeId,
        from: String,
        to: String,
    },
    /// A primitive or opaque value type present in the candidate but absent
    /// from the base.
    ValueTypeAdded { id: TypeId, name: String },
    /// A primitive or opaque value type present in the base but absent from
    /// the candidate.
    ValueTypeDropped { id: TypeId, name: String },
    /// A primitive or opaque value type kept by identity but renamed.
    ValueTypeRenamed {
        id: TypeId,
        from: String,
        to: String,
    },
    /// A retained value type changed its representation category.
    ValueTypeKindChanged { id: TypeId, name: String },
    /// A retained value type changed its mutability contract.
    ValueTypeMutabilityChanged { id: TypeId, name: String },
    /// A retained value type changed its persistence contract.
    ValueTypePersistenceChanged { id: TypeId, name: String },
    /// A retained value type changed its representation contract.
    ValueTypeRepresentationChanged { id: TypeId, name: String },
    /// A record value type present in the candidate but absent from the base.
    RecordValueTypeAdded { id: TypeId, name: String },
    /// A record value type present in the base but absent from the candidate.
    RecordValueTypeDropped { id: TypeId, name: String },
    /// A record value type kept by identity but renamed.
    RecordValueTypeRenamed {
        id: TypeId,
        from: String,
        to: String,
    },
    /// A field added to a retained object or record value type (by owner identity).
    FieldAdded {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A field dropped from a retained object or record value type (by owner identity).
    FieldDropped {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A field kept by identity but renamed inside a retained object or record value type.
    FieldRenamed {
        owner: TypeId,
        id: FieldId,
        from: String,
        to: String,
    },
    /// An enum type present in the candidate but absent from the base.
    EnumTypeAdded { id: TypeId, name: String },
    /// An enum type present in the base but absent from the candidate.
    EnumTypeDropped { id: TypeId, name: String },
    /// An enum type kept by identity but renamed.
    EnumTypeRenamed {
        id: TypeId,
        from: String,
        to: String,
    },
    /// A retained enum type changed its declared label set.
    EnumLabelsChanged { id: TypeId, name: String },
    /// A function present in the candidate but absent from the base.
    FunctionAdded { id: FunctionId, name: String },
    /// A function present in the base but absent from the candidate.
    FunctionDropped { id: FunctionId, name: String },
    /// A function kept by identity but renamed.
    FunctionRenamed {
        id: FunctionId,
        from: String,
        to: String,
    },
    /// A parameter added to a retained function (by owner identity).
    ParameterAdded {
        owner: FunctionId,
        id: ParameterId,
        name: String,
    },
    /// A parameter dropped from a retained function (by owner identity).
    ParameterDropped {
        owner: FunctionId,
        id: ParameterId,
        name: String,
    },
    /// A parameter kept by identity but renamed inside a retained function.
    ParameterRenamed {
        owner: FunctionId,
        id: ParameterId,
        from: String,
        to: String,
    },
    /// A retained function parameter changed its declaration ordinal.
    ParameterOrdinalChanged {
        owner: FunctionId,
        id: ParameterId,
        name: String,
        from: u32,
        to: u32,
    },
    /// A retained object or record value field changed its resolved type.
    FieldTypeChanged {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A retained object or record value field changed its declaration ordinal.
    FieldOrdinalChanged {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A retained object field changed its nullability.
    FieldNullabilityChanged {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A retained object field changed its uniqueness.
    FieldUniquenessChanged {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A retained object field changed its default expression or on-delete
    /// policy.
    FieldConstraintChanged {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A retained function changed its return type.
    FunctionReturnChanged { id: FunctionId, name: String },
    /// A retained function changed its execution domain.
    FunctionDomainChanged { id: FunctionId, name: String },
    /// A retained function changed its security mode.
    FunctionSecurityChanged { id: FunctionId, name: String },
    /// A retained function changed its transaction mode.
    FunctionTransactionChanged { id: FunctionId, name: String },
    /// A retained function changed its volatility.
    FunctionVolatilityChanged { id: FunctionId, name: String },
    /// A retained function parameter changed its resolved type.
    ParameterTypeChanged {
        owner: FunctionId,
        id: ParameterId,
        name: String,
    },
    /// A retained function parameter changed its default expression.
    ParameterDefaultChanged {
        owner: FunctionId,
        id: ParameterId,
        name: String,
    },
}

impl SemanticChange {
    /// Returns a stable one-line category label for the change kind.
    pub fn category(&self) -> &'static str {
        match self {
            Self::SchemaAdded { .. } | Self::SchemaRenamed { .. } => "schema",
            Self::SchemaDropped { .. } => "schema",
            Self::ObjectTypeAdded { .. }
            | Self::ObjectTypeRenamed { .. }
            | Self::ObjectTypeDropped { .. } => "object_type",
            Self::ValueTypeAdded { .. }
            | Self::ValueTypeRenamed { .. }
            | Self::ValueTypeDropped { .. }
            | Self::ValueTypeKindChanged { .. }
            | Self::ValueTypeMutabilityChanged { .. }
            | Self::ValueTypePersistenceChanged { .. }
            | Self::ValueTypeRepresentationChanged { .. } => "value_type",
            Self::RecordValueTypeAdded { .. }
            | Self::RecordValueTypeRenamed { .. }
            | Self::RecordValueTypeDropped { .. } => "record_value_type",
            Self::FieldAdded { .. }
            | Self::FieldRenamed { .. }
            | Self::FieldDropped { .. }
            | Self::FieldTypeChanged { .. }
            | Self::FieldOrdinalChanged { .. }
            | Self::FieldNullabilityChanged { .. }
            | Self::FieldUniquenessChanged { .. }
            | Self::FieldConstraintChanged { .. } => "field",
            Self::EnumTypeAdded { .. }
            | Self::EnumTypeRenamed { .. }
            | Self::EnumTypeDropped { .. }
            | Self::EnumLabelsChanged { .. } => "enum_type",
            Self::FunctionAdded { .. }
            | Self::FunctionRenamed { .. }
            | Self::FunctionDropped { .. }
            | Self::FunctionReturnChanged { .. }
            | Self::FunctionDomainChanged { .. }
            | Self::FunctionSecurityChanged { .. }
            | Self::FunctionTransactionChanged { .. }
            | Self::FunctionVolatilityChanged { .. } => "function",
            Self::ParameterAdded { .. }
            | Self::ParameterRenamed { .. }
            | Self::ParameterDropped { .. }
            | Self::ParameterTypeChanged { .. }
            | Self::ParameterDefaultChanged { .. }
            | Self::ParameterOrdinalChanged { .. } => "parameter",
        }
    }
}

/// The complete ordered set of semantic changes from one catalogue to another.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogueSemanticDiff {
    changes: Vec<SemanticChange>,
}

impl CatalogueSemanticDiff {
    /// Creates an empty diff.
    pub const fn new() -> Self {
        Self {
            changes: Vec::new(),
        }
    }

    /// Returns the closed change list in deterministic comparison order.
    pub fn changes(&self) -> &[SemanticChange] {
        &self.changes
    }

    /// Returns true when no semantic change was detected.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    fn push(&mut self, change: SemanticChange) {
        self.changes.push(change);
    }
}

/// Compares two immutable catalogue snapshots by stable identity (work ADR
/// 0066).
///
/// Definitions present in both snapshots under the same stable identity are
/// the same definition; a different resolved name is a rename, and dependent
/// references survive because they resolve by identity. Nested definitions
/// (object and record value fields, function parameters) are compared under
/// their retained owner identity whether or not the owner name changed; a
/// dropped owner
/// carries no nested changes.
pub fn catalogue_diff(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
) -> CatalogueSemanticDiff {
    let mut diff = CatalogueSemanticDiff::new();

    diff_schemas(base, candidate, &mut diff);
    diff_object_types(base, candidate, &mut diff);
    diff_value_types(base, candidate, &mut diff);
    diff_enum_types(base, candidate, &mut diff);
    diff_record_value_types(base, candidate, &mut diff);
    diff_functions(base, candidate, &mut diff);

    diff
}

fn diff_schemas(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for schema in candidate.schemas() {
        match base.schema_by_id(schema.id()) {
            Some(found) if found.name() == schema.name() => {}
            Some(found) => diff.push(SemanticChange::SchemaRenamed {
                id: schema.id(),
                from: qualified(found.name()),
                to: qualified(schema.name()),
            }),
            None => diff.push(SemanticChange::SchemaAdded {
                id: schema.id(),
                name: qualified(schema.name()),
            }),
        }
    }
    for schema in base.schemas() {
        if candidate.schema_by_id(schema.id()).is_none() {
            diff.push(SemanticChange::SchemaDropped {
                id: schema.id(),
                name: qualified(schema.name()),
            });
        }
    }
}

fn diff_object_types(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for definition in candidate.object_types() {
        match base.object_type_by_id(definition.id()) {
            Some(found) if found.name() == definition.name() => {
                diff_fields(found, definition, definition.id(), diff);
            }
            Some(found) => {
                diff.push(SemanticChange::ObjectTypeRenamed {
                    id: definition.id(),
                    from: qualified(found.name()),
                    to: qualified(definition.name()),
                });
                diff_fields(found, definition, definition.id(), diff);
            }
            None => diff.push(SemanticChange::ObjectTypeAdded {
                id: definition.id(),
                name: qualified(definition.name()),
            }),
        }
    }
    for definition in base.object_types() {
        if candidate.object_type_by_id(definition.id()).is_none() {
            diff.push(SemanticChange::ObjectTypeDropped {
                id: definition.id(),
                name: qualified(definition.name()),
            });
        }
    }
}

fn diff_value_types(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for definition in candidate.value_types() {
        match base.value_type_by_id(definition.id()) {
            Some(found) if found.name() == definition.name() => {
                diff_value_type_payload(found, definition, definition.id(), diff);
            }
            Some(found) => {
                diff.push(SemanticChange::ValueTypeRenamed {
                    id: definition.id(),
                    from: qualified(found.name()),
                    to: qualified(definition.name()),
                });
                diff_value_type_payload(found, definition, definition.id(), diff);
            }
            None => diff.push(SemanticChange::ValueTypeAdded {
                id: definition.id(),
                name: qualified(definition.name()),
            }),
        }
    }
    for definition in base.value_types() {
        if candidate.value_type_by_id(definition.id()).is_none() {
            diff.push(SemanticChange::ValueTypeDropped {
                id: definition.id(),
                name: qualified(definition.name()),
            });
        }
    }
}

fn diff_value_type_payload(
    base: &ValueTypeDefinition,
    candidate: &ValueTypeDefinition,
    id: TypeId,
    diff: &mut CatalogueSemanticDiff,
) {
    let name = qualified(candidate.name());
    if base.kind() != candidate.kind() {
        diff.push(SemanticChange::ValueTypeKindChanged {
            id,
            name: name.clone(),
        });
    }
    if base.mutability() != candidate.mutability() {
        diff.push(SemanticChange::ValueTypeMutabilityChanged {
            id,
            name: name.clone(),
        });
    }
    if base.persistence() != candidate.persistence() {
        diff.push(SemanticChange::ValueTypePersistenceChanged {
            id,
            name: name.clone(),
        });
    }
    if base.representation_contract() != candidate.representation_contract() {
        diff.push(SemanticChange::ValueTypeRepresentationChanged { id, name });
    }
}

fn diff_record_value_types(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for definition in candidate.record_value_types() {
        match base.record_value_type_by_id(definition.id()) {
            Some(found) if found.name() == definition.name() => {
                diff_record_value_fields(found, definition, definition.id(), diff);
            }
            Some(found) => {
                diff.push(SemanticChange::RecordValueTypeRenamed {
                    id: definition.id(),
                    from: qualified(found.name()),
                    to: qualified(definition.name()),
                });
                diff_record_value_fields(found, definition, definition.id(), diff);
            }
            None => diff.push(SemanticChange::RecordValueTypeAdded {
                id: definition.id(),
                name: qualified(definition.name()),
            }),
        }
    }
    for definition in base.record_value_types() {
        if candidate.record_value_type_by_id(definition.id()).is_none() {
            diff.push(SemanticChange::RecordValueTypeDropped {
                id: definition.id(),
                name: qualified(definition.name()),
            });
        }
    }
}

fn diff_record_value_fields(
    base: &RecordValueTypeDefinition,
    candidate: &RecordValueTypeDefinition,
    owner: TypeId,
    diff: &mut CatalogueSemanticDiff,
) {
    for field in candidate.fields() {
        match base.field_by_id(field.id()) {
            Some(found) if found.name() == field.name() => {
                diff_record_value_field_payload(found, field, owner, diff);
            }
            Some(found) => {
                diff.push(SemanticChange::FieldRenamed {
                    owner,
                    id: field.id(),
                    from: found.name().to_owned(),
                    to: field.name().to_owned(),
                });
                diff_record_value_field_payload(found, field, owner, diff);
            }
            None => diff.push(SemanticChange::FieldAdded {
                owner,
                id: field.id(),
                name: field.name().to_owned(),
            }),
        }
    }
    for field in base.fields() {
        if candidate.field_by_id(field.id()).is_none() {
            diff.push(SemanticChange::FieldDropped {
                owner,
                id: field.id(),
                name: field.name().to_owned(),
            });
        }
    }
}

fn diff_record_value_field_payload(
    base: &RecordValueFieldDefinition,
    candidate: &RecordValueFieldDefinition,
    owner: TypeId,
    diff: &mut CatalogueSemanticDiff,
) {
    let id = candidate.id();
    let name = candidate.name().to_owned();
    if base.descriptor() != candidate.descriptor() {
        diff.push(SemanticChange::FieldTypeChanged {
            owner,
            id,
            name: name.clone(),
        });
    }
    if base.ordinal() != candidate.ordinal() {
        diff.push(SemanticChange::FieldOrdinalChanged { owner, id, name });
    }
}

fn diff_fields(
    base: &ObjectTypeDefinition,
    candidate: &ObjectTypeDefinition,
    owner: TypeId,
    diff: &mut CatalogueSemanticDiff,
) {
    for field in candidate.fields() {
        match base.field_by_id(field.id()) {
            Some(found) if found.name() == field.name() => {
                diff_field_payload(found, field, owner, diff);
            }
            Some(found) => {
                diff.push(SemanticChange::FieldRenamed {
                    owner,
                    id: field.id(),
                    from: found.name().to_owned(),
                    to: field.name().to_owned(),
                });
                diff_field_payload(found, field, owner, diff);
            }
            None => diff.push(SemanticChange::FieldAdded {
                owner,
                id: field.id(),
                name: field.name().to_owned(),
            }),
        }
    }
    for field in base.fields() {
        if candidate.field_by_id(field.id()).is_none() {
            diff.push(SemanticChange::FieldDropped {
                owner,
                id: field.id(),
                name: field.name().to_owned(),
            });
        }
    }
}

fn diff_field_payload(
    base: &FieldDefinition,
    candidate: &FieldDefinition,
    owner: TypeId,
    diff: &mut CatalogueSemanticDiff,
) {
    let id = candidate.id();
    let name = candidate.name().to_owned();
    if base.resolved_type() != candidate.resolved_type() {
        diff.push(SemanticChange::FieldTypeChanged {
            owner,
            id,
            name: name.clone(),
        });
    }
    if base.ordinal() != candidate.ordinal() {
        diff.push(SemanticChange::FieldOrdinalChanged {
            owner,
            id,
            name: name.clone(),
        });
    }
    if base.nullable() != candidate.nullable() {
        diff.push(SemanticChange::FieldNullabilityChanged {
            owner,
            id,
            name: name.clone(),
        });
    }
    if base.unique() != candidate.unique() {
        diff.push(SemanticChange::FieldUniquenessChanged {
            owner,
            id,
            name: name.clone(),
        });
    }
    if base.default_expression() != candidate.default_expression()
        || base.on_delete() != candidate.on_delete()
    {
        diff.push(SemanticChange::FieldConstraintChanged { owner, id, name });
    }
}

fn diff_enum_types(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for definition in candidate.enum_types() {
        match base.enum_type_by_id(definition.id()) {
            Some(found) if found.name() == definition.name() => {
                if found.labels() != definition.labels() {
                    diff.push(SemanticChange::EnumLabelsChanged {
                        id: definition.id(),
                        name: qualified(definition.name()),
                    });
                }
            }
            Some(found) => {
                diff.push(SemanticChange::EnumTypeRenamed {
                    id: definition.id(),
                    from: qualified(found.name()),
                    to: qualified(definition.name()),
                });
                if found.labels() != definition.labels() {
                    diff.push(SemanticChange::EnumLabelsChanged {
                        id: definition.id(),
                        name: qualified(definition.name()),
                    });
                }
            }
            None => diff.push(SemanticChange::EnumTypeAdded {
                id: definition.id(),
                name: qualified(definition.name()),
            }),
        }
    }
    for definition in base.enum_types() {
        if candidate.enum_type_by_id(definition.id()).is_none() {
            diff.push(SemanticChange::EnumTypeDropped {
                id: definition.id(),
                name: qualified(definition.name()),
            });
        }
    }
}

fn diff_functions(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
    diff: &mut CatalogueSemanticDiff,
) {
    for definition in candidate.functions() {
        match base.function_by_id(definition.id()) {
            Some(found) if found.name() == definition.name() => {
                diff_parameters(found, definition, definition.id(), diff);
                diff_function_payload(found, definition, definition.id(), diff);
            }
            Some(found) => {
                diff.push(SemanticChange::FunctionRenamed {
                    id: definition.id(),
                    from: qualified(found.name()),
                    to: qualified(definition.name()),
                });
                diff_parameters(found, definition, definition.id(), diff);
                diff_function_payload(found, definition, definition.id(), diff);
            }
            None => diff.push(SemanticChange::FunctionAdded {
                id: definition.id(),
                name: qualified(definition.name()),
            }),
        }
    }
    for definition in base.functions() {
        if candidate.function_by_id(definition.id()).is_none() {
            diff.push(SemanticChange::FunctionDropped {
                id: definition.id(),
                name: qualified(definition.name()),
            });
        }
    }
}

fn diff_function_payload(
    base: &FunctionDefinition,
    candidate: &FunctionDefinition,
    id: FunctionId,
    diff: &mut CatalogueSemanticDiff,
) {
    let name = qualified(candidate.name());
    if base.return_type() != candidate.return_type() {
        diff.push(SemanticChange::FunctionReturnChanged {
            id,
            name: name.clone(),
        });
    }
    if base.domain() != candidate.domain() {
        diff.push(SemanticChange::FunctionDomainChanged {
            id,
            name: name.clone(),
        });
    }
    if base.security() != candidate.security() {
        diff.push(SemanticChange::FunctionSecurityChanged {
            id,
            name: name.clone(),
        });
    }
    if base.transaction() != candidate.transaction() {
        diff.push(SemanticChange::FunctionTransactionChanged {
            id,
            name: name.clone(),
        });
    }
    if base.volatility() != candidate.volatility() {
        diff.push(SemanticChange::FunctionVolatilityChanged { id, name });
    }
}

fn diff_parameters(
    base: &FunctionDefinition,
    candidate: &FunctionDefinition,
    owner: FunctionId,
    diff: &mut CatalogueSemanticDiff,
) {
    for parameter in candidate.parameters() {
        match base.parameter_by_id(parameter.id()) {
            Some(found) if found.name() == parameter.name() => {
                diff_parameter_payload(found, parameter, owner, diff);
            }
            Some(found) => {
                diff.push(SemanticChange::ParameterRenamed {
                    owner,
                    id: parameter.id(),
                    from: found.name().to_owned(),
                    to: parameter.name().to_owned(),
                });
                diff_parameter_payload(found, parameter, owner, diff);
            }
            None => diff.push(SemanticChange::ParameterAdded {
                owner,
                id: parameter.id(),
                name: parameter.name().to_owned(),
            }),
        }
    }
    for parameter in base.parameters() {
        if candidate.parameter_by_id(parameter.id()).is_none() {
            diff.push(SemanticChange::ParameterDropped {
                owner,
                id: parameter.id(),
                name: parameter.name().to_owned(),
            });
        }
    }
}

fn diff_parameter_payload(
    base: &ParameterDefinition,
    candidate: &ParameterDefinition,
    owner: FunctionId,
    diff: &mut CatalogueSemanticDiff,
) {
    if base.ordinal() != candidate.ordinal() {
        diff.push(SemanticChange::ParameterOrdinalChanged {
            owner,
            id: candidate.id(),
            name: candidate.name().to_owned(),
            from: base.ordinal(),
            to: candidate.ordinal(),
        });
    }
    if base.resolved_type() != candidate.resolved_type() {
        diff.push(SemanticChange::ParameterTypeChanged {
            owner,
            id: candidate.id(),
            name: candidate.name().to_owned(),
        });
    }
    if base.default_expression() != candidate.default_expression() {
        diff.push(SemanticChange::ParameterDefaultChanged {
            owner,
            id: candidate.id(),
            name: candidate.name().to_owned(),
        });
    }
}

fn qualified(name: &crate::catalogue::QualifiedSemanticName) -> String {
    name.parts().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{
        EnumTypeDefinition, FieldDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ParameterDefinition, QualifiedSemanticName,
        RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    };
    use crate::{
        CatalogueRevisionId, ExpressionId, FunctionRevisionId,
        types::{ResolvedType, StandardScalar, TypeDescriptor},
    };

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn schema(id: u8, name_parts: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(SchemaId::from_bytes([id; 16]), name(name_parts))
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

    fn parameter(id: u8, name: &str, ordinal: u32) -> ParameterDefinition {
        ParameterDefinition::new(
            ParameterId::from_bytes([id; 16]),
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )
    }

    fn function(
        id: u8,
        name_parts: &[&str],
        parameters: Vec<ParameterDefinition>,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([id; 16]),
            name(name_parts),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([id; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn value_type(
        id: u8,
        name_parts: &[&str],
        kind: crate::catalogue::ValueTypeKind,
        persistence: ValueTypePersistence,
        representation_contract: &str,
    ) -> ValueTypeDefinition {
        match kind {
            crate::catalogue::ValueTypeKind::Primitive => ValueTypeDefinition::primitive(
                TypeId::from_bytes([id; 16]),
                name(name_parts),
                ValueTypeMutability::Immutable,
                persistence,
                representation_contract,
            ),
            crate::catalogue::ValueTypeKind::Opaque => ValueTypeDefinition::opaque(
                TypeId::from_bytes([id; 16]),
                name(name_parts),
                representation_contract,
            ),
        }
    }

    fn record_field(id: u8, name: &str, ordinal: u32, target: u8) -> RecordValueFieldDefinition {
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([id; 16]),
            name,
            ordinal,
            TypeDescriptor::named(TypeId::from_bytes([target; 16])),
        )
        .unwrap()
    }

    fn record_value_type(
        id: u8,
        name_parts: &[&str],
        fields: Vec<RecordValueFieldDefinition>,
    ) -> RecordValueTypeDefinition {
        RecordValueTypeDefinition::new(TypeId::from_bytes([id; 16]), name(name_parts), fields)
    }

    fn enum_type(id: u8, name_parts: &[&str]) -> EnumTypeDefinition {
        EnumTypeDefinition::new(
            TypeId::from_bytes([id; 16]),
            name(name_parts),
            vec!["lead".to_owned()],
        )
    }

    fn snapshot(
        schemas: Vec<SchemaDefinition>,
        types: Vec<ObjectTypeDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([7; 16]), schemas, types).unwrap()
    }

    fn value_snapshot(
        schemas: Vec<SchemaDefinition>,
        values: Vec<ValueTypeDefinition>,
        records: Vec<RecordValueTypeDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions_and_record_value_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            schemas,
            Vec::new(),
            values,
            Vec::new(),
            records,
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn record_snapshot(
        schemas: Vec<SchemaDefinition>,
        enums: Vec<EnumTypeDefinition>,
        records: Vec<RecordValueTypeDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions_and_record_value_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            schemas,
            Vec::new(),
            Vec::new(),
            enums,
            records,
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn full_snapshot(
        schemas: Vec<SchemaDefinition>,
        types: Vec<ObjectTypeDefinition>,
        enums: Vec<EnumTypeDefinition>,
        functions: Vec<FunctionDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions_and_enum_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            schemas,
            types,
            Vec::new(),
            enums,
            Vec::new(),
            functions,
        )
        .unwrap()
    }

    #[test]
    fn identical_catalogues_produce_no_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![enum_type(4, &["app", "stage"])],
            vec![function(5, &["app", "read"], vec![])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![enum_type(4, &["app", "stage"])],
            vec![function(5, &["app", "read"], vec![])],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert!(
            diff.is_empty(),
            "identical catalogues must not differ: {:?}",
            diff
        );
    }

    #[test]
    fn rename_preserves_identity_and_reports_a_rename() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![],
            vec![function(5, &["app", "read"], vec![parameter(6, "p_q", 0)])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            // Same TypeId; the renamed owner retains its nested field.
            vec![object(2, &["app", "gadget"], vec![field(3, "name", 0)])],
            vec![],
            // Same FunctionId; renamed function retains its parameter.
            vec![function(5, &["app", "load"], vec![parameter(6, "p_q", 0)])],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::ObjectTypeRenamed {
                    id: TypeId::from_bytes([2; 16]),
                    from: "app.widget".to_owned(),
                    to: "app.gadget".to_owned(),
                },
                SemanticChange::FunctionRenamed {
                    id: FunctionId::from_bytes([5; 16]),
                    from: "app.read".to_owned(),
                    to: "app.load".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn renamed_object_type_reports_nested_field_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "gadget"], vec![field(3, "label", 0)])],
            vec![],
            vec![],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::ObjectTypeRenamed {
                    id: TypeId::from_bytes([2; 16]),
                    from: "app.widget".to_owned(),
                    to: "app.gadget".to_owned(),
                },
                SemanticChange::FieldRenamed {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    from: "name".to_owned(),
                    to: "label".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn renamed_function_reports_nested_parameter_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(5, &["app", "read"], vec![parameter(6, "p_q", 0)])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "load"],
                vec![parameter(6, "p_query", 0)],
            )],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FunctionRenamed {
                    id: FunctionId::from_bytes([5; 16]),
                    from: "app.read".to_owned(),
                    to: "app.load".to_owned(),
                },
                SemanticChange::ParameterRenamed {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([6; 16]),
                    from: "p_q".to_owned(),
                    to: "p_query".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_owner_reports_nested_field_and_parameter_renames() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![],
            vec![function(5, &["app", "read"], vec![parameter(6, "p_q", 0)])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            // Same owner name: the nested field rename is reported.
            vec![object(2, &["app", "widget"], vec![field(3, "label", 0)])],
            vec![],
            // Same function name: the nested parameter rename is reported.
            vec![function(
                5,
                &["app", "read"],
                vec![parameter(6, "p_query", 0)],
            )],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FieldRenamed {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    from: "name".to_owned(),
                    to: "label".to_owned(),
                },
                SemanticChange::ParameterRenamed {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([6; 16]),
                    from: "p_q".to_owned(),
                    to: "p_query".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn renamed_field_reports_payload_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "widget"], vec![field(3, "name", 0)])],
            vec![],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(
                2,
                &["app", "widget"],
                vec![FieldDefinition::new(
                    FieldId::from_bytes([3; 16]),
                    "label",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![],
            vec![],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FieldRenamed {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    from: "name".to_owned(),
                    to: "label".to_owned(),
                },
                SemanticChange::FieldTypeChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "label".to_owned(),
                },
                SemanticChange::FieldNullabilityChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "label".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn renamed_parameter_reports_type_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(5, &["app", "read"], vec![parameter(6, "p_q", 0)])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "read"],
                vec![ParameterDefinition::new(
                    ParameterId::from_bytes([6; 16]),
                    "p_query",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            )],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::ParameterRenamed {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([6; 16]),
                    from: "p_q".to_owned(),
                    to: "p_query".to_owned(),
                },
                SemanticChange::ParameterTypeChanged {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([6; 16]),
                    name: "p_query".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_parameter_reports_default_expression_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "read"],
                vec![ParameterDefinition::new(
                    ParameterId::from_bytes([6; 16]),
                    "p_q",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    Some(ExpressionId::from_bytes([7; 16])),
                )],
            )],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "read"],
                vec![ParameterDefinition::new(
                    ParameterId::from_bytes([6; 16]),
                    "p_q",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    Some(ExpressionId::from_bytes([8; 16])),
                )],
            )],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[SemanticChange::ParameterDefaultChanged {
                owner: FunctionId::from_bytes([5; 16]),
                id: ParameterId::from_bytes([6; 16]),
                name: "p_q".to_owned(),
            }]
        );
    }

    #[test]
    fn retained_parameters_report_declaration_ordinal_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "read"],
                vec![parameter(6, "first", 0), parameter(7, "second", 1)],
            )],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(
                5,
                &["app", "read"],
                vec![parameter(7, "second", 0), parameter(6, "first", 1)],
            )],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::ParameterOrdinalChanged {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([7; 16]),
                    name: "second".to_owned(),
                    from: 1,
                    to: 0,
                },
                SemanticChange::ParameterOrdinalChanged {
                    owner: FunctionId::from_bytes([5; 16]),
                    id: ParameterId::from_bytes([6; 16]),
                    name: "first".to_owned(),
                    from: 0,
                    to: 1,
                },
            ]
        );
        assert!(
            diff.changes()
                .iter()
                .all(|change| change.category() == "parameter")
        );
    }

    #[test]
    fn add_and_drop_report_identity_keyed_entries() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(2, &["app", "kept"], vec![field(3, "name", 0)])],
            vec![],
            vec![function(4, &["app", "gone"], vec![])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"]), schema(5, &["extra"])],
            vec![
                object(2, &["app", "kept"], vec![field(3, "name", 0)]),
                object(6, &["app", "new"], vec![]),
            ],
            vec![],
            vec![function(7, &["app", "fresh"], vec![])],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::SchemaAdded {
                    id: SchemaId::from_bytes([5; 16]),
                    name: "extra".to_owned(),
                },
                SemanticChange::ObjectTypeAdded {
                    id: TypeId::from_bytes([6; 16]),
                    name: "app.new".to_owned(),
                },
                SemanticChange::FunctionAdded {
                    id: FunctionId::from_bytes([7; 16]),
                    name: "app.fresh".to_owned(),
                },
                SemanticChange::FunctionDropped {
                    id: FunctionId::from_bytes([4; 16]),
                    name: "app.gone".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn empty_catalogue_against_nonempty_reports_every_definition_added() {
        let empty = snapshot(Vec::new(), Vec::new());
        let populated = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(2, &["app", "read"], vec![])],
        );
        let diff = catalogue_diff(&empty, &populated);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::SchemaAdded {
                    id: SchemaId::from_bytes([1; 16]),
                    name: "app".to_owned(),
                },
                SemanticChange::FunctionAdded {
                    id: FunctionId::from_bytes([2; 16]),
                    name: "app.read".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn enum_rename_is_reported_by_identity() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![enum_type(2, &["app", "stage"])],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![enum_type(2, &["app", "phase"])],
            vec![],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[SemanticChange::EnumTypeRenamed {
                id: TypeId::from_bytes([2; 16]),
                from: "app.stage".to_owned(),
                to: "app.phase".to_owned(),
            }]
        );
    }

    #[test]
    fn renamed_enum_reports_label_changes_independently() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![enum_type(2, &["app", "stage"])],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes([2; 16]),
                name(&["app", "phase"]),
                vec!["lead".to_owned(), "won".to_owned()],
            )],
            vec![],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::EnumTypeRenamed {
                    id: TypeId::from_bytes([2; 16]),
                    from: "app.stage".to_owned(),
                    to: "app.phase".to_owned(),
                },
                SemanticChange::EnumLabelsChanged {
                    id: TypeId::from_bytes([2; 16]),
                    name: "app.phase".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_enum_reports_label_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![enum_type(2, &["app", "stage"])],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes([2; 16]),
                name(&["app", "stage"]),
                vec!["lead".to_owned(), "won".to_owned()],
            )],
            vec![],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[SemanticChange::EnumLabelsChanged {
                id: TypeId::from_bytes([2; 16]),
                name: "app.stage".to_owned(),
            }]
        );
    }

    #[test]
    fn retained_field_reports_type_nullability_unique_and_constraint_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(
                2,
                &["app", "widget"],
                vec![FieldDefinition::new(
                    FieldId::from_bytes([3; 16]),
                    "count",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(
                2,
                &["app", "widget"],
                vec![FieldDefinition::new(
                    FieldId::from_bytes([3; 16]),
                    "count",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                    true,
                    None,
                    None,
                )],
            )],
            vec![],
            vec![],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FieldTypeChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "count".to_owned(),
                },
                SemanticChange::FieldNullabilityChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "count".to_owned(),
                },
                SemanticChange::FieldUniquenessChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "count".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_object_fields_report_ordinal_changes_when_reordered() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(
                2,
                &["app", "widget"],
                vec![field(3, "first", 0), field(4, "second", 1)],
            )],
            vec![],
            vec![],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![object(
                2,
                &["app", "widget"],
                vec![field(4, "second", 0), field(3, "first", 1)],
            )],
            vec![],
            vec![],
        );

        let diff = catalogue_diff(&base, &candidate);

        assert_eq!(diff.changes().len(), 2);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FieldOrdinalChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([4; 16]),
                    name: "second".to_owned(),
                },
                SemanticChange::FieldOrdinalChanged {
                    owner: TypeId::from_bytes([2; 16]),
                    id: FieldId::from_bytes([3; 16]),
                    name: "first".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_function_reports_payload_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes([5; 16]),
                name(&["app", "read"]),
                FunctionDomain::Server,
                vec![parameter(6, "p_q", 0)],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([5; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes([5; 16]),
                name(&["app", "read"]),
                FunctionDomain::Server,
                vec![parameter(6, "p_q", 0)],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
                FunctionRevisionId::from_bytes([5; 16]),
                FunctionSecurity::Definer,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            )],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[
                SemanticChange::FunctionReturnChanged {
                    id: FunctionId::from_bytes([5; 16]),
                    name: "app.read".to_owned(),
                },
                SemanticChange::FunctionSecurityChanged {
                    id: FunctionId::from_bytes([5; 16]),
                    name: "app.read".to_owned(),
                },
                SemanticChange::FunctionTransactionChanged {
                    id: FunctionId::from_bytes([5; 16]),
                    name: "app.read".to_owned(),
                },
                SemanticChange::FunctionVolatilityChanged {
                    id: FunctionId::from_bytes([5; 16]),
                    name: "app.read".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn retained_function_reports_domain_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes([5; 16]),
                name(&["app", "read"]),
                FunctionDomain::Server,
                vec![],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([5; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            )],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes([5; 16]),
                name(&["app", "read"]),
                FunctionDomain::Client,
                vec![],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([5; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            )],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[SemanticChange::FunctionDomainChanged {
                id: FunctionId::from_bytes([5; 16]),
                name: "app.read".to_owned(),
            }]
        );
    }

    #[test]
    fn retained_parameter_reports_type_changes() {
        let base = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![function(5, &["app", "read"], vec![parameter(6, "p_q", 0)])],
        );
        let candidate = full_snapshot(
            vec![schema(1, &["app"])],
            vec![],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes([5; 16]),
                name(&["app", "read"]),
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    ParameterId::from_bytes([6; 16]),
                    "p_q",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([5; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )],
        );
        let diff = catalogue_diff(&base, &candidate);
        assert_eq!(
            diff.changes(),
            &[SemanticChange::ParameterTypeChanged {
                owner: FunctionId::from_bytes([5; 16]),
                id: ParameterId::from_bytes([6; 16]),
                name: "p_q".to_owned(),
            }]
        );
    }

    #[test]
    fn value_type_diff_reports_identity_and_payload_changes() {
        let base = value_snapshot(
            vec![schema(1, &["app"])],
            vec![
                value_type(
                    2,
                    &["app", "money"],
                    crate::catalogue::ValueTypeKind::Primitive,
                    ValueTypePersistence::Persistable,
                    "kernel.money@1",
                ),
                value_type(
                    3,
                    &["app", "legacy"],
                    crate::catalogue::ValueTypeKind::Primitive,
                    ValueTypePersistence::Persistable,
                    "kernel.legacy@1",
                ),
            ],
            vec![],
        );
        let candidate = value_snapshot(
            vec![schema(1, &["app"])],
            vec![
                value_type(
                    2,
                    &["app", "currency"],
                    crate::catalogue::ValueTypeKind::Opaque,
                    ValueTypePersistence::Transient,
                    "codec.currency@2",
                ),
                value_type(
                    4,
                    &["app", "fresh"],
                    crate::catalogue::ValueTypeKind::Primitive,
                    ValueTypePersistence::Persistable,
                    "kernel.fresh@1",
                ),
            ],
            vec![],
        );

        assert_eq!(
            catalogue_diff(&base, &candidate).changes(),
            &[
                SemanticChange::ValueTypeRenamed {
                    id: TypeId::from_bytes([2; 16]),
                    from: "app.money".to_owned(),
                    to: "app.currency".to_owned(),
                },
                SemanticChange::ValueTypeKindChanged {
                    id: TypeId::from_bytes([2; 16]),
                    name: "app.currency".to_owned(),
                },
                SemanticChange::ValueTypePersistenceChanged {
                    id: TypeId::from_bytes([2; 16]),
                    name: "app.currency".to_owned(),
                },
                SemanticChange::ValueTypeRepresentationChanged {
                    id: TypeId::from_bytes([2; 16]),
                    name: "app.currency".to_owned(),
                },
                SemanticChange::ValueTypeAdded {
                    id: TypeId::from_bytes([4; 16]),
                    name: "app.fresh".to_owned(),
                },
                SemanticChange::ValueTypeDropped {
                    id: TypeId::from_bytes([3; 16]),
                    name: "app.legacy".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn record_value_type_diff_reports_identity_and_field_changes() {
        let base = record_snapshot(
            vec![schema(1, &["app"])],
            vec![
                enum_type(2, &["app", "text"]),
                enum_type(3, &["app", "number"]),
            ],
            vec![
                record_value_type(
                    10,
                    &["app", "point"],
                    vec![record_field(11, "x", 0, 2), record_field(12, "y", 1, 2)],
                ),
                record_value_type(13, &["app", "legacy"], vec![record_field(14, "x", 0, 2)]),
            ],
        );
        let candidate = record_snapshot(
            vec![schema(1, &["app"])],
            vec![
                enum_type(2, &["app", "text"]),
                enum_type(3, &["app", "number"]),
            ],
            vec![
                record_value_type(
                    10,
                    &["app", "coordinate"],
                    vec![
                        record_field(15, "latitude", 0, 2),
                        record_field(11, "longitude", 1, 3),
                    ],
                ),
                record_value_type(16, &["app", "fresh"], vec![record_field(17, "x", 0, 2)]),
            ],
        );

        assert_eq!(
            catalogue_diff(&base, &candidate).changes(),
            &[
                SemanticChange::RecordValueTypeRenamed {
                    id: TypeId::from_bytes([10; 16]),
                    from: "app.point".to_owned(),
                    to: "app.coordinate".to_owned(),
                },
                SemanticChange::FieldAdded {
                    owner: TypeId::from_bytes([10; 16]),
                    id: FieldId::from_bytes([15; 16]),
                    name: "latitude".to_owned(),
                },
                SemanticChange::FieldRenamed {
                    owner: TypeId::from_bytes([10; 16]),
                    id: FieldId::from_bytes([11; 16]),
                    from: "x".to_owned(),
                    to: "longitude".to_owned(),
                },
                SemanticChange::FieldTypeChanged {
                    owner: TypeId::from_bytes([10; 16]),
                    id: FieldId::from_bytes([11; 16]),
                    name: "longitude".to_owned(),
                },
                SemanticChange::FieldOrdinalChanged {
                    owner: TypeId::from_bytes([10; 16]),
                    id: FieldId::from_bytes([11; 16]),
                    name: "longitude".to_owned(),
                },
                SemanticChange::FieldDropped {
                    owner: TypeId::from_bytes([10; 16]),
                    id: FieldId::from_bytes([12; 16]),
                    name: "y".to_owned(),
                },
                SemanticChange::RecordValueTypeAdded {
                    id: TypeId::from_bytes([16; 16]),
                    name: "app.fresh".to_owned(),
                },
                SemanticChange::RecordValueTypeDropped {
                    id: TypeId::from_bytes([13; 16]),
                    name: "app.legacy".to_owned(),
                },
            ]
        );
    }
}
