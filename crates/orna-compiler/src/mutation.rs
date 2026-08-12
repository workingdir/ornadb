//! Semantic checking for the single-row server-function mutation slice.
//!
//! This module deliberately stops at a source-free, identity-bearing plan. It
//! does not know about a physical backend or the catalogue-dependent checks
//! that are applied when the plan is lowered to an execution artifact.

use orna_core::{
    FieldId, FunctionId, ParameterId, TypeId, catalogue::QualifiedSemanticName,
    types::StandardScalar,
};
use orna_syntax::{
    DeleteStatement, InsertStatement, MutationValue, NamePart, QualifiedName, RecordConstructor,
    RecordConstructorFieldValue, SourceSpan, UpdateStatement,
};

use crate::{
    CompilerDiagnostic, DiagnosticCode, SourceLocation, normalise_name_part,
    normalise_qualified_name, semantic_diagnostic,
};
use crate::{relational::IntrinsicBooleanType, resolver::SemanticType};

/// A source-free checked one-row mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationPlanIr<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    operation: MutationOperation<G, P>,
    target_object: T,
    assignments: Vec<MutationAssignment<T, F, G, P>>,
    returned_object: T,
}

impl<T, F, G, P> MutationPlanIr<T, F, G, P> {
    /// Returns the checked mutation operation.
    pub(crate) const fn operation(&self) -> &MutationOperation<G, P> {
        &self.operation
    }

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
        let operation = match self.operation {
            MutationOperation::Insert => MutationOperation::Insert,
            MutationOperation::Update {
                selector_owner,
                selector_parameter,
            } => MutationOperation::Update {
                selector_owner: map_function(selector_owner)?,
                selector_parameter: map_parameter(selector_parameter)?,
            },
        };
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
                        &mut map_field,
                        &mut map_function,
                        &mut map_parameter,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, E>>()?;

        Ok(MutationPlanIr {
            operation,
            target_object,
            assignments,
            returned_object,
        })
    }
}

/// A source-free checked single-object DELETE plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeletePlanIr<T = TypeId, G = FunctionId, P = ParameterId> {
    target_object: T,
    selector_owner: G,
    selector_parameter: P,
}

impl<T, G, P> DeletePlanIr<T, G, P> {
    /// Creates one checked DELETE plan from resolved identities.
    pub(crate) const fn new(target_object: T, selector_owner: G, selector_parameter: P) -> Self {
        Self {
            target_object,
            selector_owner,
            selector_parameter,
        }
    }

    /// Returns the object type selected for deletion.
    pub(crate) const fn target_object(&self) -> T
    where
        T: Copy,
    {
        self.target_object
    }

    /// Returns the function that owns the selector parameter.
    pub(crate) const fn selector_owner(&self) -> G
    where
        G: Copy,
    {
        self.selector_owner
    }

    /// Returns the owner-qualified selector parameter identity.
    pub(crate) const fn selector_parameter(&self) -> P
    where
        P: Copy,
    {
        self.selector_parameter
    }
}

impl<T, G, P> DeletePlanIr<T, G, P>
where
    T: Copy,
    G: Copy,
    P: Copy,
{
    /// Rewrites every identity, rejecting the complete plan when any mapping fails.
    pub(crate) fn try_map_identities<T2, G2, P2, E>(
        &self,
        mut map_type: impl FnMut(T) -> Result<T2, E>,
        mut map_function: impl FnMut(G) -> Result<G2, E>,
        mut map_parameter: impl FnMut(P) -> Result<P2, E>,
    ) -> Result<DeletePlanIr<T2, G2, P2>, E> {
        Ok(DeletePlanIr {
            target_object: map_type(self.target_object)?,
            selector_owner: map_function(self.selector_owner)?,
            selector_parameter: map_parameter(self.selector_parameter)?,
        })
    }
}

/// The closed source-free mutation operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MutationOperation<G = FunctionId, P = ParameterId> {
    /// Inserts one object.
    Insert,
    /// Updates the object selected by one owner-qualified parameter.
    Update {
        selector_owner: G,
        selector_parameter: P,
    },
}

fn map_expression<T, F, G, P, T2, F2, G2, P2, E>(
    expression: &MutationExpression<T, G, P, F>,
    map_type: &mut impl FnMut(T) -> Result<T2, E>,
    map_field: &mut impl FnMut(F) -> Result<F2, E>,
    map_function: &mut impl FnMut(G) -> Result<G2, E>,
    map_parameter: &mut impl FnMut(P) -> Result<P2, E>,
) -> Result<MutationExpression<T2, G2, P2, F2>, E>
where
    T: Copy,
    F: Copy,
    G: Copy,
    P: Copy,
{
    let kind = match &expression.kind {
        MutationExpressionKind::ParameterRead { owner, parameter } => {
            MutationExpressionKind::ParameterRead {
                owner: map_function(*owner)?,
                parameter: map_parameter(*parameter)?,
            }
        }
        MutationExpressionKind::BooleanLiteral { value } => {
            MutationExpressionKind::BooleanLiteral { value: *value }
        }
        MutationExpressionKind::TypedNull => MutationExpressionKind::TypedNull,
        MutationExpressionKind::RecordConstructor {
            record_type,
            fields,
        } => MutationExpressionKind::RecordConstructor {
            record_type: map_type(*record_type)?,
            fields: fields
                .iter()
                .map(|field| {
                    Ok(MutationRecordFieldExpression {
                        owner: map_type(field.owner)?,
                        field: map_field(field.field)?,
                        kind: match field.kind {
                            MutationRecordFieldExpressionKind::ParameterRead {
                                owner,
                                parameter,
                            } => MutationRecordFieldExpressionKind::ParameterRead {
                                owner: map_function(owner)?,
                                parameter: map_parameter(parameter)?,
                            },
                            MutationRecordFieldExpressionKind::BooleanLiteral { value } => {
                                MutationRecordFieldExpressionKind::BooleanLiteral { value }
                            }
                        },
                        value_type: map_value_type(field.value_type, map_type)?,
                    })
                })
                .collect::<Result<Vec<_>, E>>()?,
        },
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
        standard_value_type: value_type.standard_value_type,
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
    expression: MutationExpression<T, G, P, F>,
}

impl<T, F, G, P> MutationAssignment<T, F, G, P> {
    /// Creates one source-free assignment.
    pub(crate) const fn new(
        owner: T,
        field: F,
        expression: MutationExpression<T, G, P, F>,
    ) -> Self {
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
    pub(crate) fn expression(&self) -> &MutationExpression<T, G, P, F> {
        &self.expression
    }
}

/// A checked mutation expression and its semantic value facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationExpression<T = TypeId, G = FunctionId, P = ParameterId, F = FieldId> {
    kind: MutationExpressionKind<T, G, P, F>,
    value_type: MutationValueType<T>,
}

impl<T, G, P, F> MutationExpression<T, G, P, F> {
    /// Creates an expression with its already-resolved semantic value facts.
    pub(crate) const fn new(
        kind: MutationExpressionKind<T, G, P, F>,
        value_type: MutationValueType<T>,
    ) -> Self {
        Self { kind, value_type }
    }

    /// Returns the source expression kind.
    pub(crate) const fn kind(&self) -> &MutationExpressionKind<T, G, P, F> {
        &self.kind
    }

    /// Returns the expression's semantic value facts.
    pub(crate) const fn value_type(&self) -> &MutationValueType<T> {
        &self.value_type
    }
}

/// The closed expression forms supported by SERVER mutations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MutationExpressionKind<T = TypeId, G = FunctionId, P = ParameterId, F = FieldId> {
    /// Reads one declared parameter of the enclosing function.
    ParameterRead { owner: G, parameter: P },
    /// A boolean literal.
    BooleanLiteral { value: bool },
    /// A typed NULL contextualized by its target field.
    TypedNull,
    /// One nominal record value with fields in declaration order.
    RecordConstructor {
        /// The resolved nominal record value type.
        record_type: T,
        /// Checked fields in record declaration order.
        fields: Vec<MutationRecordFieldExpression<T, F, G, P>>,
    },
}

