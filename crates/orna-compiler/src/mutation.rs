//! Semantic checking for the single-row server-function mutation slice.
//!
//! This module deliberately stops at a source-free, identity-bearing plan. It
//! does not know about a physical backend or the catalogue-dependent checks
//! that are applied when the plan is lowered to an execution artifact.

use orna_core::{
    FieldId, FunctionId, ParameterId, TypeId, catalogue::QualifiedSemanticName,
    types::StandardScalar,
};
#[cfg(test)]
use orna_syntax::NamePart;
use orna_syntax::{InsertStatement, InsertValue, SourceSpan};

use crate::resolver::SemanticType;
use crate::{
    CompilerDiagnostic, DiagnosticCode, SourceLocation, normalise_name_part,
    normalise_qualified_name, semantic_diagnostic,
};

/// A source-free checked one-row mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationPlanIr<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    target_object: T,
    assignments: Vec<MutationAssignment<T, F, G, P>>,
    returned_object: T,
}

impl<T, F, G, P> MutationPlanIr<T, F, G, P> {
    /// Returns the target object identity.
    pub(crate) const fn target_object(&self) -> T
    where
        T: Copy,
    {
        self.target_object
    }

    /// Returns assignments in source positional order.
    pub(crate) fn assignments(&self) -> &[MutationAssignment<T, F, G, P>] {
        &self.assignments
    }

    /// Returns the object identity produced by `RETURNING REF`.
    pub(crate) const fn returned_object(&self) -> T
    where
        T: Copy,
    {
        self.returned_object
    }
}

impl<T, F, G, P> MutationPlanIr<T, F, G, P>
where
    T: Copy,
    F: Copy,
    G: Copy,
    P: Copy,
{
    /// Rewrites every identity in this plan, rejecting the complete rewrite
    /// when any supplied mapping fails.
    pub(crate) fn try_map_identities<T2, F2, G2, P2, E>(
        &self,
        mut map_type: impl FnMut(T) -> Result<T2, E>,
        mut map_field: impl FnMut(F) -> Result<F2, E>,
        mut map_function: impl FnMut(G) -> Result<G2, E>,
        mut map_parameter: impl FnMut(P) -> Result<P2, E>,
    ) -> Result<MutationPlanIr<T2, F2, G2, P2>, E> {
        let target_object = map_type(self.target_object)?;
        let returned_object = map_type(self.returned_object)?;
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(MutationAssignment {
                    owner: map_type(assignment.owner)?,
                    field: map_field(assignment.field)?,
                    expression: map_expression(
                        &assignment.expression,
                        &mut map_type,
                        &mut map_function,
                        &mut map_parameter,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, E>>()?;

        Ok(MutationPlanIr {
            target_object,
            assignments,
            returned_object,
        })
    }
}

fn map_expression<T, G, P, T2, G2, P2, E>(
    expression: &MutationExpression<T, G, P>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
    map_function: &mut impl FnMut(G) -> Result<G2, E>,
    map_parameter: &mut impl FnMut(P) -> Result<P2, E>,
) -> Result<MutationExpression<T2, G2, P2>, E>
where
    T: Copy,
    G: Copy,
    P: Copy,
{
    let kind = match expression.kind {
        MutationExpressionKind::ParameterRead { owner, parameter } => {
            MutationExpressionKind::ParameterRead {
                owner: map_function(owner)?,
                parameter: map_parameter(parameter)?,
            }
        }
        MutationExpressionKind::BooleanLiteral { value } => {
            MutationExpressionKind::BooleanLiteral { value }
        }
        MutationExpressionKind::TypedNull => MutationExpressionKind::TypedNull,
    };

    Ok(MutationExpression {
        kind,
        value_type: map_value_type(expression.value_type, map_type)?,
    })
}

fn map_value_type<T, T2, E>(
    value_type: MutationValueType<T>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
) -> Result<MutationValueType<T2>, E>
where
    T: Copy,
{
    Ok(MutationValueType {
        semantic_type: map_semantic_type(value_type.semantic_type, map_type)?,
        nullable: value_type.nullable,
    })
}

fn map_semantic_type<T, T2, E>(
    semantic_type: SemanticType<T>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
) -> Result<SemanticType<T2>, E> {
    Ok(match semantic_type {
        SemanticType::Scalar(scalar) => SemanticType::Scalar(scalar),
        SemanticType::Named(id) => SemanticType::Named(map_type(id)?),
        SemanticType::Reference { target } => SemanticType::Reference {
            target: map_type(target)?,
        },
    })
}

/// One target-field assignment in a mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationAssignment<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    owner: T,
    field: F,
    expression: MutationExpression<T, G, P>,
}

impl<T, F, G, P> MutationAssignment<T, F, G, P> {
    /// Creates one source-free assignment.
    pub(crate) const fn new(owner: T, field: F, expression: MutationExpression<T, G, P>) -> Self {
        Self {
            owner,
            field,
            expression,
        }
    }

    /// Returns the owning object identity.
    pub(crate) const fn owner(&self) -> T
    where
        T: Copy,
    {
        self.owner
    }

    /// Returns the field identity.
    pub(crate) const fn field(&self) -> F
    where
        F: Copy,
    {
        self.field
    }

    /// Returns the assigned expression.
    pub(crate) fn expression(&self) -> &MutationExpression<T, G, P> {
        &self.expression
    }
}

/// A checked mutation expression and its semantic value facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationExpression<T = TypeId, G = FunctionId, P = ParameterId> {
    kind: MutationExpressionKind<G, P>,
    value_type: MutationValueType<T>,
}

impl<T, G, P> MutationExpression<T, G, P> {
    /// Creates an expression with its already-resolved semantic value facts.
    pub(crate) const fn new(
        kind: MutationExpressionKind<G, P>,
        value_type: MutationValueType<T>,
    ) -> Self {
        Self { kind, value_type }
    }

    /// Returns the source expression kind.
    pub(crate) const fn kind(&self) -> &MutationExpressionKind<G, P> {
        &self.kind
    }

    /// Returns the expression's semantic value facts.
    pub(crate) const fn value_type(&self) -> &MutationValueType<T> {
        &self.value_type
    }
}

/// The closed expression forms supported by a server INSERT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MutationExpressionKind<G = FunctionId, P = ParameterId> {
    /// Reads one declared parameter of the enclosing function.
    ParameterRead { owner: G, parameter: P },
    /// A boolean literal.
    BooleanLiteral { value: bool },
    /// A typed NULL contextualized by its target field.
    TypedNull,
}

