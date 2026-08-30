//! Relational query preparation and validation tests.

use super::*;
#[test]
fn prepares_a_complete_source_catalogue_artifact_and_reference_revision() {
    let active = empty_active();
    let report = checked_report(SOURCE, active.catalogue());

    let prepared = prepare(&report, active.pair(), &active).unwrap();

    assert_eq!(prepared.expected_base(), active.pair());
    assert_eq!(prepared.source().parent(), Some(active.pair().source()));
    assert_eq!(prepared.source().units().len(), 1);
    assert_eq!(prepared.source().units()[0].logical_path(), "tasks.orna");
    assert_eq!(prepared.source().units()[0].content(), SOURCE);
    assert_eq!(
        source_unit_content_digest(SOURCE).unwrap(),
        prepared.source().units()[0].content_hash()
    );
    assert_eq!(
        source_bundle_digest(prepared.source().units()).unwrap(),
        prepared.source().bundle_hash()
    );
    assert_eq!(
        orna_core::canonical_hash::source_revision_digest(prepared.source()).unwrap(),
        prepared.source().revision_hash()
    );

    let catalogue = prepared.candidate();
    assert_eq!(catalogue.schemas().len(), 1);
    assert_eq!(catalogue.object_types().len(), 2);
    assert_eq!(catalogue.functions().len(), 1);
    assert_eq!(prepared.expressions().len(), 4);
    assert!(prepared.expressions().iter().all(|artifact| {
        artifact_payload_digest(artifact.payload()).unwrap() == artifact.content_hash()
    }));
    assert_eq!(prepared.new_function_revisions().len(), 1);
    assert_eq!(prepared.new_function_revisions()[0].revision_number(), 1);

    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let title = task.field_by_name("title").unwrap();
    let completed = task.field_by_name("completed").unwrap();
    let priority = task.field_by_name("priority").unwrap();
    let note = task.field_by_name("note").unwrap();
    let assignee = task.field_by_name("assignee").unwrap();
    assert_eq!(
        assignee.resolved_type(),
        ResolvedType::reference(person.id())
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), title).payload()).unwrap(),
        ConstantExpression::Text("todo".to_owned())
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), completed).payload())
            .unwrap(),
        ConstantExpression::Boolean(false)
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), priority).payload()).unwrap(),
        ConstantExpression::Integer(7)
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), note).payload()).unwrap(),
        ConstantExpression::Null
    );

    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(function.current_revision(), revision.id());
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let declaration_origin = revision.declaration_origin();
    let source = prepared
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == declaration_origin.source_unit())
        .unwrap();
    assert_eq!(
        function_declaration_digest(
            &source.content().as_bytes()
                [declaration_origin.byte_start() as usize..declaration_origin.byte_end() as usize]
        )
        .unwrap(),
        revision.declaration_content_hash()
    );
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(
        DistinctServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(plan.scan.object_type, task.id());
    assert!(matches!(
        plan.projections[0].kind,
        ExpressionKind::ObjectReference { .. }
    ));
    let ExpressionKind::FieldPath { ref steps, .. } = plan.projections[1].kind else {
        panic!("second projection is not a field path");
    };
    assert_eq!(steps[0].owner, task.id());
    assert_eq!(steps[0].field, title.id());

    assert_eq!(prepared.references().len(), 6);
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..6).collect::<Vec<_>>()
    );
    assert!(prepared.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));
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
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: title.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: title.id(),
                },
            ),
        ]
    );
    assert_eq!(prepared.origins().len(), 16);
    assert_eq!(
        catalogue_digest(
            catalogue,
            prepared.new_function_revisions(),
            prepared.expressions(),
            prepared.origins(),
            prepared.references(),
        )
        .unwrap(),
        prepared.catalogue_hash()
    );
}

#[test]
fn prepares_direct_boolean_predicates_as_version_one_server_plans_and_replays_by_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DIRECT_BOOLEAN_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let catalogue = initial.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let revision = &initial.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(
        DistinctServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    let selection = plan
        .selection
        .as_ref()
        .expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
        panic!("direct predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type.nullable);
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
        ]
    );

    let initial_revision = revision.clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
    let replay = prepare(
        &checked_report(DIRECT_BOOLEAN_REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );

    let changed = prepare(
        &checked_report(DIRECT_BOOLEAN_CHANGED_PREDICATE_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed_revision = &changed.new_function_revisions()[0];
    assert_ne!(changed_revision.id(), initial_revision.id());
    assert_ne!(
        changed_revision.semantic_hash(),
        initial_revision.semantic_hash()
    );
    assert_ne!(
        changed_revision.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
}

#[test]
fn prepares_scalar_select_as_single_return_and_one_column_server_plan() {
    let active = empty_active();
    let report = checked_report(SCALAR_SELECT_SOURCE, active.catalogue());
    assert_eq!(report.diagnostics(), &[]);

    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let function = &prepared.candidate().functions()[0];
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer))
    );
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.projections.len(), 1);
    assert_eq!(
        plan.projections[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Integer)
    );
}