/// One checked field expression within a nominal record constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationRecordFieldExpression<T, F, G, P> {
    owner: T,
    field: F,
    kind: MutationRecordFieldExpressionKind<G, P>,
    value_type: MutationValueType<T>,
}

impl<T: Copy, F: Copy, G, P> MutationRecordFieldExpression<T, F, G, P> {
    /// Returns the nominal record value type that owns this field.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "artifact lowering is the next slice")
    )]
    pub(crate) const fn owner(&self) -> T {
        self.owner
    }

    /// Returns the stable record field identity.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "artifact lowering is the next slice")
    )]
    pub(crate) const fn field(&self) -> F {
        self.field
    }

    /// Returns the checked child expression kind.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "artifact lowering is the next slice")
    )]
    pub(crate) const fn kind(&self) -> &MutationRecordFieldExpressionKind<G, P> {
        &self.kind
    }

    /// Returns the checked child semantic value facts.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "artifact lowering is the next slice")
    )]
    pub(crate) const fn value_type(&self) -> &MutationValueType<T> {
        &self.value_type
    }
}

/// The closed expression forms accepted inside a record constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MutationRecordFieldExpressionKind<G, P> {
    /// Reads one declared parameter of the enclosing function.
    ParameterRead { owner: G, parameter: P },
    /// A Boolean literal.
    BooleanLiteral { value: bool },
}

/// A semantic type together with its nullability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MutationValueType<T = TypeId> {
    semantic_type: SemanticType<T>,
    standard_value_type: Option<TypeId>,
    nullable: bool,
}

impl<T> MutationValueType<T> {
    /// Creates semantic value facts.
    pub(crate) const fn new(semantic_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            semantic_type,
            standard_value_type: None,
            nullable,
        }
    }

    /// Attaches the supplied standard value-type identity.
    pub(crate) const fn with_standard_value_type(mut self, type_id: TypeId) -> Self {
        self.standard_value_type = Some(type_id);
        self
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

    /// Returns the supplied standard value-type identity when available.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
    }
}

/// One declared function parameter available to mutation checking.
///
/// The name is expected to be normalized according to Orna identifier rules;
/// its location points at the declaration name for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationParameter<T = TypeId, P = ParameterId> {
    name: String,
    id: P,
    semantic_type: SemanticType<T>,
    standard_value_type: Option<TypeId>,
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
            standard_value_type: None,
            location,
        }
    }

    /// Attaches the supplied standard value-type identity.
    pub(crate) const fn with_standard_value_type(mut self, type_id: TypeId) -> Self {
        self.standard_value_type = Some(type_id);
        self
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

    /// Returns the supplied standard value-type identity when available.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
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
    standard_value_type: Option<TypeId>,
    nullable: bool,
}

impl<T, F> MutationField<T, F> {
    /// Creates one mutation-visible field descriptor.
    pub(crate) const fn new(id: F, semantic_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            id,
            semantic_type,
            standard_value_type: None,
            nullable,
        }
    }

    /// Attaches the supplied standard value-type identity.
    pub(crate) const fn with_standard_value_type(mut self, type_id: TypeId) -> Self {
        self.standard_value_type = Some(type_id);
        self
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

    /// Returns the supplied standard value-type identity when available.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
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

    /// Finds a record value type identity by its normalized qualified name.
    fn record_type_id_by_name(&self, _name: &QualifiedSemanticName) -> Option<T> {
        None
    }

    /// Finds an exact field by normalized name on one record value type.
    fn record_field_by_name(&self, _owner: T, _name: &str) -> Option<MutationField<T, F>> {
        None
    }

    /// Visits record fields in deterministic declaration order.
    fn visit_record_fields(&self, _owner: T, _visitor: &mut dyn FnMut(&str, MutationField<T, F>)) {}

    /// Reports whether one named type identity is an active enum.
    fn named_type_is_enum(&self, _id: T) -> bool {
        false
    }
}

/// Evidence retained for every identity touched by a checked mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MutationReference<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    /// The target object type written by the mutation.
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
    /// The nominal record value type named by one constructor.
    NamedValueType {
        value_type: T,
        location: SourceLocation,
    },
    /// A declared function parameter read by one assignment.
    ParameterRead {
        owner: G,
        parameter: P,
        location: SourceLocation,
    },
    /// An object identity referenced by a selector or returned result.
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
            | Self::NamedValueType { location, .. }
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
    expression_uses: Vec<MutationExpressionUse<T>>,
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

    /// Returns standard-backed expression uses in deterministic semantic order.
    pub(crate) fn expression_uses(&self) -> &[MutationExpressionUse<T>] {
        &self.expression_uses
    }
}

/// One value-producing expression and its exact source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationExpressionUse<T> {
    value_type: MutationValueType<T>,
    location: SourceLocation,
}

impl<T: Copy> MutationExpressionUse<T> {
    /// Returns the checked semantic value facts.
    pub(crate) const fn value_type(&self) -> &MutationValueType<T> {
        &self.value_type
    }

    /// Returns the exact child-expression source location.
    pub(crate) const fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked DELETE plan and its source evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeleteCheck<T = TypeId, F = FieldId, G = FunctionId, P = ParameterId> {
    plan: DeletePlanIr<T, G, P>,
    references: Vec<MutationReference<T, F, G, P>>,
}

impl<T, F, G, P> DeleteCheck<T, F, G, P> {
    /// Returns the source-free checked DELETE plan.
    pub(crate) const fn plan(&self) -> &DeletePlanIr<T, G, P> {
        &self.plan
    }

    /// Returns identity evidence in deterministic source order.
    pub(crate) fn references(&self) -> &[MutationReference<T, F, G, P>] {
        &self.references
    }
}

const SUPPORTED_MUTATION_TYPES: &str =
    "BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF";

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
    check_insert_with_intrinsic_boolean_in(
        insert,
        catalogue,
        function,
        parameters,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one parsed INSERT with explicit intrinsic Boolean provenance.
pub(crate) fn check_insert_with_intrinsic_boolean_in<T, F, G, P>(
    insert: &InsertStatement,
    catalogue: &impl MutationCatalogue<T, F>,
    function: G,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<MutationCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    if let Some(diagnostics) =
        missing_insert_boolean_diagnostics(insert, logical_path, intrinsic_boolean)
    {
        return Err(diagnostics);
    }
    let enum_parameter_names = insert
        .values
        .iter()
        .filter_map(|value| match value {
            MutationValue::RecordConstructor(constructor) => Some(&constructor.fields),
            _ => None,
        })
        .flatten()
        .filter_map(|field| match &field.value {
            RecordConstructorFieldValue::Parameter(name) => Some(normalise_name_part(name)),
            _ => None,
        })
        .collect::<Vec<_>>();
    validate_parameter_types(
        "INSERT",
        parameters,
        logical_path,
        catalogue,
        &enum_parameter_names,
    )?;

    if insert.target_fields.is_empty() || insert.values.is_empty() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::DomainIncompatible,
            "INSERT requires at least one target field and one value",
            logical_path,
            &insert.span,
        )]);
    }
    if insert.target_fields.len() != insert.values.len() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "INSERT lists {} {} but {} {}; each field requires one value",
                insert.target_fields.len(),
                if insert.target_fields.len() == 1 {
                    "field"
                } else {
                    "fields"
                },
                insert.values.len(),
                if insert.values.len() == 1 {
                    "value"
                } else {
                    "values"
                }
            ),
            logical_path,
            &insert.span,
        )]);
    }

    let (target_name, target_object) =
        resolve_mutation_target(&insert.target_object, logical_path, catalogue)?;
    let checked_assignments = check_assignments(
        &AssignmentCheckContext {
            operation: "INSERT",
            function,
            parameters,
            logical_path,
            intrinsic_boolean,
        },
        &insert.target_object,
        &target_name,
        target_object,
        insert.target_fields.iter().zip(&insert.values),
        true,
        catalogue,
    )?;
    let assignments = checked_assignments.assignments;
    let mut references = checked_assignments.references;

    let normalized_target_alias = normalise_name_part(&insert.target_alias);
    let normalized_returning_alias = normalise_name_part(&insert.returning_alias);
    if normalized_target_alias != normalized_returning_alias {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!(
                "RETURNING REF must use the INSERT target alias {normalized_target_alias}, not {normalized_returning_alias}"
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
            operation: MutationOperation::Insert,
            target_object,
            assignments,
            returned_object: target_object,
        },
        references,
        expression_uses: checked_assignments.expression_uses,
    })
}

