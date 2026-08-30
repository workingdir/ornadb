//! Server mutation preparation and validation tests.

use super::*;
#[test]
fn prepares_a_complete_server_mutation_artifact_and_reuses_only_equal_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(MUTATION_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = initial.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &initial.new_function_revisions()[0];
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::INSERT_FORMAT_VERSION
    );
    assert_eq!(
        revision.language_version(),
        SERVER_MUTATION_PLAN_LANGUAGE_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );

    let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.target(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 4);
    assert_eq!(plan.assignments()[0].owner(), task.id());
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[1].id());
    assert_eq!(plan.assignments()[2].field(), task.fields()[2].id());
    assert_eq!(plan.assignments()[3].field(), task.fields()[3].id());
    assert!(plan
        .assignments()
        .iter()
        .all(|assignment| assignment.owner() == task.id()));
    assert_eq!(
        plan.assignments()[0].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(!plan.assignments()[0].expression().nullable());
    assert_eq!(
        plan.assignments()[1].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.assignments()[1].expression().nullable());
    assert_eq!(
        plan.assignments()[2].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(
        plan.assignments()[3].expression().resolved_type(),
        ResolvedType::reference(person.id())
    );
    assert!(!plan.assignments()[3].expression().nullable());
    assert!(matches!(
        plan.assignments()[0].expression().kind(),
        DurableMutationExpressionKind::Parameter { owner, parameter }
            if *owner == function.id() && *parameter == function.parameters()[0].id()
    ));
    assert!(matches!(
        plan.assignments()[1].expression().kind(),
        DurableMutationExpressionKind::BooleanLiteral { value: false }
    ));
    assert!(plan.assignments()[2].expression().nullable());
    assert!(matches!(
        plan.assignments()[2].expression().kind(),
        DurableMutationExpressionKind::TypedNull
    ));
    assert!(matches!(
        plan.assignments()[3].expression().kind(),
        DurableMutationExpressionKind::Parameter { owner, parameter }
            if *owner == function.id() && *parameter == function.parameters()[2].id()
    ));
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(person.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id()
                }
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[1].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[3].id()
                }
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[2].id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
        ]
    );
    assert_eq!(
        initial.references()[2].source_origin().byte_start() as usize,
        MUTATION_SOURCE.rfind("tasks.task AS created").unwrap()
    );
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..10).collect::<Vec<_>>()
    );
    assert!(initial.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));

    let active = activate(&initial, vec![revision.clone()], Vec::new());
    let reformatted = prepare(
        &checked_report(MUTATION_REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(reformatted.new_function_revisions().is_empty());
    assert_eq!(
        reformatted.candidate().functions()[0].current_revision(),
        revision.id()
    );

    let changed = prepare(
        &checked_report(MUTATION_CHANGED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert_eq!(changed.new_function_revisions().len(), 1);
    assert_ne!(changed.new_function_revisions()[0].id(), revision.id());
    assert_ne!(
        changed.new_function_revisions()[0].semantic_hash(),
        revision.semantic_hash()
    );
}

#[test]
fn prepares_update_version_two_with_selector_and_exact_references() {
    let empty = empty_active();
    let prepared = prepare(
        &checked_report(UPDATE_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = prepared.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::UPDATE_FORMAT_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.format_version(), 2);
    assert_eq!(plan.target(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 2);
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[2].id());
    assert_eq!(
        plan.operation(),
        &ServerMutationOperation::Update {
            selector: orna_artifact::server_mutation_plan::MutationSelector::new(
                function.id(),
                function.parameters()[0].id(),
            )
        }
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(person.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[1].id(),
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..11).collect::<Vec<_>>()
    );
}

#[test]
fn prepares_delete_version_three_with_boolean_result_and_exact_references() {
    let empty = empty_active();
    let prepared = prepare(
        &checked_report(DELETE_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = prepared.candidate();
    let target = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::DELETE_FORMAT_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(
        revision.language_version(),
        SERVER_MUTATION_PLAN_LANGUAGE_VERSION
    );
    let plan = ServerDeletePlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.target(), target.id());
    assert_eq!(plan.selector().owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert!(matches!(
        function.return_type(),
        FunctionReturn::Rows(columns)
            if columns.len() == 1
                && columns[0].resolved_type()
                    == ResolvedType::Scalar(StandardScalar::Boolean)
    ));
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| {
                (
                    reference.ordinal(),
                    reference.source_origin().byte_start() as usize,
                    reference.source_origin().byte_end() as usize,
                )
            })
            .collect::<Vec<_>>(),
        [
            (0, "p_task REF ", "tasks.task"),
            (1, "DELETE FROM ", "tasks.task"),
            (2, "WHERE REF(", "removed"),
            (3, "= ", "p_task"),
        ]
        .into_iter()
        .zip([
            "p_task REF tasks.task",
            "DELETE FROM tasks.task",
            "WHERE REF(removed)",
            "= p_task RETURNING",
        ])
        .map(|((ordinal, prefix, token), context)| {
            let start = DELETE_SOURCE.find(context).unwrap() + prefix.len();
            (ordinal, start, start + token.len())
        })
        .collect::<Vec<_>>()
    );
}

#[test]
fn mutation_preparation_revalidates_durable_catalogue_and_reference_facts() {
    let target_id = TypeId::from_bytes([41; 16]);
    let title_id = FieldId::from_bytes([42; 16]);
    let note_id = FieldId::from_bytes([43; 16]);
    let function_id = FunctionId::from_bytes([44; 16]);
    let parameter_id = ParameterId::from_bytes([45; 16]);
    let text = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
    let target = ObjectTypeDefinition::new(
        target_id,
        semantic_name(&["tasks", "task"]),
        vec![
            FieldDefinition::new(title_id, "title", 0, text, false, false, None, None),
            FieldDefinition::new(note_id, "note", 1, text, true, false, None, None),
        ],
    );
    let function = FunctionDefinition::new(
        function_id,
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "title",
            0,
            text,
            None,
        )],
        FunctionReturn::Rows(Vec::new()),
        FunctionRevisionId::from_bytes([46; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let parameter = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::ParameterRead {
                owner: function_id,
                parameter: parameter_id,
            },
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
        ),
    );
    assert!(validate_mutation_assignments(
        std::slice::from_ref(&parameter),
        &target,
        &function,
        true,
    )
    .is_ok());

    let cross_owner = MutationAssignment::new(
        TypeId::from_bytes([47; 16]),
        title_id,
        parameter.expression().clone(),
    );
    let unknown_field = MutationAssignment::new(
        target_id,
        FieldId::from_bytes([48; 16]),
        parameter.expression().clone(),
    );
    let wrong_field_type = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::BooleanLiteral { value: true },
            MutationValueType::new(SemanticType::scalar(StandardScalar::Boolean), false),
        ),
    );
    let wrong_parameter_type = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::ParameterRead {
                owner: function_id,
                parameter: parameter_id,
            },
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
        ),
    );
    let function_with_wrong_parameter_type = FunctionDefinition::new(
        function_id,
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "title",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Rows(Vec::new()),
        FunctionRevisionId::from_bytes([46; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let nullable_null = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::TypedNull,
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
        ),
    );
    for assignments in [
        vec![cross_owner],
        vec![unknown_field],
        vec![wrong_field_type],
        vec![nullable_null],
        Vec::new(),
    ] {
        assert!(validate_mutation_assignments(&assignments, &target, &function, true).is_err());
    }
    assert!(matches!(
        validate_mutation_assignments(
            &[wrong_parameter_type],
            &target,
            &function_with_wrong_parameter_type,
            true,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter type differs from its expression"
        })
    ));

    let expected = vec![
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(target_id),
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: target_id,
                field: title_id,
            },
        ),
    ];
    assert!(validate_reference_sequence(
        &expected,
        &expected,
        "mutation definition references differ from the checked body"
    )
    .is_ok());
    let mut reordered = expected.clone();
    reordered.reverse();
    assert!(validate_reference_sequence(
        &expected,
        &reordered,
        "mutation definition references differ from the checked body"
    )
    .is_err());
    assert!(validate_reference_sequence(
        &expected,
        &expected[..1],
        "mutation definition references differ from the checked body"
    )
    .is_err());
}