/// A semantic type together with its nullability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MutationValueType<T = TypeId> {
    semantic_type: SemanticType<T>,
    nullable: bool,
}

impl<T> MutationValueType<T> {
    /// Creates semantic value facts.
    pub(crate) const fn new(semantic_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            semantic_type,
            nullable,
        }
    }

    /// Returns the semantic type.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.semantic_type
    }

    /// Reports whether this value may be NULL.
    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// One declared function parameter available to INSERT checking.
///
/// The name is expected to be normalized according to Orna identifier rules;
/// its location points at the declaration name for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationParameter<T = TypeId, P = ParameterId> {
    name: String,
    id: P,
    semantic_type: SemanticType<T>,
    location: SourceSpan,
}

impl<T, P> MutationParameter<T, P> {
    /// Creates one declared parameter descriptor.
    pub(crate) fn new(
        name: impl Into<String>,
        id: P,
        semantic_type: SemanticType<T>,
        location: SourceSpan,
    ) -> Self {
        Self {
            name: name.into(),
            id,
            semantic_type,
            location,
        }
    }

    /// Returns the normalized parameter name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the parameter identity.
    pub(crate) fn id(&self) -> P
    where
        P: Copy,
    {
        self.id
    }

    /// Returns the declared semantic type.
    pub(crate) fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.semantic_type
    }

    /// Returns the declaration location.
    pub(crate) fn location(&self) -> &SourceSpan {
        &self.location
    }
}

/// One target field descriptor supplied by a mutation catalogue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MutationField<T, F> {
    id: F,
    semantic_type: SemanticType<T>,
    nullable: bool,
}

