//! Semantic checking for the initial Orna relational query slice.
//!
//! This module owns an Orna relational IR. It records catalogue identities and
//! source-independent type facts, without selecting a storage backend.
//! Unknown fields and aliases use `UnknownQualifiedName`, because the stable
//! diagnostic set does not define a separate unknown-field code.

use std::fmt;

#[cfg(test)]
use orna_core::catalogue::CatalogueSnapshot;
use orna_core::{
    FieldId, FunctionId, ParameterId, TypeId,
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{NamePart, QueryExpression, SelectQuantifier, SelectQuery, SourceSpan};

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

/// Availability of the intrinsic Boolean type while relational source is checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntrinsicBooleanType {
    /// Legacy checking retains scalar-only compatibility facts.
    Legacy,
    /// Standard-backed checking supplies the durable Boolean identity.
    Standard(TypeId),
    /// Standard-backed checking has no Boolean compatibility contract.
    Missing,
}

/// A checked one-source relational query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationalQueryIr<T = TypeId, F = FieldId> {
    scan: ScanIr<T>,
    projections: Vec<ExpressionIr<T, F>>,
    selection: Option<ExpressionIr<T, F>>,
    ordering: Vec<OrderingIr<T, F>>,
}

/// A checked parameter-free `SELECT DISTINCT` query.
///
/// This is deliberately separate from `RelationalQueryIr`. The operation
/// excludes ordering and has its own projection type domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DistinctQueryIr<T = TypeId, F = FieldId> {
    scan: ScanIr<T>,
    projections: Vec<ExpressionIr<T, F>>,
    selection: Option<ExpressionIr<T, F>>,
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

/// A checked SERVER query selected by one direct unique Text field.
///
/// This is separate from both general relational queries and identity-selected
/// queries. The selector retains every fact required by the later version-four
/// artifact without admitting a general parameter expression.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct UniqueTextSelectedQueryIr<
    T = TypeId,
    F = FieldId,
    G = FunctionId,
    P = ParameterId,
> {
    scan: ScanIr<T>,
    projections: Vec<ExpressionIr<T, F>>,
    selector: UniqueTextQuerySelector<T, F, G, P>,
}

/// The fixed `source_alias.unique_text_field = text_parameter` selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct UniqueTextQuerySelector<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId>
{
    scan_object_type: T,
    field_owner: T,
    field: F,
    parameter_owner: G,
    parameter: P,
    text_type: ValueType<T>,
    parameter_required_non_null: bool,
}

/// The durable version and payload emitted for one checked server query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EncodedServerPlan {
    format_version: u32,
    payload: Vec<u8>,
}

impl EncodedServerPlan {
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
impl<T, F, G, P> UniqueTextSelectedQueryIr<T, F, G, P> {
    /// Returns the query scan.
    pub(crate) const fn scan(&self) -> &ScanIr<T> {
        &self.scan
    }

    /// Returns projections in source order.
    pub(crate) fn projections(&self) -> &[ExpressionIr<T, F>] {
        &self.projections
    }

