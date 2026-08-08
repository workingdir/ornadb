//! Semantic checking for the initial Orna relational query slice.
//!
//! This module owns an Orna relational IR. It records catalogue identities and
//! source-independent type facts, without selecting a storage backend.
//! Unknown fields and aliases use `UnknownQualifiedName`, because the stable
//! diagnostic set does not define a separate unknown-field code.

use orna_core::{
    FieldId, TypeId,
    catalogue::CatalogueSnapshot,
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{NamePart, QueryExpression, SelectQuery, SourceSpan};

use crate::{
    CompilerDiagnostic, DiagnosticCode, normalise_name_part, normalise_qualified_name,
    semantic_diagnostic,
};

/// A deterministic input position in one relational query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputSlot(u8);

impl InputSlot {
    const PRIMARY: Self = Self(0);
}

/// A checked one-source relational query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationalQueryIr {
    scan: ScanIr,
    projections: Vec<ExpressionIr>,
    selection: Option<ExpressionIr>,
    ordering: Vec<OrderingIr>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl RelationalQueryIr {
    pub(crate) fn scan(&self) -> &ScanIr {
        &self.scan
    }

    pub(crate) fn projections(&self) -> &[ExpressionIr] {
        &self.projections
    }

    pub(crate) fn selection(&self) -> Option<&ExpressionIr> {
        self.selection.as_ref()
    }

    pub(crate) fn ordering(&self) -> &[OrderingIr] {
        &self.ordering
    }
}

/// The single object input scanned by this query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScanIr {
    input: InputSlot,
    object_type: TypeId,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ScanIr {
    pub(crate) const fn input(&self) -> InputSlot {
        self.input
    }

    pub(crate) const fn object_type(&self) -> TypeId {
        self.object_type
    }
}

/// A checked ordering expression and its source-selected ordering rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OrderingIr {
    expression: ExpressionIr,
    direction: SortDirection,
    null_order: NullOrder,
}

#[cfg_attr(not(test), allow(dead_code))]
impl OrderingIr {
    pub(crate) fn expression(&self) -> &ExpressionIr {
        &self.expression
    }

    pub(crate) const fn direction(&self) -> SortDirection {
        self.direction
    }

    pub(crate) const fn null_order(&self) -> NullOrder {
        self.null_order
    }
}

/// The source-selected ordering direction, before default resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortDirection {
    Unspecified,
    Ascending,
    Descending,
}

/// The source-selected null ordering, before default resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NullOrder {
    Unspecified,
}

/// One checked relational expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpressionIr {
    kind: ExpressionKind,
    value_type: ValueType,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ExpressionIr {
    pub(crate) fn kind(&self) -> &ExpressionKind {
        &self.kind
    }

    pub(crate) const fn value_type(&self) -> ValueType {
        self.value_type
    }
}

/// The resolved meaning of one relational expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpressionKind {
    ObjectReference {
        input: InputSlot,
    },
    FieldPath {
        input: InputSlot,
        steps: Vec<ResolvedFieldStep>,
    },
    BooleanLiteral {
        value: bool,
    },
    Equality {
        left: Box<ExpressionIr>,
        right: Box<ExpressionIr>,
    },
}

/// One stable field reference in an ordered path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFieldStep {
    owner: TypeId,
    field: FieldId,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ResolvedFieldStep {
    pub(crate) const fn owner(&self) -> TypeId {
        self.owner
    }

    pub(crate) const fn field(&self) -> FieldId {
        self.field
    }
}