impl<T, F> MutationField<T, F> {
    /// Creates one mutation-visible field descriptor.
    pub(crate) const fn new(id: F, semantic_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            id,
            semantic_type,
            nullable,
        }
    }

    /// Returns the field identity.
    pub(crate) const fn id(&self) -> F
    where
        F: Copy,
    {
        self.id
    }

    /// Returns the field semantic type.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.semantic_type
    }

    /// Reports whether the field accepts NULL.
    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// The catalogue lookup contract needed by mutation checking.
pub(crate) trait MutationCatalogue<T, F> {
    /// Finds an object type identity by its normalized qualified name.
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<T>;

    /// Finds an exact field by normalized name on one object type.
    fn field_by_name(&self, owner: T, name: &str) -> Option<MutationField<T, F>>;

    /// Visits fields in deterministic declaration order.
    fn visit_fields(&self, owner: T, visitor: &mut dyn FnMut(&str, MutationField<T, F>));
}

/// Evidence retained for every identity touched by a checked mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MutationReference<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    /// The target object type selected by `INSERT INTO`.
    WriteObject {
        object_type: T,
        location: SourceLocation,
    },
    /// A field written by one assignment.
    WriteField {
        owner: T,
        field: F,
        location: SourceLocation,
    },
    /// A declared function parameter read by one assignment.
    ParameterRead {
        owner: G,
        parameter: P,
        location: SourceLocation,
    },
    /// The returned object reference.
    ObjectReference {
        object_type: T,
        location: SourceLocation,
    },
}

impl<T, F, G, P> MutationReference<T, F, G, P> {
    /// Returns the source location that produced this reference.
    pub(crate) fn location(&self) -> &SourceLocation {
        match self {
            Self::WriteObject { location, .. }
            | Self::WriteField { location, .. }
            | Self::ParameterRead { location, .. }
            | Self::ObjectReference { location, .. } => location,
        }
    }
}

/// A checked mutation plan and its source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationCheck<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    plan: MutationPlanIr<T, F, G, P>,
    references: Vec<MutationReference<T, F, G, P>>,
}

impl<T, F, G, P> MutationCheck<T, F, G, P> {
    /// Returns the source-free checked plan.
    pub(crate) fn plan(&self) -> &MutationPlanIr<T, F, G, P> {
        &self.plan
    }

    /// Returns identity evidence in deterministic source order.
    pub(crate) fn references(&self) -> &[MutationReference<T, F, G, P>] {
        &self.references
    }
}

