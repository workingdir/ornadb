use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, ParameterId, SchemaId, TypeId,
    catalogue::{
        CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
        SchemaDefinition,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{QueryExpression, SelectQuantifier, SourceSpan, parse};

use super::{
    ExpressionKind, IdentitySelectedQueryReference, IntrinsicBooleanType, NullOrder,
    QueryParameter, QueryReferenceKind, QueryReferenceTarget, SortDirection,
    UniqueTextSelectedQueryReference, ValueType, check_distinct_query_in,
    check_identity_selected_query_in, check_identity_selected_query_with_intrinsic_boolean_in,
    check_query, check_query_in, check_query_with_intrinsic_boolean_in,
    check_unique_text_selected_query_in, resolved_values_match, supports_server_select_distinct,
    supports_server_select_distinct_value, supports_server_select_equality,
    supports_server_select_equality_value,
};
use crate::DiagnosticCode;
use crate::resolver::{
    QueryCatalogue, QueryField, QueryObjectType, ResolutionCatalogue, SemanticType,
};

const TASK_TYPE: TypeId = TypeId::from_bytes([1; 16]);
const PERSON_TYPE: TypeId = TypeId::from_bytes([2; 16]);
const ASSIGNEE_FIELD: FieldId = FieldId::from_bytes([11; 16]);
const COMPLETED_FIELD: FieldId = FieldId::from_bytes([12; 16]);
const TITLE_FIELD: FieldId = FieldId::from_bytes([13; 16]);
const SCORE_FIELD: FieldId = FieldId::from_bytes([14; 16]);
const PERSON_NAME_FIELD: FieldId = FieldId::from_bytes([21; 16]);
const PERSON_ACTIVE_FIELD: FieldId = FieldId::from_bytes([22; 16]);
const SELECTOR_OWNER: FunctionId = FunctionId::from_bytes([31; 16]);
const SELECTOR_PARAMETER: ParameterId = ParameterId::from_bytes([32; 16]);

fn catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([9; 16]),
        vec![schema(1, &["tasks"]), schema(2, &["people"])],
        vec![
            ObjectTypeDefinition::new(
                TASK_TYPE,
                name(&["tasks", "task"]),
                vec![
                    field(
                        ASSIGNEE_FIELD,
                        "assignee",
                        0,
                        ResolvedType::reference(PERSON_TYPE),
                        true,
                    ),
                    field(
                        COMPLETED_FIELD,
                        "completed",
                        1,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                    field(
                        TITLE_FIELD,
                        "title",
                        2,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        true,
                    ),
                    field(
                        SCORE_FIELD,
                        "score",
                        3,
                        ResolvedType::scalar(StandardScalar::Float),
                        false,
                    ),
                ],
            ),
            ObjectTypeDefinition::new(
                PERSON_TYPE,
                name(&["people", "person"]),
                vec![
                    field(
                        PERSON_NAME_FIELD,
                        "name",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        false,
                    ),
                    field(
                        PERSON_ACTIVE_FIELD,
                        "active",
                        1,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                ],
            ),
        ],
    )
    .unwrap()
}

fn schema(id: u8, parts: &[&str]) -> SchemaDefinition {
    SchemaDefinition::new(SchemaId::from_bytes([id; 16]), name(parts))
}

fn name(parts: &[&str]) -> QualifiedSemanticName {
    QualifiedSemanticName::new(parts.iter().copied()).unwrap()
}

fn field(
    id: FieldId,
    name: &str,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
) -> FieldDefinition {
    FieldDefinition::new(
        id,
        name,
        ordinal,
        resolved_type,
        nullable,
        false,
        None,
        None,
    )
}

fn query(query_source: &str) -> orna_syntax::SelectQuery {
    let source = format!("CREATE SERVER FUNCTION tasks.query() RETURNS BOOL AS {query_source};");
    let parsed = parse(&source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    let body = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("test function has a SELECT body");
    body.query.clone()
}

fn selector_parameter(name: &str, semantic_type: SemanticType<TypeId>) -> QueryParameter {
    QueryParameter::new(name, SELECTOR_PARAMETER, semantic_type)
}

fn provenance_catalogue(
    left: Option<TypeId>,
    right: Option<TypeId>,
) -> ResolutionCatalogue<TypeId, FieldId> {
    provenance_catalogue_with_compatibility(
        (StandardScalar::Boolean, left),
        (StandardScalar::Boolean, right),
    )
}

fn provenance_catalogue_with_compatibility(
    left: (StandardScalar, Option<TypeId>),
    right: (StandardScalar, Option<TypeId>),
) -> ResolutionCatalogue<TypeId, FieldId> {
    let left = left.1.map_or_else(
        || QueryField::new(COMPLETED_FIELD, SemanticType::scalar(left.0), false),
        |type_id| {
            QueryField::new(COMPLETED_FIELD, SemanticType::scalar(left.0), false)
                .with_standard_value_type(type_id)
        },
    );
    let right = right.1.map_or_else(
        || QueryField::new(SCORE_FIELD, SemanticType::scalar(right.0), false),
        |type_id| {
            QueryField::new(SCORE_FIELD, SemanticType::scalar(right.0), false)
                .with_standard_value_type(type_id)
        },
    );
    ResolutionCatalogue::new(vec![QueryObjectType::new(
        TASK_TYPE,
        name(&["tasks", "task"]),
        vec![("left".to_owned(), left), ("right".to_owned(), right)],
    )])
    .unwrap()
}

fn reference_provenance_catalogue(
    reference_target: TypeId,
) -> ResolutionCatalogue<TypeId, FieldId> {
    ResolutionCatalogue::new(vec![
        QueryObjectType::new(
            TASK_TYPE,
            name(&["tasks", "task"]),
            vec![(
                "other".to_owned(),
                QueryField::new(
                    ASSIGNEE_FIELD,
                    SemanticType::reference(reference_target),
                    false,
                ),
            )],
        ),
        QueryObjectType::new(PERSON_TYPE, name(&["people", "person"]), Vec::new()),
    ])
    .unwrap()
}

fn inconsistent_provenance_catalogue(
    semantic_type: SemanticType<TypeId>,
) -> ResolutionCatalogue<TypeId, FieldId> {
    ResolutionCatalogue::new(vec![
        QueryObjectType::new(
            TASK_TYPE,
            name(&["tasks", "task"]),
            vec![(("value").to_owned(), {
                QueryField::new(COMPLETED_FIELD, semantic_type, false)
                    .with_standard_value_type(TypeId::from_bytes([0x61; 16]))
            })],
        ),
        QueryObjectType::new(PERSON_TYPE, name(&["people", "person"]), Vec::new()),
    ])
    .unwrap()
}

fn assert_one_diagnostic(
    diagnostics: &[crate::CompilerDiagnostic],
    code: DiagnosticCode,
    message: &str,
    span: &SourceSpan,
) {
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code(), code);
    assert_eq!(diagnostics[0].message(), message);
    assert_eq!(diagnostics[0].location().logical_path(), "tasks.orna");
    assert_eq!(diagnostics[0].location().span().start(), span.start);
    assert_eq!(diagnostics[0].location().span().end(), span.end);
}

#[test]
fn checks_the_tasks_query_with_stable_ids_and_nullability() {
    let query = query(
        "SELECT REF(t), t.assignee.name, t.completed FROM tasks.task t WHERE t.completed = FALSE ORDER BY t.assignee.name DESC",
    );

    let ir = check_query(&query, &catalogue(), "tasks.orna").unwrap();

    assert_eq!(ir.scan().object_type(), TASK_TYPE);
    let ExpressionKind::ObjectReference { input } = ir.projections()[0].kind() else {
        panic!("expected an object reference");
    };
    assert_eq!(ir.scan().input(), *input);
    let ExpressionKind::FieldPath { steps, .. } = ir.projections()[1].kind() else {
        panic!("expected a field path");
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].owner(), TASK_TYPE);
    assert_eq!(steps[0].field(), ASSIGNEE_FIELD);
    assert_eq!(steps[1].owner(), PERSON_TYPE);
    assert_eq!(steps[1].field(), PERSON_NAME_FIELD);
    assert!(ir.projections()[1].value_type().nullable());
    assert!(!ir.projections()[2].value_type().nullable());
    assert!(
        ir.selection().unwrap().value_type().resolved_type()
            == ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!ir.selection().unwrap().value_type().nullable());
    assert_eq!(ir.ordering()[0].direction(), SortDirection::Descending);
    assert_eq!(ir.ordering()[0].null_order(), NullOrder::Unspecified);
    assert!(ir.ordering()[0].expression().value_type().nullable());
}