#[test]
fn prepares_identity_selected_query_as_a_version_two_server_plan() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let catalogue = prepared.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), 2);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let plan = IdentitySelectedServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.selector().owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(DistinctServerPlan::decode(revision.artifact().payload()).is_err());
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("title").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id()
                }
            ),
        ]
    );
}

#[test]
fn prepares_unique_text_selected_query_as_a_version_four_server_plan_with_exact_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(UNIQUE_TEXT_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let catalogue = prepared.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    let email = task.field_by_name("email").unwrap();
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), 4);
    let plan = UniqueTextSelectedServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(
        plan.selector(),
        &SelectBindValue::Text {
            scan_object_type: task.id(),
            field_owner: task.id(),
            field: email.id(),
            parameter_owner: function.id(),
            parameter: function.parameters()[0].id(),
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            field_nullable: true,
            parameter_required_non_null: true,
        }
    );
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(IdentitySelectedServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(DistinctServerPlan::decode(revision.artifact().payload()).is_err());
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .filter(|(kind, _)| {
                matches!(
                    kind,
                    DefinitionReferenceKind::QueryObject
                        | DefinitionReferenceKind::QueryField
                        | DefinitionReferenceKind::ParameterRead
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("title").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: email.id(),
                },
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
}

#[test]
fn prepares_distinct_query_as_a_version_three_server_plan_with_exact_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(DISTINCT_SOURCE, active.catalogue()),
        active.pair(),
        &active,
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
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
    assert_eq!(
        function_semantic_digest(
            function,
            revision.language_version(),
            revision.artifact(),
            prepared.expressions(),
            prepared.references(),
        )
        .unwrap(),
        revision.semantic_hash()
    );
    assert_eq!(function.domain(), FunctionDomain::Server);
    assert_eq!(function.security(), FunctionSecurity::Invoker);
    assert_eq!(function.transaction(), Some(FunctionTransaction::ReadOnly));
    assert_eq!(function.volatility(), FunctionVolatility::Stable);
    assert!(function.parameters().is_empty());
    assert!(matches!(
        function.return_type(),
        FunctionReturn::Rows(columns)
            if columns.iter().map(FunctionReturnColumnDefinition::resolved_type).collect::<Vec<_>>()
                == vec![
                    ResolvedType::reference(task.id()),
                    ResolvedType::scalar(StandardScalar::Boolean),
                    ResolvedType::scalar(StandardScalar::Boolean),
                ]
    ));

    let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(revision.artifact().version(), plan.format_version());
    assert_eq!(plan.scan().input, 0);
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.projections().len(), 3);
    assert!(matches!(
        plan.projections()[0].kind,
        ExpressionKind::ObjectReference { input: 0 }
    ));
    assert_eq!(
        plan.projections()[0].value_type.resolved_type,
        ResolvedType::reference(task.id())
    );
    assert!(!plan.projections()[0].value_type.nullable);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[1].kind else {
        panic!("second DISTINCT projection must be a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        plan.projections()[1].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(plan.projections()[1].value_type.nullable);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[2].kind else {
        panic!("third DISTINCT projection must be a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner, task.id());
    assert_eq!(
        steps[0].field,
        task.field_by_name("completed").unwrap().id()
    );
    assert_eq!(
        plan.projections()[2].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.projections()[2].value_type.nullable);
    let selection = plan.selection().expect("fixture has a selection");
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!selection.value_type.nullable);
    assert!(matches!(selection.kind, ExpressionKind::Equality { .. }));
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(IdentitySelectedServerPlan::decode(revision.artifact().payload()).is_err());

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
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..7).collect::<Vec<_>>()
    );
}