/// The resolved type and nullability of a relational expression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueType {
    resolved_type: ResolvedType,
    nullable: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ValueType {
    pub(crate) const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// Checks a parsed one-source `SELECT` query against an immutable catalogue.
///
/// The result contains stable catalogue identities only. Any semantic error
/// rejects the complete query and returns source-located diagnostics.
pub(crate) fn check_query(
    query: &SelectQuery,
    catalogue: &CatalogueSnapshot,
    logical_path: &str,
) -> Result<RelationalQueryIr, Vec<CompilerDiagnostic>> {
    let source_name = normalise_qualified_name(&query.source_object.object_type);
    let Some(source_object) = catalogue.object_type_by_name(&source_name) else {
        return Err(vec![diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unknown object type {source_name}"),
            logical_path,
            &query.source_object.object_type.span,
        )]);
    };

    let input = InputSlot::PRIMARY;
    let context = InputContext {
        input,
        object_type: source_object.id(),
        alias: normalise_name_part(&query.source_object.alias),
    };
    let mut diagnostics = Vec::new();

    let projections = query
        .projections
        .iter()
        .filter_map(|expression| {
            check_expression(
                expression,
                &context,
                catalogue,
                logical_path,
                &mut diagnostics,
            )
        })
        .collect::<Vec<_>>();

    let selection = query.predicate.as_ref().and_then(|expression| {
        let source_span = expression.span();
        let expression = check_expression(
            expression,
            &context,
            catalogue,
            logical_path,
            &mut diagnostics,
        )?;
        if expression.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "WHERE requires a BOOLEAN expression",
                logical_path,
                source_span,
            ));
            return None;
        }
        Some(expression)
    });

    let ordering = query
        .ordering
        .iter()
        .filter_map(|ordering| {
            check_expression(
                &ordering.expression,
                &context,
                catalogue,
                logical_path,
                &mut diagnostics,
            )
            .map(|expression| OrderingIr {
                expression,
                direction: match ordering.direction {
                    orna_syntax::OrderingDirection::Unspecified => SortDirection::Unspecified,
                    orna_syntax::OrderingDirection::Ascending => SortDirection::Ascending,
                    orna_syntax::OrderingDirection::Descending => SortDirection::Descending,
                },
                null_order: match ordering.null_order {
                    orna_syntax::NullOrdering::Unspecified => NullOrder::Unspecified,
                },
            })
        })
        .collect::<Vec<_>>();

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    Ok(RelationalQueryIr {
        scan: ScanIr {
            input,
            object_type: context.object_type,
        },
        projections,
        selection,
        ordering,
    })
}

struct InputContext {
    input: InputSlot,
    object_type: TypeId,
    alias: String,
}

fn check_expression(
    expression: &QueryExpression,
    context: &InputContext,
    catalogue: &CatalogueSnapshot,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ExpressionIr> {
    match expression {
        QueryExpression::ObjectReference { alias, .. } => {
            check_alias(alias, context, logical_path, diagnostics)?;
            Some(ExpressionIr {
                kind: ExpressionKind::ObjectReference {
                    input: context.input,
                },
                value_type: ValueType {
                    resolved_type: ResolvedType::reference(context.object_type),
                    nullable: false,
                },
            })
        }
        QueryExpression::FieldPath { root, members, .. } => {
            check_alias(root, context, logical_path, diagnostics)?;
            check_field_path(members, context, catalogue, logical_path, diagnostics)
        }
        QueryExpression::BooleanLiteral { value, .. } => Some(ExpressionIr {
            kind: ExpressionKind::BooleanLiteral { value: *value },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }),
        QueryExpression::Equality { left, right, span } => {
            let left = check_expression(left, context, catalogue, logical_path, diagnostics);
            let right = check_expression(right, context, catalogue, logical_path, diagnostics);
            let (Some(left), Some(right)) = (left, right) else {
                return None;
            };

            if left.value_type.resolved_type != right.value_type.resolved_type {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "equality requires expressions with compatible types",
                    logical_path,
                    span,
                ));
                return None;
            }

            Some(ExpressionIr {
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: left.value_type.nullable || right.value_type.nullable,
                },
                kind: ExpressionKind::Equality {
                    left: Box::new(left),
                    right: Box::new(right),
                },
            })
        }
    }
}