#[test]
fn propagates_nullable_references_into_field_paths_and_equality() {
    let query = query("SELECT t.assignee.name, t.assignee = t.assignee FROM tasks.task t");

    let ir = check_query(&query, &catalogue(), "tasks.orna").unwrap();

    assert!(ir.projections()[0].value_type().nullable());
    assert!(ir.projections()[1].value_type().nullable());
}

#[test]
fn resolves_quoted_and_unquoted_identifiers_with_orna_name_rules() {
    let query = query("SELECT REF(\"t\") FROM TASKS.TASK \"t\"");

    let ir = check_query(&query, &catalogue(), "tasks.orna").unwrap();

    assert_eq!(ir.scan().object_type(), TASK_TYPE);
}

#[test]
fn rejects_unknown_ref_and_path_aliases_at_the_alias_span() {
    for query_source in [
        "SELECT REF(other) FROM tasks.task t",
        "SELECT other.title FROM tasks.task t",
    ] {
        let query = query(query_source);
        let query_start = query.span.start;
        let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

        assert_eq!(diagnostics[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            diagnostics[0].location().span().start(),
            query_start + query_source.find("other").unwrap()
        );
    }
}

#[test]
fn rejects_unknown_fields_at_the_member_span() {
    let query_source = "SELECT t.unknown FROM tasks.task t";
    let query = query(query_source);
    let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics[0].code(), DiagnosticCode::UnknownQualifiedName);
    assert_eq!(
        diagnostics[0].location().span().start(),
        query.span.start + query_source.find("unknown").unwrap()
    );
}

#[test]
fn rejects_non_reference_traversal_at_the_member_span() {
    let query_source = "SELECT t.title.name FROM tasks.task t";
    let query = query(query_source);
    let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(
        diagnostics[0].code(),
        DiagnosticCode::InvalidReferenceTarget
    );
    assert_eq!(
        diagnostics[0].location().span().start(),
        query.span.start + query_source.find("title").unwrap()
    );
}

#[test]
fn rejects_equality_with_incompatible_types() {
    let query_source = "SELECT t.completed = t.title FROM tasks.task t";
    let query = query(query_source);
    let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        diagnostics[0].location().span().start(),
        query.span.start + query_source.find("t.completed = t.title").unwrap()
    );
}

#[test]
fn rejects_inconsistent_standard_evidence_at_the_final_field() {
    let query = query("SELECT t.value FROM tasks.task t");

    let diagnostics = check_query_in(
        &query,
        &inconsistent_provenance_catalogue(SemanticType::Named(PERSON_TYPE)),
        "tasks.orna",
    )
    .unwrap_err();

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "field value has inconsistent standard value-type evidence",
        match &query.projections[0] {
            QueryExpression::FieldPath { members, .. } => &members[0].span,
            _ => unreachable!(),
        },
    );
}

#[test]
fn rejects_inconsistent_standard_evidence_at_the_intermediate_field() {
    let query = query("SELECT t.value.next FROM tasks.task t");

    let diagnostics = check_query_in(
        &query,
        &inconsistent_provenance_catalogue(SemanticType::reference(PERSON_TYPE)),
        "tasks.orna",
    )
    .unwrap_err();

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "field value has inconsistent standard value-type evidence",
        match &query.projections[0] {
            QueryExpression::FieldPath { members, .. } => &members[0].span,
            _ => unreachable!(),
        },
    );
}

#[test]
fn standard_value_equality_requires_matching_supplied_type_ids() {
    let query_source = "SELECT t.left = t.right FROM tasks.task t";
    let query = query(query_source);
    let boolean = TypeId::from_bytes([0x41; 16]);
    let other_boolean = TypeId::from_bytes([0x42; 16]);

    let diagnostics = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(Some(boolean), Some(other_boolean)),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "equality requires expressions with compatible types",
        query.projections[0].span(),
    );

    let checked = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(Some(boolean), Some(boolean)),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap();
    assert_eq!(
        checked.plan().projections()[0]
            .value_type()
            .standard_value_type(),
        Some(boolean)
    );

    let compatibility_mismatch = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue_with_compatibility(
            (StandardScalar::Boolean, Some(boolean)),
            (StandardScalar::Integer, Some(boolean)),
        ),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap();
    assert_eq!(
        compatibility_mismatch.plan().projections()[0]
            .value_type()
            .standard_value_type(),
        Some(boolean)
    );

    let mixed_diagnostics = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(Some(boolean), None),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap_err();
    assert_one_diagnostic(
        &mixed_diagnostics,
        DiagnosticCode::TypeMismatch,
        "equality requires expressions with compatible types",
        query.projections[0].span(),
    );
}

#[test]
fn standard_ref_equality_remains_keyed_by_the_checked_object_target() {
    let query = query("SELECT REF(t) = t.other FROM tasks.task t");
    let boolean = TypeId::from_bytes([0x43; 16]);

    let checked = check_query_with_intrinsic_boolean_in(
        &query,
        &reference_provenance_catalogue(TASK_TYPE),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap();

    assert_eq!(
        checked.plan().projections()[0]
            .value_type()
            .standard_value_type(),
        Some(boolean)
    );
    assert!(matches!(
        checked.plan().projections()[0].kind(),
        ExpressionKind::Equality { left, right }
            if left.value_type().semantic_type() == SemanticType::reference(TASK_TYPE)
                && right.value_type().semantic_type() == SemanticType::reference(TASK_TYPE)
    ));

    let diagnostics = check_query_with_intrinsic_boolean_in(
        &query,
        &reference_provenance_catalogue(PERSON_TYPE),
        "tasks.orna",
        IntrinsicBooleanType::Standard(boolean),
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "equality requires expressions with compatible types",
        query.projections[0].span(),
    );
}

#[test]
fn standard_boolean_expressions_fail_closed_when_the_contract_is_missing() {
    let query = query("SELECT TRUE FROM tasks.task t");

    let diagnostics = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(None, None),
        "tasks.orna",
        IntrinsicBooleanType::Missing,
    )
    .unwrap_err();

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        query.projections[0].span(),
    );
}