#[test]
fn prepares_direct_boolean_distinct_predicates_as_v3_and_replays_by_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DIRECT_BOOLEAN_DISTINCT_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let catalogue = initial.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &initial.new_function_revisions()[0];

    assert_eq!(revision.revision_number(), 1);
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(
        function_semantic_digest(
            function,
            revision.language_version(),
            revision.artifact(),
            initial.expressions(),
            initial.references(),
        )
        .unwrap(),
        revision.semantic_hash()
    );

    let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
    let format_version = plan.format_version();
    assert_eq!(revision.artifact().version(), format_version);
    assert_eq!(plan.encode().unwrap(), revision.artifact().payload());
    assert_eq!(
        ServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(format_version))
    );
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(format_version))
    );
    assert_eq!(plan.scan().input, 0);
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.projections().len(), 1);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[0].kind else {
        panic!("direct DISTINCT projection must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![(task.id(), task.field_by_name("completed").unwrap().id())]
    );
    assert_eq!(
        plan.projections()[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.projections()[0].value_type.nullable);

    let selection = plan.selection().expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
        panic!("direct DISTINCT predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type.nullable);

    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
        ]
    );
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..4).collect::<Vec<_>>()
    );
    assert!(initial.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));

    let initial_revision = revision.clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
    let replay = prepare(
        &checked_report(
            DIRECT_BOOLEAN_DISTINCT_REFORMATTED_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );
    assert_ne!(replay.source().id(), active.source().id());
    assert_eq!(
        active.function_revisions(),
        std::slice::from_ref(&initial_revision)
    );
    assert_eq!(
        active.function_revisions()[0].artifact(),
        revision.artifact()
    );

    let changed = prepare(
        &checked_report(
            DIRECT_BOOLEAN_DISTINCT_CHANGED_PREDICATE_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed_function = &changed.candidate().functions()[0];
    let changed_revision = &changed.new_function_revisions()[0];
    assert_eq!(changed_function.id(), function.id());
    assert_eq!(changed_revision.revision_number(), 2);
    assert_ne!(changed_revision.id(), initial_revision.id());
    assert_ne!(
        changed_revision.semantic_hash(),
        initial_revision.semantic_hash()
    );
    assert_ne!(
        changed_revision.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
    assert_eq!(changed_revision.artifact().version(), format_version);
    let changed_plan = DistinctServerPlan::decode(changed_revision.artifact().payload()).unwrap();
    let changed_selection = changed_plan
        .selection()
        .expect("changed fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &changed_selection.kind else {
        panic!("changed direct DISTINCT predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![(task.id(), task.field_by_name("completed").unwrap().id())]
    );
    assert!(!changed_selection.value_type.nullable);

    let (mapped_plan, mapped_function, object_types, references) =
        mapped_distinct_fixture_for(DIRECT_BOOLEAN_DISTINCT_SOURCE);
    let mapped_person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap();
    let non_nullable_owner = object_types_with_task_field(
        &object_types,
        "owner",
        ResolvedType::reference(mapped_person.id()),
        false,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &mapped_plan,
            &mapped_function,
            &non_nullable_owner,
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );
}

#[test]
fn distinct_replay_reuses_its_revision_and_removing_distinct_creates_version_one() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DISTINCT_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let initial_revision = initial.new_function_revisions()[0].clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

    for source in [DISTINCT_SOURCE, DISTINCT_REFORMATTED_SOURCE] {
        let replay = prepare(
            &checked_report(source, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(replay.new_function_revisions().is_empty());
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            initial_revision.id()
        );
        assert_eq!(
            active.function_revisions(),
            std::slice::from_ref(&initial_revision)
        );
        assert_eq!(active.function_revisions()[0], initial_revision);
        assert_eq!(
            active.function_revisions()[0].artifact(),
            initial_revision.artifact()
        );
    }

    let removed = prepare(
        &checked_report(DISTINCT_REMOVED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed = &removed.new_function_revisions()[0];
    assert_ne!(changed.id(), initial_revision.id());
    assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
    assert_ne!(
        changed.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
    assert_eq!(changed.artifact().version(), SERVER_PLAN_VERSION);
    assert!(ServerPlan::decode(changed.artifact().payload()).is_ok());
    assert!(DistinctServerPlan::decode(changed.artifact().payload()).is_err());
}

#[test]
fn distinct_preparation_validates_header_facts_in_the_accepted_order() {
    let (plan, function, object_types, references) = mapped_distinct_fixture();
    assert!(distinct_query_plan(&plan, &function, &object_types, &references).is_ok());

    assert_preparation_reason(
        distinct_query_plan(&plan, &function, &[], &references),
        "SELECT DISTINCT query scan object is absent from the candidate catalogue",
    );

    let function_with = |domain, parameters, return_type, security, transaction, volatility| {
        FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            domain,
            parameters,
            return_type,
            function.current_revision(),
            security,
            transaction,
            volatility,
        )
    };
    let bad_mode = function_with(
        FunctionDomain::Client,
        function.parameters().to_vec(),
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &bad_mode,
            &object_types,
            &distinct_query_reference_sequence(&plan, &bad_mode),
        ),
        "SELECT DISTINCT query function has unsupported execution modes",
    );

    let parameterised = function_with(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::new(),
            "unexpected",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &parameterised,
            &object_types,
            &distinct_query_reference_sequence(&plan, &parameterised),
        ),
        "SELECT DISTINCT query function declares parameters",
    );

    let single = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &single,
            &object_types,
            &distinct_query_reference_sequence(&plan, &single),
        ),
        "SELECT DISTINCT query function does not return ROWS",
    );

    let empty_rows = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(Vec::new()),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &empty_rows,
            &object_types,
            &distinct_query_reference_sequence(&plan, &empty_rows),
        ),
        "SELECT DISTINCT query function returns empty ROWS",
    );

    let wrong_count = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "one",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &wrong_count,
            &object_types,
            &distinct_query_reference_sequence(&plan, &wrong_count),
        ),
        "SELECT DISTINCT query projection count differs from its function return",
    );
}