    /// Returns the one fixed unique Text selector.
    pub(crate) const fn selector(&self) -> &UniqueTextQuerySelector<T, F, G, P> {
        &self.selector
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> UniqueTextQuerySelector<T, F, G, P> {
    /// Returns the selected scan object identity.
    pub(crate) const fn scan_object_type(&self) -> T
    where
        T: Copy,
    {
        self.scan_object_type
    }

    /// Returns the owner of the selected direct field.
    pub(crate) const fn field_owner(&self) -> T
    where
        T: Copy,
    {
        self.field_owner
    }

    /// Returns the selected direct field identity.
    pub(crate) const fn field(&self) -> F
    where
        F: Copy,
    {
        self.field
    }

    /// Returns the function that owns the selector parameter.
    pub(crate) const fn parameter_owner(&self) -> G
    where
        G: Copy,
    {
        self.parameter_owner
    }

    /// Returns the selector parameter identity.
    pub(crate) const fn parameter(&self) -> P
    where
        P: Copy,
    {
        self.parameter
    }

    /// Returns the exact resolved Text type and field nullability.
    pub(crate) const fn text_type(&self) -> ValueType<T>
    where
        T: Copy,
    {
        self.text_type
    }

    /// Reports whether the selected unique Text field can contain null.
    pub(crate) const fn field_nullable(&self) -> bool {
        self.text_type.nullable()
    }

    /// Reports the checked required non-null parameter fact.
    pub(crate) const fn parameter_required_non_null(&self) -> bool {
        self.parameter_required_non_null
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

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> UniqueTextSelectedQueryIr<T, F, G, P>
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
    ) -> Result<UniqueTextSelectedQueryIr<T2, F2, G2, P2>, E> {
        Ok(UniqueTextSelectedQueryIr {
            scan: ScanIr {
                input: self.scan.input,
                object_type: map_type(self.scan.object_type)?,
            },
            projections: self
                .projections
                .iter()
                .map(|expression| try_map_expression(expression, &mut map_type, &mut map_field))
                .collect::<Result<_, _>>()?,
            selector: UniqueTextQuerySelector {
                scan_object_type: map_type(self.selector.scan_object_type)?,
                field_owner: map_type(self.selector.field_owner)?,
                field: map_field(self.selector.field)?,
                parameter_owner: map_function(self.selector.parameter_owner)?,
                parameter: map_parameter(self.selector.parameter)?,
                text_type: self.selector.text_type.try_map_identities(&mut map_type)?,
                parameter_required_non_null: self.selector.parameter_required_non_null,
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

/// A checked `SELECT DISTINCT` query and its source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DistinctQueryCheck<T = TypeId, F = FieldId> {
    plan: DistinctQueryIr<T, F>,
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
    standard_value_type: Option<TypeId>,
    required_non_null: bool,
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
            standard_value_type: None,
            required_non_null: false,
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

    /// Attaches resolved standard value-type provenance for query checking.
    pub(crate) const fn with_standard_value_type(mut self, type_id: TypeId) -> Self {
        self.standard_value_type = Some(type_id);
        self
    }

    /// Marks this parameter as the required non-null form accepted by the selector.
    pub(crate) const fn with_required_non_null(mut self) -> Self {
        self.required_non_null = true;
        self
    }

    /// Returns the supplied standard value-type identity when one exists.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
    }

    /// Reports whether this parameter has the required non-null selector fact.
    pub(crate) const fn required_non_null(&self) -> bool {
        self.required_non_null
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

/// One ordered unique-Text-selected query reference.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum UniqueTextSelectedQueryReference<
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
impl<T, F, G, P> UniqueTextSelectedQueryReference<T, F, G, P> {
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

/// A checked unique-Text-selected query and its source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct UniqueTextSelectedQueryCheck<
    T = TypeId,
    F = FieldId,
    G = FunctionId,
    P = ParameterId,
> {
    plan: UniqueTextSelectedQueryIr<T, F, G, P>,
    references: Vec<UniqueTextSelectedQueryReference<T, F, G, P>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T, F, G, P> UniqueTextSelectedQueryCheck<T, F, G, P> {
    /// Returns the source-free checked query plan.
    pub(crate) fn plan(&self) -> &UniqueTextSelectedQueryIr<T, F, G, P> {
        &self.plan
    }

    /// Returns query references in deterministic source order.
    pub(crate) fn references(&self) -> &[UniqueTextSelectedQueryReference<T, F, G, P>] {
        &self.references
    }
}

impl<T, F> DistinctQueryCheck<T, F> {
    /// Returns the source-free checked DISTINCT query plan.
    pub(crate) fn plan(&self) -> &DistinctQueryIr<T, F> {
        &self.plan
    }

    /// Returns query references in deterministic source order.
    pub(crate) fn references(&self) -> &[QueryReference<T, F>] {
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

impl<T, F> DistinctQueryIr<T, F> {
    /// Returns the query scan.
    pub(crate) fn scan(&self) -> &ScanIr<T> {
        &self.scan
    }

    /// Returns projections in source order.
    pub(crate) fn projections(&self) -> &[ExpressionIr<T, F>] {
        &self.projections
    }

    /// Returns the optional query predicate.
    pub(crate) fn selection(&self) -> Option<&ExpressionIr<T, F>> {
        self.selection.as_ref()
    }
}

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

impl<T: Copy, F: Copy> DistinctQueryIr<T, F> {
    /// Rewrites every semantic identity while preserving the checked DISTINCT shape.
    pub(crate) fn try_map_identities<T2, F2, E>(
        &self,
        mut map_type: impl FnMut(T) -> Result<T2, E>,
        mut map_field: impl FnMut(F) -> Result<F2, E>,
    ) -> Result<DistinctQueryIr<T2, F2>, E> {
        Ok(DistinctQueryIr {
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
        })
    }
}

/// One closed malformed duplicate-preserving query shape used to test durable validation.
#[cfg(test)]
pub(crate) enum RelationalQueryTestMutation {
    /// Changes the scan object identity.
    InvalidScan,
    /// Changes the input of the field-path projection.
    InvalidProjectionFieldPathInput,
    /// Changes the input of the object-reference projection.
    InvalidObjectReferenceInput,
    /// Changes the type facts of the Boolean literal in the equality selection.
    InvalidBooleanLiteralType,
    /// Changes the result type facts of the equality selection.
    InvalidEqualityType,
    /// Changes the input of the first ordering field path.
    InvalidOrderingFieldPathInput,
    /// Replaces the selection with the valid object-reference projection.
    SelectionObjectReference,
}

#[cfg(test)]
impl RelationalQueryIr<TypeId, FieldId> {
    /// Returns one deliberately malformed duplicate-preserving query for preparation tests.
    pub(crate) fn with_test_mutation(&self, mutation: RelationalQueryTestMutation) -> Self {
        let mut query = self.clone();
        match mutation {
            RelationalQueryTestMutation::InvalidScan => {
                query.scan.object_type = TypeId::new();
            }
            RelationalQueryTestMutation::InvalidProjectionFieldPathInput => {
                let ExpressionKind::FieldPath { input, .. } = &mut query.projections[1].kind else {
                    panic!("test fixture second projection must be a field path");
                };
                *input = InputSlot(1);
            }
            RelationalQueryTestMutation::InvalidObjectReferenceInput => {
                let ExpressionKind::ObjectReference { input } = &mut query.projections[0].kind
                else {
                    panic!("test fixture first projection must be an object reference");
                };
                *input = InputSlot(1);
            }
            RelationalQueryTestMutation::InvalidBooleanLiteralType => {
                let Some(ExpressionIr {
                    kind: ExpressionKind::Equality { right, .. },
                    ..
                }) = query.selection.as_mut()
                else {
                    panic!("test fixture selection must be an equality");
                };
                right.value_type = ValueType::legacy_scalar(StandardScalar::Integer, false);
            }
            RelationalQueryTestMutation::InvalidEqualityType => {
                let Some(selection) = query.selection.as_mut() else {
                    panic!("test fixture must have a selection");
                };
                selection.value_type = ValueType::legacy_scalar(StandardScalar::Integer, false);
            }
            RelationalQueryTestMutation::InvalidOrderingFieldPathInput => {
                let ExpressionKind::FieldPath { input, .. } =
                    &mut query.ordering[0].expression.kind
                else {
                    panic!("test fixture first ordering must be a field path");
                };
                *input = InputSlot(1);
            }
            RelationalQueryTestMutation::SelectionObjectReference => {
                query.selection = Some(query.projections[0].clone());
            }
        }
        query
    }
}

/// One closed malformed DISTINCT shape used to test durable validation.
#[cfg(test)]
pub(crate) enum DistinctQueryTestMutation {
    /// Changes the input of the final field-path projection.
    InvalidFieldPathInput,
    /// Changes the input of the object-reference projection.
    InvalidObjectReferenceInput,
    /// Changes the type facts of the object-reference projection.
    InvalidObjectReferenceType,
    /// Changes the type facts of the Boolean literal in the selection.
    InvalidBooleanLiteralType,
    /// Changes the result type facts of the equality selection.
    InvalidEqualityType,
    /// Removes the optional selection.
    ClearSelection,
    /// Changes the final field-path projection type facts.
    ProjectionType {
        /// The replacement semantic type.
        semantic_type: SemanticType<TypeId>,
        /// The replacement nullability fact.
        nullable: bool,
    },
    /// Replaces the selection with the valid object-reference projection.
    SelectionObjectReference,
}

#[cfg(test)]
impl DistinctQueryIr<TypeId, FieldId> {
    /// Returns one deliberately malformed DISTINCT query for preparation tests.
    pub(crate) fn with_test_mutation(&self, mutation: DistinctQueryTestMutation) -> Self {
        let mut query = self.clone();
        match mutation {
            DistinctQueryTestMutation::InvalidFieldPathInput => {
                let ExpressionKind::FieldPath { input, .. } = &mut query.projections[2].kind else {
                    panic!("test fixture final projection must be a field path");
                };
                *input = InputSlot(1);
            }
            DistinctQueryTestMutation::InvalidObjectReferenceInput => {
                let ExpressionKind::ObjectReference { input } = &mut query.projections[0].kind
                else {
                    panic!("test fixture first projection must be an object reference");
                };
                *input = InputSlot(1);
            }
            DistinctQueryTestMutation::InvalidObjectReferenceType => {
                query.projections[0].value_type =
                    ValueType::legacy_scalar(StandardScalar::Boolean, false);
            }
            DistinctQueryTestMutation::InvalidBooleanLiteralType => {
                let Some(ExpressionIr {
                    kind: ExpressionKind::Equality { right, .. },
                    ..
                }) = query.selection.as_mut()
                else {
                    panic!("test fixture selection must be an equality");
                };
                right.value_type = ValueType::legacy_scalar(StandardScalar::Integer, false);
            }
            DistinctQueryTestMutation::InvalidEqualityType => {
                let Some(selection) = query.selection.as_mut() else {
                    panic!("test fixture must have a selection");
                };
                selection.value_type = ValueType::legacy_scalar(StandardScalar::Integer, false);
            }
            DistinctQueryTestMutation::ClearSelection => {
                query.selection = None;
            }
            DistinctQueryTestMutation::ProjectionType {
                semantic_type,
                nullable,
            } => {
                query.projections[2].value_type =
                    ValueType::from_legacy_semantic_type(semantic_type, nullable);
            }
            DistinctQueryTestMutation::SelectionObjectReference => {
                query.selection = Some(query.projections[0].clone());
            }
        }
        query
    }
}

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
        value_type: expression.value_type.try_map_identities(map_type)?,
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
    resolved_value: ResolvedValueType<T>,
    nullable: bool,
}

/// The complete resolved-value inspection result for a relational expression.
///
/// A standard value retains both its supplied identity and its version-1
/// compatibility representation. The latter supports relational allowlists
/// and legacy artifact representation, but not equality identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolvedValueType<T> {
    LegacyScalar(StandardScalar),
    StandardValue {
        type_id: TypeId,
        compatibility: StandardScalar,
    },
    Named(T),
    Reference {
        target: T,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
impl<T> ValueType<T> {
    const fn legacy_scalar(scalar: StandardScalar, nullable: bool) -> Self {
        Self {
            resolved_value: ResolvedValueType::LegacyScalar(scalar),
            nullable,
        }
    }

    const fn standard_value(
        type_id: TypeId,
        compatibility: StandardScalar,
        nullable: bool,
    ) -> Self {
        Self {
            resolved_value: ResolvedValueType::StandardValue {
                type_id,
                compatibility,
            },
            nullable,
        }
    }

    const fn named(type_id: T, nullable: bool) -> Self {
        Self {
            resolved_value: ResolvedValueType::Named(type_id),
            nullable,
        }
    }

    const fn reference(target: T, nullable: bool) -> Self {
        Self {
            resolved_value: ResolvedValueType::Reference { target },
            nullable,
        }
    }

    fn from_legacy_semantic_type(semantic_type: SemanticType<T>, nullable: bool) -> Self {
        match semantic_type {
            SemanticType::Scalar(scalar) => Self::legacy_scalar(scalar, nullable),
            SemanticType::Named(type_id) => Self::named(type_id, nullable),
            SemanticType::Reference { target } => Self::reference(target, nullable),
        }
    }

    fn from_semantic_type(
        semantic_type: SemanticType<T>,
        standard_value_type: Option<TypeId>,
        nullable: bool,
    ) -> Option<Self> {
        match (semantic_type, standard_value_type) {
            (SemanticType::Scalar(scalar), None) => Some(Self::legacy_scalar(scalar, nullable)),
            (SemanticType::Scalar(compatibility), Some(type_id)) => {
                Some(Self::standard_value(type_id, compatibility, nullable))
            }
            (SemanticType::Named(type_id), None) => Some(Self::named(type_id, nullable)),
            (SemanticType::Reference { target }, None) => Some(Self::reference(target, nullable)),
            (SemanticType::Named(_) | SemanticType::Reference { .. }, Some(_)) => None,
        }
    }

    const fn resolved_value(&self) -> &ResolvedValueType<T> {
        &self.resolved_value
    }

    /// Returns the semantic type in this query's identity domain.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        match self.resolved_value {
            ResolvedValueType::LegacyScalar(scalar)
            | ResolvedValueType::StandardValue {
                compatibility: scalar,
                ..
            } => SemanticType::Scalar(scalar),
            ResolvedValueType::Named(type_id) => SemanticType::Named(type_id),
            ResolvedValueType::Reference { target } => SemanticType::Reference { target },
        }
    }

    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Returns the supplied standard value-type identity when one exists.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        match self.resolved_value {
            ResolvedValueType::StandardValue { type_id, .. } => Some(type_id),
            ResolvedValueType::LegacyScalar(_)
            | ResolvedValueType::Named(_)
            | ResolvedValueType::Reference { .. } => None,
        }
    }

    fn try_map_identities<T2, E>(
        &self,
        map_type: &mut impl FnMut(T) -> Result<T2, E>,
    ) -> Result<ValueType<T2>, E>
    where
        T: Copy,
    {
        Ok(ValueType {
            resolved_value: match self.resolved_value {
                ResolvedValueType::LegacyScalar(scalar) => ResolvedValueType::LegacyScalar(scalar),
                ResolvedValueType::StandardValue {
                    type_id,
                    compatibility,
                } => ResolvedValueType::StandardValue {
                    type_id,
                    compatibility,
                },
                ResolvedValueType::Named(type_id) => ResolvedValueType::Named(map_type(type_id)?),
                ResolvedValueType::Reference { target } => ResolvedValueType::Reference {
                    target: map_type(target)?,
                },
            },
            nullable: self.nullable,
        })
    }
}

impl ValueType<TypeId> {
    /// Returns the exact legacy type emitted into a version-1 server artifact.
    pub(super) const fn legacy_artifact_type(&self) -> ResolvedType {
        match self.resolved_value {
            ResolvedValueType::LegacyScalar(scalar) => ResolvedType::scalar(scalar),
            ResolvedValueType::StandardValue {
                compatibility: scalar,
                ..
            } => ResolvedType::scalar(scalar),
            ResolvedValueType::Named(type_id) => ResolvedType::Named(type_id),
            ResolvedValueType::Reference { target } => ResolvedType::reference(target),
        }
    }

    #[cfg(test)]
    /// Returns the durable core type for compatibility with existing callers.
    pub(crate) const fn resolved_type(&self) -> ResolvedType {
        self.legacy_artifact_type()
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
    ) -> Result<EncodedServerPlan, orna_artifact::server_plan::ServerPlanError> {
        artifact::encode_identity_selected(self)
    }
}

impl DistinctQueryIr<TypeId, FieldId> {
    /// Encodes this checked DISTINCT query as a version-3 server plan.
    pub(crate) fn encode_distinct_server_plan(
        &self,
    ) -> Result<EncodedServerPlan, orna_artifact::server_plan::ServerPlanError> {
        artifact::encode_distinct(self)
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
#[cfg(test)]
pub(crate) fn check_query_in<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
) -> Result<QueryCheck<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    check_query_with_intrinsic_boolean_in(
        query,
        catalogue,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one source query with explicit intrinsic Boolean provenance.
pub(crate) fn check_query_with_intrinsic_boolean_in<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<QueryCheck<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    check_supported_quantifier(query, logical_path)?;

    let CheckedQuerySource {
        context,
        projections,
        mut references,
        mut diagnostics,
    } = check_source_and_projections(query, catalogue, logical_path, intrinsic_boolean)?;

    let selection = check_selection(
        query.predicate.as_ref(),
        &context,
        catalogue,
        logical_path,
        intrinsic_boolean,
        &mut diagnostics,
        &mut references,
    );

    let ordering = query
        .ordering
        .iter()
        .filter_map(|ordering| {
            check_expression(
                &ordering.expression,
                &context,
                catalogue,
                logical_path,
                intrinsic_boolean,
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

/// Checks the closed parameter-free `SELECT DISTINCT` form from ADR 0010.
#[cfg(test)]
pub(crate) fn check_distinct_query_in<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
) -> Result<DistinctQueryCheck<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    check_distinct_query_with_intrinsic_boolean_in(
        query,
        catalogue,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one `SELECT DISTINCT` query with explicit intrinsic Boolean provenance.
pub(crate) fn check_distinct_query_with_intrinsic_boolean_in<T, F>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<DistinctQueryCheck<T, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    let SelectQuantifier::Distinct { .. } = &query.quantifier else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "this query must use SELECT DISTINCT",
            logical_path,
            &query.span,
        )]);
    };
    if query.projections.is_empty() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT requires at least one projection",
            logical_path,
            &query.span,
        )]);
    }
    if let Some(ordering) = query.ordering.first() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT queries do not allow ORDER BY",
            logical_path,
            &ordering.span,
        )]);
    }

