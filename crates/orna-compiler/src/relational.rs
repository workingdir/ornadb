//! Semantic checking for the initial Orna relational query slice.
//!
//! This module owns an Orna relational IR. It records catalogue identities and
//! source-independent type facts, without selecting a storage backend.
//! Unknown fields and aliases use `UnknownQualifiedName`, because the stable
//! diagnostic set does not define a separate unknown-field code.

use std::fmt;

use orna_core::{FieldId, FunctionId, ParameterId, TypeId, types::StandardScalar};
#[cfg(test)]
use orna_core::{catalogue::CatalogueSnapshot, types::ResolvedType};
use orna_syntax::{NamePart, QueryExpression, SelectQuery, SourceSpan};

use crate::resolver::{QueryCatalogue, SemanticType};
use crate::{
    CompilerDiagnostic, DiagnosticCode, SourceLocation, normalise_name_part,
    normalise_qualified_name, semantic_diagnostic,
};

mod artifact;

/// A deterministic input position in one relational query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputSlot(u8);

impl InputSlot {
    const PRIMARY: Self = Self(0);
}

/// A checked one-source relational query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationalQueryIr<T = TypeId, F = FieldId> {
    scan: ScanIr<T>,
    projections: Vec<ExpressionIr<T, F>>,
    selection: Option<ExpressionIr<T, F>>,
    ordering: Vec<OrderingIr<T, F>>,
}

/// A checked SERVER query with one fixed identity selector.
///
/// This is deliberately separate from `RelationalQueryIr`. The selector is
/// not a general relational expression, so it cannot occur in a projection or
/// ordering term.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct IdentitySelectedQueryIr<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId>
{
    scan: ScanIr<T>,
    projections: Vec<ExpressionIr<T, F>>,
    selector: IdentityQuerySelector<G, P>,
}

/// The fixed `REF(input 0) = parameter` selector of an identity-selected query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct IdentityQuerySelector<G = FunctionId, P = ParameterId> {
    owner: G,
    parameter: P,
}