#[test]
fn missing_standard_boolean_reports_an_equality_before_both_literals() {
    let query = query("SELECT TRUE = FALSE FROM tasks.task t");

    let diagnostics = check_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(None, None),
        "tasks.orna",
        IntrinsicBooleanType::Missing,
    )
    .unwrap_err();

    assert_eq!(diagnostics.len(), 3);
    let [parent, left, right] = diagnostics.as_slice() else {
        return;
    };
    for diagnostic in [parent, left, right] {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
    }
    assert_one_diagnostic(
        std::slice::from_ref(parent),
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        query.projections[0].span(),
    );
    let QueryExpression::Equality {
        left: source_left,
        right: source_right,
        ..
    } = &query.projections[0]
    else {
        return;
    };
    assert_one_diagnostic(
        std::slice::from_ref(left),
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        source_left.span(),
    );
    assert_one_diagnostic(
        std::slice::from_ref(right),
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        source_right.span(),
    );
}

#[test]
fn missing_standard_boolean_reports_identity_projection_before_selector_equality() {
    let query = query("SELECT TRUE FROM tasks.task t WHERE REF(t) = p_task");

    let diagnostics = check_identity_selected_query_with_intrinsic_boolean_in(
        &query,
        &provenance_catalogue(None, None),
        SELECTOR_OWNER,
        &[selector_parameter(
            "p_task",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
        IntrinsicBooleanType::Missing,
    )
    .unwrap_err();

    assert_eq!(diagnostics.len(), 2);
    let [projection, selector] = diagnostics.as_slice() else {
        return;
    };
    for diagnostic in [projection, selector] {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
    }
    assert_one_diagnostic(
        std::slice::from_ref(projection),
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        query.projections[0].span(),
    );
    let Some(selector_expression) = query.predicate.as_ref() else {
        return;
    };
    assert_one_diagnostic(
        std::slice::from_ref(selector),
        DiagnosticCode::DomainIncompatible,
        "the checked standard library does not provide a Boolean value type",
        selector_expression.span(),
    );
}

#[test]
fn rejects_unsupported_server_equality_types_in_v1_and_v2() {
    let message =
        "SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values";
    for expression in ["t.title = t.title", "t.score = t.score"] {
        let v1 = query(&format!("SELECT {expression} FROM tasks.task t"));
        let diagnostics = check_query(&v1, &catalogue(), "tasks.orna").unwrap_err();
        assert_one_diagnostic(
            &diagnostics,
            DiagnosticCode::DomainIncompatible,
            message,
            v1.projections[0].span(),
        );

        let v2 = query(&format!(
            "SELECT {expression} FROM tasks.task t WHERE REF(t) = p_task"
        ));
        let diagnostics = check_identity_selected_query_in(
            &v2,
            &catalogue(),
            SELECTOR_OWNER,
            &[selector_parameter(
                "p_task",
                SemanticType::reference(TASK_TYPE),
            )],
            "tasks.orna",
        )
        .unwrap_err();
        assert_one_diagnostic(
            &diagnostics,
            DiagnosticCode::DomainIncompatible,
            message,
            v2.projections[0].span(),
        );
    }
}

#[test]
fn rejects_distinct_before_v1_source_name_or_expression_checks() {
    let query = query("SELECT DISTINCT missing.unknown FROM missing.object missing");
    let SelectQuantifier::Distinct { source } = &query.quantifier else {
        panic!("fixture must retain DISTINCT");
    };

    let result = check_query(&query, &catalogue(), "tasks.orna");
    let diagnostics = result.expect_err("DISTINCT must not encode the v1 relational IR");

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "SELECT DISTINCT is not available yet",
        &source.span,
    );
}

#[test]
fn rejects_distinct_before_identity_selected_shape_or_semantic_checks() {
    let query = query("SELECT DISTINCT missing.unknown FROM missing.object missing");
    let SelectQuantifier::Distinct { source } = &query.quantifier else {
        panic!("fixture must retain DISTINCT");
    };

    let result = check_identity_selected_query_in(
        &query,
        &catalogue(),
        SELECTOR_OWNER,
        &[] as &[QueryParameter],
        "tasks.orna",
    );
    let diagnostics = result.expect_err("DISTINCT must not encode the identity-selected IR");

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "SELECT DISTINCT is not available yet",
        &source.span,
    );
}

#[test]
fn checks_distinct_query_with_nullable_projections_and_a_predicate() {
    let query = query(
        "SELECT DISTINCT t.completed, t.assignee FROM tasks.task t WHERE t.completed = FALSE",
    );
    let check = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap();
    let plan = check.plan();

    assert_eq!(plan.scan().object_type(), TASK_TYPE);
    assert_eq!(plan.projections().len(), 2);
    assert_eq!(
        plan.projections()[0].value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.projections()[0].value_type().nullable());
    assert_eq!(
        plan.projections()[1].value_type().semantic_type(),
        SemanticType::reference(PERSON_TYPE)
    );
    assert!(plan.projections()[1].value_type().nullable());
    assert!(matches!(
        plan.selection().map(|selection| selection.kind()),
        Some(ExpressionKind::Equality { .. })
    ));
    assert!(!plan.selection().unwrap().value_type().nullable());
}

#[test]
fn checks_direct_boolean_distinct_predicates_with_exact_types() {
    let root = query("SELECT DISTINCT t.completed FROM tasks.task t WHERE t.completed");
    let root = check_distinct_query_in(&root, &catalogue(), "tasks.orna").unwrap();
    let root_selection = root.plan().selection().expect("fixture has a selection");
    let ExpressionKind::FieldPath { steps, .. } = root_selection.kind() else {
        panic!("root predicate must be a field path");
    };
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner(), step.field()))
            .collect::<Vec<_>>(),
        vec![(TASK_TYPE, COMPLETED_FIELD)]
    );
    assert_eq!(
        root_selection.value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(!root_selection.value_type().nullable());

    let nullable = query("SELECT DISTINCT t.completed FROM tasks.task t WHERE t.assignee.active");
    let nullable = check_distinct_query_in(&nullable, &catalogue(), "tasks.orna").unwrap();
    let nullable_selection = nullable
        .plan()
        .selection()
        .expect("fixture has a selection");
    let ExpressionKind::FieldPath { steps, .. } = nullable_selection.kind() else {
        panic!("nullable predicate must be a field path");
    };
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner(), step.field()))
            .collect::<Vec<_>>(),
        vec![
            (TASK_TYPE, ASSIGNEE_FIELD),
            (PERSON_TYPE, PERSON_ACTIVE_FIELD),
        ]
    );
    assert_eq!(
        nullable_selection.value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(nullable_selection.value_type().nullable());

    for (source, expected) in [("TRUE", true), ("FALSE", false)] {
        let literal = query(&format!(
            "SELECT DISTINCT t.completed FROM tasks.task t WHERE {source}"
        ));
        let literal = check_distinct_query_in(&literal, &catalogue(), "tasks.orna").unwrap();
        let selection = literal.plan().selection().expect("fixture has a selection");
        assert!(matches!(
            selection.kind(),
            ExpressionKind::BooleanLiteral { value } if *value == expected
        ));
        assert_eq!(
            selection.value_type().semantic_type(),
            SemanticType::scalar(StandardScalar::Boolean)
        );
        assert!(!selection.value_type().nullable());
        assert_eq!(literal.references().len(), 2);
        assert_eq!(
            literal.references()[0].kind(),
            QueryReferenceKind::QueryObject
        );
        assert_eq!(
            literal.references()[0].target(),
            &QueryReferenceTarget::Object(TASK_TYPE)
        );
        assert_eq!(
            literal.references()[1].kind(),
            QueryReferenceKind::QueryField
        );
        assert_eq!(
            literal.references()[1].target(),
            &QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            }
        );
    }
}