#[test]
fn version_one_preparation_revalidates_headers_facts_and_evidence_before_encoding() {
    let (plan, function, object_types, references) = mapped_version_one_fixture();
    assert!(version_one_query_plan(&plan, &function, &object_types, &references).is_ok());

    let function_with = |domain, parameters, return_type, security, transaction, volatility| {
        FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            domain,
            parameters,
            return_type,
            function.current_revision(),
            security,
            transaction,
            volatility,
        )
    };
    for (transaction, volatility) in [
        (None, FunctionVolatility::Immutable),
        (
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        ),
        (
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        ),
    ] {
        let accepted = function_with(
            FunctionDomain::Server,
            Vec::new(),
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            transaction,
            volatility,
        );
        assert!(
            version_one_query_plan(
                &plan,
                &accepted,
                &object_types,
                &version_one_query_reference_sequence(&plan, &accepted),
            )
            .is_ok()
        );
    }

    assert_preparation_reason(
        version_one_query_plan(
            &plan.with_test_mutation(crate::relational::RelationalQueryTestMutation::InvalidScan),
            &function,
            &object_types,
            &references,
        ),
        "SERVER SELECT query scan object is absent from the candidate catalogue",
    );

    let manual = function_with(
        FunctionDomain::Server,
        Vec::new(),
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Manual),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &manual,
            &object_types,
            &version_one_query_reference_sequence(&plan, &manual),
        ),
        "SERVER SELECT query function has unsupported execution modes",
    );
    for (domain, security) in [
        (FunctionDomain::Client, FunctionSecurity::Invoker),
        (FunctionDomain::Server, FunctionSecurity::Definer),
    ] {
        let unsupported = function_with(
            domain,
            Vec::new(),
            function.return_type().clone(),
            security,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &unsupported,
                &object_types,
                &version_one_query_reference_sequence(&plan, &unsupported),
            ),
            "SERVER SELECT query function has unsupported execution modes",
        );
    }

    let parameterised = function_with(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::new(),
            "unexpected",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &parameterised,
            &object_types,
            &version_one_query_reference_sequence(&plan, &parameterised),
        ),
        "SERVER SELECT query function declares parameters",
    );

    let single = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &single,
            &object_types,
            &version_one_query_reference_sequence(&plan, &single),
        ),
        "SERVER SELECT query function does not return ROWS",
    );

    let empty_rows = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(Vec::new()),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &empty_rows,
            &object_types,
            &version_one_query_reference_sequence(&plan, &empty_rows),
        ),
        "SERVER SELECT query function returns empty ROWS",
    );

    let wrong_count = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "only",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &wrong_count,
            &object_types,
            &version_one_query_reference_sequence(&plan, &wrong_count),
        ),
        "SERVER SELECT query projection count differs from its function return",
    );

    let FunctionReturn::Rows(columns) = function.return_type() else {
        panic!("fixture must return rows");
    };
    let mut wrong_columns = columns.to_vec();
    wrong_columns[1] = FunctionReturnColumnDefinition::new(
        wrong_columns[1].name(),
        wrong_columns[1].ordinal(),
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    let wrong_return = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(wrong_columns),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &wrong_return,
            &object_types,
            &version_one_query_reference_sequence(&plan, &wrong_return),
        ),
        "SERVER SELECT query projection differs from its function return",
    );

    for (mutation, reason) in [
        (
            crate::relational::RelationalQueryTestMutation::InvalidProjectionFieldPathInput,
            "SERVER SELECT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidObjectReferenceInput,
            "SERVER SELECT query object reference has inconsistent facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidBooleanLiteralType,
            "SERVER SELECT query BOOLEAN expression has inconsistent type facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidEqualityType,
            "SERVER SELECT query equality expression has inconsistent type facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidOrderingFieldPathInput,
            "SERVER SELECT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::RelationalQueryTestMutation::SelectionObjectReference,
            "SERVER SELECT query selection is not BOOLEAN",
        ),
    ] {
        let malformed = plan.with_test_mutation(mutation);
        assert_preparation_reason(
            version_one_query_plan(
                &malformed,
                &function,
                &object_types,
                &version_one_query_reference_sequence(&malformed, &function),
            ),
            reason,
        );
    }

    let unknown_field = plan
        .try_map_identities(Ok::<_, PrepareError>, |_| {
            Ok::<_, PrepareError>(FieldId::new())
        })
        .unwrap();
    assert_preparation_reason(
        version_one_query_plan(
            &unknown_field,
            &function,
            &object_types,
            &version_one_query_reference_sequence(&unknown_field, &function),
        ),
        "SERVER SELECT query field path field is absent from its source object",
    );
    let wrong_owner = plan
        .try_map_identities(
            {
                let mut calls = 0;
                move |type_id| {
                    calls += 1;
                    Ok::<_, PrepareError>(if calls == 3 { TypeId::new() } else { type_id })
                }
            },
            Ok::<_, PrepareError>,
        )
        .unwrap();
    assert_preparation_reason(
        version_one_query_plan(
            &wrong_owner,
            &function,
            &object_types,
            &version_one_query_reference_sequence(&wrong_owner, &function),
        ),
        "SERVER SELECT query field path owner differs from its source object",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types_with_task_field(
                &object_types,
                "title",
                ResolvedType::scalar(StandardScalar::Boolean),
                true,
            ),
            &references,
        ),
        "SERVER SELECT query field path type differs from its source field",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types_with_task_field(
                &object_types,
                "title",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
            &references,
        ),
        "SERVER SELECT query field path type differs from its source field",
    );
    assert_preparation_reason(
        plan.try_map_identities(
            |_| {
                Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                    reason: "type mapping failure",
                })
            },
            Ok,
        ),
        "type mapping failure",
    );
    assert_preparation_reason(
        plan.try_map_identities(Ok::<_, PrepareError>, |_| {
            Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
                reason: "field mapping failure",
            })
        }),
        "field mapping failure",
    );

    let mut wrong_evidence = references.clone();
    wrong_evidence.reverse();
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_evidence),
        "SERVER SELECT definition references differ from the checked function body",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types,
            &references[..references.len() - 1],
        ),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut extra_evidence = references.clone();
    extra_evidence.push(references[0]);
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &extra_evidence),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut wrong_kind = references.clone();
    wrong_kind[0].0 = DefinitionReferenceKind::QueryObject;
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_kind),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut wrong_target = references.clone();
    wrong_target[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_target),
        "SERVER SELECT definition references differ from the checked function body",
    );

    let (direct_plan, direct_function, direct_objects, _) =
        mapped_version_one_fixture_for(DIRECT_BOOLEAN_SOURCE);
    let direct_references = version_one_query_reference_sequence(&direct_plan, &direct_function);
    assert_preparation_reason(
        version_one_query_plan(
            &direct_plan,
            &direct_function,
            &object_types_with_task_field(
                &direct_objects,
                "owner",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
            &direct_references,
        ),
        "SERVER SELECT query field path continues through a non-reference field",
    );
    let direct_task = direct_objects
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    assert_preparation_reason(
        version_one_query_plan(
            &direct_plan,
            &direct_function,
            std::slice::from_ref(&direct_task),
            &direct_references,
        ),
        "SERVER SELECT query field path target is absent from the candidate catalogue",
    );

    let (reference_plan, reference_function, reference_objects, _) =
        mapped_version_one_fixture_for(VERSION_ONE_REFERENCE_SOURCE);
    let reference_task = reference_objects
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    assert_preparation_reason(
        version_one_query_plan(
            &reference_plan,
            &reference_function,
            std::slice::from_ref(&reference_task),
            &version_one_query_reference_sequence(&reference_plan, &reference_function),
        ),
        "SERVER SELECT query field path target is absent from the candidate catalogue",
    );
}