    let CheckedQuerySource {
        context,
        projections,
        mut references,
        mut diagnostics,
    } = check_source_and_projections(query, catalogue, logical_path, intrinsic_boolean)?;

    let selection = check_selection(
        query.predicate.as_ref(),
        &context,
        catalogue,
        logical_path,
        intrinsic_boolean,
        &mut diagnostics,
        &mut references,
    );

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    for (source, projection) in query.projections.iter().zip(&projections) {
        if !supports_server_select_distinct_value(projection.value_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
                logical_path,
                source.span(),
            ));
        }
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    Ok(DistinctQueryCheck {
        plan: DistinctQueryIr {
            scan: ScanIr {
                input: context.input,
                object_type: context.object_type,
            },
            projections,
            selection,
        },
        references,
    })
}

/// Checks an optional query predicate and appends its evidence after projections.
fn check_selection<T, F>(
    predicate: Option<&QueryExpression>,
    context: &InputContext<T>,
    catalogue: &impl QueryCatalogue<T, F>,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<QueryReference<T, F>>,
) -> Option<ExpressionIr<T, F>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
{
    predicate.and_then(|expression| {
        let source_span = expression.span();
        let expression = check_expression(
            expression,
            context,
            catalogue,
            logical_path,
            intrinsic_boolean,
            diagnostics,
            references,
        )?;
        if !is_boolean_value(expression.value_type.resolved_value()) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "WHERE requires a BOOLEAN expression",
                logical_path,
                source_span,
            ));
            return None;
        }
        Some(expression)
    })
}