/// Checks one parsed INSERT against a caller-supplied identity catalogue.
pub(crate) fn check_insert_in<T, F, G, P>(
    insert: &InsertStatement,
    catalogue: &impl MutationCatalogue<T, F>,
    function: G,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
) -> Result<MutationCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    // Validate every declaration before looking at whether that parameter is
    // used. This keeps unsupported parameter domains fail-closed.
    let parameter_diagnostics = parameters
        .iter()
        .filter_map(|parameter| {
            if supported_semantic_type(parameter.semantic_type()) {
                None
            } else {
                Some(semantic_diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    format!(
                        "parameter {} uses a type outside the mutation domain",
                        parameter.name()
                    ),
                    logical_path,
                    parameter.location(),
                ))
            }
        })
        .collect::<Vec<_>>();
    if !parameter_diagnostics.is_empty() {
        return Err(parameter_diagnostics);
    }

    if insert.target_fields.is_empty() || insert.values.is_empty() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::DomainIncompatible,
            "INSERT requires a non-empty field list and value list",
            logical_path,
            &insert.span,
        )]);
    }
    if insert.target_fields.len() != insert.values.len() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::TypeMismatch,
            "INSERT field and value lists must have equal arity",
            logical_path,
            &insert.span,
        )]);
    }

    let target_name = normalise_qualified_name(&insert.target_object);
    let Some(target_object) = catalogue.object_type_id_by_name(&target_name) else {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unknown object type {target_name}"),
            logical_path,
            &insert.target_object.span,
        )]);
    };

    let mut references = vec![MutationReference::WriteObject {
        object_type: target_object,
        location: SourceLocation::from_syntax(logical_path, &insert.target_object.span),
    }];
    let mut assignments = Vec::with_capacity(insert.target_fields.len());
    let mut assigned_names = Vec::<String>::with_capacity(insert.target_fields.len());

    for (field_name, value) in insert.target_fields.iter().zip(&insert.values) {
        let normalized_field_name = normalise_name_part(field_name);
        if assigned_names
            .iter()
            .any(|name| name == &normalized_field_name)
        {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("duplicate target field {normalized_field_name}"),
                logical_path,
                &field_name.span,
            )]);
        }
        assigned_names.push(normalized_field_name.clone());

        let Some(field) = catalogue.field_by_name(target_object, &normalized_field_name) else {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown field {normalized_field_name} on {target_name}"),
                logical_path,
                &field_name.span,
            )]);
        };
        if !supported_semantic_type(field.semantic_type()) {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!("field {normalized_field_name} uses a type outside the mutation domain"),
                logical_path,
                &field_name.span,
            )]);
        }

        references.push(MutationReference::WriteField {
            owner: target_object,
            field: field.id(),
            location: SourceLocation::from_syntax(logical_path, &field_name.span),
        });

        let expression = match value {
            InsertValue::Parameter(parameter_name) => {
                let normalized_parameter_name = normalise_name_part(parameter_name);
                let Some(parameter) = parameters
                    .iter()
                    .find(|parameter| parameter.name() == normalized_parameter_name)
                else {
                    return Err(vec![semantic_diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!("unknown function parameter {normalized_parameter_name}"),
                        logical_path,
                        &parameter_name.span,
                    )]);
                };
                let parameter_type = parameter.semantic_type();
                if parameter_type != field.semantic_type() {
                    return Err(vec![semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "INSERT value type does not match target field",
                        logical_path,
                        &parameter_name.span,
                    )]);
                }
                references.push(MutationReference::ParameterRead {
                    owner: function,
                    parameter: parameter.id(),
                    location: SourceLocation::from_syntax(logical_path, &parameter_name.span),
                });
                MutationExpression::new(
                    MutationExpressionKind::ParameterRead {
                        owner: function,
                        parameter: parameter.id(),
                    },
                    MutationValueType::new(parameter_type, false),
                )
            }
            InsertValue::BooleanLiteral { value, source } => {
                let expected = SemanticType::scalar(StandardScalar::Boolean);
                if field.semantic_type() != expected {
                    return Err(vec![semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "BOOLEAN INSERT value does not match target field",
                        logical_path,
                        &source.span,
                    )]);
                }
                MutationExpression::new(
                    MutationExpressionKind::BooleanLiteral { value: *value },
                    MutationValueType::new(expected, false),
                )
            }
            InsertValue::NullLiteral { source } => {
                if !field.nullable() {
                    return Err(vec![semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "NULL cannot be assigned to a non-nullable target field",
                        logical_path,
                        &source.span,
                    )]);
                }
                MutationExpression::new(
                    MutationExpressionKind::TypedNull,
                    MutationValueType::new(field.semantic_type(), true),
                )
            }
            _ => {
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "INSERT value form is outside the mutation domain",
                    logical_path,
                    value.span(),
                )]);
            }
        };

        assignments.push(MutationAssignment::new(
            target_object,
            field.id(),
            expression,
        ));
    }

    let mut missing_required = Vec::new();
    catalogue.visit_fields(target_object, &mut |name, field| {
        if !field.nullable() && !assigned_names.iter().any(|assigned| assigned == name) {
            missing_required.push(name.to_owned());
        }
    });
    if !missing_required.is_empty() {
        return Err(missing_required
            .into_iter()
            .map(|name| {
                semantic_diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("mandatory target field {name} is omitted"),
                    logical_path,
                    &insert.target_object.span,
                )
            })
            .collect());
    }

    let normalized_target_alias = normalise_name_part(&insert.target_alias);
    let normalized_returning_alias = normalise_name_part(&insert.returning_alias);
    if normalized_target_alias != normalized_returning_alias {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!(
                "RETURNING REF alias {normalized_returning_alias} does not name the INSERT target"
            ),
            logical_path,
            &insert.returning_alias.span,
        )]);
    }

    references.push(MutationReference::ObjectReference {
        object_type: target_object,
        location: SourceLocation::from_syntax(logical_path, &insert.returning_alias.span),
    });

    Ok(MutationCheck {
        plan: MutationPlanIr {
            target_object,
            assignments,
            returned_object: target_object,
        },
        references,
    })
}