#[test]
fn distinct_preparation_revalidates_candidate_facts_and_evidence() {
    let (plan, function, object_types, references) = mapped_distinct_fixture();
    let person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap()
        .clone();
    let task = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    let owner = task.field_by_name("owner").unwrap().clone();
    let completed = task.field_by_name("completed").unwrap().clone();

    let missing_field_plan = mapped_distinct_plan(&plan, Ok, |_| {
        Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
            reason: "mapping failure",
        })
    });
    assert_preparation_reason(missing_field_plan, "mapping failure");

    let missing_type_plan = mapped_distinct_plan(
        &plan,
        |_| {
            Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                reason: "type mapping failure",
            })
        },
        Ok,
    );
    assert_preparation_reason(missing_type_plan, "type mapping failure");

    let object_reference_mismatch = mapped_distinct_plan(
        &plan,
        {
            let mut calls = 0;
            let task = task.id();
            let person = person.id();
            move |_| {
                calls += 1;
                Ok(if calls == 2 { person } else { task })
            }
        },
        Ok,
    )
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &object_reference_mismatch,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&object_reference_mismatch, &function),
        ),
        "SELECT DISTINCT query object reference has inconsistent facts",
    );

    let initial_owner_mismatch = mapped_distinct_plan(
        &plan,
        {
            let mut calls = 0;
            let task = task.id();
            let person = person.id();
            move |_| {
                calls += 1;
                Ok(if calls == 3 { person } else { task })
            }
        },
        Ok,
    )
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &initial_owner_mismatch,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&initial_owner_mismatch, &function),
        ),
        "SELECT DISTINCT query field path owner differs from its source object",
    );

    for (mutation, reason) in [
        (
            crate::relational::DistinctQueryTestMutation::InvalidFieldPathInput,
            "SELECT DISTINCT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceInput,
            "SELECT DISTINCT query object reference has inconsistent facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceType,
            "SELECT DISTINCT query object reference has inconsistent facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidBooleanLiteralType,
            "SELECT DISTINCT query BOOLEAN expression has inconsistent type facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidEqualityType,
            "SELECT DISTINCT query equality expression has inconsistent type facts",
        ),
    ] {
        let malformed = plan.with_test_mutation(mutation);
        assert_preparation_reason(
            distinct_query_plan(
                &malformed,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&malformed, &function),
            ),
            reason,
        );
    }

    let unknown_field = mapped_distinct_plan(&plan, Ok, |field_id| {
        if field_id == owner.id() {
            Ok(FieldId::new())
        } else {
            Ok(field_id)
        }
    })
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &unknown_field,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&unknown_field, &function),
        ),
        "SELECT DISTINCT query field path field is absent from its source object",
    );

    let wrong_final_type = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            owner.clone(),
            FieldDefinition::new(
                completed.id(),
                completed.name(),
                completed.ordinal(),
                ResolvedType::scalar(StandardScalar::Integer),
                completed.nullable(),
                completed.unique(),
                completed.default_expression(),
                completed.on_delete(),
            ),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), wrong_final_type],
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );

    let nullable_final = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            owner.clone(),
            FieldDefinition::new(
                completed.id(),
                completed.name(),
                completed.ordinal(),
                completed.resolved_type(),
                true,
                completed.unique(),
                completed.default_expression(),
                completed.on_delete(),
            ),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), nullable_final],
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );

    let non_reference_owner = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            FieldDefinition::new(
                owner.id(),
                owner.name(),
                owner.ordinal(),
                ResolvedType::scalar(StandardScalar::Boolean),
                owner.nullable(),
                owner.unique(),
                owner.default_expression(),
                owner.on_delete(),
            ),
            completed.clone(),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), non_reference_owner],
            &references,
        ),
        "SELECT DISTINCT query field path continues through a non-reference field",
    );

    let missing_target_owner = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            FieldDefinition::new(
                owner.id(),
                owner.name(),
                owner.ordinal(),
                ResolvedType::reference(TypeId::new()),
                owner.nullable(),
                owner.unique(),
                owner.default_expression(),
                owner.on_delete(),
            ),
            completed.clone(),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), missing_target_owner],
            &references,
        ),
        "SELECT DISTINCT query field path target is absent from the candidate catalogue",
    );

    let wrong_return = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
            FunctionReturnColumnDefinition::new(
                "active",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            FunctionReturnColumnDefinition::new(
                "completed",
                2,
                ResolvedType::scalar(StandardScalar::Integer),
            ),
        ]),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &wrong_return,
            &object_types,
            &distinct_query_reference_sequence(&plan, &wrong_return),
        ),
        "SELECT DISTINCT query projection differs from its function return",
    );

    for invalid_references in [
        references[..references.len() - 1].to_vec(),
        {
            let mut extra = references.clone();
            extra.push(references[0]);
            extra
        },
        {
            let mut reordered = references.clone();
            reordered.reverse();
            reordered
        },
        {
            let mut wrong_kind = references.clone();
            wrong_kind[1].0 = DefinitionReferenceKind::QueryField;
            wrong_kind
        },
        {
            let mut wrong_target = references.clone();
            wrong_target[1].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
            wrong_target
        },
    ] {
        assert_preparation_reason(
            distinct_query_plan(&plan, &function, &object_types, &invalid_references),
            "SELECT DISTINCT definition references differ from the checked function body",
        );
    }
}

