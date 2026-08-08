//! Conversion from compiler-owned relational IR to canonical artifacts.
//!
//! This module is the only compiler seam for server-plan artifacts. The
//! artifact model receives stable semantic identities and execution facts. It
//! does not receive source text, source locations, syntax values, or storage
//! backend concepts.

use orna_artifact::server_plan::{
    Expression, ExpressionKind, FieldStep, IdentitySelectedServerPlan, IdentitySelector, NullOrder,
    Ordering, Scan, ServerPlan, ServerPlanError, SortDirection, ValueType,
};
use orna_core::{FieldId, FunctionId, ParameterId, TypeId};

use super::{
    EncodedIdentitySelectedServerPlan, ExpressionIr, ExpressionKind as CompilerExpressionKind,
    IdentitySelectedQueryIr, InputSlot, NullOrder as CompilerNullOrder, OrderingIr,
    RelationalQueryIr, ResolvedFieldStep, ScanIr, SortDirection as CompilerSortDirection,
    ValueType as CompilerValueType,
};
/// Converts and encodes one checked relational query into canonical bytes.
pub(super) fn encode(
    query: &RelationalQueryIr<TypeId, FieldId>,
) -> Result<Vec<u8>, ServerPlanError> {
    adapt(query).encode()
}

fn adapt(query: &RelationalQueryIr<TypeId, FieldId>) -> ServerPlan {
    ServerPlan {
        scan: adapt_scan(&query.scan),
        projections: query.projections.iter().map(adapt_expression).collect(),
        selection: query.selection.as_ref().map(adapt_expression),
        ordering: query.ordering.iter().map(adapt_ordering).collect(),
    }
}

/// Converts and encodes one checked identity-selected query into version-2 bytes.
pub(super) fn encode_identity_selected(
    query: &IdentitySelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
) -> Result<EncodedIdentitySelectedServerPlan, ServerPlanError> {
    let plan = adapt_identity_selected(query)?;
    Ok(EncodedIdentitySelectedServerPlan {
        format_version: plan.format_version(),
        payload: plan.encode()?,
    })
}

fn adapt_identity_selected(
    query: &IdentitySelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
) -> Result<IdentitySelectedServerPlan, ServerPlanError> {
    IdentitySelectedServerPlan::new(
        adapt_scan(&query.scan),
        query.projections.iter().map(adapt_expression),
        IdentitySelector::new(query.selector.owner, query.selector.parameter),
    )
}

fn adapt_scan(scan: &ScanIr<TypeId>) -> Scan {
    Scan {
        input: adapt_input(scan.input),
        object_type: scan.object_type,
    }
}

fn adapt_ordering(ordering: &OrderingIr<TypeId, FieldId>) -> Ordering {
    Ordering {
        expression: adapt_expression(&ordering.expression),
        direction: adapt_sort_direction(ordering.direction),
        null_order: adapt_null_order(ordering.null_order),
    }
}

fn adapt_expression(expression: &ExpressionIr<TypeId, FieldId>) -> Expression {
    Expression {
        kind: match &expression.kind {
            CompilerExpressionKind::ObjectReference { input } => ExpressionKind::ObjectReference {
                input: adapt_input(*input),
            },
            CompilerExpressionKind::FieldPath { input, steps } => ExpressionKind::FieldPath {
                input: adapt_input(*input),
                steps: steps.iter().copied().map(adapt_field_step).collect(),
            },
            CompilerExpressionKind::BooleanLiteral { value } => {
                ExpressionKind::BooleanLiteral { value: *value }
            }
            CompilerExpressionKind::Equality { left, right } => ExpressionKind::Equality {
                left: Box::new(adapt_expression(left)),
                right: Box::new(adapt_expression(right)),
            },
        },
        value_type: adapt_value_type(expression.value_type),
    }
}

const fn adapt_input(input: InputSlot) -> u32 {
    input.0 as u32
}

const fn adapt_field_step(step: ResolvedFieldStep<TypeId, FieldId>) -> FieldStep {
    FieldStep {
        owner: step.owner,
        field: step.field,
    }
}

