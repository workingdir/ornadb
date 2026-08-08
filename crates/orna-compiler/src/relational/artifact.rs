//! Conversion from compiler-owned relational IR to canonical artifacts.
//!
//! This module is the only compiler seam for server-plan artifacts. The
//! artifact model receives stable semantic identities and execution facts. It
//! does not receive source text, source locations, syntax values, or storage
//! backend concepts.

use orna_artifact::server_plan::{
    Expression, ExpressionKind, FieldStep, NullOrder, Ordering, Scan, ServerPlan, ServerPlanError,
    SortDirection, ValueType,
};

use super::{
    ExpressionIr, ExpressionKind as CompilerExpressionKind, InputSlot,
    NullOrder as CompilerNullOrder, OrderingIr, RelationalQueryIr, ResolvedFieldStep, ScanIr,
    SortDirection as CompilerSortDirection, ValueType as CompilerValueType,
};

/// Converts and encodes one checked relational query into canonical bytes.
pub(super) fn encode(query: &RelationalQueryIr) -> Result<Vec<u8>, ServerPlanError> {
    adapt(query).encode()
}

fn adapt(query: &RelationalQueryIr) -> ServerPlan {
    ServerPlan {
        scan: adapt_scan(&query.scan),
        projections: query.projections.iter().map(adapt_expression).collect(),
        selection: query.selection.as_ref().map(adapt_expression),
        ordering: query.ordering.iter().map(adapt_ordering).collect(),
    }
}

fn adapt_scan(scan: &ScanIr) -> Scan {
    Scan {
        input: adapt_input(scan.input),
        object_type: scan.object_type,
    }
}

fn adapt_ordering(ordering: &OrderingIr) -> Ordering {
    Ordering {
        expression: adapt_expression(&ordering.expression),
        direction: adapt_sort_direction(ordering.direction),
        null_order: adapt_null_order(ordering.null_order),
    }
}

fn adapt_expression(expression: &ExpressionIr) -> Expression {
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

const fn adapt_field_step(step: ResolvedFieldStep) -> FieldStep {
    FieldStep {
        owner: step.owner,
        field: step.field,
    }
}

const fn adapt_value_type(value_type: CompilerValueType) -> ValueType {
    ValueType {
        resolved_type: value_type.resolved_type,
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
    use orna_artifact::server_plan::{
        Expression, ExpressionKind, NullOrder, ServerPlan, SortDirection,
    };
    use orna_core::{
        CatalogueRevisionId,
        catalogue::{CatalogueSnapshot, QualifiedSemanticName},
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use crate::check;

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

    #[test]
    fn encodes_a_checked_server_function_without_source_semantics() {
        let bundle =
            SourceBundle::new([SourceUnit::new("source_semantic_marker.orna", SOURCE)]).unwrap();
        let base = CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([0; 16]), vec![], vec![])
            .unwrap();
        let report = check(&bundle, &base);

        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let checked = &report.checked_bundle().unwrap().server_functions()[0];
        let encoded = checked.plan().encode_server_plan().unwrap();
        assert_eq!(checked.plan().encode_server_plan().unwrap(), encoded);

        let decoded = ServerPlan::decode(&encoded).unwrap();
        let catalogue = report.candidate().unwrap();
        let task = object(catalogue, &["semantic_schema_marker", "task_type_marker"]);
        let person = object(catalogue, &["semantic_schema_marker", "person_type_marker"]);
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
