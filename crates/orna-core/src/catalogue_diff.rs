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
    catalogue::{CatalogueSnapshot, FieldDefinition, FunctionDefinition, ObjectTypeDefinition},
};

/// What happened to one durable definition between two catalogues.
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
    /// A field added to a retained object type (by owner identity).
    FieldAdded {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A field dropped from a retained object type (by owner identity).
    FieldDropped {
        owner: TypeId,
        id: FieldId,
        name: String,
    },
    /// A field kept by identity but renamed inside a retained object type.
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
    /// A retained object field changed its resolved type.
    FieldTypeChanged {
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
            Self::FieldAdded { .. }
            | Self::FieldRenamed { .. }
            | Self::FieldDropped { .. }
            | Self::FieldTypeChanged { .. }
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
            | Self::ParameterTypeChanged { .. } => "parameter",
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
/// (object fields, function parameters) are compared under their retained
/// owner identity only; a renamed or dropped owner carries no nested changes.
pub fn catalogue_diff(
    base: &CatalogueSnapshot,
    candidate: &CatalogueSnapshot,
) -> CatalogueSemanticDiff {
    let mut diff = CatalogueSemanticDiff::new();

    diff_schemas(base, candidate, &mut diff);
    diff_object_types(base, candidate, &mut diff);
    diff_enum_types(base, candidate, &mut diff);
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
            Some(found) => diff.push(SemanticChange::ObjectTypeRenamed {
                id: definition.id(),
                from: qualified(found.name()),
                to: qualified(definition.name()),
            }),
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
            Some(found) => diff.push(SemanticChange::FieldRenamed {
                owner,
                id: field.id(),
                from: found.name().to_owned(),
                to: field.name().to_owned(),
            }),
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
            Some(found) => diff.push(SemanticChange::EnumTypeRenamed {
                id: definition.id(),
                from: qualified(found.name()),
                to: qualified(definition.name()),
            }),
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
            Some(found) => diff.push(SemanticChange::FunctionRenamed {
                id: definition.id(),
                from: qualified(found.name()),
                to: qualified(definition.name()),
            }),
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
                if found.resolved_type() != parameter.resolved_type() {
                    diff.push(SemanticChange::ParameterTypeChanged {
                        owner,
                        id: parameter.id(),
                        name: parameter.name().to_owned(),
                    });
                }
            }
            Some(found) => diff.push(SemanticChange::ParameterRenamed {
                owner,
                id: parameter.id(),
                from: found.name().to_owned(),
                to: parameter.name().to_owned(),
            }),
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

fn qualified(name: &crate::catalogue::QualifiedSemanticName) -> String {
    name.parts().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{
        EnumTypeDefinition, FieldDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ParameterDefinition, QualifiedSemanticName,
        SchemaDefinition,
    };
    use crate::{
        CatalogueRevisionId, FunctionRevisionId,
        types::{ResolvedType, StandardScalar},
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
            // Same TypeId; the renamed owner carries no nested field change.
            vec![object(2, &["app", "gadget"], vec![field(3, "label", 0)])],
            vec![],
            // Same FunctionId; renamed function carries no parameter delta.
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
}