#[test]
fn rejects_distinct_closed_shapes_before_source_resolution() {
    let mut empty = query("SELECT DISTINCT t.completed FROM missing.object t");
    empty.projections.clear();
    let diagnostics = check_distinct_query_in(&empty, &catalogue(), "tasks.orna").unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "SELECT DISTINCT requires at least one projection",
        &empty.span,
    );

    let mut ordering = query("SELECT DISTINCT t.completed FROM missing.object t");
    ordering.ordering =
        query("SELECT t.completed FROM missing.object t ORDER BY t.completed").ordering;
    let ordering_span = ordering.ordering[0].span.clone();
    let diagnostics = check_distinct_query_in(&ordering, &catalogue(), "tasks.orna").unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "SELECT DISTINCT queries do not allow ORDER BY",
        &ordering_span,
    );
}

#[test]
fn distinct_checker_requires_the_distinct_quantifier_before_source_resolution() {
    let query = query("SELECT t.completed FROM missing.object t");
    let diagnostics = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "this query must use SELECT DISTINCT",
        &query.span,
    );
}

#[test]
fn records_distinct_evidence_in_source_projection_predicate_order() {
    let source = "SELECT DISTINCT REF(t), t.assignee, t.completed FROM tasks.task t WHERE t.completed = t.completed";
    let query = query(source);
    let check = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap();
    let references = check.references();
    let completed_starts = source
        .match_indices("t.completed")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    let expected = [
        (
            QueryReferenceKind::QueryObject,
            QueryReferenceTarget::Object(TASK_TYPE),
            source.find("tasks.task").unwrap(),
            "tasks.task".len(),
        ),
        (
            QueryReferenceKind::ObjectReference,
            QueryReferenceTarget::Object(TASK_TYPE),
            source.find("REF(t)").unwrap() + "REF(".len(),
            "t".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: ASSIGNEE_FIELD,
            },
            source.find("t.assignee").unwrap() + "t.".len(),
            "assignee".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            },
            completed_starts[0] + "t.".len(),
            "completed".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            },
            completed_starts[1] + "t.".len(),
            "completed".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            },
            completed_starts[2] + "t.".len(),
            "completed".len(),
        ),
    ];

    assert_eq!(references.len(), expected.len());
    for (reference, (kind, target, offset, length)) in references.iter().zip(expected) {
        assert_eq!(reference.kind(), kind);
        assert_eq!(reference.target(), &target);
        assert_eq!(reference.location().logical_path(), "tasks.orna");
        assert_eq!(
            reference.location().span().start(),
            query.span.start + offset
        );
        assert_eq!(
            reference.location().span().end(),
            query.span.start + offset + length
        );
    }
}

#[test]
fn reports_unsupported_distinct_projections_in_projection_order() {
    let query = query("SELECT DISTINCT t.title, t.score FROM tasks.task t");
    let diagnostics = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics.len(), 2);
    for (diagnostic, projection) in diagnostics.iter().zip(&query.projections) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
        );
        assert_eq!(diagnostic.location().logical_path(), "tasks.orna");
        assert_eq!(
            diagnostic.location().span().start(),
            projection.span().start
        );
        assert_eq!(diagnostic.location().span().end(), projection.span().end);
    }
}

#[test]
fn returns_source_semantic_diagnostics_before_distinct_domain_errors() {
    let query = query("SELECT DISTINCT t.title FROM missing.object t");
    let diagnostics = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::UnknownQualifiedName,
        "unknown object type missing.object",
        &query.source_object.object_type.span,
    );
}

#[test]
fn returns_projection_and_predicate_semantic_diagnostics_before_distinct_domain_errors() {
    let query =
        query("SELECT DISTINCT t.unknown, t.title FROM tasks.task t WHERE t.score = t.completed");
    let diagnostics = check_distinct_query_in(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics.len(), 2);
    assert_one_diagnostic(
        &diagnostics[..1],
        DiagnosticCode::UnknownQualifiedName,
        "unknown field unknown on tasks.task",
        match &query.projections[0] {
            QueryExpression::FieldPath { members, .. } => &members[0].span,
            _ => unreachable!(),
        },
    );
    assert_one_diagnostic(
        &diagnostics[1..],
        DiagnosticCode::TypeMismatch,
        "equality requires expressions with compatible types",
        query.predicate.as_ref().unwrap().span(),
    );
}

#[test]
fn distinct_type_domain_is_exact_and_independent_from_equality() {
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        assert_eq!(
            supports_server_select_distinct(SemanticType::<TypeId>::scalar(scalar)),
            expected,
            "unexpected DISTINCT support for {scalar:?}"
        );
    }
    assert!(supports_server_select_distinct(SemanticType::reference(
        TASK_TYPE
    )));
    assert!(!supports_server_select_distinct(SemanticType::Named(
        TASK_TYPE
    )));
}

#[test]
fn resolved_value_projection_preserves_identity_and_legacy_artifact_types() {
    let supplied = TypeId::from_bytes([0x51; 16]);
    let other = TypeId::from_bytes([0x52; 16]);
    let standard = ValueType::standard_value(supplied, StandardScalar::Boolean, false);
    let same = ValueType::standard_value(supplied, StandardScalar::Boolean, false);
    let different = ValueType::standard_value(other, StandardScalar::Boolean, false);
    let legacy = ValueType::legacy_scalar(StandardScalar::Boolean, false);

    assert_eq!(standard.resolved_value(), same.resolved_value());
    assert_ne!(standard.resolved_value(), different.resolved_value());
    assert_ne!(standard.resolved_value(), legacy.resolved_value());
    assert!(resolved_values_match(
        standard.resolved_value(),
        ValueType::standard_value(supplied, StandardScalar::Integer, false).resolved_value(),
    ));
    assert!(!resolved_values_match(
        standard.resolved_value(),
        different.resolved_value(),
    ));
    assert!(!resolved_values_match(
        standard.resolved_value(),
        legacy.resolved_value(),
    ));
    assert_eq!(
        standard.legacy_artifact_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(
        ValueType::named(TASK_TYPE, false).legacy_artifact_type(),
        ResolvedType::Named(TASK_TYPE)
    );
    assert_eq!(
        ValueType::reference(PERSON_TYPE, true).legacy_artifact_type(),
        ResolvedType::reference(PERSON_TYPE)
    );
}

#[test]
fn standard_and_legacy_value_allowlists_are_exact() {
    let standard_id = TypeId::from_bytes([0x53; 16]);
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        let legacy = ValueType::<TypeId>::legacy_scalar(scalar, false);
        let standard = ValueType::<TypeId>::standard_value(standard_id, scalar, false);
        assert_eq!(supports_server_select_equality_value(legacy), expected);
        assert_eq!(supports_server_select_distinct_value(legacy), expected);
        assert_eq!(supports_server_select_equality_value(standard), expected);
        assert_eq!(supports_server_select_distinct_value(standard), expected);
    }
    let reference = ValueType::reference(TASK_TYPE, false);
    assert!(supports_server_select_equality_value(reference));
    assert!(supports_server_select_distinct_value(reference));
    let named = ValueType::named(TASK_TYPE, false);
    assert!(!supports_server_select_equality_value(named));
    assert!(!supports_server_select_distinct_value(named));
}