/// Rejects query quantifiers that this relational IR cannot encode.
fn check_supported_quantifier(
    query: &SelectQuery,
    logical_path: &str,
) -> Result<(), Vec<CompilerDiagnostic>> {
    match &query.quantifier {
        SelectQuantifier::All => Ok(()),
        SelectQuantifier::Distinct { source } => Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT is not available yet",
            logical_path,
            &source.span,
        )]),
        _ => Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "this SELECT form is not available yet",
            logical_path,
            &query.span,
        )]),
    }
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
    intrinsic_boolean: IntrinsicBooleanType,
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
                intrinsic_boolean,
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
    check_identity_selected_query_with_intrinsic_boolean_in(
        query,
        catalogue,
        function,
        parameters,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one identity-selected query with explicit intrinsic Boolean provenance.
pub(crate) fn check_identity_selected_query_with_intrinsic_boolean_in<T, F, G, P>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    function: G,
    parameters: &[QueryParameter<T, P>],
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<IdentitySelectedQueryCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
    G: Copy,
    P: Copy,
{
    check_supported_quantifier(query, logical_path)?;

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
    } = check_source_and_projections(query, catalogue, logical_path, intrinsic_boolean)?;
    let mut diagnostics = diagnostics;
    let _ = intrinsic_boolean_value_type::<T>(
        intrinsic_boolean,
        logical_path,
        query
            .predicate
            .as_ref()
            .map_or(&query.span, |predicate| predicate.span()),
        &mut diagnostics,
    );
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

/// Checks the closed unique-Text-selected `SELECT` form from ADR 0052.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn check_unique_text_selected_query_in<T, F, G, P>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    function: G,
    parameters: &[QueryParameter<T, P>],
    logical_path: &str,
) -> Result<UniqueTextSelectedQueryCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
    G: Copy,
    P: Copy,
{
    check_unique_text_selected_query_with_intrinsic_boolean_in(
        query,
        catalogue,
        function,
        parameters,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one unique-Text-selected query with explicit Boolean provenance.
pub(crate) fn check_unique_text_selected_query_with_intrinsic_boolean_in<T, F, G, P>(
    query: &SelectQuery,
    catalogue: &impl QueryCatalogue<T, F>,
    function: G,
    parameters: &[QueryParameter<T, P>],
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<UniqueTextSelectedQueryCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq + fmt::Display,
    F: Copy,
    G: Copy,
    P: Copy,
{
    check_supported_quantifier(query, logical_path)?;

    if query.projections.is_empty() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires at least one projection",
            logical_path,
            &query.span,
        )]);
    }
    if let Some(parameter) = query.projections.iter().find_map(parameter_read_in) {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT permits a parameter read only as the right operand of WHERE source_alias.unique_text_field = selector_parameter",
            logical_path,
            &parameter.span,
        )]);
    }
    if !query.ordering.is_empty() {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT does not support ORDER BY",
            logical_path,
            &query.ordering[0].span,
        )]);
    }
    if parameters.len() != 1 {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires exactly one declared parameter",
            logical_path,
            &query.span,
        )]);
    }

    let Some(QueryExpression::Equality { left, right, .. }) = query.predicate.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires WHERE source_alias.unique_text_field = selector_parameter",
            logical_path,
            query
                .predicate
                .as_ref()
                .map_or(&query.span, |predicate| predicate.span()),
        )]);
    };
    let QueryExpression::FieldPath { root, members, .. } = left.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires WHERE source_alias.unique_text_field = selector_parameter",
            logical_path,
            left.span(),
        )]);
    };
    if members.len() != 1 {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires one direct selector field",
            logical_path,
            left.span(),
        )]);
    }
    let QueryExpression::ParameterRead { parameter } = right.as_ref() else {
        return Err(vec![diagnostic(
            DiagnosticCode::DomainIncompatible,
            "unique-Text-selected SELECT requires WHERE source_alias.unique_text_field = selector_parameter",
            logical_path,
            right.span(),
        )]);
    };

    let CheckedQuerySource {
        context,
        projections,
        references,
        diagnostics,
    } = check_source_and_projections(query, catalogue, logical_path, intrinsic_boolean)?;
    let mut diagnostics = diagnostics;
    let _ = intrinsic_boolean_value_type::<T>(
        intrinsic_boolean,
        logical_path,
        query
            .predicate
            .as_ref()
            .map_or(&query.span, |predicate| predicate.span()),
        &mut diagnostics,
    );
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let mut diagnostics = Vec::new();
    if check_alias(root, &context, logical_path, &mut diagnostics).is_none() {
        return Err(diagnostics);
    }
    let selector = &parameters[0];
    let selector_name = normalise_name_part(parameter);
    if selector.semantic_name() != selector_name {
        return Err(vec![diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("this function has no parameter named {selector_name}"),
            logical_path,
            &parameter.span,
        )]);
    }
    if !selector.required_non_null() {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("selector parameter {selector_name} must be required and non-null TEXT"),
            logical_path,
            &parameter.span,
        )]);
    }

    let member = &members[0];
    let field_name = normalise_name_part(member);
    let Some(field) = catalogue.field_by_name(context.object_type, &field_name) else {
        let object_type_name = catalogue
            .object_type_name_by_id(context.object_type)
            .expect("checked source object must retain its catalogue name");
        return Err(vec![diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unknown field {field_name} on {object_type_name}"),
            logical_path,
            &member.span,
        )]);
    };
    let Some(field_text_type) = ValueType::from_semantic_type(
        field.semantic_type(),
        field.standard_value_type(),
        field.nullable(),
    ) else {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("field {field_name} has inconsistent standard value-type evidence"),
            logical_path,
            &member.span,
        )]);
    };
    let Some(parameter_text_type) = ValueType::from_semantic_type(
        selector.semantic_type(),
        selector.standard_value_type(),
        false,
    ) else {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "selector parameter {selector_name} has inconsistent standard value-type evidence"
            ),
            logical_path,
            &parameter.span,
        )]);
    };
    if !field.unique()
        || field_text_type.semantic_type()
            != SemanticType::scalar(StandardScalar::CharacterLargeObject)
    {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("selector field {field_name} must be UNIQUE TEXT"),
            logical_path,
            &member.span,
        )]);
    }
    if parameter_text_type.semantic_type()
        != SemanticType::scalar(StandardScalar::CharacterLargeObject)
        || parameter_text_type.resolved_value() != field_text_type.resolved_value()
    {
        return Err(vec![diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "selector parameter {selector_name} must use the selected field's exact TEXT type"
            ),
            logical_path,
            &parameter.span,
        )]);
    }

    let mut references = references
        .into_iter()
        .map(unique_text_selected_reference)
        .collect::<Vec<_>>();
    references.push(UniqueTextSelectedQueryReference::QueryField {
        owner: context.object_type,
        field: field.id(),
        location: SourceLocation::from_syntax(logical_path, &member.span),
    });
    references.push(UniqueTextSelectedQueryReference::ParameterRead {
        owner: function,
        parameter: selector.parameter(),
        location: SourceLocation::from_syntax(logical_path, &parameter.span),
    });
    Ok(UniqueTextSelectedQueryCheck {
        plan: UniqueTextSelectedQueryIr {
            scan: ScanIr {
                input: context.input,
                object_type: context.object_type,
            },
            projections,
            selector: UniqueTextQuerySelector {
                scan_object_type: context.object_type,
                field_owner: context.object_type,
                field: field.id(),
                parameter_owner: function,
                parameter: selector.parameter(),
                text_type: field_text_type,
                parameter_required_non_null: selector.required_non_null(),
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

fn unique_text_selected_reference<T, F, G, P>(
    reference: QueryReference<T, F>,
) -> UniqueTextSelectedQueryReference<T, F, G, P> {
    match reference {
        QueryReference {
            kind: QueryReferenceKind::QueryObject,
            target: QueryReferenceTarget::Object(object_type),
            location,
        } => UniqueTextSelectedQueryReference::QueryObject {
            object_type,
            location,
        },
        QueryReference {
            kind: QueryReferenceKind::ObjectReference,
            target: QueryReferenceTarget::Object(object_type),
            location,
        } => UniqueTextSelectedQueryReference::ObjectReference {
            object_type,
            location,
        },
        QueryReference {
            kind: QueryReferenceKind::QueryField,
            target: QueryReferenceTarget::Field { owner, field },
            location,
        } => UniqueTextSelectedQueryReference::QueryField {
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
    intrinsic_boolean: IntrinsicBooleanType,
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
                value_type: ValueType::reference(context.object_type, false),
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
        QueryExpression::BooleanLiteral { value, source } => {
            let value_type = intrinsic_boolean_value_type(
                intrinsic_boolean,
                logical_path,
                &source.span,
                diagnostics,
            )?;
            Some(ExpressionIr {
                kind: ExpressionKind::BooleanLiteral { value: *value },
                value_type,
            })
        }
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
            let value_type =
                intrinsic_boolean_value_type(intrinsic_boolean, logical_path, span, diagnostics);
            let left = check_expression(
                left,
                context,
                catalogue,
                logical_path,
                intrinsic_boolean,
                diagnostics,
                references,
            );
            let right = check_expression(
                right,
                context,
                catalogue,
                logical_path,
                intrinsic_boolean,
                diagnostics,
                references,
            );
            let (Some(mut value_type), Some(left), Some(right)) = (value_type, left, right) else {
                return None;
            };

            if !resolved_values_match(
                left.value_type.resolved_value(),
                right.value_type.resolved_value(),
            ) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "equality requires expressions with compatible types",
                    logical_path,
                    span,
                ));
                return None;
            }
            if !supports_server_select_equality_value(left.value_type) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
                    logical_path,
                    span,
                ));
                return None;
            }
            value_type.nullable = left.value_type.nullable || right.value_type.nullable;
            Some(ExpressionIr {
                value_type,
                kind: ExpressionKind::Equality {
                    left: Box::new(left),
                    right: Box::new(right),
                },
            })
        }
    }
}