/// The durable version and payload emitted for one identity-selected query.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct EncodedIdentitySelectedServerPlan {
    format_version: u32,
    payload: Vec<u8>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl EncodedIdentitySelectedServerPlan {
    /// Returns the artifact version selected by the artifact model.
    pub(crate) const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Returns the canonical artifact payload for this exact version.
    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> IdentitySelectedQueryIr<T, F, G, P> {
    /// Returns the query scan.
    pub(crate) const fn scan(&self) -> &ScanIr<T> {
        &self.scan
    }

    /// Returns projections in source order.
    pub(crate) fn projections(&self) -> &[ExpressionIr<T, F>] {
        &self.projections
    }

    /// Returns the one fixed identity selector.
    pub(crate) const fn selector(&self) -> &IdentityQuerySelector<G, P> {
        &self.selector
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<G, P> IdentityQuerySelector<G, P> {
    /// Returns the function that owns the selector parameter.
    pub(crate) const fn owner(&self) -> G
    where
        G: Copy,
    {
        self.owner
    }

    /// Returns the selector parameter identity.
    pub(crate) const fn parameter(&self) -> P
    where
        P: Copy,
    {
        self.parameter
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> IdentitySelectedQueryIr<T, F, G, P>
where
    T: Copy,
    F: Copy,
    G: Copy,
    P: Copy,
{
    /// Rewrites every identity, rejecting the complete query when any mapping fails.
    pub(crate) fn try_map_identities<T2, F2, G2, P2, E>(
        &self,
        mut map_type: impl FnMut(T) -> Result<T2, E>,
        mut map_field: impl FnMut(F) -> Result<F2, E>,
        mut map_function: impl FnMut(G) -> Result<G2, E>,
        mut map_parameter: impl FnMut(P) -> Result<P2, E>,
    ) -> Result<IdentitySelectedQueryIr<T2, F2, G2, P2>, E> {
        Ok(IdentitySelectedQueryIr {
            scan: ScanIr {
                input: self.scan.input,
                object_type: map_type(self.scan.object_type)?,
            },
            projections: self
                .projections
                .iter()
                .map(|expression| try_map_expression(expression, &mut map_type, &mut map_field))
                .collect::<Result<_, _>>()?,
            selector: IdentityQuerySelector {
                owner: map_function(self.selector.owner)?,
                parameter: map_parameter(self.selector.parameter)?,
            },
        })
    }
}

/// A checked relational query and the source references that produced it.
///
/// The plan keeps no source data. References retain owned compiler locations
/// for consumers that need source evidence before identity rewriting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QueryCheck<T = TypeId, F = FieldId> {
    plan: RelationalQueryIr<T, F>,
    references: Vec<QueryReference<T, F>>,
}

/// A declared function parameter available to identity-selected query checking.
///
/// `semantic_name` must use the normalised Orna identifier form. The remaining
/// fields retain facts that are not represented by a relational expression.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct QueryParameter<T = TypeId, P = ParameterId> {
    semantic_name: String,
    parameter: P,
    semantic_type: SemanticType<T>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, P> QueryParameter<T, P> {
    /// Creates one declared query parameter descriptor.
    pub(crate) fn new(
        semantic_name: impl Into<String>,
        parameter: P,
        semantic_type: SemanticType<T>,
    ) -> Self {
        Self {
            semantic_name: semantic_name.into(),
            parameter,
            semantic_type,
        }
    }

    /// Returns the normalised declared parameter name.
    pub(crate) fn semantic_name(&self) -> &str {
        &self.semantic_name
    }

    /// Returns the declared parameter identity.
    pub(crate) const fn parameter(&self) -> P
    where
        P: Copy,
    {
        self.parameter
    }

    /// Returns the exact semantic parameter type.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.semantic_type
    }
}

/// One ordered identity-selected query reference.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum IdentitySelectedQueryReference<
    T = TypeId,
    F = FieldId,
    G = FunctionId,
    P = ParameterId,
> {
    QueryObject {
        object_type: T,
        location: SourceLocation,
    },
    ObjectReference {
        object_type: T,
        location: SourceLocation,
    },
    QueryField {
        owner: T,
        field: F,
        location: SourceLocation,
    },
    ParameterRead {
        owner: G,
        parameter: P,
        location: SourceLocation,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> IdentitySelectedQueryReference<T, F, G, P> {
    /// Returns the source location that produced this reference.
    pub(crate) fn location(&self) -> &SourceLocation {
        match self {
            Self::QueryObject { location, .. }
            | Self::ObjectReference { location, .. }
            | Self::QueryField { location, .. }
            | Self::ParameterRead { location, .. } => location,
        }
    }
}

/// A checked identity-selected query and its source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct IdentitySelectedQueryCheck<
    T = TypeId,
    F = FieldId,
    G = FunctionId,
    P = ParameterId,
> {
    plan: IdentitySelectedQueryIr<T, F, G, P>,
    references: Vec<IdentitySelectedQueryReference<T, F, G, P>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> IdentitySelectedQueryCheck<T, F, G, P> {
    /// Returns the source-free checked query plan.
    pub(crate) fn plan(&self) -> &IdentitySelectedQueryIr<T, F, G, P> {
        &self.plan
    }

    /// Returns query references in deterministic source order.
    pub(crate) fn references(&self) -> &[IdentitySelectedQueryReference<T, F, G, P>] {
        &self.references
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> QueryCheck<T, F> {
    pub(crate) fn plan(&self) -> &RelationalQueryIr<T, F> {
        &self.plan
    }

    pub(crate) fn references(&self) -> &[QueryReference<T, F>] {
        &self.references
    }

    fn into_plan(self) -> RelationalQueryIr<T, F> {
        self.plan
    }
}

/// One source reference that was resolved while checking a query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QueryReference<T = TypeId, F = FieldId> {
    kind: QueryReferenceKind,
    target: QueryReferenceTarget<T, F>,
    location: SourceLocation,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> QueryReference<T, F> {
    pub(crate) const fn kind(&self) -> QueryReferenceKind {
        self.kind
    }

    pub(crate) fn target(&self) -> &QueryReferenceTarget<T, F> {
        &self.target
    }

    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The query construct that produced a source reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueryReferenceKind {
    QueryObject,
    ObjectReference,
    QueryField,
}

/// The resolved catalogue identity for a query reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueryReferenceTarget<T = TypeId, F = FieldId> {
    Object(T),
    Field { owner: T, field: F },
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> RelationalQueryIr<T, F> {
    pub(crate) fn scan(&self) -> &ScanIr<T> {
        &self.scan
    }

    pub(crate) fn projections(&self) -> &[ExpressionIr<T, F>] {
        &self.projections
    }

    pub(crate) fn selection(&self) -> Option<&ExpressionIr<T, F>> {
        self.selection.as_ref()
    }

    pub(crate) fn ordering(&self) -> &[OrderingIr<T, F>] {
        &self.ordering
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the preparation stage consumes checked identity mappings in the next slice"
    )
)]
impl<T: Copy, F: Copy> RelationalQueryIr<T, F> {
    /// Rewrites every semantic identity while preserving the checked plan shape.
    ///
    /// The caller supplies one fallible mapping for each identity kind. An
    /// error rejects the complete rewritten plan.
    pub(crate) fn try_map_identities<T2, F2, E>(
        &self,
        mut map_type: impl FnMut(T) -> Result<T2, E>,
        mut map_field: impl FnMut(F) -> Result<F2, E>,
    ) -> Result<RelationalQueryIr<T2, F2>, E> {
        Ok(RelationalQueryIr {
            scan: ScanIr {
                input: self.scan.input,
                object_type: map_type(self.scan.object_type)?,
            },
            projections: self
                .projections
                .iter()
                .map(|expression| try_map_expression(expression, &mut map_type, &mut map_field))
                .collect::<Result<_, _>>()?,
            selection: self
                .selection
                .as_ref()
                .map(|expression| try_map_expression(expression, &mut map_type, &mut map_field))
                .transpose()?,
            ordering: self
                .ordering
                .iter()
                .map(|ordering| {
                    Ok(OrderingIr {
                        expression: try_map_expression(
                            &ordering.expression,
                            &mut map_type,
                            &mut map_field,
                        )?,
                        direction: ordering.direction,
                        null_order: ordering.null_order,
                    })
                })
                .collect::<Result<_, E>>()?,
        })
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the preparation stage consumes checked identity mappings in the next slice"
    )
)]
fn try_map_expression<T: Copy, F: Copy, T2, F2, E>(
    expression: &ExpressionIr<T, F>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
    map_field: &mut impl FnMut(F) -> Result<F2, E>,
) -> Result<ExpressionIr<T2, F2>, E> {
    let kind = match &expression.kind {
        ExpressionKind::ObjectReference { input } => {
            ExpressionKind::ObjectReference { input: *input }
        }
        ExpressionKind::FieldPath { input, steps } => ExpressionKind::FieldPath {
            input: *input,
            steps: steps
                .iter()
                .map(|step| {
                    Ok(ResolvedFieldStep {
                        owner: map_type(step.owner)?,
                        field: map_field(step.field)?,
                    })
                })
                .collect::<Result<_, E>>()?,
        },
        ExpressionKind::BooleanLiteral { value } => {
            ExpressionKind::BooleanLiteral { value: *value }
        }
        ExpressionKind::Equality { left, right } => ExpressionKind::Equality {
            left: Box::new(try_map_expression(left, map_type, map_field)?),
            right: Box::new(try_map_expression(right, map_type, map_field)?),
        },
    };

    Ok(ExpressionIr {
        kind,
        value_type: ValueType {
            semantic_type: try_map_semantic_type(expression.value_type.semantic_type, map_type)?,
            nullable: expression.value_type.nullable,
        },
    })
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the preparation stage consumes checked identity mappings in the next slice"
    )
)]
fn try_map_semantic_type<T: Copy, T2, E>(
    semantic_type: SemanticType<T>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
) -> Result<SemanticType<T2>, E> {
    Ok(match semantic_type {
        SemanticType::Scalar(scalar) => SemanticType::Scalar(scalar),
        SemanticType::Named(type_id) => SemanticType::Named(map_type(type_id)?),
        SemanticType::Reference { target } => SemanticType::Reference {
            target: map_type(target)?,
        },
    })
}

/// The single object input scanned by this query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScanIr<T = TypeId> {
    input: InputSlot,
    object_type: T,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T> ScanIr<T> {
    pub(crate) const fn input(&self) -> InputSlot {
        self.input
    }

    pub(crate) const fn object_type(&self) -> T
    where
        T: Copy,
    {
        self.object_type
    }
}

/// A checked ordering expression and its source-selected ordering rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OrderingIr<T = TypeId, F = FieldId> {
    expression: ExpressionIr<T, F>,
    direction: SortDirection,
    null_order: NullOrder,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> OrderingIr<T, F> {
    pub(crate) fn expression(&self) -> &ExpressionIr<T, F> {
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
pub(crate) struct ExpressionIr<T = TypeId, F = FieldId> {
    kind: ExpressionKind<T, F>,
    value_type: ValueType<T>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> ExpressionIr<T, F> {
    pub(crate) fn kind(&self) -> &ExpressionKind<T, F> {
        &self.kind
    }

    pub(crate) const fn value_type(&self) -> ValueType<T>
    where
        T: Copy,
    {
        self.value_type
    }
}

/// The resolved meaning of one relational expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpressionKind<T = TypeId, F = FieldId> {
    ObjectReference {
        input: InputSlot,
    },
    FieldPath {
        input: InputSlot,
        steps: Vec<ResolvedFieldStep<T, F>>,
    },
    BooleanLiteral {
        value: bool,
    },
    Equality {
        left: Box<ExpressionIr<T, F>>,
        right: Box<ExpressionIr<T, F>>,
    },
}

/// One stable field reference in an ordered path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFieldStep<T = TypeId, F = FieldId> {
    owner: T,
    field: F,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F> ResolvedFieldStep<T, F> {
    pub(crate) const fn owner(&self) -> T
    where
        T: Copy,
    {
        self.owner
    }

    pub(crate) const fn field(&self) -> F
    where
        F: Copy,
    {
        self.field
    }
}

/// The resolved type and nullability of a relational expression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueType<T = TypeId> {
    semantic_type: SemanticType<T>,
    nullable: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T> ValueType<T> {
    /// Returns the semantic type in this query's identity domain.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.semantic_type
    }

    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }
}

#[cfg(test)]
impl ValueType<TypeId> {
    /// Returns the durable core type for compatibility with existing callers.
    pub(crate) const fn resolved_type(&self) -> ResolvedType {
        self.semantic_type.into_core()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl RelationalQueryIr<TypeId, FieldId> {
    /// Encodes this durable checked query as one canonical server-plan artifact.
    pub(crate) fn encode_server_plan(
        &self,
    ) -> Result<Vec<u8>, orna_artifact::server_plan::ServerPlanError> {
        artifact::encode(self)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl IdentitySelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId> {
    /// Encodes this checked identity-selected query as a version-2 server plan.
    pub(crate) fn encode_identity_selected_server_plan(
        &self,
    ) -> Result<EncodedIdentitySelectedServerPlan, orna_artifact::server_plan::ServerPlanError>
    {
        artifact::encode_identity_selected(self)
    }
}

/// Checks a parsed one-source `SELECT` query against an immutable catalogue.
///
/// The result contains stable catalogue identities only. Any semantic error
/// rejects the complete query and returns source-located diagnostics.
#[cfg(test)]
pub(crate) fn check_query(
    query: &SelectQuery,
    catalogue: &CatalogueSnapshot,
    logical_path: &str,
) -> Result<RelationalQueryIr, Vec<CompilerDiagnostic>> {
    check_query_in(query, catalogue, logical_path).map(QueryCheck::into_plan)
}

/// Checks a parsed one-source `SELECT` query against one identity domain.
///
/// The result keeps the identities supplied by the catalogue. This permits
/// resolver-local checked identities without creating durable core identities.
pub(crate) fn check_query_in<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
) -> Result<QueryCheck<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    let CheckedQuerySource {
        context,
        projections,
        mut references,
        mut diagnostics,
    } = check_source_and_projections(query, catalogue, logical_path)?;

    let selection = query.predicate.as_ref().and_then(|expression| {
        let source_span = expression.span();
        let expression = check_expression(
            expression,
            &context,
            catalogue,
            logical_path,
            &mut diagnostics,
            &mut references,
        )?;
        if expression.value_type.semantic_type != SemanticType::scalar(StandardScalar::Boolean) {
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
                &mut references,
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

    Ok(QueryCheck {
        plan: RelationalQueryIr {
            scan: ScanIr {
                input: context.input,
                object_type: context.object_type,
            },
            projections,
            selection,
            ordering,
        },
        references,
    })
}

struct CheckedQuerySource<T, F> {
    context: InputContext<T>,
    projections: Vec<ExpressionIr<T, F>>,
    references: Vec<QueryReference<T, F>>,
    diagnostics: Vec<CompilerDiagnostic>,
}

/// Resolves the one source and checks projections shared by both query plans.
fn check_source_and_projections<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
) -> Result<CheckedQuerySource<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    let source_name = normalise_qualified_name(&query.source_object.object_type);
    let Some(source_object) = catalogue.object_type_id_by_name(&source_name) else {
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
        object_type: source_object,
        alias: normalise_name_part(&query.source_object.alias),
    };
    let mut diagnostics = Vec::new();
    let mut references = vec![QueryReference {
        kind: QueryReferenceKind::QueryObject,
        target: QueryReferenceTarget::Object(source_object),
        location: SourceLocation::from_syntax(logical_path, &query.source_object.object_type.span),
    }];

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
                &mut references,
            )
        })
        .collect::<Vec<_>>();

    Ok(CheckedQuerySource {
        context,
        projections,
        references,
        diagnostics,
    })
}

/// Checks the closed identity-selected `SELECT` form from ADR 0009.
///
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn check_identity_selected_query_in<T, F, G, P>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    function: G,
    parameters: &[QueryParameter<T, P>],
    logical_path: &str,
) -> Result<IdentitySelectedQueryCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
    G: Copy,
    P: Copy,
{
    if query.projections.is_empty() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "identity-selected SELECT requires at least one projection",
            logical_path,
            &query.span,
        )]);
    }
    if let Some(parameter) = query.projections.iter().find_map(parameter_read_in) {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions permit a parameter read only as the right operand of WHERE REF(source_alias) = selector_parameter",
            logical_path,
            &parameter.span,
        )]);
    }
    if !query.ordering.is_empty() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions do not support ORDER BY",
            logical_path,
            &query.ordering[0].span,
        )]);
    }
    if parameters.len() != 1 {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require exactly one declared parameter",
            logical_path,
            &query.span,
        )]);
    }

    let Some(QueryExpression::Equality { left, right, .. }) = query.predicate.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter",
            logical_path,
            match query.predicate.as_ref() {
                Some(expression) => expression.span(),
                None => &query.span,
            },
        )]);
    };
    let QueryExpression::ObjectReference { alias, .. } = left.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter",
            logical_path,
            left.span(),
        )]);
    };
    let QueryExpression::ParameterRead { parameter } = right.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter",
            logical_path,
            right.span(),
        )]);
    };

    let CheckedQuerySource {
        context,
        projections,
        references,
        diagnostics,
    } = check_source_and_projections(query, catalogue, logical_path)?;
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let mut diagnostics = Vec::new();
    if check_alias(alias, &context, logical_path, &mut diagnostics).is_none() {
        return Err(diagnostics);
    }
    let selector = &parameters[0];
    let selector_name = normalise_name_part(parameter);
    let source_name = normalise_qualified_name(&query.source_object.object_type);
    if selector.semantic_name() != selector_name {
        return Err(vec![diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("this function has no parameter named {selector_name}"),
            logical_path,
            &parameter.span,
        )]);
    }
    if selector.semantic_type() != SemanticType::reference(context.object_type) {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("selector parameter {selector_name} must use REF {source_name}",),
            logical_path,
            &parameter.span,
        )]);
    }
    let mut references = references
        .into_iter()
        .map(identity_selected_reference)
        .collect::<Vec<_>>();
    references.push(IdentitySelectedQueryReference::ObjectReference {
        object_type: context.object_type,
        location: SourceLocation::from_syntax(logical_path, &alias.span),
    });
    references.push(IdentitySelectedQueryReference::ParameterRead {
        owner: function,
        parameter: selector.parameter(),
        location: SourceLocation::from_syntax(logical_path, &parameter.span),
    });
    Ok(IdentitySelectedQueryCheck {
        plan: IdentitySelectedQueryIr {
            scan: ScanIr {
                input: context.input,
                object_type: context.object_type,
            },
            projections,
            selector: IdentityQuerySelector {
                owner: function,
                parameter: selector.parameter(),
            },
        },
        references,
    })
}