#[test]
fn maps_distinct_identities_and_rejects_each_type_or_field_failure() {
    let query =
        query("SELECT DISTINCT REF(t), t.assignee FROM tasks.task t WHERE t.completed = FALSE");
    let plan = check_distinct_query_in(&query, &catalogue(), "tasks.orna")
        .unwrap()
        .plan()
        .clone();

    let mapped = plan
        .try_map_identities(
            |type_id| Ok::<u8, &str>(type_id.to_bytes()[0]),
            |field_id| Ok::<u16, &str>(field_id.to_bytes()[0].into()),
        )
        .unwrap();
    assert_eq!(mapped.scan().object_type(), TASK_TYPE.to_bytes()[0]);
    let ExpressionKind::FieldPath { steps, .. } = mapped.projections()[1].kind() else {
        panic!("second projection must remain a field path");
    };
    assert_eq!(steps[0].owner(), TASK_TYPE.to_bytes()[0]);
    assert_eq!(steps[0].field(), ASSIGNEE_FIELD.to_bytes()[0] as u16);

    assert_eq!(
        plan.try_map_identities(|_| Err::<u8, _>("type"), |_| Ok::<u16, &str>(0),),
        Err("type")
    );
    assert_eq!(
        plan.try_map_identities(|_| Ok::<u8, &str>(0), |_| Err::<u16, _>("field"),),
        Err("field")
    );

    let mut type_callbacks = 0;
    assert_eq!(
        plan.try_map_identities(
            |_| {
                type_callbacks += 1;
                if type_callbacks == 2 {
                    Err("later type")
                } else {
                    Ok(0_u8)
                }
            },
            |_| Ok::<u16, &str>(0),
        ),
        Err("later type")
    );
    assert_eq!(type_callbacks, 2);

    let mut field_callbacks = 0;
    assert_eq!(
        plan.try_map_identities(
            |_| Ok::<u8, &str>(0),
            |_| {
                field_callbacks += 1;
                if field_callbacks == 2 {
                    Err("later field")
                } else {
                    Ok(0_u16)
                }
            },
        ),
        Err("later field")
    );
    assert_eq!(field_callbacks, 2);
}

#[test]
fn server_select_equality_allowlist_is_exact() {
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        assert_eq!(
            supports_server_select_equality(SemanticType::<TypeId>::scalar(scalar)),
            expected,
            "unexpected equality support for {scalar:?}"
        );
    }
    assert!(supports_server_select_equality(SemanticType::reference(
        TASK_TYPE
    )));
    assert!(!supports_server_select_equality(SemanticType::Named(
        TASK_TYPE
    )));
}

#[test]
fn rejects_a_non_boolean_where_expression() {
    let mut duplicate_preserving =
        query("SELECT t.title FROM tasks.task t WHERE t.completed = FALSE");
    duplicate_preserving.predicate = Some(duplicate_preserving.projections[0].clone());

    let diagnostics = check_query(&duplicate_preserving, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        diagnostics[0].message(),
        "WHERE requires a BOOLEAN expression"
    );
    assert_eq!(
        diagnostics[0].location().span().start(),
        duplicate_preserving.projections[0].span().start
    );

    let distinct = query("SELECT DISTINCT t.completed FROM tasks.task t WHERE t.title");
    let diagnostics = check_distinct_query_in(&distinct, &catalogue(), "tasks.orna").unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "WHERE requires a BOOLEAN expression",
        distinct.predicate.as_ref().unwrap().span(),
    );
}

#[test]
fn accepts_a_nullable_boolean_where_expression() {
    let query = query("SELECT t.title FROM tasks.task t WHERE t.assignee = t.assignee");

    let ir = check_query(&query, &catalogue(), "tasks.orna").unwrap();

    assert!(ir.selection().unwrap().value_type().nullable());
}

#[test]
fn rejects_unchecked_selector_parameters_at_the_exact_parameter_span() {
    let query = query("SELECT REF(t) FROM tasks.task t WHERE REF(t) = p_task");
    let expected_span = match query.predicate.as_ref() {
        Some(QueryExpression::Equality { right, .. }) => right.span().clone(),
        _ => panic!("fixture must contain the selector parameter"),
    };

    let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostics[0].message(),
        "SERVER SELECT parameter selectors are not yet supported"
    );
    assert_eq!(diagnostics[0].location().logical_path(), "tasks.orna");
    assert_eq!(
        diagnostics[0].location().span().start(),
        expected_span.start
    );
    assert_eq!(diagnostics[0].location().span().end(), expected_span.end);
}