#[test]
fn distinct_preparation_has_an_exhaustive_projection_domain_and_boolean_selection() {
    let (plan, function, object_types, _) = mapped_distinct_fixture();
    let person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap();

    for scalar in StandardScalar::ALL {
        let semantic_type = SemanticType::scalar(scalar);
        let malformed = plan
            .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
            .with_test_mutation(
                crate::relational::DistinctQueryTestMutation::ProjectionType {
                    semantic_type,
                    nullable: false,
                },
            );
        let candidate =
            object_types_with_distinct_completed_type(&object_types, ResolvedType::scalar(scalar));
        let function =
            distinct_function_with_completed_type(&function, ResolvedType::scalar(scalar));
        let references = distinct_query_reference_sequence(&malformed, &function);
        let accepted = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        let result = distinct_query_plan(&malformed, &function, &candidate, &references);
        if accepted {
            assert!(result.is_ok(), "{scalar:?} must be accepted: {result:?}");
        } else {
            assert_preparation_reason(
                result,
                "SELECT DISTINCT query projection has an unsupported type",
            );
        }
    }

    let reference = SemanticType::reference(person.id());
    let reference_plan = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
        .with_test_mutation(
            crate::relational::DistinctQueryTestMutation::ProjectionType {
                semantic_type: reference,
                nullable: false,
            },
        );
    let reference_function =
        distinct_function_with_completed_type(&function, ResolvedType::reference(person.id()));
    assert!(
        distinct_query_plan(
            &reference_plan,
            &reference_function,
            &object_types_with_distinct_completed_type(
                &object_types,
                ResolvedType::reference(person.id()),
            ),
            &distinct_query_reference_sequence(&reference_plan, &reference_function),
        )
        .is_ok()
    );

    let named_plan = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
        .with_test_mutation(
            crate::relational::DistinctQueryTestMutation::ProjectionType {
                semantic_type: SemanticType::Named(person.id()),
                nullable: false,
            },
        );
    let named_function =
        distinct_function_with_completed_type(&function, ResolvedType::Named(person.id()));
    assert_preparation_reason(
        distinct_query_plan(
            &named_plan,
            &named_function,
            &object_types_with_distinct_completed_type(
                &object_types,
                ResolvedType::Named(person.id()),
            ),
            &distinct_query_reference_sequence(&named_plan, &named_function),
        ),
        "SELECT DISTINCT query projection has an unsupported type",
    );

    let non_boolean_selection = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::SelectionObjectReference);
    assert_preparation_reason(
        distinct_query_plan(
            &non_boolean_selection,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&non_boolean_selection, &function),
        ),
        "SELECT DISTINCT query selection is not BOOLEAN",
    );
}