fn check_field_path(
    members: &[NamePart],
    context: &InputContext,
    catalogue: &CatalogueSnapshot,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ExpressionIr> {
    let mut owner = context.object_type;
    let mut nullable = false;
    let mut steps = Vec::with_capacity(members.len());

    for (index, member) in members.iter().enumerate() {
        let Some(object_type) = catalogue.object_type_by_id(owner) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::InvalidReferenceTarget,
                format!("REF target type {owner} is not an object type"),
                logical_path,
                &member.span,
            ));
            return None;
        };
        let member_name = normalise_name_part(member);
        let Some(field) = object_type.field_by_name(&member_name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown field {member_name} on {}", object_type.name()),
                logical_path,
                &member.span,
            ));
            return None;
        };

        steps.push(ResolvedFieldStep {
            owner,
            field: field.id(),
        });
        nullable |= field.nullable();

        if index + 1 == members.len() {
            return Some(ExpressionIr {
                kind: ExpressionKind::FieldPath {
                    input: context.input,
                    steps,
                },
                value_type: ValueType {
                    resolved_type: field.resolved_type(),
                    nullable,
                },
            });
        }

        let ResolvedType::Reference { target } = field.resolved_type() else {
            diagnostics.push(diagnostic(
                DiagnosticCode::InvalidReferenceTarget,
                format!("field {member_name} is not a REF and cannot be traversed"),
                logical_path,
                &member.span,
            ));
            return None;
        };
        owner = target;
    }

    unreachable!("syntax requires a field path to contain at least one member")
}

fn check_alias(
    alias: &NamePart,
    context: &InputContext,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<()> {
    let alias_name = normalise_name_part(alias);
    if alias_name == context.alias {
        return Some(());
    }

    diagnostics.push(diagnostic(
        DiagnosticCode::UnknownQualifiedName,
        format!("unknown query alias {alias_name}"),
        logical_path,
        &alias.span,
    ));
    None
}

fn diagnostic(
    code: DiagnosticCode,
    message: impl Into<String>,
    logical_path: &str,
    span: &SourceSpan,
) -> CompilerDiagnostic {
    semantic_diagnostic(code, message, logical_path, span)
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, FieldId, SchemaId, TypeId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            SchemaDefinition,
        },
        types::{ResolvedType, StandardScalar},
    };
    use orna_syntax::{ServerFunctionBody, parse};

    use super::{ExpressionKind, NullOrder, SortDirection, check_query};
    use crate::DiagnosticCode;

    const TASK_TYPE: TypeId = TypeId::from_bytes([1; 16]);
    const PERSON_TYPE: TypeId = TypeId::from_bytes([2; 16]);
    const ASSIGNEE_FIELD: FieldId = FieldId::from_bytes([11; 16]);
    const COMPLETED_FIELD: FieldId = FieldId::from_bytes([12; 16]);
    const TITLE_FIELD: FieldId = FieldId::from_bytes([13; 16]);
    const PERSON_NAME_FIELD: FieldId = FieldId::from_bytes([21; 16]);

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
                    ],
                ),
                ObjectTypeDefinition::new(
                    PERSON_TYPE,
                    name(&["people", "person"]),
                    vec![field(
                        PERSON_NAME_FIELD,
                        "name",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        false,
                    )],
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
        let source =
            format!("CREATE SERVER FUNCTION tasks.query() RETURNS BOOL AS {query_source};");
        let parsed = parse(&source);
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let ServerFunctionBody::SqlQuery(body) = &parsed.server_functions()[0].body;
        body.query.clone()
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
    fn rejects_a_non_boolean_where_expression() {
        let mut query = query("SELECT t.title FROM tasks.task t WHERE t.completed = FALSE");
        query.predicate = Some(query.projections[0].clone());

        let diagnostics = check_query(&query, &catalogue(), "tasks.orna").unwrap_err();

        assert_eq!(diagnostics[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            diagnostics[0].message(),
            "WHERE requires a BOOLEAN expression"
        );
        assert_eq!(
            diagnostics[0].location().span().start(),
            query.projections[0].span().start
        );
    }

    #[test]
    fn accepts_a_nullable_boolean_where_expression() {
        let query = query("SELECT t.title FROM tasks.task t WHERE t.assignee = t.assignee");

        let ir = check_query(&query, &catalogue(), "tasks.orna").unwrap();

        assert!(ir.selection().unwrap().value_type().nullable());
    }
}