const fn adapt_value_type(value_type: CompilerValueType<TypeId>) -> ValueType {
    ValueType {
        resolved_type: value_type.semantic_type().into_core(),
        nullable: value_type.nullable,
    }
}

const fn adapt_sort_direction(direction: CompilerSortDirection) -> SortDirection {
    match direction {
        CompilerSortDirection::Unspecified => SortDirection::Unspecified,
        CompilerSortDirection::Ascending => SortDirection::Ascending,
        CompilerSortDirection::Descending => SortDirection::Descending,
    }
}

const fn adapt_null_order(null_order: CompilerNullOrder) -> NullOrder {
    match null_order {
        CompilerNullOrder::Unspecified => NullOrder::Unspecified,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use orna_artifact::server_plan::{
        Expression, ExpressionKind, IdentitySelectedServerPlan, NullOrder, ServerPlan,
        SortDirection,
    };
    use orna_core::{
        CatalogueRevisionId, FieldId, FunctionId, ParameterId, SchemaId, TypeId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            SchemaDefinition,
        },
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };
    use orna_syntax::parse;

    use crate::{CheckedFieldId, CheckedTypeId};

    use super::super::{QueryParameter, RelationalQueryIr, check_identity_selected_query_in};

    const TASK_TYPE: TypeId = TypeId::from_bytes([1; 16]);
    const PERSON_TYPE: TypeId = TypeId::from_bytes([2; 16]);
    const ASSIGNEE_FIELD: FieldId = FieldId::from_bytes([11; 16]);
    const COMPLETED_FIELD: FieldId = FieldId::from_bytes([12; 16]);
    const TITLE_FIELD: FieldId = FieldId::from_bytes([13; 16]);
    const PERSON_NAME_FIELD: FieldId = FieldId::from_bytes([21; 16]);
    const SELECTOR_OWNER: FunctionId = FunctionId::from_bytes([31; 16]);
    const SELECTOR_PARAMETER: ParameterId = ParameterId::from_bytes([32; 16]);

    const SOURCE: &str = "CREATE SCHEMA semantic_schema_marker; \
        CREATE TYPE semantic_schema_marker.person_type_marker AS OBJECT ( \
            person_name_marker TEXT NOT NULL \
        ); \
        CREATE TYPE semantic_schema_marker.task_type_marker AS OBJECT ( \
            assignee_marker REF semantic_schema_marker.person_type_marker, \
            title_marker TEXT, \
            completed_marker BOOL NOT NULL \
        ); \
        CREATE SERVER FUNCTION semantic_schema_marker.tasks_function_marker() \
        RETURNS ROWS ( \
            task_marker REF semantic_schema_marker.task_type_marker, \
            title_marker TEXT, \
            assignee_name_marker TEXT, \
            equality_marker BOOL \
        ) AS SELECT \
            REF(t_alias_marker), \
            t_alias_marker.title_marker, \
            t_alias_marker.assignee_marker.person_name_marker, \
            t_alias_marker.assignee_marker = t_alias_marker.assignee_marker \
        FROM semantic_schema_marker.task_type_marker t_alias_marker \
        WHERE t_alias_marker.completed_marker = FALSE \
        ORDER BY \
            t_alias_marker.title_marker, \
            t_alias_marker.assignee_marker.person_name_marker ASC, \
            t_alias_marker.completed_marker DESC;";

    const IDENTITY_SELECTED_SOURCE: &str = "CREATE SERVER FUNCTION semantic_schema_marker.get( \
        p_task REF semantic_schema_marker.task_type_marker \
        ) RETURNS ROWS ( \
            task_marker REF semantic_schema_marker.task_type_marker, \
            title_marker TEXT \
        ) AS SELECT REF(t_alias_marker), t_alias_marker.title_marker \
        FROM semantic_schema_marker.task_type_marker t_alias_marker \
        WHERE REF(t_alias_marker) = p_task;";

    #[test]
    fn encodes_a_checked_server_function_without_source_semantics() {
        let parsed = parse(SOURCE);
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("test function has a SELECT body");
        let catalogue = catalogue();
        let checked =
            super::super::check_query(&body.query, &catalogue, "source_semantic_marker.orna")
                .unwrap();
        let encoded = checked.encode_server_plan().unwrap();
        assert_eq!(checked.encode_server_plan().unwrap(), encoded);

        let decoded = ServerPlan::decode(&encoded).unwrap();
        let task = object(&catalogue, &["semantic_schema_marker", "task_type_marker"]);
        let person = object(
            &catalogue,
            &["semantic_schema_marker", "person_type_marker"],
        );
        let assignee = task.field_by_name("assignee_marker").unwrap();
        let title = task.field_by_name("title_marker").unwrap();
        let completed = task.field_by_name("completed_marker").unwrap();
        let person_name = person.field_by_name("person_name_marker").unwrap();

        assert_eq!(decoded.scan.input, 0);
        assert_eq!(decoded.scan.object_type, task.id());
        assert_eq!(decoded.projections.len(), 4);
        assert_object_reference(&decoded.projections[0], task.id());
        assert_field_path(
            &decoded.projections[1],
            &[(task.id(), title.id())],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_field_path(
            &decoded.projections[2],
            &[(task.id(), assignee.id()), (person.id(), person_name.id())],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_equality(
            &decoded.projections[3],
            ResolvedType::reference(person.id()),
            true,
        );

        let selection = decoded.selection.as_ref().unwrap();
        assert_equality(
            selection,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        );
        let ExpressionKind::Equality { left, right } = &selection.kind else {
            unreachable!("equality was asserted above")
        };
        assert_field_path(
            left,
            &[(task.id(), completed.id())],
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        );
        assert!(matches!(
            right.kind,
            ExpressionKind::BooleanLiteral { value: false }
        ));

        assert_eq!(decoded.ordering.len(), 3);
        assert_eq!(decoded.ordering[0].direction, SortDirection::Unspecified);
        assert_eq!(decoded.ordering[1].direction, SortDirection::Ascending);
        assert_eq!(decoded.ordering[2].direction, SortDirection::Descending);
        assert_field_path(
            &decoded.ordering[0].expression,
            &[(task.id(), title.id())],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_field_path(
            &decoded.ordering[1].expression,
            &[(task.id(), assignee.id()), (person.id(), person_name.id())],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_field_path(
            &decoded.ordering[2].expression,
            &[(task.id(), completed.id())],
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        );
        for ordering in &decoded.ordering {
            assert_eq!(ordering.null_order, NullOrder::Unspecified);
        }

        for text in [
            "source_semantic_marker.orna",
            "semantic_schema_marker",
            "task_type_marker",
            "person_type_marker",
            "tasks_function_marker",
            "t_alias_marker",
            "assignee_marker",
            "title_marker",
            "person_name_marker",
            "completed_marker",
            "equality_marker",
        ] {
            assert!(
                !encoded
                    .windows(text.len())
                    .any(|window| window == text.as_bytes()),
                "artifact contains submitted source name {text:?}"
            );
        }
    }

    #[test]
    fn encodes_identity_selected_queries_with_coupled_version_and_payload() {
        let parsed = parse(IDENTITY_SELECTED_SOURCE);
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("test function has a SELECT body");
        let checked = check_identity_selected_query_in(
            &body.query,
            &catalogue(),
            SELECTOR_OWNER,
            &[QueryParameter::new(
                "p_task",
                SELECTOR_PARAMETER,
                crate::resolver::SemanticType::reference(TASK_TYPE),
            )],
            "identity_selected.orna",
        )
        .unwrap();

        let encoded = checked
            .plan()
            .encode_identity_selected_server_plan()
            .unwrap();
        let decoded = IdentitySelectedServerPlan::decode(encoded.payload()).unwrap();
        assert_eq!(encoded.format_version(), decoded.format_version());
        assert_eq!(decoded.selector().owner(), SELECTOR_OWNER);
        assert_eq!(decoded.selector().parameter(), SELECTOR_PARAMETER);
        assert_eq!(decoded.projections().len(), 2);
        assert_object_reference(&decoded.projections()[0], TASK_TYPE);
        assert_field_path(
            &decoded.projections()[1],
            &[(TASK_TYPE, TITLE_FIELD)],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
    }

    #[test]
    fn maps_checked_identities_before_encoding_durable_server_plan_bytes() {
        let fixture = checked_plan_fixture();
        let mapped = fixture
            .plan
            .try_map_identities(
                |type_id| {
                    fixture
                        .type_ids
                        .get(&type_id)
                        .copied()
                        .ok_or("unknown type")
                },
                |field_id| {
                    fixture
                        .field_ids
                        .get(&field_id)
                        .copied()
                        .ok_or("unknown field")
                },
            )
            .unwrap();

        let encoded = mapped.encode_server_plan().unwrap();
        let decoded = ServerPlan::decode(&encoded).unwrap();
        let parsed = parse(SOURCE);
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("test function has a SELECT body");
        let expected =
            super::super::check_query(&body.query, &catalogue(), "source_semantic_marker.orna")
                .unwrap();
        let expected = ServerPlan::decode(&expected.encode_server_plan().unwrap()).unwrap();

        assert_eq!(decoded, expected);
        assert_eq!(decoded.scan.object_type, TASK_TYPE);
        assert_object_reference(&decoded.projections[0], TASK_TYPE);
        assert_field_path(
            &decoded.projections[1],
            &[(TASK_TYPE, TITLE_FIELD)],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_field_path(
            &decoded.projections[2],
            &[
                (TASK_TYPE, ASSIGNEE_FIELD),
                (PERSON_TYPE, PERSON_NAME_FIELD),
            ],
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        );
        assert_equality(
            &decoded.projections[3],
            ResolvedType::reference(PERSON_TYPE),
            true,
        );
        assert_eq!(decoded.ordering.len(), 3);
        assert_eq!(decoded.ordering[0].direction, SortDirection::Unspecified);
        assert_eq!(decoded.ordering[1].direction, SortDirection::Ascending);
        assert_eq!(decoded.ordering[2].direction, SortDirection::Descending);
        assert!(
            decoded
                .ordering
                .iter()
                .all(|ordering| ordering.null_order == NullOrder::Unspecified)
        );

        for checked_marker in fixture.checked_markers {
            assert!(
                !encoded
                    .windows(checked_marker.len())
                    .any(|window| window == checked_marker.as_bytes()),
                "artifact contains checked identity representation {checked_marker:?}"
            );
        }
    }

    #[test]
    fn rejects_the_complete_identity_rewrite_when_a_mapping_is_missing() {
        let fixture = checked_plan_fixture();
        let missing_field = fixture
            .field_ids
            .iter()
            .find_map(|(checked, durable)| (*durable == TITLE_FIELD).then_some(*checked))
            .unwrap();

        let result = fixture.plan.try_map_identities(
            |type_id| {
                fixture
                    .type_ids
                    .get(&type_id)
                    .copied()
                    .ok_or("unknown type")
            },
            |field_id| {
                if field_id == missing_field {
                    Err("missing durable field identity")
                } else {
                    fixture
                        .field_ids
                        .get(&field_id)
                        .copied()
                        .ok_or("unknown field")
                }
            },
        );

        assert_eq!(result, Err("missing durable field identity"));
    }

    struct CheckedPlanFixture {
        plan: RelationalQueryIr<CheckedTypeId, CheckedFieldId>,
        type_ids: HashMap<CheckedTypeId, TypeId>,
        field_ids: HashMap<CheckedFieldId, FieldId>,
        checked_markers: Vec<String>,
    }

    fn checked_plan_fixture() -> CheckedPlanFixture {
        let bundle = SourceBundle::new([SourceUnit::new("checked.orna", SOURCE)]).unwrap();
        let base = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([99; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let report = crate::check(&bundle, &base);
        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let person_name = &person.fields()[0];
        let assignee = &task.fields()[0];
        let title = &task.fields()[1];
        let completed = &task.fields()[2];

        let type_ids = HashMap::from([(person.id(), PERSON_TYPE), (task.id(), TASK_TYPE)]);
        let field_ids = HashMap::from([
            (person_name.id(), PERSON_NAME_FIELD),
            (assignee.id(), ASSIGNEE_FIELD),
            (title.id(), TITLE_FIELD),
            (completed.id(), COMPLETED_FIELD),
        ]);
        let checked_markers = type_ids
            .keys()
            .map(ToString::to_string)
            .chain(field_ids.keys().map(ToString::to_string))
            .collect();

        assert!(type_ids.keys().all(|id| id.is_provisional()));
        assert!(field_ids.keys().all(|id| id.is_provisional()));

        CheckedPlanFixture {
            plan: checked.server_functions()[0]
                .query_plan()
                .expect("fixture has a SELECT body")
                .clone(),
            type_ids,
            field_ids,
            checked_markers,
        }
    }

    fn catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([9; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                name(&["semantic_schema_marker"]),
            )],
            vec![
                ObjectTypeDefinition::new(
                    PERSON_TYPE,
                    name(&["semantic_schema_marker", "person_type_marker"]),
                    vec![field(
                        PERSON_NAME_FIELD,
                        "person_name_marker",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        false,
                    )],
                ),
                ObjectTypeDefinition::new(
                    TASK_TYPE,
                    name(&["semantic_schema_marker", "task_type_marker"]),
                    vec![
                        field(
                            ASSIGNEE_FIELD,
                            "assignee_marker",
                            0,
                            ResolvedType::reference(PERSON_TYPE),
                            true,
                        ),
                        field(
                            TITLE_FIELD,
                            "title_marker",
                            1,
                            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                            true,
                        ),
                        field(
                            COMPLETED_FIELD,
                            "completed_marker",
                            2,
                            ResolvedType::scalar(StandardScalar::Boolean),
                            false,
                        ),
                    ],
                ),
            ],
        )
        .unwrap()
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

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn object<'a>(
        catalogue: &'a CatalogueSnapshot,
        parts: &[&str],
    ) -> &'a orna_core::catalogue::ObjectTypeDefinition {
        let name = QualifiedSemanticName::new(parts.iter().copied()).unwrap();
        catalogue.object_type_by_name(&name).unwrap()
    }

    fn assert_object_reference(expression: &Expression, object_type: orna_core::TypeId) {
        assert_eq!(
            expression.value_type.resolved_type,
            ResolvedType::reference(object_type)
        );
        assert!(!expression.value_type.nullable);
        assert_eq!(
            expression.kind,
            ExpressionKind::ObjectReference { input: 0 }
        );
    }

    fn assert_field_path(
        expression: &Expression,
        expected_steps: &[(orna_core::TypeId, orna_core::FieldId)],
        resolved_type: ResolvedType,
        nullable: bool,
    ) {
        assert_eq!(expression.value_type.resolved_type, resolved_type);
        assert_eq!(expression.value_type.nullable, nullable);
        let ExpressionKind::FieldPath { input, steps } = &expression.kind else {
            panic!("expected a field path")
        };
        assert_eq!(*input, 0);
        assert_eq!(steps.len(), expected_steps.len());
        for (step, (owner, field)) in steps.iter().zip(expected_steps) {
            assert_eq!(step.owner, *owner);
            assert_eq!(step.field, *field);
        }
    }

    fn assert_equality(expression: &Expression, operand_type: ResolvedType, nullable: bool) {
        assert_eq!(
            expression.value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert_eq!(expression.value_type.nullable, nullable);
        let ExpressionKind::Equality { left, right } = &expression.kind else {
            panic!("expected an equality expression")
        };
        assert_eq!(left.value_type.resolved_type, operand_type);
        assert_eq!(right.value_type.resolved_type, operand_type);
        assert_eq!(left.value_type.nullable, nullable);
        assert_eq!(right.value_type.nullable, nullable);
    }
}