fn intrinsic_boolean_value_type<T>(
    intrinsic_boolean: IntrinsicBooleanType,
    logical_path: &str,
    span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ValueType<T>> {
    let standard_value_type = match intrinsic_boolean {
        IntrinsicBooleanType::Legacy => None,
        IntrinsicBooleanType::Standard(type_id) => Some(type_id),
        IntrinsicBooleanType::Missing => {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                logical_path,
                span,
            ));
            return None;
        }
    };
    Some(match standard_value_type {
        Some(type_id) => ValueType::standard_value(type_id, StandardScalar::Boolean, false),
        None => ValueType::legacy_scalar(StandardScalar::Boolean, false),
    })
}

pub(crate) fn supports_server_select_equality<T>(semantic_type: SemanticType<T>) -> bool {
    supports_server_select_equality_value(ValueType::from_legacy_semantic_type(
        semantic_type,
        false,
    ))
}

/// Returns whether one projection type has accepted Orna DISTINCT semantics.
pub(crate) fn supports_server_select_distinct<T>(semantic_type: SemanticType<T>) -> bool {
    supports_server_select_distinct_value(ValueType::from_legacy_semantic_type(
        semantic_type,
        false,
    ))
}

fn supports_server_select_equality_value<T>(value_type: ValueType<T>) -> bool {
    supports_resolved_value(value_type.resolved_value())
}