#[test]
fn record_constructor_preparation_rejects_a_nullable_object_field() {
    let boolean_id = TypeId::from_bytes([0x91; 16]);
    let record_id = TypeId::from_bytes([0x92; 16]);
    let record_field_id = FieldId::from_bytes([0x93; 16]);
    let standard = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x94; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x95; 16]),
            semantic_name(&["std"]),
        )],
        vec![],
        vec![orna_core::catalogue::ValueTypeDefinition::primitive(
            boolean_id,
            semantic_name(&["std", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![],
    )
    .unwrap();
    let record = RecordValueTypeDefinition::new(
        record_id,
        semantic_name(&["tasks", "flags"]),
        vec![RecordValueFieldDefinition::try_new_descriptor(
            record_field_id,
            "active",
            0,
            TypeDescriptor::named(boolean_id),
        )
        .unwrap()],
    );
    let target_field = FieldDefinition::new(
        FieldId::from_bytes([0x96; 16]),
        "flags",
        0,
        ResolvedType::named(record_id),
        true,
        false,
        None,
        None,
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0x97; 16]),
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Rows(vec![]),
        FunctionRevisionId::from_bytes([0x98; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let boolean_type = MutationValueType::new(SemanticType::scalar(StandardScalar::Boolean), false)
        .with_standard_value_type(boolean_id);
    let expression = MutationExpression::new(
        MutationExpressionKind::RecordConstructor {
            record_type: record_id,
            fields: vec![MutationRecordFieldExpression::new(
                record_id,
                record_field_id,
                MutationRecordFieldExpressionKind::BooleanLiteral { value: true },
                boolean_type,
            )],
        },
        MutationValueType::new(SemanticType::Named(record_id), false),
    );

    assert!(matches!(
        server_mutation_expression(
            &expression,
            &function,
            &target_field,
            &[],
            std::slice::from_ref(&record),
            Some(&standard),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor targets a nullable object field"
        })
    ));
}

#[test]
fn mutation_parameter_validation_rejects_unused_unsupported_types_and_defaults() {
    let function_id = FunctionId::from_bytes([51; 16]);
    let valid_parameter_id = ParameterId::from_bytes([52; 16]);
    let unused_parameter_id = ParameterId::from_bytes([53; 16]);
    let function_with_unused = |resolved_type, default_expression| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "create"]),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(
                    valid_parameter_id,
                    "used_title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
                ParameterDefinition::new(
                    unused_parameter_id,
                    "unused",
                    1,
                    resolved_type,
                    default_expression,
                ),
            ],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([54; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };

    let unsupported = function_with_unused(ResolvedType::scalar(StandardScalar::Decimal), None);
    assert!(matches!(
        validate_mutation_parameters(&unsupported, &[]),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter has an unsupported runtime type"
        })
    ));

    let defaulted = function_with_unused(
        ResolvedType::scalar(StandardScalar::Integer),
        Some(ExpressionId::from_bytes([55; 16])),
    );
    assert!(matches!(
        validate_mutation_parameters(&defaulted, &[]),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter has an unsupported default expression"
        })
    ));
}