/// Checks one parsed UPDATE against a caller-supplied identity catalogue.
pub(crate) fn check_update_in<T, F, G, P>(
    update: &UpdateStatement,
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
    check_update_with_intrinsic_boolean_in(
        update,
        catalogue,
        function,
        parameters,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one parsed UPDATE with explicit intrinsic Boolean provenance.
pub(crate) fn check_update_with_intrinsic_boolean_in<T, F, G, P>(
    update: &UpdateStatement,
    catalogue: &impl MutationCatalogue<T, F>,
    function: G,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<MutationCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    if let Some(diagnostics) =
        missing_update_boolean_diagnostics(update, logical_path, intrinsic_boolean)
    {
        return Err(diagnostics);
    }
    validate_parameter_types("UPDATE", parameters, logical_path, catalogue, &[])?;
    if update.assignments.is_empty() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::DomainIncompatible,
            "UPDATE requires at least one field assignment",
            logical_path,
            &update.span,
        )]);
    }

    let (target_name, target_object) =
        resolve_mutation_target(&update.target_object, logical_path, catalogue)?;

    let normalized_target_alias = normalise_name_part(&update.target_alias);
    let normalized_returning_alias = normalise_name_part(&update.returning_alias);
    if normalized_target_alias != normalized_returning_alias {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!(
                "RETURNING REF must use the UPDATE target alias {normalized_target_alias}, not {normalized_returning_alias}"
            ),
            logical_path,
            &update.returning_alias.span,
        )]);
    }

    let checked_assignments = check_assignments(
        &AssignmentCheckContext {
            operation: "UPDATE",
            function,
            parameters,
            logical_path,
            intrinsic_boolean,
        },
        &update.target_object,
        &target_name,
        target_object,
        update
            .assignments
            .iter()
            .map(|assignment| (&assignment.target_field, &assignment.value)),
        false,
        catalogue,
    )?;

    let assignments = checked_assignments.assignments;
    let mut references = checked_assignments.references;

    let selector_parameter = resolve_identity_selector(
        &IdentitySelectorContext {
            operation: "UPDATE",
            target_name: &target_name,
            target_alias: &update.target_alias,
            selector_alias: &update.selector_alias,
            selector_parameter: &update.selector_parameter,
            logical_path,
        },
        target_object,
        parameters,
    )?;

    references.push(MutationReference::ObjectReference {
        object_type: target_object,
        location: SourceLocation::from_syntax(logical_path, &update.selector_alias.span),
    });
    references.push(MutationReference::ParameterRead {
        owner: function,
        parameter: selector_parameter,
        location: SourceLocation::from_syntax(logical_path, &update.selector_parameter.span),
    });
    references.push(MutationReference::ObjectReference {
        object_type: target_object,
        location: SourceLocation::from_syntax(logical_path, &update.returning_alias.span),
    });

    Ok(MutationCheck {
        plan: MutationPlanIr {
            operation: MutationOperation::Update {
                selector_owner: function,
                selector_parameter,
            },
            target_object,
            assignments,
            returned_object: target_object,
        },
        references,
        expression_uses: checked_assignments.expression_uses,
    })
}

/// Checks one parsed DELETE against a caller-supplied identity catalogue.
pub(crate) fn check_delete_in<T, F, G, P>(
    delete: &DeleteStatement,
    catalogue: &impl MutationCatalogue<T, F>,
    function: G,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
) -> Result<DeleteCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy,
    G: Copy,
    P: Copy,
{
    check_delete_with_intrinsic_boolean_in(
        delete,
        catalogue,
        function,
        parameters,
        logical_path,
        IntrinsicBooleanType::Legacy,
    )
}

/// Checks one parsed DELETE with explicit intrinsic Boolean provenance.
pub(crate) fn check_delete_with_intrinsic_boolean_in<T, F, G, P>(
    delete: &DeleteStatement,
    catalogue: &impl MutationCatalogue<T, F>,
    function: G,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Result<DeleteCheck<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy,
    G: Copy,
    P: Copy,
{
    if let Some(diagnostics) =
        missing_delete_boolean_diagnostics(delete, logical_path, intrinsic_boolean)
    {
        return Err(diagnostics);
    }
    validate_parameter_types("DELETE", parameters, logical_path, catalogue, &[])?;
    let (target_name, target_object) =
        resolve_mutation_target(&delete.target_object, logical_path, catalogue)?;
    let selector_parameter = resolve_identity_selector(
        &IdentitySelectorContext {
            operation: "DELETE",
            target_name: &target_name,
            target_alias: &delete.target_alias,
            selector_alias: &delete.selector_alias,
            selector_parameter: &delete.selector_parameter,
            logical_path,
        },
        target_object,
        parameters,
    )?;

    Ok(DeleteCheck {
        plan: DeletePlanIr::new(target_object, function, selector_parameter),
        references: vec![
            MutationReference::WriteObject {
                object_type: target_object,
                location: SourceLocation::from_syntax(logical_path, &delete.target_object.span),
            },
            MutationReference::ObjectReference {
                object_type: target_object,
                location: SourceLocation::from_syntax(logical_path, &delete.selector_alias.span),
            },
            MutationReference::ParameterRead {
                owner: function,
                parameter: selector_parameter,
                location: SourceLocation::from_syntax(
                    logical_path,
                    &delete.selector_parameter.span,
                ),
            },
        ],
    })
}

struct CheckedAssignments<T, F, G, P> {
    assignments: Vec<MutationAssignment<T, F, G, P>>,
    references: Vec<MutationReference<T, F, G, P>>,
    expression_uses: Vec<MutationExpressionUse<T>>,
}

fn resolve_mutation_target<T, F>(
    target_source: &QualifiedName,
    logical_path: &str,
    catalogue: &impl MutationCatalogue<T, F>,
) -> Result<(QualifiedSemanticName, T), Vec<CompilerDiagnostic>>
where
    T: Copy,
{
    let target_name = normalise_qualified_name(target_source);
    let Some(target_object) = catalogue.object_type_id_by_name(&target_name) else {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unknown object type {target_name}"),
            logical_path,
            &target_source.span,
        )]);
    };
    Ok((target_name, target_object))
}

struct IdentitySelectorContext<'a> {
    operation: &'static str,
    target_name: &'a QualifiedSemanticName,
    target_alias: &'a NamePart,
    selector_alias: &'a NamePart,
    selector_parameter: &'a NamePart,
    logical_path: &'a str,
}

fn resolve_identity_selector<T, P>(
    context: &IdentitySelectorContext<'_>,
    target_object: T,
    parameters: &[MutationParameter<T, P>],
) -> Result<P, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    P: Copy,
{
    let normalized_target_alias = normalise_name_part(context.target_alias);
    let normalized_selector_alias = normalise_name_part(context.selector_alias);
    if normalized_target_alias != normalized_selector_alias {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!(
                "WHERE REF must use the {} target alias {normalized_target_alias}, not {normalized_selector_alias}",
                context.operation
            ),
            context.logical_path,
            &context.selector_alias.span,
        )]);
    }

    let normalized_selector_parameter = normalise_name_part(context.selector_parameter);
    let Some(selector) = parameters
        .iter()
        .find(|parameter| parameter.name() == normalized_selector_parameter)
    else {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("this function has no parameter named {normalized_selector_parameter}"),
            context.logical_path,
            &context.selector_parameter.span,
        )]);
    };
    if selector.semantic_type() != SemanticType::reference(target_object) {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "selector parameter {normalized_selector_parameter} must use REF {}",
                context.target_name
            ),
            context.logical_path,
            &context.selector_parameter.span,
        )]);
    }
    Ok(selector.id())
}