fn parameter_read_in(expression: &QueryExpression) -> Option<&NamePart> {
    match expression {
        QueryExpression::ParameterRead { parameter } => Some(parameter),
        QueryExpression::Equality { left, right, .. } => {
            parameter_read_in(left).or_else(|| parameter_read_in(right))
        }
        QueryExpression::ObjectReference { .. }
        | QueryExpression::FieldPath { .. }
        | QueryExpression::BooleanLiteral { .. } => None,
    }
}

fn identity_selected_reference<T, F, G, P>(
    reference: QueryReference<T, F>,
) -> IdentitySelectedQueryReference<T, F, G, P> {
    match reference {
        QueryReference {
            kind: QueryReferenceKind::QueryObject,
            target: QueryReferenceTarget::Object(object_type),
            location,
        } => IdentitySelectedQueryReference::QueryObject {
            object_type,
            location,
        },
        QueryReference {
            kind: QueryReferenceKind::ObjectReference,
            target: QueryReferenceTarget::Object(object_type),
            location,
        } => IdentitySelectedQueryReference::ObjectReference {
            object_type,
            location,
        },
        QueryReference {
            kind: QueryReferenceKind::QueryField,
            target: QueryReferenceTarget::Field { owner, field },
            location,
        } => IdentitySelectedQueryReference::QueryField {
            owner,
            field,
            location,
        },
        _ => unreachable!("query reference kind and target always agree"),
    }
}