#[test]
fn distinct_preparation_requires_the_final_projected_reference_target() {
    let (plan, function, object_types, references) =
        mapped_distinct_fixture_for(DISTINCT_REFERENCE_SOURCE);
    let task = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();

    assert_preparation_reason(
        distinct_query_plan(&plan, &function, &[task], &references),
        "SELECT DISTINCT query field path target is absent from the candidate catalogue",
    );
}

#[test]
fn identity_selected_query_replay_reuses_and_selector_rename_revises() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let initial_revision = initial.new_function_revisions()[0].clone();
    let initial_parameter = initial.candidate().functions()[0].parameters()[0].id();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

    let replay = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );
    assert_eq!(
        replay.candidate().functions()[0].parameters()[0].id(),
        initial_parameter
    );

    let renamed = prepare(
        &checked_report(
            IDENTITY_SELECTED_RENAMED_SELECTOR_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed = &renamed.new_function_revisions()[0];
    assert_eq!(
        changed.revision_number(),
        initial_revision.revision_number() + 1
    );
    assert_ne!(changed.id(), initial_revision.id());
    assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
    assert_ne!(
        changed.artifact().payload(),
        initial_revision.artifact().payload()
    );
    assert_ne!(
        renamed.candidate().functions()[0].parameters()[0].id(),
        initial_parameter
    );
    assert_eq!(changed.artifact().version(), 2);
}

#[test]
fn prepares_nullable_multi_hop_equality_projection_with_complete_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(
            IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("name").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: prepared.candidate().functions()[0].id(),
                    parameter: prepared.candidate().functions()[0].parameters()[0].id()
                }
            ),
        ]
    );
}