#[test]
fn mutation_selector_validation_requires_exact_owner_parameter_and_target() {
    let function_id = FunctionId::from_bytes([61; 16]);
    let parameter_id = ParameterId::from_bytes([62; 16]);
    let target = TypeId::from_bytes([63; 16]);
    let function_with = |resolved_type| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "update"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                resolved_type,
                None,
            )],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([64; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };
    let valid = function_with(ResolvedType::reference(target));
    assert!(validate_mutation_selector(function_id, parameter_id, target, &valid).is_ok());
    assert!(matches!(
        validate_mutation_selector(
            FunctionId::from_bytes([65; 16]),
            parameter_id,
            target,
            &valid,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector owner differs from its enclosing function"
        })
    ));
    assert!(matches!(
        validate_mutation_selector(
            function_id,
            ParameterId::from_bytes([66; 16]),
            target,
            &valid,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter is not declared by its enclosing function"
        })
    ));
    let wrong_type = function_with(ResolvedType::scalar(StandardScalar::BigInt));
    assert!(matches!(
        validate_mutation_selector(function_id, parameter_id, target, &wrong_type),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter does not reference its target object"
        })
    ));
    let wrong_target = function_with(ResolvedType::reference(TypeId::from_bytes([67; 16])));
    assert!(matches!(
        validate_mutation_selector(function_id, parameter_id, target, &wrong_target),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter does not reference its target object"
        })
    ));
}

#[test]
fn delete_preparation_revalidates_target_modes_result_and_evidence() {
    let function_id = FunctionId::from_bytes([71; 16]);
    let parameter_id = ParameterId::from_bytes([72; 16]);
    let target_id = TypeId::from_bytes([73; 16]);
    let revision_id = FunctionRevisionId::from_bytes([74; 16]);
    let target =
        ObjectTypeDefinition::new(target_id, semantic_name(&["tasks", "task"]), Vec::new());
    let function_with = |return_type, security| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "remove"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "p_task",
                0,
                ResolvedType::reference(target_id),
                None,
            )],
            return_type,
            revision_id,
            security,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };
    let boolean_rows = || {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "deleted",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )])
    };
    let function = function_with(boolean_rows(), FunctionSecurity::Invoker);
    let plan = DeletePlanIr::new(target_id, function_id, parameter_id);
    let references = delete_reference_sequence(&plan, &function);

    assert!(
        server_delete_plan(&plan, &function, std::slice::from_ref(&target), &references).is_ok()
    );
    assert!(matches!(
        server_delete_plan(&plan, &function, &[], &references),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE target object is absent from the candidate catalogue"
        })
    ));

    let definer = function_with(boolean_rows(), FunctionSecurity::Definer);
    assert!(matches!(
        server_delete_plan(
            &plan,
            &definer,
            std::slice::from_ref(&target),
            &delete_reference_sequence(&plan, &definer),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function has unsupported execution modes"
        })
    ));

    let wrong_result = function_with(
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "deleted",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )]),
        FunctionSecurity::Invoker,
    );
    assert!(matches!(
        server_delete_plan(
            &plan,
            &wrong_result,
            std::slice::from_ref(&target),
            &delete_reference_sequence(&plan, &wrong_result),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function does not return exactly one BOOLEAN column"
        })
    ));

    assert!(matches!(
        server_delete_plan(
            &plan,
            &function,
            std::slice::from_ref(&target),
            &references[..references.len() - 1],
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation definition references differ from the checked body"
        })
    ));
}