fn supported_semantic_type<T>(semantic_type: SemanticType<T>) -> bool {
    match semantic_type {
        SemanticType::Scalar(
            StandardScalar::Boolean
            | StandardScalar::Integer
            | StandardScalar::BigInt
            | StandardScalar::Float
            | StandardScalar::CharacterLargeObject
            | StandardScalar::BinaryLargeObject,
        )
        | SemanticType::Reference { .. } => true,
        SemanticType::Scalar(_) | SemanticType::Named(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{FieldId, FunctionId, ParameterId, TypeId};
    use orna_syntax::{QualifiedName, SourceSlice, SourceSpan};

    #[derive(Clone)]
    struct TestCatalogue {
        target: TypeId,
        name: QualifiedSemanticName,
        fields: Vec<(String, MutationField<TypeId, FieldId>)>,
    }

    impl MutationCatalogue<TypeId, FieldId> for TestCatalogue {
        fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<TypeId> {
            (name == &self.name).then_some(self.target)
        }

        fn field_by_name(
            &self,
            owner: TypeId,
            name: &str,
        ) -> Option<MutationField<TypeId, FieldId>> {
            (owner == self.target)
                .then(|| {
                    self.fields
                        .iter()
                        .find(|(field_name, _)| field_name == name)
                })
                .flatten()
                .map(|(_, field)| *field)
        }

        fn visit_fields(
            &self,
            owner: TypeId,
            visitor: &mut dyn FnMut(&str, MutationField<TypeId, FieldId>),
        ) {
            if owner == self.target {
                for (name, field) in &self.fields {
                    visitor(name, *field);
                }
            }
        }
    }

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan { start, end }
    }

    fn name(text: &str, start: usize) -> NamePart {
        NamePart {
            text: text.to_owned(),
            span: span(start, start + text.len()),
        }
    }

    fn insert(fields: Vec<NamePart>, values: Vec<InsertValue>, returning: &str) -> InsertStatement {
        InsertStatement {
            target_object: QualifiedName {
                parts: vec![name("crm", 7), name("person", 11)],
                span: span(7, 18),
            },
            target_alias: name("p", 22),
            target_fields: fields,
            values,
            returning_alias: name(returning, 50),
            span: span(0, 60),
        }
    }

    fn boolean(value: bool, start: usize) -> InsertValue {
        InsertValue::BooleanLiteral {
            value,
            source: SourceSlice {
                text: (if value { "TRUE" } else { "FALSE" }).to_owned(),
                span: span(start, start + 4 + usize::from(!value)),
            },
        }
    }

    fn parameter(text: &str, start: usize) -> InsertValue {
        InsertValue::Parameter(name(text, start))
    }

    fn null(start: usize) -> InsertValue {
        InsertValue::NullLiteral {
            source: SourceSlice {
                text: "NULL".to_owned(),
                span: span(start, start + 4),
            },
        }
    }

    fn catalogue() -> TestCatalogue {
        let target = TypeId::from_bytes([1; 16]);
        TestCatalogue {
            target,
            name: QualifiedSemanticName::new(["crm", "person"]).unwrap(),
            fields: vec![
                (
                    "name".to_owned(),
                    MutationField::new(
                        FieldId::from_bytes([2; 16]),
                        SemanticType::scalar(StandardScalar::CharacterLargeObject),
                        false,
                    ),
                ),
                (
                    "active".to_owned(),
                    MutationField::new(
                        FieldId::from_bytes([3; 16]),
                        SemanticType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                ),
                (
                    "note".to_owned(),
                    MutationField::new(
                        FieldId::from_bytes([4; 16]),
                        SemanticType::scalar(StandardScalar::CharacterLargeObject),
                        true,
                    ),
                ),
                (
                    "owner".to_owned(),
                    MutationField::new(
                        FieldId::from_bytes([5; 16]),
                        SemanticType::reference(TypeId::from_bytes([8; 16])),
                        true,
                    ),
                ),
            ],
        }
    }

    #[test]
    fn checks_mixed_insert_and_orders_evidence() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let parameter_id = ParameterId::from_bytes([6; 16]);
        let name_field = FieldId::from_bytes([2; 16]);
        let active_field = FieldId::from_bytes([3; 16]);
        let note_field = FieldId::from_bytes([4; 16]);
        let text_type = SemanticType::scalar(StandardScalar::CharacterLargeObject);
        let boolean_type = SemanticType::scalar(StandardScalar::Boolean);
        let parameters = vec![MutationParameter::new(
            "name",
            parameter_id,
            text_type,
            span(100, 104),
        )];
        let check = check_insert_in(
            &insert(
                vec![name("name", 25), name("active", 31), name("note", 39)],
                vec![parameter("name", 47), boolean(true, 54), null(59)],
                "p",
            ),
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap();

        let expected_plan = MutationPlanIr {
            target_object: catalogue.target,
            assignments: vec![
                MutationAssignment::new(
                    catalogue.target,
                    name_field,
                    MutationExpression::new(
                        MutationExpressionKind::ParameterRead {
                            owner: function,
                            parameter: parameter_id,
                        },
                        MutationValueType::new(text_type, false),
                    ),
                ),
                MutationAssignment::new(
                    catalogue.target,
                    active_field,
                    MutationExpression::new(
                        MutationExpressionKind::BooleanLiteral { value: true },
                        MutationValueType::new(boolean_type, false),
                    ),
                ),
                MutationAssignment::new(
                    catalogue.target,
                    note_field,
                    MutationExpression::new(
                        MutationExpressionKind::TypedNull,
                        MutationValueType::new(text_type, true),
                    ),
                ),
            ],
            returned_object: catalogue.target,
        };
        assert_eq!(check.plan(), &expected_plan);

        let location = |start, end| SourceLocation::from_syntax("mutation.orna", &span(start, end));
        let expected_references = vec![
            MutationReference::WriteObject {
                object_type: catalogue.target,
                location: location(7, 18),
            },
            MutationReference::WriteField {
                owner: catalogue.target,
                field: name_field,
                location: location(25, 29),
            },
            MutationReference::ParameterRead {
                owner: function,
                parameter: parameter_id,
                location: location(47, 51),
            },
            MutationReference::WriteField {
                owner: catalogue.target,
                field: active_field,
                location: location(31, 37),
            },
            MutationReference::WriteField {
                owner: catalogue.target,
                field: note_field,
                location: location(39, 43),
            },
            MutationReference::ObjectReference {
                object_type: catalogue.target,
                location: location(50, 51),
            },
        ];
        assert_eq!(check.references(), expected_references.as_slice());
        assert_eq!(
            check.references()[0].location().logical_path(),
            "mutation.orna"
        );
        assert_eq!(check.references()[0].location().span().start(), 7);
        assert_eq!(check.references()[0].location().span().end(), 18);
        assert_eq!(check.references()[5].location().span().start(), 50);
        assert_eq!(check.references()[5].location().span().end(), 51);
        assert_eq!(check.plan().returned_object(), catalogue.target);
        assert_eq!(check.plan().assignments().len(), 3);
        assert_eq!(check.plan().assignments()[0].field(), name_field);
        assert_eq!(
            check.plan().assignments()[0].expression().kind(),
            &MutationExpressionKind::ParameterRead {
                owner: function,
                parameter: parameter_id,
            }
        );
        assert!(
            !check.plan().assignments()[0]
                .expression()
                .value_type()
                .nullable()
        );
    }

    #[test]
    fn rejects_duplicates_after_case_normalisation_and_unknown_names() {
        let catalogue = catalogue();
        let insert = insert(
            vec![name("name", 25), name("\"name\"", 31), name("active", 39)],
            vec![
                parameter("name", 47),
                parameter("name", 54),
                boolean(true, 61),
            ],
            "p",
        );
        let parameters = vec![MutationParameter::new(
            "name",
            ParameterId::from_bytes([6; 16]),
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
            span(100, 104),
        )];
        let error = check_insert_in(
            &insert,
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(error[0].code(), DiagnosticCode::DuplicateDefinition);
        assert_eq!(error[0].location().span().start(), 31);
    }

    #[test]
    fn accepts_nullable_omission_but_rejects_mandatory_omission() {
        let catalogue = catalogue();
        let parameters = vec![MutationParameter::new(
            "name",
            ParameterId::from_bytes([6; 16]),
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
            span(100, 104),
        )];
        let check = check_insert_in(
            &insert(vec![name("name", 25)], vec![parameter("name", 31)], "p"),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        );
        assert_eq!(check.unwrap_err()[0].code(), DiagnosticCode::TypeMismatch);

        let valid = check_insert_in(
            &insert(
                vec![name("name", 25), name("active", 31)],
                vec![parameter("name", 39), boolean(true, 46)],
                "p",
            ),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        );
        assert!(valid.is_ok());
    }

    #[test]
    fn rejects_unused_unsupported_parameter_type() {
        let catalogue = catalogue();
        let parameters = vec![
            MutationParameter::new(
                "name",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(90, 94),
            ),
            MutationParameter::new(
                "unused",
                ParameterId::from_bytes([7; 16]),
                SemanticType::scalar(StandardScalar::Decimal),
                span(100, 106),
            ),
        ];
        let error = check_insert_in(
            &insert(
                vec![name("name", 25), name("active", 31)],
                vec![parameter("name", 39), boolean(true, 46)],
                "p",
            ),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(error[0].code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(error[0].location().span().start(), 100);
        assert_eq!(error[0].location().span().end(), 106);
    }

    #[test]
    fn accepts_unused_supported_parameters_and_rejects_malformed_shapes() {
        let catalogue = catalogue();
        let parameters = vec![
            MutationParameter::new(
                "name",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(100, 104),
            ),
            MutationParameter::new(
                "unused",
                ParameterId::from_bytes([7; 16]),
                SemanticType::scalar(StandardScalar::Boolean),
                span(105, 111),
            ),
        ];
        let valid = check_insert_in(
            &insert(
                vec![name("name", 25), name("active", 31)],
                vec![parameter("name", 39), boolean(true, 46)],
                "p",
            ),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        );
        assert!(valid.is_ok());

        let malformed = check_insert_in(
            &insert(Vec::new(), Vec::new(), "p"),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(malformed[0].code(), DiagnosticCode::DomainIncompatible);

        let arity = check_insert_in(
            &insert(
                vec![name("name", 25), name("active", 31)],
                vec![parameter("name", 39)],
                "p",
            ),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(arity[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(arity[0].location().span().start(), 0);
    }

    #[test]
    fn rejects_unknown_names_and_alias_mismatch_at_their_source_spans() {
        let catalogue = catalogue();
        let parameters = vec![MutationParameter::new(
            "name",
            ParameterId::from_bytes([6; 16]),
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
            span(100, 104),
        )];
        let function = FunctionId::from_bytes([5; 16]);

        let mut unknown_target = insert(
            vec![name("name", 25), name("active", 31)],
            vec![parameter("name", 39), boolean(true, 46)],
            "p",
        );
        unknown_target.target_object.parts[1].text = "missing".to_owned();
        let diagnostic = check_insert_in(
            &unknown_target,
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(diagnostic[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic[0].location().span().start(), 7);
        assert_eq!(diagnostic[0].location().span().end(), 18);

        let unknown_field = insert(
            vec![name("missing", 25), name("active", 31)],
            vec![parameter("name", 39), boolean(true, 46)],
            "p",
        );
        let diagnostic = check_insert_in(
            &unknown_field,
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(diagnostic[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic[0].location().span().start(), 25);

        let unknown_parameter = insert(
            vec![name("name", 25), name("active", 31)],
            vec![parameter("missing", 39), boolean(true, 46)],
            "p",
        );
        let diagnostic = check_insert_in(
            &unknown_parameter,
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(diagnostic[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic[0].location().span().start(), 39);

        let unknown_alias = insert(
            vec![name("name", 25), name("active", 31)],
            vec![parameter("name", 39), boolean(true, 46)],
            "other",
        );
        let diagnostic = check_insert_in(
            &unknown_alias,
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(diagnostic[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic[0].location().span().start(), 50);
        assert_eq!(diagnostic[0].location().span().end(), 55);
    }

    #[test]
    fn rejects_duplicate_types_boolean_and_non_nullable_null() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let parameters = vec![
            MutationParameter::new(
                "count",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::Integer),
                span(100, 105),
            ),
            MutationParameter::new(
                "owner",
                ParameterId::from_bytes([7; 16]),
                SemanticType::reference(TypeId::from_bytes([9; 16])),
                span(106, 111),
            ),
        ];

        let scalar_ref = check_insert_in(
            &insert(vec![name("name", 25)], vec![parameter("count", 31)], "p"),
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(scalar_ref[0].code(), DiagnosticCode::TypeMismatch);

        let ref_target = check_insert_in(
            &insert(vec![name("owner", 25)], vec![parameter("owner", 31)], "p"),
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(ref_target[0].code(), DiagnosticCode::TypeMismatch);

        let bool_wrong = check_insert_in(
            &insert(vec![name("name", 25)], vec![boolean(true, 31)], "p"),
            &catalogue,
            function,
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(bool_wrong[0].code(), DiagnosticCode::TypeMismatch);

        let null_mandatory = check_insert_in(
            &insert(vec![name("active", 25)], vec![null(32)], "p"),
            &catalogue,
            function,
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(null_mandatory[0].code(), DiagnosticCode::TypeMismatch);
    }

    #[test]
    fn maps_all_identity_classes_and_rejects_any_failed_mapping() {
        let plan = MutationPlanIr {
            target_object: 1_u8,
            assignments: vec![
                MutationAssignment::new(
                    1_u8,
                    2_u8,
                    MutationExpression::new(
                        MutationExpressionKind::ParameterRead {
                            owner: 3_u8,
                            parameter: 4_u8,
                        },
                        MutationValueType::new(SemanticType::Reference { target: 5_u8 }, false),
                    ),
                ),
                MutationAssignment::new(
                    1_u8,
                    7_u8,
                    MutationExpression::new(
                        MutationExpressionKind::BooleanLiteral { value: true },
                        MutationValueType::new(SemanticType::Named(6_u8), true),
                    ),
                ),
            ],
            returned_object: 1_u8,
        };
        let mapped = plan
            .try_map_identities(
                |value| Ok::<_, ()>(value + 10),
                |value| Ok::<_, ()>(value + 20),
                |value| Ok::<_, ()>(value + 30),
                |value| Ok::<_, ()>(value + 40),
            )
            .unwrap();
        assert_eq!(mapped.target_object(), 11);
        assert_eq!(mapped.assignments()[0].owner(), 11);
        assert_eq!(mapped.assignments()[0].field(), 22);
        assert_eq!(
            mapped.assignments()[0]
                .expression()
                .value_type()
                .semantic_type(),
            SemanticType::Reference { target: 15 }
        );
        assert_eq!(
            mapped.assignments()[1]
                .expression()
                .value_type()
                .semantic_type(),
            SemanticType::Named(16)
        );
        assert!(
            plan.try_map_identities(
                |_| Err::<u8, _>("type"),
                Ok::<_, &str>,
                Ok::<_, &str>,
                Ok::<_, &str>,
            )
            .is_err()
        );
        assert!(
            plan.try_map_identities(
                Ok::<_, &str>,
                Ok::<_, &str>,
                |_| Err::<u8, _>("function"),
                Ok::<_, &str>,
            )
            .is_err()
        );
        assert!(
            plan.try_map_identities(
                Ok::<_, &str>,
                Ok::<_, &str>,
                Ok::<_, &str>,
                |_| Err::<u8, _>("parameter"),
            )
            .is_err()
        );
        assert!(
            plan.try_map_identities(
                Ok::<_, &str>,
                |_| Err::<u8, _>("field"),
                Ok::<_, &str>,
                Ok::<_, &str>,
            )
            .is_err()
        );
    }
}