fn check_assignments<'a, T, F, G, P>(
    context: &AssignmentCheckContext<'_, T, G, P>,
    target_source: &QualifiedName,
    target_name: &QualifiedSemanticName,
    target_object: T,
    assignment_sources: impl IntoIterator<Item = (&'a NamePart, &'a MutationValue)>,
    require_all_non_nullable_fields: bool,
    catalogue: &impl MutationCatalogue<T, F>,
) -> Result<CheckedAssignments<T, F, G, P>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    let mut references = vec![MutationReference::WriteObject {
        object_type: target_object,
        location: SourceLocation::from_syntax(context.logical_path, &target_source.span),
    }];
    let mut assignments = Vec::new();
    let mut expression_uses = Vec::new();
    let mut assigned_names = Vec::<String>::new();
    for (field_name, value) in assignment_sources {
        let normalized_field_name = normalise_name_part(field_name);
        if assigned_names
            .iter()
            .any(|name| name == &normalized_field_name)
        {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "field {normalized_field_name} appears more than once in this {}",
                    context.operation
                ),
                context.logical_path,
                &field_name.span,
            )]);
        }
        assigned_names.push(normalized_field_name.clone());
        let Some(field) = catalogue.field_by_name(target_object, &normalized_field_name) else {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("object type {target_name} has no field named {normalized_field_name}"),
                context.logical_path,
                &field_name.span,
            )]);
        };
        if !supported_semantic_type(field.semantic_type())
            && !matches!(
                (field.semantic_type(), value),
                (SemanticType::Named(_), MutationValue::RecordConstructor(_))
            )
        {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!(
                    "{} does not yet support the type of field {normalized_field_name}; supported types are {SUPPORTED_MUTATION_TYPES}",
                    context.operation
                ),
                context.logical_path,
                &field_name.span,
            )]);
        }
        references.push(MutationReference::WriteField {
            owner: target_object,
            field: field.id(),
            location: SourceLocation::from_syntax(context.logical_path, &field_name.span),
        });
        let expression = check_assignment_expression(
            context,
            &normalized_field_name,
            field,
            value,
            &mut references,
            &mut expression_uses,
            catalogue,
        )?;
        assignments.push(MutationAssignment::new(
            target_object,
            field.id(),
            expression,
        ));
    }
    if require_all_non_nullable_fields {
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
                        format!(
                            "field {name} is required, but this {} does not provide a value",
                            context.operation
                        ),
                        context.logical_path,
                        &target_source.span,
                    )
                })
                .collect());
        }
    }
    Ok(CheckedAssignments {
        assignments,
        references,
        expression_uses,
    })
}

struct AssignmentCheckContext<'a, T, G, P> {
    operation: &'static str,
    function: G,
    parameters: &'a [MutationParameter<T, P>],
    logical_path: &'a str,
    intrinsic_boolean: IntrinsicBooleanType,
}

fn check_assignment_expression<T, F, G, P>(
    context: &AssignmentCheckContext<'_, T, G, P>,
    field_name: &str,
    field: MutationField<T, F>,
    value: &MutationValue,
    references: &mut Vec<MutationReference<T, F, G, P>>,
    expression_uses: &mut Vec<MutationExpressionUse<T>>,
    catalogue: &impl MutationCatalogue<T, F>,
) -> Result<MutationExpression<T, G, P, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    let expression = match value {
        MutationValue::Parameter(parameter_name) => {
            let normalized_parameter_name = normalise_name_part(parameter_name);
            let Some(parameter) = context
                .parameters
                .iter()
                .find(|parameter| parameter.name() == normalized_parameter_name)
            else {
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("this function has no parameter named {normalized_parameter_name}"),
                    context.logical_path,
                    &parameter_name.span,
                )]);
            };
            let parameter_type = parameter.semantic_type();
            if parameter_type != field.semantic_type()
                || parameter.standard_value_type() != field.standard_value_type()
            {
                let action = if context.operation == "INSERT" {
                    "inserted into"
                } else {
                    "assigned to"
                };
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "parameter {normalized_parameter_name} cannot be {action} field {field_name} because their types do not match"
                    ),
                    context.logical_path,
                    &parameter_name.span,
                )]);
            }
            references.push(MutationReference::ParameterRead {
                owner: context.function,
                parameter: parameter.id(),
                location: SourceLocation::from_syntax(context.logical_path, &parameter_name.span),
            });
            let value_type =
                mutation_value_type(parameter_type, parameter.standard_value_type(), false);
            MutationExpression::new(
                MutationExpressionKind::ParameterRead {
                    owner: context.function,
                    parameter: parameter.id(),
                },
                value_type,
            )
        }
        MutationValue::RecordConstructor(constructor) => {
            if context.operation != "INSERT" {
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    format!(
                        "{} does not support record constructors; they are only accepted by INSERT",
                        context.operation
                    ),
                    context.logical_path,
                    &constructor.span,
                )]);
            }
            check_record_constructor(
                context,
                field_name,
                field,
                constructor,
                references,
                expression_uses,
                catalogue,
            )?
        }
        MutationValue::BooleanLiteral { value, source } => {
            let expected = SemanticType::scalar(StandardScalar::Boolean);
            if field.semantic_type() != expected {
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("field {field_name} is not BOOLEAN, so it cannot accept TRUE or FALSE"),
                    context.logical_path,
                    &source.span,
                )]);
            }
            MutationExpression::new(
                MutationExpressionKind::BooleanLiteral { value: *value },
                intrinsic_boolean_value_type(
                    context.intrinsic_boolean,
                    context.logical_path,
                    &source.span,
                )?,
            )
        }
        MutationValue::NullLiteral { source } => {
            if !field.nullable() {
                return Err(vec![semantic_diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("field {field_name} does not allow NULL"),
                    context.logical_path,
                    &source.span,
                )]);
            }
            MutationExpression::new(
                MutationExpressionKind::TypedNull,
                mutation_value_type(field.semantic_type(), field.standard_value_type(), true),
            )
        }
        _ => {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!(
                    "{} values can only be function parameters, TRUE, FALSE, or NULL",
                    context.operation
                ),
                context.logical_path,
                value.span(),
            )]);
        }
    };
    if !matches!(value, MutationValue::RecordConstructor(_)) {
        expression_uses.push(MutationExpressionUse {
            value_type: *expression.value_type(),
            location: SourceLocation::from_syntax(context.logical_path, value.span()),
        });
    }
    Ok(expression)
}