#[test]
fn checks_identity_selected_query_with_one_ordered_evidence_sequence() {
    let source = "SELECT REF(t), t.title FROM tasks.task t WHERE REF(t) = P_TASK";
    let query = query(source);
    let check = check_identity_selected_query_in(
        &query,
        &catalogue(),
        SELECTOR_OWNER,
        &[selector_parameter(
            "p_task",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
    )
    .unwrap();

    assert_eq!(check.plan().scan().object_type(), TASK_TYPE);
    assert_eq!(check.plan().projections().len(), 2);
    assert_eq!(check.plan().selector().owner(), SELECTOR_OWNER);
    assert_eq!(check.plan().selector().parameter(), SELECTOR_PARAMETER);

    let references = check.references();
    assert_eq!(references.len(), 5);
    let parameter_span = match query.predicate.as_ref() {
        Some(QueryExpression::Equality { right, .. }) => right.span(),
        _ => unreachable!(),
    };
    let expected = [
        (source.find("tasks.task").unwrap(), "tasks.task".len()),
        (source.find("REF(t)").unwrap() + 4, 1),
        (source.find("title").unwrap(), "title".len()),
        (source.rfind("REF(t)").unwrap() + 4, 1),
        (
            parameter_span.start - query.span.start,
            parameter_span.end - parameter_span.start,
        ),
    ];
    for (reference, (offset, length)) in references.iter().zip(expected) {
        assert_eq!(reference.location().logical_path(), "tasks.orna");
        assert_eq!(
            reference.location().span().start(),
            query.span.start + offset
        );
        assert_eq!(
            reference.location().span().end(),
            query.span.start + offset + length
        );
    }
    assert!(matches!(
        references[0],
        IdentitySelectedQueryReference::QueryObject {
            object_type: TASK_TYPE,
            ..
        }
    ));
    assert!(matches!(
        references[1],
        IdentitySelectedQueryReference::ObjectReference {
            object_type: TASK_TYPE,
            ..
        }
    ));
    assert!(matches!(
        references[2],
        IdentitySelectedQueryReference::QueryField {
            owner: TASK_TYPE,
            field: TITLE_FIELD,
            ..
        }
    ));
    assert!(matches!(
        references[3],
        IdentitySelectedQueryReference::ObjectReference {
            object_type: TASK_TYPE,
            ..
        }
    ));
    assert!(matches!(
        references[4],
        IdentitySelectedQueryReference::ParameterRead {
            owner: SELECTOR_OWNER,
            parameter: SELECTOR_PARAMETER,
            ..
        }
    ));
}

#[test]
fn checks_unique_text_selected_query_with_direct_field_facts_and_evidence() {
    let email = FieldId::from_bytes([24; 16]);
    let catalogue = ResolutionCatalogue::new(vec![QueryObjectType::new(
        TASK_TYPE,
        name(&["tasks", "task"]),
        vec![
            (
                "title".to_owned(),
                QueryField::new(
                    TITLE_FIELD,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
            ),
            (
                "email".to_owned(),
                QueryField::new(
                    email,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                )
                .with_unique(),
            ),
        ],
    )])
    .unwrap();
    let source = "SELECT t.title FROM tasks.task t WHERE t.email = p_email";
    let query = query(source);
    let check = check_unique_text_selected_query_in(
        &query,
        &catalogue,
        SELECTOR_OWNER,
        &[QueryParameter::new(
            "p_email",
            SELECTOR_PARAMETER,
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
        )
        .with_required_non_null()],
        "tasks.orna",
    )
    .unwrap();

    assert_eq!(check.plan().scan().object_type(), TASK_TYPE);
    assert_eq!(check.plan().projections().len(), 1);
    let selector = check.plan().selector();
    assert_eq!(selector.scan_object_type(), TASK_TYPE);
    assert_eq!(selector.field_owner(), TASK_TYPE);
    assert_eq!(selector.field(), email);
    assert_eq!(selector.parameter_owner(), SELECTOR_OWNER);
    assert_eq!(selector.parameter(), SELECTOR_PARAMETER);
    assert_eq!(
        selector.text_type().semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(selector.field_nullable());
    assert!(selector.parameter_required_non_null());

    let references = check.references();
    assert_eq!(references.len(), 4);
    assert!(matches!(
        references[0],
        UniqueTextSelectedQueryReference::QueryObject {
            object_type: TASK_TYPE,
            ..
        }
    ));
    assert!(matches!(
        references[1],
        UniqueTextSelectedQueryReference::QueryField {
            owner: TASK_TYPE,
            field: TITLE_FIELD,
            ..
        }
    ));
    assert!(matches!(
        references[2],
        UniqueTextSelectedQueryReference::QueryField {
            owner: TASK_TYPE,
            field,
            ..
        } if field == email
    ));
    assert!(matches!(
        references[3],
        UniqueTextSelectedQueryReference::ParameterRead {
            owner: SELECTOR_OWNER,
            parameter: SELECTOR_PARAMETER,
            ..
        }
    ));
    assert_eq!(
        references[2].location().span().start(),
        query.span.start + source.rfind("t.email").unwrap() + 2
    );
    assert_eq!(
        references[3].location().span().start(),
        query.span.start + source.rfind("p_email").unwrap()
    );

    let mapped = check
        .plan()
        .try_map_identities(
            |_| Ok::<u8, ()>(1),
            |_| Ok::<u16, ()>(2),
            |_| Ok::<u32, ()>(3),
            |_| Ok::<u64, ()>(4),
        )
        .unwrap();
    assert_eq!(mapped.selector().scan_object_type(), 1);
    assert_eq!(mapped.selector().field_owner(), 1);
    assert_eq!(mapped.selector().field(), 2);
    assert_eq!(mapped.selector().parameter_owner(), 3);
    assert_eq!(mapped.selector().parameter(), 4);
}

#[test]
fn unique_text_selected_query_requires_matching_v2_text_value_identity() {
    let email = FieldId::from_bytes([24; 16]);
    let text = TypeId::from_bytes([0xa1; 16]);
    let other_text = TypeId::from_bytes([0xa2; 16]);
    let catalogue = ResolutionCatalogue::new(vec![QueryObjectType::new(
        TASK_TYPE,
        name(&["tasks", "task"]),
        vec![
            (
                "title".to_owned(),
                QueryField::new(
                    TITLE_FIELD,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )
                .with_standard_value_type(text),
            ),
            (
                "email".to_owned(),
                QueryField::new(
                    email,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )
                .with_standard_value_type(text)
                .with_unique(),
            ),
        ],
    )])
    .unwrap();
    let query = query("SELECT t.title FROM tasks.task t WHERE t.email = p_email");
    let parameter = |type_id| {
        QueryParameter::new(
            "p_email",
            SELECTOR_PARAMETER,
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
        )
        .with_standard_value_type(type_id)
        .with_required_non_null()
    };

    let v2 = check_unique_text_selected_query_in(
        &query,
        &catalogue,
        SELECTOR_OWNER,
        &[parameter(text)],
        "tasks.orna",
    )
    .unwrap();
    assert_eq!(
        v2.plan().selector().text_type().standard_value_type(),
        Some(text)
    );

    let diagnostics = check_unique_text_selected_query_in(
        &query,
        &catalogue,
        SELECTOR_OWNER,
        &[parameter(other_text)],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "selector parameter p_email must use the selected field's exact TEXT type",
        match query.predicate.as_ref() {
            Some(QueryExpression::Equality { right, .. }) => right.span(),
            _ => unreachable!(),
        },
    );

    let legacy_parameter = QueryParameter::new(
        "p_email",
        SELECTOR_PARAMETER,
        SemanticType::scalar(StandardScalar::CharacterLargeObject),
    )
    .with_required_non_null();
    let diagnostics = check_unique_text_selected_query_in(
        &query,
        &catalogue,
        SELECTOR_OWNER,
        &[legacy_parameter],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "selector parameter p_email must use the selected field's exact TEXT type",
        match query.predicate.as_ref() {
            Some(QueryExpression::Equality { right, .. }) => right.span(),
            _ => unreachable!(),
        },
    );

    let legacy_catalogue = ResolutionCatalogue::new(vec![QueryObjectType::new(
        TASK_TYPE,
        name(&["tasks", "task"]),
        vec![
            (
                "title".to_owned(),
                QueryField::new(
                    TITLE_FIELD,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
            ),
            (
                "email".to_owned(),
                QueryField::new(
                    email,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )
                .with_unique(),
            ),
        ],
    )])
    .unwrap();
    let diagnostics = check_unique_text_selected_query_in(
        &query,
        &legacy_catalogue,
        SELECTOR_OWNER,
        &[parameter(text)],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::TypeMismatch,
        "selector parameter p_email must use the selected field's exact TEXT type",
        match query.predicate.as_ref() {
            Some(QueryExpression::Equality { right, .. }) => right.span(),
            _ => unreachable!(),
        },
    );
}

#[test]
fn rejects_identity_selected_query_shapes_parameters_and_aliases_at_exact_locations() {
    let valid = query("SELECT REF(t) FROM tasks.task t WHERE REF(t) = p_task");
    let parameter = selector_parameter("p_task", SemanticType::reference(TASK_TYPE));
    let shape = "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter";

    let missing = query("SELECT REF(t) FROM tasks.task t");
    let diagnostics = check_identity_selected_query_in(
        &missing,
        &catalogue(),
        SELECTOR_OWNER,
        std::slice::from_ref(&parameter),
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        shape,
        &missing.span,
    );

    let mut non_equality = valid.clone();
    let predicate_span = non_equality.predicate.as_ref().unwrap().span().clone();
    non_equality.predicate = Some(QueryExpression::BooleanLiteral {
        value: true,
        source: orna_syntax::SourceSlice {
            text: "TRUE".to_owned(),
            span: predicate_span.clone(),
        },
    });
    let diagnostics = check_identity_selected_query_in(
        &non_equality,
        &catalogue(),
        SELECTOR_OWNER,
        std::slice::from_ref(&parameter),
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        shape,
        &predicate_span,
    );

    let mut wrong_left = valid.clone();
    let mut field_query = query("SELECT t.title FROM tasks.task t");
    let field_path = field_query.projections.remove(0);
    let left_span = field_path.span().clone();
    let QueryExpression::Equality { left, .. } = wrong_left.predicate.as_mut().unwrap() else {
        unreachable!()
    };
    **left = field_path;
    let diagnostics = check_identity_selected_query_in(
        &wrong_left,
        &catalogue(),
        SELECTOR_OWNER,
        std::slice::from_ref(&parameter),
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        shape,
        &left_span,
    );

    let mut wrong_right = valid.clone();
    let right_span = match wrong_right.predicate.as_ref().unwrap() {
        QueryExpression::Equality { right, .. } => right.span().clone(),
        _ => unreachable!(),
    };
    let QueryExpression::Equality { right, .. } = wrong_right.predicate.as_mut().unwrap() else {
        unreachable!()
    };
    **right = QueryExpression::BooleanLiteral {
        value: true,
        source: orna_syntax::SourceSlice {
            text: "TRUE".to_owned(),
            span: right_span.clone(),
        },
    };
    let diagnostics = check_identity_selected_query_in(
        &wrong_right,
        &catalogue(),
        SELECTOR_OWNER,
        std::slice::from_ref(&parameter),
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        shape,
        &right_span,
    );

    let mut parameter_projection = valid.clone();
    let parameter_span = match parameter_projection.predicate.as_ref().unwrap() {
        QueryExpression::Equality { right, .. } => right.span().clone(),
        _ => unreachable!(),
    };
    parameter_projection.projections[0] = match parameter_projection.predicate.as_ref().unwrap() {
        QueryExpression::Equality { right, .. } => right.as_ref().clone(),
        _ => unreachable!(),
    };
    let diagnostics = check_identity_selected_query_in(
        &parameter_projection,
        &catalogue(),
        SELECTOR_OWNER,
        std::slice::from_ref(&parameter),
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "parameterised SELECT SERVER functions permit a parameter read only as the right operand of WHERE REF(source_alias) = selector_parameter",
        &parameter_span,
    );

    for parameters in [Vec::new(), vec![parameter.clone(), parameter.clone()]] {
        let diagnostics = check_identity_selected_query_in(
            &valid,
            &catalogue(),
            SELECTOR_OWNER,
            &parameters,
            "tasks.orna",
        )
        .unwrap_err();
        assert_one_diagnostic(
            &diagnostics,
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require exactly one declared parameter",
            &valid.span,
        );
    }

    let selector_span = match valid.predicate.as_ref().unwrap() {
        QueryExpression::Equality { right, .. } => right.span().clone(),
        _ => unreachable!(),
    };
    let diagnostics = check_identity_selected_query_in(
        &valid,
        &catalogue(),
        SELECTOR_OWNER,
        &[selector_parameter(
            "other",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::UnknownQualifiedName,
        "this function has no parameter named p_task",
        &selector_span,
    );

    for semantic_type in [
        SemanticType::scalar(StandardScalar::Boolean),
        SemanticType::reference(PERSON_TYPE),
    ] {
        let diagnostics = check_identity_selected_query_in(
            &valid,
            &catalogue(),
            SELECTOR_OWNER,
            &[selector_parameter("p_task", semantic_type)],
            "tasks.orna",
        )
        .unwrap_err();
        assert_one_diagnostic(
            &diagnostics,
            DiagnosticCode::TypeMismatch,
            "selector parameter p_task must use REF tasks.task",
            &selector_span,
        );
    }

    let alias = query("SELECT REF(t) FROM tasks.task t WHERE REF(other) = p_task");
    let alias_span = match alias.predicate.as_ref().unwrap() {
        QueryExpression::Equality { left, .. } => match left.as_ref() {
            QueryExpression::ObjectReference { alias, .. } => alias.span.clone(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    let diagnostics = check_identity_selected_query_in(
        &alias,
        &catalogue(),
        SELECTOR_OWNER,
        &[selector_parameter(
            "p_task",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::UnknownQualifiedName,
        "unknown query alias other",
        &alias_span,
    );

    let mut ordering = query("SELECT REF(t) FROM tasks.task t ORDER BY t.title");
    ordering.predicate = valid.predicate.clone();
    let diagnostics = check_identity_selected_query_in(
        &ordering,
        &catalogue(),
        SELECTOR_OWNER,
        &[selector_parameter(
            "p_task",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
    )
    .unwrap_err();
    assert_one_diagnostic(
        &diagnostics,
        DiagnosticCode::DomainIncompatible,
        "parameterised SELECT SERVER functions do not support ORDER BY",
        &ordering.ordering[0].span,
    );
}

#[test]
fn preserves_v1_multi_diagnostic_accumulation() {
    let query =
        query("SELECT t.unknown FROM tasks.task t WHERE other.title = t.title ORDER BY t.missing");
    let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();
    assert_eq!(diagnostics.len(), 3);
    for (diagnostic, (name, message)) in diagnostics.iter().zip([
        ("unknown", "unknown field unknown on tasks.task"),
        ("other", "unknown query alias other"),
        ("missing", "unknown field missing on tasks.task"),
    ]) {
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic.message(), message);
        assert_eq!(diagnostic.location().logical_path(), "tasks.orna");
        assert_eq!(
                diagnostic.location().span().start(),
                query.span.start
                    + "SELECT t.unknown FROM tasks.task t WHERE other.title = t.title ORDER BY t.missing"
                        .find(name)
                        .unwrap()
            );
        assert_eq!(
            diagnostic.location().span().end(),
            diagnostic.location().span().start() + name.len()
        );
    }
}

#[test]
fn rejects_each_identity_selected_identity_mapping_failure() {
    let query = query("SELECT t.title FROM tasks.task t WHERE REF(t) = p_task");
    let plan = check_identity_selected_query_in(
        &query,
        &catalogue(),
        SELECTOR_OWNER,
        &[selector_parameter(
            "p_task",
            SemanticType::reference(TASK_TYPE),
        )],
        "tasks.orna",
    )
    .unwrap()
    .plan()
    .clone();
    assert_eq!(
        plan.try_map_identities(
            |_| Err::<TypeId, _>("type"),
            Ok::<FieldId, &str>,
            Ok::<FunctionId, &str>,
            Ok::<ParameterId, &str>
        ),
        Err("type")
    );
    assert_eq!(
        plan.try_map_identities(
            Ok::<TypeId, &str>,
            |_| Err::<FieldId, _>("field"),
            Ok::<FunctionId, &str>,
            Ok::<ParameterId, &str>
        ),
        Err("field")
    );
    assert_eq!(
        plan.try_map_identities(
            Ok::<TypeId, &str>,
            Ok::<FieldId, &str>,
            |_| Err::<FunctionId, _>("function"),
            Ok::<ParameterId, &str>
        ),
        Err("function")
    );
    assert_eq!(
        plan.try_map_identities(
            Ok::<TypeId, &str>,
            Ok::<FieldId, &str>,
            Ok::<FunctionId, &str>,
            |_| Err::<ParameterId, _>("parameter")
        ),
        Err("parameter")
    );
}

#[test]
fn records_ordered_source_evidence_for_each_resolved_query_reference() {
    let query_source = "SELECT REF(t), t.assignee.name, t.assignee.name FROM tasks.task t WHERE t.completed = t.completed ORDER BY t.assignee.name DESC";
    let query = query(query_source);
    let check = check_query_in(&query, &catalogue(), "tasks.orna").unwrap();
    let references = check.references();
    let path_starts = query_source
        .match_indices("t.assignee.name")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    let completed_starts = query_source
        .match_indices("t.completed")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    let reference_start = query_source.find("REF(t)").unwrap() + 4;
    let object_type_start = query_source.find("tasks.task").unwrap();
    let expected = [
        (
            QueryReferenceKind::QueryObject,
            QueryReferenceTarget::Object(TASK_TYPE),
            object_type_start,
            "tasks.task".len(),
        ),
        (
            QueryReferenceKind::ObjectReference,
            QueryReferenceTarget::Object(TASK_TYPE),
            reference_start,
            "t".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: ASSIGNEE_FIELD,
            },
            path_starts[0] + 2,
            "assignee".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: PERSON_TYPE,
                field: PERSON_NAME_FIELD,
            },
            path_starts[0] + 11,
            "name".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: ASSIGNEE_FIELD,
            },
            path_starts[1] + 2,
            "assignee".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: PERSON_TYPE,
                field: PERSON_NAME_FIELD,
            },
            path_starts[1] + 11,
            "name".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            },
            completed_starts[0] + 2,
            "completed".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: COMPLETED_FIELD,
            },
            completed_starts[1] + 2,
            "completed".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: TASK_TYPE,
                field: ASSIGNEE_FIELD,
            },
            path_starts[2] + 2,
            "assignee".len(),
        ),
        (
            QueryReferenceKind::QueryField,
            QueryReferenceTarget::Field {
                owner: PERSON_TYPE,
                field: PERSON_NAME_FIELD,
            },
            path_starts[2] + 11,
            "name".len(),
        ),
    ];

    assert_eq!(references.len(), expected.len());
    for (reference, (kind, target, start, length)) in references.iter().zip(expected) {
        assert_eq!(reference.kind(), kind);
        assert_eq!(reference.target(), &target);
        assert_eq!(reference.location().logical_path(), "tasks.orna");
        assert_eq!(
            reference.location().span().start(),
            query.span.start + start
        );
        assert_eq!(
            reference.location().span().end(),
            query.span.start + start + length
        );
    }
}

#[test]
fn checks_a_query_with_non_core_copyable_identities() {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestTypeId(u8);
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestFieldId(u8);

    struct TestCatalogue {
        task_name: QualifiedSemanticName,
        person_name: QualifiedSemanticName,
    }

    impl std::fmt::Display for TestTypeId {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{}", self.0)
        }
    }

    impl QueryCatalogue<TestTypeId, TestFieldId> for TestCatalogue {
        fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<TestTypeId> {
            if name == &self.task_name {
                Some(TestTypeId(1))
            } else if name == &self.person_name {
                Some(TestTypeId(2))
            } else {
                None
            }
        }

        fn object_type_name_by_id(&self, id: TestTypeId) -> Option<&QualifiedSemanticName> {
            match id {
                TestTypeId(1) => Some(&self.task_name),
                TestTypeId(2) => Some(&self.person_name),
                TestTypeId(_) => None,
            }
        }

        fn field_by_name(
            &self,
            owner: TestTypeId,
            name: &str,
        ) -> Option<QueryField<TestTypeId, TestFieldId>> {
            match (owner, name) {
                (TestTypeId(1), "person") => Some(QueryField::new(
                    TestFieldId(10),
                    SemanticType::reference(TestTypeId(2)),
                    true,
                )),
                (TestTypeId(2), "name") => Some(QueryField::new(
                    TestFieldId(20),
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )),
                _ => None,
            }
        }

        fn field_by_id(
            &self,
            owner: TestTypeId,
            id: TestFieldId,
        ) -> Option<QueryField<TestTypeId, TestFieldId>> {
            match (owner, id) {
                (TestTypeId(1), TestFieldId(10)) => self.field_by_name(owner, "person"),
                (TestTypeId(2), TestFieldId(20)) => self.field_by_name(owner, "name"),
                _ => None,
            }
        }
    }

    let catalogue = TestCatalogue {
        task_name: name(&["test", "task"]),
        person_name: name(&["test", "person"]),
    };
    let query = query("SELECT t.person.name FROM test.task t");
    let check = check_query_in(&query, &catalogue, "test.orna").unwrap();
    let ir = check.plan();

    assert_eq!(ir.scan().object_type(), TestTypeId(1));
    assert_eq!(
        check.references()[0].kind(),
        QueryReferenceKind::QueryObject
    );
    assert_eq!(
        check.references()[0].target(),
        &QueryReferenceTarget::Object(TestTypeId(1))
    );
    assert_eq!(check.references()[1].kind(), QueryReferenceKind::QueryField);
    assert_eq!(
        check.references()[1].target(),
        &QueryReferenceTarget::Field {
            owner: TestTypeId(1),
            field: TestFieldId(10),
        }
    );
    let ExpressionKind::FieldPath { steps, .. } = ir.projections()[0].kind() else {
        panic!("expected a field path");
    };
    assert_eq!(
        steps,
        &[
            super::ResolvedFieldStep {
                owner: TestTypeId(1),
                field: TestFieldId(10),
            },
            super::ResolvedFieldStep {
                owner: TestTypeId(2),
                field: TestFieldId(20),
            }
        ]
    );
    assert_eq!(
        ir.projections()[0].value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(ir.projections()[0].value_type().nullable());
}