struct InputContext<T> {
    input: InputSlot,
    object_type: T,
    alias: String,
}

fn check_expression<T, F>(
    expression: &QueryExpression,
    context: &InputContext<T>,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<QueryReference<T, F>>,
) -> Option<ExpressionIr<T, F>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    match expression {
        QueryExpression::ObjectReference { alias, .. } => {
            check_alias(alias, context, logical_path, diagnostics)?;
            references.push(QueryReference {
                kind: QueryReferenceKind::ObjectReference,
                target: QueryReferenceTarget::Object(context.object_type),
                location: SourceLocation::from_syntax(logical_path, &alias.span),
            });
            Some(ExpressionIr {
                kind: ExpressionKind::ObjectReference {
                    input: context.input,
                },
                value_type: ValueType {
                    semantic_type: SemanticType::reference(context.object_type),
                    nullable: false,
                },
            })
        }
        QueryExpression::FieldPath { root, members, .. } => {
            check_alias(root, context, logical_path, diagnostics)?;
            check_field_path(
                members,
                context,
                catalogue,
                logical_path,
                diagnostics,
                references,
            )
        }
        QueryExpression::BooleanLiteral { value, .. } => Some(ExpressionIr {
            kind: ExpressionKind::BooleanLiteral { value: *value },
            value_type: ValueType {
                semantic_type: SemanticType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }),
        QueryExpression::ParameterRead { parameter } => {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "SERVER SELECT parameter selectors are not yet supported",
                logical_path,
                &parameter.span,
            ));
            None
        }
        QueryExpression::Equality { left, right, span } => {
            let left = check_expression(
                left,
                context,
                catalogue,
                logical_path,
                diagnostics,
                references,
            );
            let right = check_expression(
                right,
                context,
                catalogue,
                logical_path,
                diagnostics,
                references,
            );
            let (Some(left), Some(right)) = (left, right) else {
                return None;
            };

            if left.value_type.semantic_type != right.value_type.semantic_type {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "equality requires expressions with compatible types",
                    logical_path,
                    span,
                ));
                return None;
            }
            if !supports_server_select_equality(left.value_type.semantic_type) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
                    logical_path,
                    span,
                ));
                return None;
            }

            Some(ExpressionIr {
                value_type: ValueType {
                    semantic_type: SemanticType::scalar(StandardScalar::Boolean),
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

pub(crate) fn supports_server_select_equality<T>(semantic_type: SemanticType<T>) -> bool {
    matches!(
        semantic_type,
        SemanticType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        ) | SemanticType::Reference { .. }
    )
}

fn check_field_path<T, F>(
    members: &[NamePart],
    context: &InputContext<T>,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<QueryReference<T, F>>,
) -> Option<ExpressionIr<T, F>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    let mut owner = context.object_type;
    let mut nullable = false;
    let mut steps = Vec::with_capacity(members.len());

    for (index, member) in members.iter().enumerate() {
        let Some(object_type_name) = catalogue.object_type_name_by_id(owner) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::InvalidReferenceTarget,
                format!("REF target type {owner} is not an object type"),
                logical_path,
                &member.span,
            ));
            return None;
        };
        let member_name = normalise_name_part(member);
        let Some(field) = catalogue.field_by_name(owner, &member_name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown field {member_name} on {object_type_name}"),
                logical_path,
                &member.span,
            ));
            return None;
        };

        references.push(QueryReference {
            kind: QueryReferenceKind::QueryField,
            target: QueryReferenceTarget::Field {
                owner,
                field: field.id(),
            },
            location: SourceLocation::from_syntax(logical_path, &member.span),
        });
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
                    semantic_type: field.semantic_type(),
                    nullable,
                },
            });
        }

        let SemanticType::Reference { target } = field.semantic_type() else {
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

fn check_alias<T>(
    alias: &NamePart,
    context: &InputContext<T>,
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
        CatalogueRevisionId, FieldId, FunctionId, ParameterId, SchemaId, TypeId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            SchemaDefinition,
        },
        types::{ResolvedType, StandardScalar},
    };
    use orna_syntax::{QueryExpression, SourceSpan, parse};

    use super::{
        ExpressionKind, IdentitySelectedQueryReference, NullOrder, QueryParameter,
        QueryReferenceKind, QueryReferenceTarget, SortDirection, check_identity_selected_query_in,
        check_query, check_query_in, supports_server_select_equality,
    };
    use crate::DiagnosticCode;
    use crate::resolver::{QueryCatalogue, QueryField, SemanticType};

    const TASK_TYPE: TypeId = TypeId::from_bytes([1; 16]);
    const PERSON_TYPE: TypeId = TypeId::from_bytes([2; 16]);
    const ASSIGNEE_FIELD: FieldId = FieldId::from_bytes([11; 16]);
    const COMPLETED_FIELD: FieldId = FieldId::from_bytes([12; 16]);
    const TITLE_FIELD: FieldId = FieldId::from_bytes([13; 16]);
    const SCORE_FIELD: FieldId = FieldId::from_bytes([14; 16]);
    const PERSON_NAME_FIELD: FieldId = FieldId::from_bytes([21; 16]);
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
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("test function has a SELECT body");
        body.query.clone()
    }

    fn selector_parameter(name: &str, semantic_type: SemanticType<TypeId>) -> QueryParameter {
        QueryParameter::new(name, SELECTOR_PARAMETER, semantic_type)
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
        let QueryExpression::Equality { right, .. } = wrong_right.predicate.as_mut().unwrap()
        else {
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
        parameter_projection.projections[0] = match parameter_projection.predicate.as_ref().unwrap()
        {
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
        let query = query(
            "SELECT t.unknown FROM tasks.task t WHERE other.title = t.title ORDER BY t.missing",
        );
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
}