#[allow(clippy::too_many_arguments)]
fn check_record_constructor<T, F, G, P>(
    context: &AssignmentCheckContext<'_, T, G, P>,
    target_field_name: &str,
    target_field: MutationField<T, F>,
    constructor: &RecordConstructor,
    references: &mut Vec<MutationReference<T, F, G, P>>,
    expression_uses: &mut Vec<MutationExpressionUse<T>>,
    catalogue: &impl MutationCatalogue<T, F>,
) -> Result<MutationExpression<T, G, P, F>, Vec<CompilerDiagnostic>>
where
    T: Copy + Eq,
    F: Copy + Eq,
    G: Copy,
    P: Copy,
{
    let record_name = normalise_qualified_name(&constructor.record_type);
    let Some(record_type) = catalogue.record_type_id_by_name(&record_name) else {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unknown record value type {record_name}"),
            context.logical_path,
            &constructor.record_type.span,
        )]);
    };
    if target_field.semantic_type() != SemanticType::Named(record_type) || target_field.nullable() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "record constructor {record_name} requires a non-null field of that exact record type, but field {target_field_name} does not match"
            ),
            context.logical_path,
            &constructor.record_type.span,
        )]);
    }
    references.push(MutationReference::NamedValueType {
        value_type: record_type,
        location: SourceLocation::from_syntax(context.logical_path, &constructor.record_type.span),
    });
    expression_uses.push(MutationExpressionUse {
        value_type: MutationValueType::new(SemanticType::Named(record_type), false),
        location: SourceLocation::from_syntax(context.logical_path, &constructor.span),
    });

    let mut supplied = Vec::with_capacity(constructor.fields.len());
    for source_field in &constructor.fields {
        let name = normalise_name_part(&source_field.name);
        if supplied
            .iter()
            .any(|(existing, _): &(String, _)| existing == &name)
        {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("record field {name} appears more than once in this constructor"),
                context.logical_path,
                &source_field.name.span,
            )]);
        }
        if catalogue.record_field_by_name(record_type, &name).is_none() {
            return Err(vec![semantic_diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("record value type {record_name} has no field named {name}"),
                context.logical_path,
                &source_field.name.span,
            )]);
        }
        supplied.push((name, source_field));
    }

    let mut checked_fields = Vec::with_capacity(constructor.fields.len());
    let mut diagnostics = Vec::new();
    catalogue.visit_record_fields(record_type, &mut |name, record_field| {
        let Some((_, source_field)) = supplied.iter().find(|(supplied, _)| supplied == name) else {
            diagnostics.push(semantic_diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("record field {name} is required, but this constructor does not provide it"),
                context.logical_path,
                &constructor.span,
            ));
            return;
        };
        let (child_kind, child_value_type) = match &source_field.value {
            RecordConstructorFieldValue::Parameter(parameter_name) => {
                let parameter_name_normalized = normalise_name_part(parameter_name);
                let Some(parameter) = context
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name() == parameter_name_normalized)
                else {
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!(
                            "this function has no parameter named {parameter_name_normalized}"
                        ),
                        context.logical_path,
                        &parameter_name.span,
                    ));
                    return;
                };
                if parameter.semantic_type() != record_field.semantic_type()
                    || parameter.standard_value_type() != record_field.standard_value_type()
                    || matches!(parameter.semantic_type(), SemanticType::Named(id) if !catalogue.named_type_is_enum(id))
                {
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "parameter {parameter_name_normalized} cannot initialise record field {name} because their types do not match"
                        ),
                        context.logical_path,
                        &parameter_name.span,
                    ));
                    return;
                }
                (
                    MutationRecordFieldExpressionKind::ParameterRead {
                        owner: context.function,
                        parameter: parameter.id(),
                    },
                    mutation_value_type(
                        parameter.semantic_type(),
                        parameter.standard_value_type(),
                        false,
                    ),
                )
            }
            RecordConstructorFieldValue::BooleanLiteral { value, source } => {
                if record_field.semantic_type()
                    != SemanticType::scalar(StandardScalar::Boolean)
                {
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "record field {name} is not BOOLEAN, so it cannot accept TRUE or FALSE"
                        ),
                        context.logical_path,
                        &source.span,
                    ));
                    return;
                }
                let Ok(value_type) = intrinsic_boolean_value_type(
                    context.intrinsic_boolean,
                    context.logical_path,
                    &source.span,
                ) else {
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::DomainIncompatible,
                        MISSING_BOOLEAN_MESSAGE,
                        context.logical_path,
                        &source.span,
                    ));
                    return;
                };
                if value_type.standard_value_type() != record_field.standard_value_type() {
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("record field {name} does not use the active Boolean type"),
                        context.logical_path,
                        &source.span,
                    ));
                    return;
                }
                (
                    MutationRecordFieldExpressionKind::BooleanLiteral { value: *value },
                    value_type,
                )
            }
            _ => {
                diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "record constructor fields only accept declared parameters, TRUE, or FALSE",
                    context.logical_path,
                    source_field.value.span(),
                ));
                return;
            }
        };
        references.push(MutationReference::WriteField {
            owner: record_type,
            field: record_field.id(),
            location: SourceLocation::from_syntax(
                context.logical_path,
                &source_field.name.span,
            ),
        });
        if let MutationRecordFieldExpressionKind::ParameterRead { owner, parameter } = child_kind {
            references.push(MutationReference::ParameterRead {
                owner,
                parameter,
                location: SourceLocation::from_syntax(
                    context.logical_path,
                    source_field.value.span(),
                ),
            });
        }
        expression_uses.push(MutationExpressionUse {
            value_type: child_value_type,
            location: SourceLocation::from_syntax(
                context.logical_path,
                source_field.value.span(),
            ),
        });
        checked_fields.push(MutationRecordFieldExpression {
            owner: record_type,
            field: record_field.id(),
            kind: child_kind,
            value_type: child_value_type,
        });
    });
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    if checked_fields.len() != constructor.fields.len() {
        return Err(vec![semantic_diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "record constructor {record_name} does not provide the exact declared field set"
            ),
            context.logical_path,
            &constructor.span,
        )]);
    }
    Ok(MutationExpression::new(
        MutationExpressionKind::RecordConstructor {
            record_type,
            fields: checked_fields,
        },
        MutationValueType::new(SemanticType::Named(record_type), false),
    ))
}

const MISSING_BOOLEAN_MESSAGE: &str =
    "the checked standard library does not provide a Boolean value type";