fn supports_server_select_distinct_value<T>(value_type: ValueType<T>) -> bool {
    supports_resolved_value(value_type.resolved_value())
}

fn resolved_values_match<T: Eq>(left: &ResolvedValueType<T>, right: &ResolvedValueType<T>) -> bool {
    match (left, right) {
        (
            ResolvedValueType::StandardValue {
                type_id: left_type_id,
                ..
            },
            ResolvedValueType::StandardValue {
                type_id: right_type_id,
                ..
            },
        ) => left_type_id == right_type_id,
        (
            ResolvedValueType::LegacyScalar(left_scalar),
            ResolvedValueType::LegacyScalar(right_scalar),
        ) => left_scalar == right_scalar,
        (ResolvedValueType::Named(left_type_id), ResolvedValueType::Named(right_type_id)) => {
            left_type_id == right_type_id
        }
        (
            ResolvedValueType::Reference {
                target: left_target,
            },
            ResolvedValueType::Reference {
                target: right_target,
            },
        ) => left_target == right_target,
        _ => false,
    }
}

fn supports_resolved_value<T>(value_type: &ResolvedValueType<T>) -> bool {
    matches!(
        value_type,
        ResolvedValueType::LegacyScalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        ) | ResolvedValueType::StandardValue {
            compatibility: StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject,
            ..
        } | ResolvedValueType::Reference { .. }
    )
}

fn is_boolean_value<T>(value_type: &ResolvedValueType<T>) -> bool {
    matches!(
        value_type,
        ResolvedValueType::LegacyScalar(StandardScalar::Boolean)
            | ResolvedValueType::StandardValue {
                compatibility: StandardScalar::Boolean,
                ..
            }
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

        let Some(value_type) = ValueType::from_semantic_type(
            field.semantic_type(),
            field.standard_value_type(),
            nullable | field.nullable(),
        ) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("field {member_name} has inconsistent standard value-type evidence"),
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
                value_type,
            });
        }

        let SemanticType::Reference { target } = value_type.semantic_type() else {
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
mod tests;