#[test]
fn identity_selected_validator_rejects_private_plan_and_evidence_mismatches() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared.candidate().object_types()[0].clone();
    let function = prepared.candidate().functions()[0].clone();
    let checked = checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue());
    let checked_function = &checked.checked_bundle().unwrap().server_functions()[0];
    let map = |owner, parameter, scan, field| {
        checked_function
            .identity_selected_query_plan()
            .unwrap()
            .try_map_identities(
                |_| Ok::<_, PrepareError>(scan),
                |_| Ok::<_, PrepareError>(field),
                |_| Ok::<_, PrepareError>(owner),
                |_| Ok::<_, PrepareError>(parameter),
            )
            .unwrap()
    };
    let plan = map(
        function.id(),
        function.parameters()[0].id(),
        task.id(),
        task.fields()[0].id(),
    );
    let references = identity_selected_query_reference_sequence(&plan, &function);
    let expect = |result: Result<_, PrepareError>, reason| {
        assert!(
            matches!(result, Err(PrepareError::InvalidCheckedBundle { reason: actual }) if actual == reason)
        );
    };
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                function.parameters()[0].id(),
                TypeId::new(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query scan object is absent from the candidate catalogue",
    );
    let wrong_mode = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(&plan, &wrong_mode, std::slice::from_ref(&task), &references),
        "identity-selected query function has unsupported execution modes",
    );
    let wrong_selector_type = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            function.parameters()[0].id(),
            function.parameters()[0].name(),
            0,
            ResolvedType::reference(TypeId::new()),
            None,
        )],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_selector_type,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter does not reference its scan object",
    );
    let non_rows = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(&plan, &non_rows, std::slice::from_ref(&task), &references),
        "identity-selected query function does not return ROWS",
    );
    let wrong_count = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Rows(Vec::new()),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_count,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query projection count differs from its function return",
    );
    let wrong_return_type = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
            FunctionReturnColumnDefinition::new(
                "title",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
        ]),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_return_type,
            std::slice::from_ref(&task),
            &identity_selected_query_reference_sequence(&plan, &wrong_return_type),
        ),
        "identity-selected query projection differs from its function return",
    );
    let no_parameters = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &no_parameters,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query function does not declare exactly one parameter",
    );
    let two_parameters = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![
            function.parameters()[0].clone(),
            function.parameters()[0].clone(),
        ],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &two_parameters,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query function does not declare exactly one parameter",
    );
    let default_parameter = ParameterDefinition::new(
        function.parameters()[0].id(),
        function.parameters()[0].name(),
        0,
        function.parameters()[0].resolved_type(),
        Some(ExpressionId::new()),
    );
    let with_default = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![default_parameter],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &with_default,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter has an unsupported default expression",
    );
    expect(
        identity_selected_query_plan(
            &map(
                FunctionId::new(),
                function.parameters()[0].id(),
                task.id(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector owner differs from its enclosing function",
    );
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                ParameterId::new(),
                task.id(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter is not its enclosing function parameter",
    );
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                function.parameters()[0].id(),
                task.id(),
                FieldId::new(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query field path field is absent from its source object",
    );
    let wrong_final_type = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            task.fields()[0].id(),
            task.fields()[0].name(),
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
            false,
            None,
            None,
        )],
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&wrong_final_type),
            &references,
        ),
        "identity-selected query field path type differs from its source field",
    );
    let nullable_final = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            task.fields()[0].id(),
            task.fields()[0].name(),
            0,
            task.fields()[0].resolved_type(),
            true,
            false,
            None,
            None,
        )],
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&nullable_final),
            &references,
        ),
        "identity-selected query field path type differs from its source field",
    );
    let mut wrong_evidence = references.clone();
    wrong_evidence.reverse();
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &wrong_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &references[..references.len() - 1],
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let mut extra_evidence = references.clone();
    extra_evidence.push(references[0]);
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &extra_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let mut wrong_target_evidence = references.clone();
    wrong_target_evidence[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &wrong_target_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let checked_plan = checked_function.identity_selected_query_plan().unwrap();
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Err::<TypeId, _>("type identity"),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "type identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Err::<FieldId, _>("field identity"),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "field identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Err::<FunctionId, _>("function identity"),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "function identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Err::<ParameterId, _>("parameter identity"),
            )
            .unwrap_err(),
        "parameter identity"
    );
}

#[test]
fn identity_selected_validator_rejects_multi_hop_catalogue_mismatches() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(
            IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    let person = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap()
        .clone();
    let function = prepared.candidate().functions()[0].clone();
    let checked = checked_report(
        IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
        active.catalogue(),
    );
    let checked_plan = checked.checked_bundle().unwrap().server_functions()[0]
        .identity_selected_query_plan()
        .unwrap();
    let owner_field = task.field_by_name("owner").unwrap();
    let name_field = person.field_by_name("name").unwrap();
    let map_plan = |type_ids: [TypeId; 7]| {
        let mut type_index = 0;
        let mut field_index = 0;
        let field_ids = [
            owner_field.id(),
            name_field.id(),
            owner_field.id(),
            name_field.id(),
        ];
        let plan = checked_plan
            .try_map_identities(
                |_| {
                    let mapped = type_ids[type_index];
                    type_index += 1;
                    Ok::<_, PrepareError>(mapped)
                },
                |_| {
                    let mapped = field_ids[field_index];
                    field_index += 1;
                    Ok::<_, PrepareError>(mapped)
                },
                |_| Ok::<_, PrepareError>(function.id()),
                |_| Ok::<_, PrepareError>(function.parameters()[0].id()),
            )
            .unwrap();
        assert_eq!(type_index, type_ids.len());
        assert_eq!(field_index, field_ids.len());
        plan
    };
    let exact_types = [
        task.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
    ];
    let plan = map_plan(exact_types);
    let references = identity_selected_query_reference_sequence(&plan, &function);
    let non_reference_task = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            owner_field.id(),
            owner_field.name(),
            owner_field.ordinal(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            owner_field.nullable(),
            false,
            None,
            None,
        )],
    );
    assert!(matches!(
        identity_selected_query_plan(
            &plan,
            &function,
            &[non_reference_task, person.clone()],
            &references,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query field path continues through a non-reference field"
        })
    ));

    let wrong_owner_plan = map_plan([
        task.id(),
        person.id(),
        person.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
    ]);
    assert!(matches!(
        identity_selected_query_plan(
            &wrong_owner_plan,
            &function,
            &[task, person],
            &identity_selected_query_reference_sequence(&wrong_owner_plan, &function),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query field path owner differs from its source object"
        })
    ));
}