fn missing_insert_boolean_diagnostics(
    insert: &InsertStatement,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Option<Vec<CompilerDiagnostic>> {
    let spans = insert
        .values
        .iter()
        .flat_map(|value| match value {
            MutationValue::BooleanLiteral { source, .. } => vec![&source.span],
            MutationValue::RecordConstructor(constructor) => constructor
                .fields
                .iter()
                .filter_map(|field| match &field.value {
                    RecordConstructorFieldValue::BooleanLiteral { source, .. } => {
                        Some(&source.span)
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();
    missing_boolean_diagnostics(intrinsic_boolean, logical_path, spans)
}

fn missing_update_boolean_diagnostics(
    update: &UpdateStatement,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Option<Vec<CompilerDiagnostic>> {
    missing_boolean_diagnostics(
        intrinsic_boolean,
        logical_path,
        update
            .assignments
            .iter()
            .filter_map(|assignment| boolean_literal_span(&assignment.value))
            .chain(std::iter::once(&update.selector_equality_span)),
    )
}

fn missing_delete_boolean_diagnostics(
    delete: &DeleteStatement,
    logical_path: &str,
    intrinsic_boolean: IntrinsicBooleanType,
) -> Option<Vec<CompilerDiagnostic>> {
    missing_boolean_diagnostics(
        intrinsic_boolean,
        logical_path,
        [&delete.selector_equality_span, &delete.returning_true.span],
    )
}

fn boolean_literal_span(value: &MutationValue) -> Option<&SourceSpan> {
    match value {
        MutationValue::BooleanLiteral { source, .. } => Some(&source.span),
        MutationValue::RecordConstructor(constructor) => {
            constructor
                .fields
                .iter()
                .find_map(|field| match &field.value {
                    RecordConstructorFieldValue::BooleanLiteral { source, .. } => {
                        Some(&source.span)
                    }
                    RecordConstructorFieldValue::Parameter(_) => None,
                    _ => None,
                })
        }
        MutationValue::Parameter(_) | MutationValue::NullLiteral { .. } => None,
        _ => None,
    }
}

fn missing_boolean_diagnostics<'a>(
    intrinsic_boolean: IntrinsicBooleanType,
    logical_path: &str,
    spans: impl IntoIterator<Item = &'a SourceSpan>,
) -> Option<Vec<CompilerDiagnostic>> {
    if !matches!(intrinsic_boolean, IntrinsicBooleanType::Missing) {
        return None;
    }
    let diagnostics = spans
        .into_iter()
        .map(|span| {
            semantic_diagnostic(
                DiagnosticCode::DomainIncompatible,
                MISSING_BOOLEAN_MESSAGE,
                logical_path,
                span,
            )
        })
        .collect::<Vec<_>>();
    (!diagnostics.is_empty()).then_some(diagnostics)
}

fn intrinsic_boolean_value_type<T>(
    intrinsic_boolean: IntrinsicBooleanType,
    logical_path: &str,
    span: &SourceSpan,
) -> Result<MutationValueType<T>, Vec<CompilerDiagnostic>> {
    match intrinsic_boolean {
        IntrinsicBooleanType::Legacy => Ok(MutationValueType::new(
            SemanticType::scalar(StandardScalar::Boolean),
            false,
        )),
        IntrinsicBooleanType::Standard(type_id) => Ok(MutationValueType::new(
            SemanticType::scalar(StandardScalar::Boolean),
            false,
        )
        .with_standard_value_type(type_id)),
        IntrinsicBooleanType::Missing => Err(vec![semantic_diagnostic(
            DiagnosticCode::DomainIncompatible,
            MISSING_BOOLEAN_MESSAGE,
            logical_path,
            span,
        )]),
    }
}

fn mutation_value_type<T>(
    semantic_type: SemanticType<T>,
    standard_value_type: Option<TypeId>,
    nullable: bool,
) -> MutationValueType<T> {
    if let Some(type_id) = standard_value_type {
        MutationValueType::new(semantic_type, nullable).with_standard_value_type(type_id)
    } else {
        MutationValueType::new(semantic_type, nullable)
    }
}

fn validate_parameter_types<T, F, P>(
    operation: &str,
    parameters: &[MutationParameter<T, P>],
    logical_path: &str,
    catalogue: &impl MutationCatalogue<T, F>,
    enum_parameter_names: &[String],
) -> Result<(), Vec<CompilerDiagnostic>>
where
    T: Copy,
{
    // Validate every declaration before looking at whether that parameter is
    // used. This keeps unsupported parameter types fail-closed.
    let diagnostics = parameters
        .iter()
        .filter_map(|parameter| {
            if supported_semantic_type(parameter.semantic_type())
                || matches!(parameter.semantic_type(), SemanticType::Named(id) if enum_parameter_names.iter().any(|name| name == parameter.name()) && catalogue.named_type_is_enum(id))
            {
                None
            } else {
                Some(semantic_diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    format!(
                        "{operation} does not yet support the type of parameter {}; supported types are {SUPPORTED_MUTATION_TYPES}",
                        parameter.name()
                    ),
                    logical_path,
                    parameter.location(),
                ))
            }
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
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
    use orna_syntax::{
        DeleteStatement, InsertValue, QualifiedName, RecordConstructorField, SourceSlice,
        SourceSpan, UpdateAssignment, UpdateStatement,
    };

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
            returning_ref_span: span(46, 60),
            span: span(0, 60),
        }
    }

    fn update(
        assignments: Vec<UpdateAssignment>,
        selector_alias: &str,
        selector_parameter: &str,
        returning_alias: &str,
    ) -> UpdateStatement {
        let selector_parameter = name(selector_parameter, 90);
        let selector_equality_span = span(76, selector_parameter.span.end);
        UpdateStatement {
            target_object: QualifiedName {
                parts: vec![name("crm", 7), name("person", 11)],
                span: span(7, 18),
            },
            target_alias: name("p", 22),
            assignments,
            selector_alias: name(selector_alias, 80),
            selector_parameter,
            selector_equality_span,
            selector_ref_span: span(76, 88),
            returning_alias: name(returning_alias, 110),
            returning_ref_span: span(106, 120),
            span: span(0, 120),
        }
    }

    fn delete(selector_alias: &str, selector_parameter: &str) -> DeleteStatement {
        let selector_parameter = name(selector_parameter, 50);
        let selector_equality_span = span(36, selector_parameter.span.end);
        DeleteStatement {
            target_object: QualifiedName {
                parts: vec![name("crm", 12), name("person", 16)],
                span: span(12, 23),
            },
            target_alias: name("p", 27),
            selector_alias: name(selector_alias, 40),
            selector_parameter,
            selector_equality_span,
            selector_ref_span: span(36, 48),
            returning_true: SourceSlice {
                text: "TRUE".to_owned(),
                span: span(70, 74),
            },
            span: span(0, 74),
        }
    }

    fn update_assignment(field: &str, field_start: usize, value: InsertValue) -> UpdateAssignment {
        let value_end = value.span().end;
        UpdateAssignment {
            target_field: name(field, field_start),
            value,
            span: span(field_start, value_end),
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

    #[test]
    fn synthetic_selector_spans_end_at_the_parameter() {
        let update = update(Vec::new(), "p", "p_person", "p");
        assert_eq!(
            update.selector_equality_span.end,
            update.selector_parameter.span.end
        );

        let delete = delete("p", "p_person");
        assert_eq!(
            delete.selector_equality_span.end,
            delete.selector_parameter.span.end
        );
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
    fn standard_assignment_compatibility_requires_matching_value_type_identities() {
        let function = FunctionId::from_bytes([5; 16]);
        let field_type = TypeId::from_bytes([11; 16]);
        let other_type = TypeId::from_bytes([12; 16]);
        let mut catalogue = catalogue();
        catalogue.fields[0].1 = MutationField::new(
            FieldId::from_bytes([2; 16]),
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
            false,
        )
        .with_standard_value_type(field_type);
        let source = insert(
            vec![name("name", 25), name("active", 31)],
            vec![parameter("p_name", 47), boolean(true, 56)],
            "p",
        );

        let mismatched = check_insert_in(
            &source,
            &catalogue,
            function,
            &[MutationParameter::new(
                "p_name",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(100, 106),
            )
            .with_standard_value_type(other_type)],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(mismatched.len(), 1);
        assert_eq!(mismatched[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            mismatched[0].message(),
            "parameter p_name cannot be inserted into field name because their types do not match"
        );
        assert_eq!(mismatched[0].location().span().start(), 47);
        assert_eq!(mismatched[0].location().span().end(), 53);

        let matching = check_insert_in(
            &source,
            &catalogue,
            function,
            &[MutationParameter::new(
                "p_name",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(100, 106),
            )
            .with_standard_value_type(field_type)],
            "mutation.orna",
        )
        .unwrap();
        assert_eq!(
            matching.plan().assignments()[0]
                .expression()
                .value_type()
                .standard_value_type(),
            Some(field_type)
        );

        let mixed = check_insert_in(
            &source,
            &catalogue,
            function,
            &[MutationParameter::new(
                "p_name",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(100, 106),
            )],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(mixed[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            mixed[0].message(),
            "parameter p_name cannot be inserted into field name because their types do not match"
        );

        let update_mismatch = check_update_in(
            &update(
                vec![update_assignment("name", 30, parameter("p_name", 37))],
                "p",
                "p_person",
                "p",
            ),
            &catalogue,
            function,
            &[
                MutationParameter::new(
                    "p_name",
                    ParameterId::from_bytes([6; 16]),
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    span(100, 106),
                )
                .with_standard_value_type(other_type),
                MutationParameter::new(
                    "p_person",
                    ParameterId::from_bytes([7; 16]),
                    SemanticType::reference(catalogue.target),
                    span(110, 118),
                ),
            ],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(update_mismatch.len(), 1);
        assert_eq!(update_mismatch[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            update_mismatch[0].message(),
            "parameter p_name cannot be assigned to field name because their types do not match"
        );
        assert_eq!(update_mismatch[0].location().span().start(), 37);
        assert_eq!(update_mismatch[0].location().span().end(), 43);
    }

    #[test]
    fn update_rejects_a_record_constructor_before_record_catalogue_lookup() {
        let record_type = TypeId::from_bytes([9; 16]);
        let mut catalogue = catalogue();
        catalogue.fields.push((
            "flags".to_owned(),
            MutationField::new(
                FieldId::from_bytes([10; 16]),
                SemanticType::Named(record_type),
                false,
            ),
        ));
        let constructor = InsertValue::RecordConstructor(RecordConstructor {
            record_type: QualifiedName {
                parts: vec![name("app", 37), name("flags", 41)],
                span: span(37, 50),
            },
            fields: vec![RecordConstructorField {
                name: name("active", 51),
                value: RecordConstructorFieldValue::BooleanLiteral {
                    value: true,
                    source: SourceSlice {
                        text: "TRUE".to_owned(),
                        span: span(59, 63),
                    },
                },
                span: span(51, 63),
            }],
            span: span(37, 64),
        });
        let statement = update(
            vec![update_assignment("flags", 29, constructor)],
            "p",
            "p_person",
            "p",
        );

        let diagnostics = check_update_in(
            &statement,
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &[MutationParameter::new(
                "p_person",
                ParameterId::from_bytes([6; 16]),
                SemanticType::reference(catalogue.target),
                span(100, 108),
            )],
            "mutation.orna",
        )
        .unwrap_err();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostics[0].message(),
            "UPDATE does not support record constructors; they are only accepted by INSERT"
        );
        assert_eq!(diagnostics[0].location().span().start(), 37);
        assert_eq!(diagnostics[0].location().span().end(), 64);
    }

    #[test]
    fn missing_intrinsic_boolean_rejects_insert_literal_without_a_plan() {
        let diagnostics = check_insert_with_intrinsic_boolean_in(
            &insert(vec![name("active", 25)], vec![boolean(true, 34)], "p"),
            &catalogue(),
            FunctionId::from_bytes([5; 16]),
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
            crate::relational::IntrinsicBooleanType::Missing,
        )
        .unwrap_err();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostics[0].message(),
            "the checked standard library does not provide a Boolean value type"
        );
        assert_eq!(diagnostics[0].location().span().start(), 34);
        assert_eq!(diagnostics[0].location().span().end(), 38);
    }

    #[test]
    fn missing_intrinsic_boolean_reports_update_and_delete_paths_in_source_order() {
        let update = update(
            vec![
                update_assignment("active", 30, boolean(true, 39)),
                update_assignment("active", 46, boolean(false, 55)),
            ],
            "p",
            "p_person",
            "p",
        );
        let update_diagnostics = check_update_with_intrinsic_boolean_in(
            &update,
            &catalogue(),
            FunctionId::from_bytes([5; 16]),
            &[MutationParameter::new(
                "p_person",
                ParameterId::from_bytes([6; 16]),
                SemanticType::reference(TypeId::from_bytes([1; 16])),
                span(100, 108),
            )],
            "mutation.orna",
            crate::relational::IntrinsicBooleanType::Missing,
        )
        .unwrap_err();
        assert_eq!(update_diagnostics.len(), 3);
        assert_eq!(
            update_diagnostics
                .iter()
                .map(|diagnostic| {
                    (
                        diagnostic.code(),
                        diagnostic.message(),
                        diagnostic.location().span().start(),
                        diagnostic.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    DiagnosticCode::DomainIncompatible,
                    MISSING_BOOLEAN_MESSAGE,
                    39,
                    43,
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    MISSING_BOOLEAN_MESSAGE,
                    55,
                    60,
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    MISSING_BOOLEAN_MESSAGE,
                    update.selector_equality_span.start,
                    update.selector_equality_span.end,
                ),
            ]
        );

        let delete = delete("p", "p_person");
        let delete_diagnostics = check_delete_with_intrinsic_boolean_in(
            &delete,
            &catalogue(),
            FunctionId::from_bytes([5; 16]),
            &[MutationParameter::new(
                "p_person",
                ParameterId::from_bytes([6; 16]),
                SemanticType::reference(TypeId::from_bytes([1; 16])),
                span(100, 108),
            )],
            "mutation.orna",
            crate::relational::IntrinsicBooleanType::Missing,
        )
        .unwrap_err();
        assert_eq!(delete_diagnostics.len(), 2);
        assert_eq!(
            delete_diagnostics
                .iter()
                .map(|diagnostic| {
                    (
                        diagnostic.code(),
                        diagnostic.message(),
                        diagnostic.location().span().start(),
                        diagnostic.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    DiagnosticCode::DomainIncompatible,
                    MISSING_BOOLEAN_MESSAGE,
                    delete.selector_equality_span.start,
                    delete.selector_equality_span.end,
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    MISSING_BOOLEAN_MESSAGE,
                    delete.returning_true.span.start,
                    delete.returning_true.span.end,
                ),
            ]
        );
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
            operation: MutationOperation::Insert,
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
    fn checks_identity_selected_update_and_orders_evidence() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let selector_id = ParameterId::from_bytes([6; 16]);
        let name_id = ParameterId::from_bytes([7; 16]);
        let parameters = vec![
            MutationParameter::new(
                "p_person",
                selector_id,
                SemanticType::reference(catalogue.target),
                span(200, 208),
            ),
            MutationParameter::new(
                "p_name",
                name_id,
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                span(210, 216),
            ),
        ];
        let update = update(
            vec![
                update_assignment("name", 30, parameter("p_name", 37)),
                update_assignment("active", 47, boolean(false, 56)),
                update_assignment("note", 64, null(71)),
            ],
            "p",
            "p_person",
            "p",
        );

        let check =
            check_update_in(&update, &catalogue, function, &parameters, "mutation.orna").unwrap();

        assert_eq!(
            check.plan().operation(),
            &MutationOperation::Update {
                selector_owner: function,
                selector_parameter: selector_id,
            }
        );
        assert_eq!(check.plan().target_object(), catalogue.target);
        assert_eq!(check.plan().returned_object(), catalogue.target);
        assert_eq!(check.plan().assignments().len(), 3);
        let location = |start, end| SourceLocation::from_syntax("mutation.orna", &span(start, end));
        assert_eq!(
            check.references(),
            [
                MutationReference::WriteObject {
                    object_type: catalogue.target,
                    location: location(7, 18),
                },
                MutationReference::WriteField {
                    owner: catalogue.target,
                    field: FieldId::from_bytes([2; 16]),
                    location: location(30, 34),
                },
                MutationReference::ParameterRead {
                    owner: function,
                    parameter: name_id,
                    location: location(37, 43),
                },
                MutationReference::WriteField {
                    owner: catalogue.target,
                    field: FieldId::from_bytes([3; 16]),
                    location: location(47, 53),
                },
                MutationReference::WriteField {
                    owner: catalogue.target,
                    field: FieldId::from_bytes([4; 16]),
                    location: location(64, 68),
                },
                MutationReference::ObjectReference {
                    object_type: catalogue.target,
                    location: location(80, 81),
                },
                MutationReference::ParameterRead {
                    owner: function,
                    parameter: selector_id,
                    location: location(90, 98),
                },
                MutationReference::ObjectReference {
                    object_type: catalogue.target,
                    location: location(110, 111),
                },
            ]
        );
    }

    #[test]
    fn rejects_update_selector_with_unknown_or_wrong_reference_type() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let update = update(
            vec![update_assignment("note", 30, null(37))],
            "p",
            "selected",
            "p",
        );

        let unknown = check_update_in(
            &update,
            &catalogue,
            function,
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            unknown[0].message(),
            "this function has no parameter named selected"
        );
        assert_eq!(unknown[0].location().span().start(), 90);
        assert_eq!(unknown[0].location().span().end(), 98);

        let wrong_type = check_update_in(
            &update,
            &catalogue,
            function,
            &[MutationParameter::new(
                "selected",
                ParameterId::from_bytes([6; 16]),
                SemanticType::scalar(StandardScalar::BigInt),
                span(200, 208),
            )],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(wrong_type.len(), 1);
        assert_eq!(wrong_type[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            wrong_type[0].message(),
            "selector parameter selected must use REF crm.person"
        );
        assert_eq!(wrong_type[0].location().span().start(), 90);
        assert_eq!(wrong_type[0].location().span().end(), 98);
    }

    #[test]
    fn checks_identity_selected_delete_and_orders_evidence() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let selector_id = ParameterId::from_bytes([6; 16]);
        let parameters = [
            MutationParameter::new(
                "p_person",
                selector_id,
                SemanticType::reference(catalogue.target),
                span(100, 108),
            ),
            MutationParameter::new(
                "unused",
                ParameterId::from_bytes([7; 16]),
                SemanticType::scalar(StandardScalar::Boolean),
                span(110, 116),
            ),
        ];

        let check = check_delete_in(
            &delete("p", "p_person"),
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap();

        assert_eq!(check.plan().target_object(), catalogue.target);
        assert_eq!(check.plan().selector_owner(), function);
        assert_eq!(check.plan().selector_parameter(), selector_id);
        let location = |start, end| SourceLocation::from_syntax("mutation.orna", &span(start, end));
        assert_eq!(
            check.references(),
            [
                MutationReference::WriteObject {
                    object_type: catalogue.target,
                    location: location(12, 23),
                },
                MutationReference::ObjectReference {
                    object_type: catalogue.target,
                    location: location(40, 41),
                },
                MutationReference::ParameterRead {
                    owner: function,
                    parameter: selector_id,
                    location: location(50, 58),
                },
            ]
        );

        let mapped = check
            .plan()
            .try_map_identities(
                |id| (id == catalogue.target).then_some(11).ok_or("type"),
                |id| (id == function).then_some(12).ok_or("function"),
                |id| (id == selector_id).then_some(13).ok_or("parameter"),
            )
            .unwrap();
        assert_eq!(mapped.target_object(), 11);
        assert_eq!(mapped.selector_owner(), 12);
        assert_eq!(mapped.selector_parameter(), 13);
    }

    #[test]
    fn rejects_invalid_delete_target_alias_selector_and_parameter_domain() {
        let catalogue = catalogue();
        let function = FunctionId::from_bytes([5; 16]);
        let selector_id = ParameterId::from_bytes([6; 16]);
        let valid_parameter = MutationParameter::new(
            "p_person",
            selector_id,
            SemanticType::reference(catalogue.target),
            span(100, 108),
        );

        let alias = check_delete_in(
            &delete("other", "p_person"),
            &catalogue,
            function,
            std::slice::from_ref(&valid_parameter),
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(alias[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            alias[0].message(),
            "WHERE REF must use the DELETE target alias p, not other"
        );
        assert_eq!(alias[0].location().span().start(), 40);
        assert_eq!(alias[0].location().span().end(), 45);

        let unknown = check_delete_in(
            &delete("p", "missing"),
            &catalogue,
            function,
            std::slice::from_ref(&valid_parameter),
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(unknown[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            unknown[0].message(),
            "this function has no parameter named missing"
        );
        assert_eq!(unknown[0].location().span().start(), 50);
        assert_eq!(unknown[0].location().span().end(), 57);

        for semantic_type in [
            SemanticType::scalar(StandardScalar::BigInt),
            SemanticType::reference(TypeId::from_bytes([9; 16])),
        ] {
            let wrong = check_delete_in(
                &delete("p", "p_person"),
                &catalogue,
                function,
                &[MutationParameter::new(
                    "p_person",
                    selector_id,
                    semantic_type,
                    span(100, 108),
                )],
                "mutation.orna",
            )
            .unwrap_err();
            assert_eq!(wrong[0].code(), DiagnosticCode::TypeMismatch);
            assert_eq!(
                wrong[0].message(),
                "selector parameter p_person must use REF crm.person"
            );
            assert_eq!(wrong[0].location().span().start(), 50);
            assert_eq!(wrong[0].location().span().end(), 58);
        }

        let unsupported = check_delete_in(
            &delete("p", "p_person"),
            &catalogue,
            function,
            &[
                valid_parameter,
                MutationParameter::new(
                    "unused",
                    ParameterId::from_bytes([7; 16]),
                    SemanticType::scalar(StandardScalar::Decimal),
                    span(110, 116),
                ),
            ],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(unsupported[0].code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            unsupported[0].message(),
            "DELETE does not yet support the type of parameter unused; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
        );
        assert_eq!(unsupported[0].location().span().start(), 110);
        assert_eq!(unsupported[0].location().span().end(), 116);

        let mut unknown_target = delete("p", "p_person");
        unknown_target.target_object.parts[1].text = "missing".to_owned();
        let unknown_target = check_delete_in(
            &unknown_target,
            &catalogue,
            function,
            &[MutationParameter::new(
                "p_person",
                selector_id,
                SemanticType::reference(catalogue.target),
                span(100, 108),
            )],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(
            unknown_target[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_eq!(
            unknown_target[0].message(),
            "unknown object type crm.missing"
        );
        assert_eq!(unknown_target[0].location().span().start(), 12);
        assert_eq!(unknown_target[0].location().span().end(), 23);
    }

    #[test]
    fn delete_identity_mapping_fails_closed_for_each_identity_class() {
        let plan = DeletePlanIr {
            target_object: TypeId::from_bytes([1; 16]),
            selector_owner: FunctionId::from_bytes([2; 16]),
            selector_parameter: ParameterId::from_bytes([3; 16]),
        };

        assert_eq!(
            plan.try_map_identities(
                |_| Err::<u8, _>("type"),
                |_| Ok::<_, &str>(2),
                |_| Ok::<_, &str>(3),
            ),
            Err("type")
        );
        assert_eq!(
            plan.try_map_identities(
                |_| Ok::<_, &str>(1),
                |_| Err::<u8, _>("function"),
                |_| Ok::<_, &str>(3),
            ),
            Err("function")
        );
        assert_eq!(
            plan.try_map_identities(
                |_| Ok::<_, &str>(1),
                |_| Ok::<_, &str>(2),
                |_| Err::<u8, _>("parameter"),
            ),
            Err("parameter")
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
        assert_eq!(
            error[0].message(),
            "field name appears more than once in this INSERT"
        );
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
        let error = check.unwrap_err();
        assert_eq!(error[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            error[0].message(),
            "field active is required, but this INSERT does not provide a value"
        );

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
        assert_eq!(
            error[0].message(),
            "INSERT does not yet support the type of parameter unused; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
        );
        assert_eq!(error[0].location().span().start(), 100);
        assert_eq!(error[0].location().span().end(), 106);
    }

    #[test]
    fn rejects_unsupported_field_type_with_the_field_name() {
        let mut catalogue = catalogue();
        catalogue.fields[0].1 = MutationField::new(
            FieldId::from_bytes([2; 16]),
            SemanticType::scalar(StandardScalar::Decimal),
            false,
        );
        let parameters = vec![MutationParameter::new(
            "name",
            ParameterId::from_bytes([6; 16]),
            SemanticType::scalar(StandardScalar::CharacterLargeObject),
            span(100, 104),
        )];

        let error = check_insert_in(
            &insert(vec![name("name", 25)], vec![parameter("name", 31)], "p"),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();

        assert_eq!(error[0].code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            error[0].message(),
            "INSERT does not yet support the type of field name; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
        );
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
        assert_eq!(
            malformed[0].message(),
            "INSERT requires at least one target field and one value"
        );

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
        assert_eq!(
            arity[0].message(),
            "INSERT lists 2 fields but 1 value; each field requires one value"
        );
        assert_eq!(arity[0].location().span().start(), 0);

        let singular_field_count = check_insert_in(
            &insert(
                vec![name("name", 25)],
                vec![parameter("name", 39), parameter("name", 46)],
                "p",
            ),
            &catalogue,
            FunctionId::from_bytes([5; 16]),
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(
            singular_field_count[0].message(),
            "INSERT lists 1 field but 2 values; each field requires one value"
        );
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
        assert_eq!(
            diagnostic[0].message(),
            "object type crm.person has no field named missing"
        );
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
        assert_eq!(
            diagnostic[0].message(),
            "this function has no parameter named missing"
        );
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
        assert_eq!(
            diagnostic[0].message(),
            "RETURNING REF must use the INSERT target alias p, not other"
        );
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
        assert_eq!(
            scalar_ref[0].message(),
            "parameter count cannot be inserted into field name because their types do not match"
        );

        let ref_target = check_insert_in(
            &insert(vec![name("owner", 25)], vec![parameter("owner", 31)], "p"),
            &catalogue,
            function,
            &parameters,
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(ref_target[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            ref_target[0].message(),
            "parameter owner cannot be inserted into field owner because their types do not match"
        );

        let bool_wrong = check_insert_in(
            &insert(vec![name("name", 25)], vec![boolean(true, 31)], "p"),
            &catalogue,
            function,
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(bool_wrong[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            bool_wrong[0].message(),
            "field name is not BOOLEAN, so it cannot accept TRUE or FALSE"
        );

        let null_mandatory = check_insert_in(
            &insert(vec![name("active", 25)], vec![null(32)], "p"),
            &catalogue,
            function,
            &[] as &[MutationParameter<TypeId, ParameterId>],
            "mutation.orna",
        )
        .unwrap_err();
        assert_eq!(null_mandatory[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            null_mandatory[0].message(),
            "field active does not allow NULL"
        );
    }

    #[test]
    fn maps_all_identity_classes_and_rejects_any_failed_mapping() {
        let plan = MutationPlanIr {
            operation: MutationOperation::Insert,
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
        let update_plan = MutationPlanIr {
            operation: MutationOperation::Update {
                selector_owner: 8_u8,
                selector_parameter: 9_u8,
            },
            target_object: plan.target_object,
            assignments: plan.assignments.clone(),
            returned_object: plan.returned_object,
        };
        let mapped_update = update_plan
            .try_map_identities(
                |value| Ok::<_, ()>(value + 10),
                |value| Ok::<_, ()>(value + 20),
                |value| Ok::<_, ()>(value + 30),
                |value| Ok::<_, ()>(value + 40),
            )
            .unwrap();
        assert_eq!(
            mapped_update.operation(),
            &MutationOperation::Update {
                selector_owner: 38,
                selector_parameter: 49,
            }
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
