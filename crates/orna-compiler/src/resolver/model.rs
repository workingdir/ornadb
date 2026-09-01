//! Checked-result values produced by semantic resolution.

use std::{collections::HashMap, error::Error, fmt, hash::Hash};

use orna_artifact::{
    client_plan::{
        ActionTargetDomain, ControlFlowBinaryOperator, ControlFlowUnaryOperator, ResourceKind,
    },
    server_parameter_echo::ServerParameterEchoError,
};

use orna_core::{
    CallSiteId, CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceUnitId, StandardLibraryRevisionId, StateSlotId, TypeBindingId, TypeId,
    canonical_hash::CanonicalHashError,
    catalogue::{
        CatalogueSnapshot, FunctionDomain, FunctionSecurity, FunctionTransaction,
        FunctionVolatility, OnDeleteAction, QualifiedSemanticName, TypeBindingKind, TypeLookupName,
        ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        DefinitionReference, DefinitionReferenceKind, ExecutableArtifact,
        FunctionSemanticHashVersion, RevisionInvariantError, Sha256Digest, SourceOrigin,
        VerifiedStandardLibrarySnapshot,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{
    FunctionSecurity as SyntaxFunctionSecurity, FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility,
};

use crate::{
    CompilerDiagnostic, ParseReport, SourceLocation,
    mutation::{MutationCatalogue, MutationField},
    relational::{
        DistinctQueryIr, IdentitySelectedQueryIr, RelationalQueryIr, UniqueTextSelectedQueryIr,
    },
};

use super::{
    CheckedExpressionId, CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId,
    CheckedTypeId,
};
mod query;
mod standard_application_report;

pub(crate) use query::{QueryCatalogue, QueryField, QueryObjectType, ResolutionCatalogue};

/// A resolved semantic type whose identities belong to the checking context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SemanticType<T> {
    /// A standard scalar type.
    Scalar(StandardScalar),
    /// A resolved non-scalar named type.
    Named(T),
    /// A reference to a resolved object type.
    Reference {
        /// The referenced object type.
        target: T,
    },
}

impl<T> SemanticType<T> {
    /// Creates a standard scalar type.
    pub const fn scalar(scalar: StandardScalar) -> Self {
        Self::Scalar(scalar)
    }

    /// Creates a typed object reference.
    pub const fn reference(target: T) -> Self {
        Self::Reference { target }
    }
}

impl SemanticType<TypeId> {
    /// Converts one legacy durable core type into the compiler identity domain.
    ///
    /// Value identities and unknown future shapes have no legacy compiler projection.
    pub(crate) const fn from_core(resolved_type: ResolvedType) -> Option<Self> {
        if let Some(scalar) = resolved_type.legacy_scalar() {
            return Some(Self::Scalar(scalar));
        }
        if let Some(type_id) = resolved_type.named_type() {
            return Some(Self::Named(type_id));
        }
        if let Some(target) = resolved_type.reference_target() {
            return Some(Self::Reference { target });
        }
        if resolved_type.value_type().is_some() {
            return None;
        }
        None
    }

    /// Converts a durable compiler type back into the core representation.
    #[cfg(test)]
    pub(crate) const fn into_core(self) -> ResolvedType {
        match self {
            Self::Scalar(scalar) => ResolvedType::Scalar(scalar),
            Self::Named(type_id) => ResolvedType::Named(type_id),
            Self::Reference { target } => ResolvedType::Reference { target },
        }
    }
}

/// The value of a default expression accepted in this first compiler slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Text(String),
}

/// A checked constant default expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefault {
    pub(super) id: CheckedExpressionId,
    pub(super) value: ConstantValue,
    pub(super) location: SourceLocation,
}

impl CheckedDefault {
    /// Returns the identity of this checked expression.
    pub const fn id(&self) -> CheckedExpressionId {
        self.id
    }

    /// Returns the checked constant value.
    pub fn value(&self) -> &ConstantValue {
        &self.value
    }

    /// Returns the location of the source expression.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked field definition without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedField {
    pub(super) id: CheckedFieldId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) nullable: bool,
    pub(super) unique: bool,
    pub(super) default: Option<CheckedDefault>,
    pub(super) on_delete: Option<OnDeleteAction>,
    pub(super) location: SourceLocation,
}

impl CheckedField {
    /// Returns the identity of the field.
    pub const fn id(&self) -> CheckedFieldId {
        self.id
    }
    /// Returns the resolved field name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }
    /// Reports whether the field permits null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
    /// Reports whether the field is unique.
    pub const fn unique(&self) -> bool {
        self.unique
    }
    /// Returns the checked default expression, when declared.
    pub fn default(&self) -> Option<&CheckedDefault> {
        self.default.as_ref()
    }
    /// Returns the resolved delete action, when declared.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
    /// Returns the source location of the field declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked object type declaration without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectType {
    pub(super) id: CheckedTypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) fields: Vec<CheckedField>,
    pub(super) location: SourceLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CheckedEnumType {
    pub(super) id: CheckedTypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) labels: Vec<String>,
    pub(super) location: SourceLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedRecordValueField {
    pub(super) id: CheckedFieldId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedRecordValueField {
    pub(crate) const fn id(&self) -> CheckedFieldId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub(crate) const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedRecordValueType {
    pub(super) id: CheckedTypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) fields: Vec<CheckedRecordValueField>,
    pub(super) location: SourceLocation,
}

impl CheckedRecordValueType {
    pub(crate) const fn id(&self) -> CheckedTypeId {
        self.id
    }

    pub(crate) fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    pub(crate) fn fields(&self) -> &[CheckedRecordValueField] {
        &self.fields
    }

    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One accepted field-name transition bound to a stable checked identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedFieldRename {
    pub(crate) owner: CheckedTypeId,
    pub(crate) field: CheckedFieldId,
    pub(crate) old_name: String,
    pub(crate) new_name: String,
}

impl CheckedObjectType {
    /// Returns the identity of the object type.
    pub const fn id(&self) -> CheckedTypeId {
        self.id
    }
    /// Returns the resolved qualified type name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }
    /// Returns checked fields in declaration order.
    pub fn fields(&self) -> &[CheckedField] {
        &self.fields
    }
    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked source bundle with resolved declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedBundle {
    pub(super) base_catalogue_revision: CatalogueRevisionId,
    pub(super) schemas: Vec<CheckedSchema>,
    pub(super) object_types: Vec<CheckedObjectType>,
    pub(super) enum_types: Vec<CheckedEnumType>,
    pub(super) record_value_types: Vec<CheckedRecordValueType>,
    pub(super) server_functions: Vec<CheckedServerFunction>,
    pub(super) client_functions: Vec<CheckedClientFunction>,
    pub(super) field_renames: Vec<CheckedFieldRename>,
}

impl CheckedBundle {
    /// Returns the immutable catalogue revision used for identity continuity.
    pub const fn base_catalogue_revision(&self) -> CatalogueRevisionId {
        self.base_catalogue_revision
    }

    /// Returns submitted schema declarations in source order.
    pub fn schemas(&self) -> &[CheckedSchema] {
        &self.schemas
    }

    /// Returns submitted object declarations in source order.
    pub fn object_types(&self) -> &[CheckedObjectType] {
        &self.object_types
    }

    /// Returns submitted enum definitions in source order.
    pub fn enum_types(
        &self,
    ) -> impl ExactSizeIterator<
        Item = (
            CheckedTypeId,
            &QualifiedSemanticName,
            &[String],
            &SourceLocation,
        ),
    > {
        self.enum_types.iter().map(|enum_type| {
            (
                enum_type.id,
                &enum_type.name,
                enum_type.labels.as_slice(),
                &enum_type.location,
            )
        })
    }

    /// Returns submitted record value definitions in source order.
    pub(crate) fn record_value_types(&self) -> &[CheckedRecordValueType] {
        &self.record_value_types
    }

    /// Returns submitted checked SERVER functions in source order.
    pub fn server_functions(&self) -> &[CheckedServerFunction] {
        &self.server_functions
    }

    /// Returns submitted checked CLIENT functions in source order.
    pub fn client_functions(&self) -> &[CheckedClientFunction] {
        &self.client_functions
    }
    /// Returns the checked function with the given identity.
    pub fn function(&self, id: CheckedFunctionId) -> Option<&CheckedClientFunction> {
        self.client_functions
            .iter()
            .find(|function| function.id == id)
    }

    pub(crate) fn field_renames(&self) -> &[CheckedFieldRename] {
        &self.field_renames
    }
}

/// A checked CLIENT local binding kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientLocalKind {
    /// A value evaluated by the local runtime.
    Value,
    /// A resource handle that is evaluated when an AWAIT reads it.
    Resource(ResourceKind),
}

/// One checked CLIENT local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedClientLocal {
    pub(super) ordinal: u32,
    pub(super) name: String,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) standard_value_type: Option<TypeId>,
    pub(super) kind: CheckedClientLocalKind,
    pub(super) location: SourceLocation,
}
impl CheckedClientLocal {
    pub(crate) const fn location(&self) -> &SourceLocation {
        &self.location
    }
}

impl CheckedClientLocal {
    pub(crate) const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub(crate) const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
    }

    pub(crate) const fn kind(&self) -> CheckedClientLocalKind {
        self.kind
    }
}

/// One checked procedural CLIENT statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientStatement {
    /// Declares and initialises one local binding.
    Let {
        /// The local ordinal declared by this statement.
        local: u32,
        /// The checked initial expression.
        expression: CheckedClientExpression,
    },
    /// Replaces one existing local binding.
    Assignment {
        /// The local ordinal assigned by this statement.
        local: u32,
        /// The checked replacement expression.
        expression: CheckedClientExpression,
    },
}

impl CheckedClientStatement {
    pub(crate) const fn local(&self) -> u32 {
        match self {
            Self::Let { local, .. } | Self::Assignment { local, .. } => *local,
        }
    }

    pub(crate) fn expression(&self) -> &CheckedClientExpression {
        match self {
            Self::Let { expression, .. } | Self::Assignment { expression, .. } => expression,
        }
    }
}

/// One checked conditional branch in a programmable CLIENT body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedClientControlFlowBranch {
    /// The strict Boolean branch condition.
    pub(crate) condition: CheckedClientExpression,
    /// The statements executed when the condition is selected.
    pub(crate) statements: Vec<CheckedClientControlFlowStatement>,
    /// The exact source location of the complete branch.
    pub(crate) location: SourceLocation,
}

impl CheckedClientControlFlowBranch {
    /// Returns the checked branch condition.
    pub(crate) fn condition(&self) -> &CheckedClientExpression {
        &self.condition
    }

    /// Returns the checked branch statements in source order.
    pub(crate) fn statements(&self) -> &[CheckedClientControlFlowStatement] {
        &self.statements
    }

    /// Returns the complete source location of this branch.
    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One checked statement in a programmable CLIENT body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientControlFlowStatement {
    /// Declares and initialises one local binding.
    Let {
        /// The local ordinal declared by this statement.
        local: u32,
        /// The checked initial expression.
        expression: CheckedClientExpression,
        /// The exact source location of the statement.
        location: SourceLocation,
    },
    /// Replaces one existing local binding.
    Assignment {
        /// The local ordinal assigned by this statement.
        local: u32,
        /// The checked replacement expression.
        expression: CheckedClientExpression,
        /// The exact source location of the statement.
        location: SourceLocation,
    },
    /// Exits the current CLIENT function.
    Return {
        /// The optional checked return expression.
        expression: Option<CheckedClientExpression>,
        /// The exact source location of the statement.
        location: SourceLocation,
    },
    /// Selects the first true branch, or the optional ELSE branch.
    If {
        /// The ordered IF and ELSIF branches.
        branches: Vec<CheckedClientControlFlowBranch>,
        /// The optional ELSE body.
        else_statements: Option<Vec<CheckedClientControlFlowStatement>>,
        /// The exact source location of the statement.
        location: SourceLocation,
    },
    /// Repeats its body while the strict Boolean condition is true.
    While {
        /// The loop condition.
        condition: CheckedClientExpression,
        /// The loop body in source order.
        statements: Vec<CheckedClientControlFlowStatement>,
        /// The exact source location of the statement.
        location: SourceLocation,
    },
}

impl CheckedClientControlFlowStatement {
    /// Returns the complete source location of this statement.
    pub(crate) fn location(&self) -> &SourceLocation {
        match self {
            Self::Let { location, .. }
            | Self::Assignment { location, .. }
            | Self::Return { location, .. }
            | Self::If { location, .. }
            | Self::While { location, .. } => location,
        }
    }
}

/// A checked CLIENT function body.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientFunctionBody {
    /// A Boolean literal returned by the function.
    BooleanLiteral {
        /// The resolved Boolean value.
        value: bool,
        /// The exact source location of the literal.
        location: SourceLocation,
    },
    /// A closed CLIENT expression returned by the function (ADR 0068).
    Expression {
        /// The checked expression returned by the function.
        expression: CheckedClientExpression,
    },
    /// A procedural CLIENT body with ordered local declarations and statements.
    Procedural {
        /// The local bindings in declaration order.
        locals: Vec<CheckedClientLocal>,
        /// The procedural statements in source order.
        statements: Vec<CheckedClientStatement>,
        /// The final checked return expression.
        return_expression: CheckedClientExpression,
    },
    /// A version-10 programmable CLIENT body with explicit control-flow
    /// statements and ordered local declarations.
    ControlFlow {
        /// The local bindings in deterministic declaration order.
        locals: Vec<CheckedClientLocal>,
        /// The root statements in source order.
        statements: Vec<CheckedClientControlFlowStatement>,
    },
    /// A closed CLIENT state block with ordered slot metadata and one return expression (ADR 0069).
    StateBlock {
        /// The checked state slots in declaration order.
        states: Vec<CheckedClientStateSlot>,
        /// The checked return expression.
        return_expression: CheckedClientExpression,
    },
    /// An external function body declared only by its runtime contract.
    ExternalContract {
        /// The exact contract identity string.
        identity: String,
        /// The exact source location of the contract literal.
        location: SourceLocation,
    },
    /// A hostile test-only body outside the accepted CLIENT subset.
    #[cfg(test)]
    Unsupported,
}

impl CheckedClientFunctionBody {
    /// Returns the closed Boolean body data.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "preparation reads the checked CLIENT body")
    )]
    pub(crate) fn as_boolean_literal(&self) -> Option<(bool, &SourceLocation)> {
        match self {
            Self::BooleanLiteral { value, location } => Some((*value, location)),
            Self::Expression { .. }
            | Self::Procedural { .. }
            | Self::ControlFlow { .. }
            | Self::StateBlock { .. }
            | Self::ExternalContract { .. } => None,
            #[cfg(test)]
            Self::Unsupported => None,
        }
    }
}

/// One checked CLIENT expression in the ADR 0068 closed surface.
///
/// The checked tree mirrors the parsed surface with every name resolved to a
/// stable identity: the callee is a [`CheckedFunctionId`], parameters read by
/// [`CheckedParameterId`], and field-path steps by [`CheckedFieldId`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientExpression {
    /// A call to one checked CLIENT function with bound arguments.
    Call {
        /// The called function identity.
        function: CheckedFunctionId,
        /// The bound arguments in call order: parameter then value.
        arguments: Vec<(CheckedParameterId, CheckedClientExpression)>,
        /// The source location of the complete call.
        location: SourceLocation,
    },
    /// A suspension over one checked CLIENT resource expression (ADR 0077).
    Await {
        /// The resource expression evaluated by the local runtime.
        expression: Box<CheckedClientExpression>,
        /// The source location of the complete await expression.
        location: SourceLocation,
    },
    /// A checked CLIENT-to-SERVER resource operation value (ADR 0077).
    Resource {
        /// The resolved resource operation metadata and bound arguments.
        operation: CheckedResourceOperation,
    },
    /// A checked CLIENT action operation value.
    Action {
        /// The resolved action operation metadata and bound arguments.
        operation: CheckedActionOperation,
    },
    /// A sealed `sys.inspect` operation over an immutable inspection carrier.
    Inspect {
        /// The checked sealed operation and its nested target expression.
        operation: CheckedInspectOperation,
    },
    /// A generic read-only view of the enclosing function's source metadata.
    SourceIntrospection {
        /// The source location of the intrinsic call.
        location: SourceLocation,
    },
    /// Reads one bounded line from the active client session.
    Input {
        /// The source location of the input expression.
        location: SourceLocation,
    },
    /// Evaluates one bounded CLI command through the active session.
    Evaluate {
        /// The command expression.
        expression: Box<CheckedClientExpression>,
        /// The source location of the evaluation expression.
        location: SourceLocation,
    },
    /// A text literal value.
    String {
        /// The unescaped text value.
        value: String,
        /// The exact source location of the literal.
        location: SourceLocation,
    },
    /// An integer literal value.
    Integer {
        /// The parsed integer value.
        value: i64,
        /// The exact source location of the literal.
        location: SourceLocation,
    },
    /// A Boolean literal value.
    Boolean {
        /// The Boolean value.
        value: bool,
        /// The exact source location of the literal.
        location: SourceLocation,
    },
    /// A read of one declared parameter.
    ParameterRead {
        /// The read parameter identity.
        parameter: CheckedParameterId,
        /// The source location of the read.
        location: SourceLocation,
    },
    /// A read of one checked procedural local binding.
    LocalRead {
        /// The local declaration ordinal.
        local: u32,
        /// The source location of the read.
        location: SourceLocation,
    },
    /// A path from one parameter through object fields.
    FieldPath {
        /// The parameter at the start of the path.
        root: CheckedParameterId,
        /// The fields selected in source order.
        fields: Vec<CheckedFieldId>,
        /// The source location of the complete path.
        location: SourceLocation,
    },
    /// A left-associative text concatenation.
    Concat {
        /// The left operand.
        left: Box<CheckedClientExpression>,
        /// The right operand.
        right: Box<CheckedClientExpression>,
        /// The source location of the complete expression.
        location: SourceLocation,
    },
    /// A checked unary arithmetic or Boolean expression.
    Unary {
        /// The checked operator.
        operator: ControlFlowUnaryOperator,
        /// The checked operand.
        expression: Box<CheckedClientExpression>,
        /// The source location of the complete expression.
        location: SourceLocation,
    },
    /// A checked arithmetic, comparison, or Boolean expression.
    Binary {
        /// The checked operator.
        operator: ControlFlowBinaryOperator,
        /// The checked left operand.
        left: Box<CheckedClientExpression>,
        /// The checked right operand.
        right: Box<CheckedClientExpression>,
        /// The source location of the complete expression.
        location: SourceLocation,
    },
    /// A parenthesized expression retained for source diagnostics while
    /// remaining semantically transparent.
    Parenthesized {
        /// The checked expression inside the parentheses.
        expression: Box<CheckedClientExpression>,
        /// The source location of the complete parenthesized expression.
        location: SourceLocation,
    },
}

/// One fixed materialized projection exposed by the sealed Inspector surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckedInspectProjection {
    InvocationNodes,
    Calls,
    Resources,
    StateCells,
    UiNodes,
    PresentationCandidates,
    RuntimeBindings,
    SecurityDecisions,
}

/// A checked sealed `sys.inspect` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedInspectOperation {
    /// Captures one target invocation using structural defaults.
    Snapshot {
        target: Box<CheckedClientExpression>,
        /// The checked `sys.inspect.snapshot_options` argument. None is the
        /// explicit structural-only default accepted by the one-argument
        /// snapshot spelling.
        options: Option<Box<CheckedClientExpression>>,
        location: SourceLocation,
    },
    /// Materializes one fixed projection from an existing snapshot.
    Projection {
        projection: CheckedInspectProjection,
        snapshot: Box<CheckedClientExpression>,
        location: SourceLocation,
    },
}

impl CheckedInspectOperation {
    /// Returns the source location of the complete sealed operation.
    pub(crate) fn location(&self) -> &SourceLocation {
        match self {
            Self::Snapshot { location, .. } | Self::Projection { location, .. } => location,
        }
    }
}

/// One checked CLIENT-to-SERVER resource operation.
///
/// The operation retains only checked identities. In particular, the target
/// is a SERVER function, arguments are bound to that function's stable
/// parameters, and the result type is derived from the target declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedResourceOperation {
    pub(super) kind: ResourceKind,
    pub(super) target: CheckedFunctionId,
    pub(super) call_site: CallSiteId,
    pub(super) arguments: Vec<(CheckedParameterId, CheckedClientExpression)>,
    pub(super) result_type: SemanticType<CheckedTypeId>,
    pub(super) standard_result_type: Option<TypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedResourceOperation {
    /// Returns the scalar/stream resource kind.
    pub(crate) const fn kind(&self) -> ResourceKind {
        self.kind
    }
    /// Returns the checked SERVER target function identity.
    pub(crate) const fn target(&self) -> CheckedFunctionId {
        self.target
    }
    /// Returns the deterministic call-site identity.
    pub(crate) const fn call_site(&self) -> CallSiteId {
        self.call_site
    }
    /// Returns canonical parameter-to-expression argument pairs.
    pub(crate) fn arguments(&self) -> &[(CheckedParameterId, CheckedClientExpression)] {
        &self.arguments
    }
    /// Returns the checked target-derived result type.
    pub(crate) const fn result_type(&self) -> SemanticType<CheckedTypeId> {
        self.result_type
    }
    /// Returns the durable standard value-type identity when one exists.
    pub(crate) const fn standard_result_type(&self) -> Option<TypeId> {
        self.standard_result_type
    }
    /// Returns the source location of the constructor expression.
    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One checked CLIENT action operation (ADR 0079).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedActionOperation {
    pub(super) target_domain: ActionTargetDomain,
    pub(super) target: CheckedFunctionId,
    pub(super) call_site: CallSiteId,
    pub(super) arguments: Vec<(CheckedParameterId, CheckedClientExpression)>,
    pub(super) result_type: SemanticType<CheckedTypeId>,
    pub(super) standard_result_type: Option<TypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedActionOperation {
    pub(crate) const fn target_domain(&self) -> ActionTargetDomain {
        self.target_domain
    }
    pub(crate) const fn target(&self) -> CheckedFunctionId {
        self.target
    }
    pub(crate) const fn call_site(&self) -> CallSiteId {
        self.call_site
    }
    pub(crate) fn arguments(&self) -> &[(CheckedParameterId, CheckedClientExpression)] {
        &self.arguments
    }
    pub(crate) const fn result_type(&self) -> SemanticType<CheckedTypeId> {
        self.result_type
    }
    pub(crate) const fn standard_result_type(&self) -> Option<TypeId> {
        self.standard_result_type
    }
    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The checked argument source of one CLIENT capability requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedClientCapabilityArgument {
    /// A literal scope value written in the declaration.
    Text(String),
    /// A reference to a declared function parameter.
    Parameter(String),
}

/// One checked CLIENT capability requirement (ADR 0060).
///
/// The checked name is the closed qualified vocabulary name (for example
/// `std.fs.read`); the argument source is the declaration's literal scope or
/// parameter reference. The invocation-time gate resolves parameter
/// references to invocation values before asking the local grant set.
///
/// The checked requirement is copied into the version-five CLIENT capability
/// envelope during preparation. Legacy direct evaluator calls may also supply
/// caller-owned declarations when they evaluate version-one to version-four
/// plans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedClientCapability {
    name: String,
    argument: CheckedClientCapabilityArgument,
}

impl CheckedClientCapability {
    /// Creates a checked capability requirement from its closed name and argument source.
    pub fn new(name: impl Into<String>, argument: CheckedClientCapabilityArgument) -> Self {
        Self {
            name: name.into(),
            argument,
        }
    }

    /// Returns the closed qualified capability name (no arguments).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declared argument source.
    pub fn argument(&self) -> &CheckedClientCapabilityArgument {
        &self.argument
    }
}

/// The scope of one checked CLIENT state slot (work ADR 0069).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckedStateScope {
    /// State private to one mounted function instance.
    Local,
    /// State retained for the client invocation session.
    Session,
    /// State associated with the authenticated principal.
    User,
}

/// The checked initial value of one CLIENT state slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedStateDefault {
    /// No DEFAULT clause was written.
    Unset,
    /// The slot starts with an explicit null value.
    Null,
    /// The slot starts with a checked CLIENT expression value.
    Expression(CheckedClientExpression),
}

/// A checked state-slot identity. Provisional values are placeholders only;
/// preparation must derive their durable identity from the eventual function id.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum CheckedStateSlotId {
    /// A slot owned by a function with a durable identity.
    Existing(StateSlotId),
    /// A slot owned by a newly declared function.
    Provisional(StateSlotId),
}

/// One checked CLIENT state slot with source-free semantic metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedClientStateSlot {
    pub(super) id: CheckedStateSlotId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) standard_value_type: Option<TypeId>,
    pub(super) scope: CheckedStateScope,
    pub(super) default: CheckedStateDefault,
    pub(super) location: SourceLocation,
}

impl CheckedClientStateSlot {
    pub(crate) const fn id(&self) -> CheckedStateSlotId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    /// Returns the standard-library value identity when this slot uses one.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
    }

    pub(crate) const fn scope(&self) -> CheckedStateScope {
        self.scope
    }

    pub(crate) fn default(&self) -> &CheckedStateDefault {
        &self.default
    }
}

/// The declared result shape of a checked CLIENT function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientReturnShape {
    /// One value is returned.
    Single,
    /// Zero or more values of the checked return element type are returned.
    Stream,
}

/// A checked CLIENT function with a closed Boolean constant body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedClientFunction {
    pub(super) id: CheckedFunctionId,
    pub(super) name: QualifiedSemanticName,
    pub(super) domain: FunctionDomain,
    pub(super) parameters: Vec<CheckedServerFunctionParameter>,
    pub(super) return_type: SemanticType<CheckedTypeId>,
    pub(super) return_shape: CheckedClientReturnShape,
    pub(super) security: orna_core::catalogue::FunctionSecurity,
    pub(super) transaction: Option<orna_core::catalogue::FunctionTransaction>,
    pub(super) volatility: orna_core::catalogue::FunctionVolatility,
    pub(super) location: SourceLocation,
    pub(super) body: CheckedClientFunctionBody,
    pub(super) references: Vec<CheckedDefinitionReference>,
    pub(super) capabilities: Vec<CheckedClientCapability>,
}

/// A stable public summary of a checked CLIENT function body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CheckedClientBodyKind {
    /// A direct expression body.
    Expression,
    /// A procedural body with locals and assignments.
    Procedural,
    /// A programmable body with branches, loops, and returns.
    ControlFlow,
    /// A state-backed expression body.
    State,
    /// A host-provided runtime contract.
    ExternalContract,
    /// A legacy Boolean body.
    BooleanLiteral,
}

impl CheckedClientFunction {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.id
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the runtime domain of the checked function.
    pub const fn domain(&self) -> FunctionDomain {
        self.domain
    }

    /// Returns checked parameters in declaration order.
    pub fn parameters(&self) -> &[CheckedServerFunctionParameter] {
        &self.parameters
    }

    /// Returns the checked return element type.
    pub const fn return_type(&self) -> SemanticType<CheckedTypeId> {
        self.return_type
    }

    /// Returns the checked CLIENT result shape.
    pub(crate) const fn return_shape(&self) -> CheckedClientReturnShape {
        self.return_shape
    }

    /// Returns the function security context mode.
    pub const fn security(&self) -> orna_core::catalogue::FunctionSecurity {
        self.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<orna_core::catalogue::FunctionTransaction> {
        self.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> orna_core::catalogue::FunctionVolatility {
        self.volatility
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns the Boolean body value and its exact source location.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "preparation reads the checked CLIENT body")
    )]
    pub(crate) fn boolean_body(&self) -> Option<(bool, &SourceLocation)> {
        self.body.as_boolean_literal()
    }
    /// Returns the complete checked CLIENT body kind without exposing resolver internals.
    pub fn body_kind(&self) -> CheckedClientBodyKind {
        match self.body {
            CheckedClientFunctionBody::BooleanLiteral { .. } => {
                CheckedClientBodyKind::BooleanLiteral
            }
            CheckedClientFunctionBody::Expression { .. } => CheckedClientBodyKind::Expression,
            CheckedClientFunctionBody::Procedural { .. } => CheckedClientBodyKind::Procedural,
            CheckedClientFunctionBody::ControlFlow { .. } => CheckedClientBodyKind::ControlFlow,
            CheckedClientFunctionBody::StateBlock { .. } => CheckedClientBodyKind::State,
            CheckedClientFunctionBody::ExternalContract { .. } => {
                CheckedClientBodyKind::ExternalContract
            }
            #[cfg(test)]
            CheckedClientFunctionBody::Unsupported => CheckedClientBodyKind::ExternalContract,
        }
    }

    /// Returns the complete checked CLIENT body for compiler-owned introspection.
    pub(crate) fn body(&self) -> &CheckedClientFunctionBody {
        &self.body
    }

    /// Returns checked definition references in source-resolution order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.references
    }

    /// Returns the checked capability requirements in declaration order.
    ///
    /// Expression and external-contract bodies retain their declared
    /// requirements; legacy Boolean bodies reject capability clauses.
    pub fn capabilities(&self) -> &[CheckedClientCapability] {
        &self.capabilities
    }

    /// Returns the checked CLIENT calls in source traversal order.
    ///
    /// Each target is a checked identity resolved from the Orna source call.
    /// An unresolved call cannot enter a successful checked bundle.
    pub fn called_functions(&self) -> Vec<CheckedFunctionId> {
        self.references
            .iter()
            .filter(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall)
            .filter_map(|reference| match reference.target() {
                CheckedDefinitionReferenceTarget::Function(id) => Some(id),
                _ => None,
            })
            .collect()
    }
}

/// One checked SERVER function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunctionParameter {
    pub(super) id: CheckedParameterId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedServerFunctionParameter {
    /// Returns the identity of the parameter.
    pub const fn id(&self) -> CheckedParameterId {
        self.id
    }

    /// Returns the resolved parameter name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    /// Returns the full parameter declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One checked `ROWS` return column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunctionReturnColumn {
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedServerFunctionReturnColumn {
    /// Returns the resolved return-column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    /// Returns the full return-column declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The checked return shape of one SERVER function.
///
/// Scalar `Single` and `STREAM<T>` carry one flat element type without
/// manufacturing a synthetic return column. Legacy `ROWS (...)` declarations
/// retain their existing ordered column representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedServerFunctionReturn {
    /// Exactly one value of the resolved scalar type.
    Single {
        /// The resolved scalar return type.
        semantic_type: SemanticType<CheckedTypeId>,
        /// The standard value identity when the scalar uses one.
        standard_value_type: Option<TypeId>,
        /// The source location of the complete scalar declaration.
        location: SourceLocation,
    },
    /// Zero or more named return columns.
    Rows(Vec<CheckedServerFunctionReturnColumn>),
    /// Zero or more values of one resolved element type.
    Stream {
        /// The resolved stream element type.
        semantic_type: SemanticType<CheckedTypeId>,
        /// The standard value identity when the element uses one.
        standard_value_type: Option<TypeId>,
        /// The source location of the complete `STREAM<T>` declaration.
        location: SourceLocation,
    },
}

/// A checked definition target referenced by one declaration or query.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CheckedDefinitionReferenceTarget {
    /// A checked object type.
    ObjectType(CheckedTypeId),
    /// A checked named value type.
    ValueType(CheckedTypeId),
    /// A checked field on an object type.
    Field {
        /// The owning checked object or record value type.
        owner: CheckedTypeId,
        /// The checked field identity.
        field: CheckedFieldId,
    },
    /// A checked SERVER function.
    Function(CheckedFunctionId),
    /// A checked parameter on a SERVER function.
    Parameter {
        /// The owning checked SERVER function.
        owner: CheckedFunctionId,
        /// The checked parameter identity.
        parameter: CheckedParameterId,
    },
    /// A checked expression.
    Expression(CheckedExpressionId),
}

/// One ordered checked definition reference with its exact source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefinitionReference {
    pub(super) target: CheckedDefinitionReferenceTarget,
    pub(super) kind: DefinitionReferenceKind,
    pub(super) location: SourceLocation,
}

impl CheckedDefinitionReference {
    /// Returns the checked target definition.
    pub const fn target(&self) -> CheckedDefinitionReferenceTarget {
        self.target
    }

    /// Returns the resolved reference category.
    pub const fn kind(&self) -> DefinitionReferenceKind {
        self.kind
    }

    /// Returns the exact source location of this reference.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked SERVER function body with its source-free execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedServerFunctionBody {
    /// A checked relational query body.
    Query(RelationalQueryIr<CheckedTypeId, CheckedFieldId>),
    /// A checked parameter-free `SELECT DISTINCT` query body.
    DistinctQuery(DistinctQueryIr<CheckedTypeId, CheckedFieldId>),
    /// A checked SERVER query with one fixed identity selector.
    IdentitySelectedQuery(
        IdentitySelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    ),
    /// A checked SERVER query selected by one unique Text field.
    UniqueTextSelectedQuery(
        UniqueTextSelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    ),
    /// A checked single-object mutation body.
    Mutation(
        crate::mutation::MutationPlanIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    ),
    /// A checked single-object DELETE body.
    Delete(crate::mutation::DeletePlanIr<CheckedTypeId, CheckedFunctionId, CheckedParameterId>),
}

/// A checked SERVER function with an Orna-owned execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunction {
    pub(super) id: CheckedFunctionId,
    pub(super) name: QualifiedSemanticName,
    pub(super) parameters: Vec<CheckedServerFunctionParameter>,
    pub(super) return_type: CheckedServerFunctionReturn,
    pub(super) security: orna_core::catalogue::FunctionSecurity,
    pub(super) transaction: Option<orna_core::catalogue::FunctionTransaction>,
    pub(super) volatility: orna_core::catalogue::FunctionVolatility,
    pub(super) location: SourceLocation,
    pub(super) body: CheckedServerFunctionBody,
    pub(super) references: Vec<CheckedDefinitionReference>,
}

impl CheckedServerFunction {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.id
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns checked parameters in declaration order.
    pub fn parameters(&self) -> &[CheckedServerFunctionParameter] {
        &self.parameters
    }

    /// Returns checked `ROWS` return columns in declaration order.
    pub fn return_columns(&self) -> &[CheckedServerFunctionReturnColumn] {
        match &self.return_type {
            CheckedServerFunctionReturn::Single { .. }
            | CheckedServerFunctionReturn::Stream { .. } => &[],
            CheckedServerFunctionReturn::Rows(columns) => columns,
        }
    }

    /// Returns the checked SERVER return shape.
    pub(crate) fn return_type(&self) -> &CheckedServerFunctionReturn {
        &self.return_type
    }

    /// Returns the function security context mode.
    pub const fn security(&self) -> orna_core::catalogue::FunctionSecurity {
        self.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<orna_core::catalogue::FunctionTransaction> {
        self.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> orna_core::catalogue::FunctionVolatility {
        self.volatility
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns checked definition references in source-resolution order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.references
    }

    /// Returns the checked duplicate-preserving relational query when present.
    pub(crate) fn query_plan(&self) -> Option<&RelationalQueryIr<CheckedTypeId, CheckedFieldId>> {
        match &self.body {
            CheckedServerFunctionBody::Query(plan) => Some(plan),
            CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::UniqueTextSelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked DISTINCT query plan when the function has one.
    pub(crate) fn distinct_query_plan(
        &self,
    ) -> Option<&DistinctQueryIr<CheckedTypeId, CheckedFieldId>> {
        match &self.body {
            CheckedServerFunctionBody::DistinctQuery(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::UniqueTextSelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked identity-selected query plan when the function has one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn identity_selected_query_plan(
        &self,
    ) -> Option<
        &IdentitySelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    > {
        match &self.body {
            CheckedServerFunctionBody::IdentitySelectedQuery(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::UniqueTextSelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked unique-Text-selected query plan when the function has one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn unique_text_selected_query_plan(
        &self,
    ) -> Option<
        &UniqueTextSelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    > {
        match &self.body {
            CheckedServerFunctionBody::UniqueTextSelectedQuery(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked INSERT or UPDATE plan when the function has that body.
    pub(crate) fn mutation_plan(
        &self,
    ) -> Option<
        &crate::mutation::MutationPlanIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    > {
        match &self.body {
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::UniqueTextSelectedQuery(_)
            | CheckedServerFunctionBody::Delete(_) => None,
            CheckedServerFunctionBody::Mutation(plan) => Some(plan),
        }
    }

    /// Returns the checked DELETE plan when the function has a DELETE body.
    pub(crate) fn delete_plan(
        &self,
    ) -> Option<&crate::mutation::DeletePlanIr<CheckedTypeId, CheckedFunctionId, CheckedParameterId>>
    {
        match &self.body {
            CheckedServerFunctionBody::Delete(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::UniqueTextSelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_) => None,
        }
    }
}

/// A checked logical schema declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedSchema {
    pub(super) id: CheckedSchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) location: SourceLocation,
}

impl CheckedSchema {
    /// Returns the identity of the schema.
    pub const fn id(&self) -> CheckedSchemaId {
        self.id
    }

    /// Returns the resolved logical schema name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One schema confirmed against a verified standard-library snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardSchema {
    pub(super) id: SchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) origin: SourceOrigin,
}

impl CheckedStandardSchema {
    /// Returns the durable schema identity.
    pub const fn id(&self) -> SchemaId {
        self.id
    }

    /// Returns the resolved schema name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the complete schema declaration origin.
    pub const fn origin(&self) -> SourceOrigin {
        self.origin
    }
}

/// One primitive or opaque value type confirmed against a verified standard snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardValueType {
    pub(super) id: TypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) kind: ValueTypeKind,
    pub(super) mutability: ValueTypeMutability,
    pub(super) persistence: ValueTypePersistence,
    pub(super) representation_contract: String,
    pub(super) origin: SourceOrigin,
}

impl CheckedStandardValueType {
    /// Returns the durable value-type identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns the resolved primary type name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the checked value-type category.
    pub const fn kind(&self) -> ValueTypeKind {
        self.kind
    }

    /// Returns the checked mutability contract.
    pub const fn mutability(&self) -> ValueTypeMutability {
        self.mutability
    }

    /// Returns the checked persistence contract.
    pub const fn persistence(&self) -> ValueTypePersistence {
        self.persistence
    }

    /// Returns the checked kernel representation contract.
    pub fn representation_contract(&self) -> &str {
        &self.representation_contract
    }

    /// Returns the complete value-type declaration origin.
    pub const fn origin(&self) -> SourceOrigin {
        self.origin
    }
}

/// One direct type binding confirmed against a verified standard-library snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardTypeBinding {
    pub(super) id: TypeBindingId,
    pub(super) kind: TypeBindingKind,
    pub(super) name: TypeLookupName,
    pub(super) target: TypeId,
    pub(super) origin: SourceOrigin,
}

impl CheckedStandardTypeBinding {
    /// Returns the durable type-binding identity.
    pub const fn id(&self) -> TypeBindingId {
        self.id
    }

    /// Returns the binding namespace.
    pub const fn kind(&self) -> TypeBindingKind {
        self.kind
    }

    /// Returns the checked lookup name.
    pub fn name(&self) -> &TypeLookupName {
        &self.name
    }

    /// Returns the direct target type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the complete binding declaration origin.
    pub const fn origin(&self) -> SourceOrigin {
        self.origin
    }
}

/// One standard-library source and catalogue agreement result.
#[derive(Clone, Debug)]
pub struct CheckedStandardLibrary {
    pub(super) verified_snapshot: VerifiedStandardLibrarySnapshot,
    pub(super) schemas: Vec<CheckedStandardSchema>,
    pub(super) value_types: Vec<CheckedStandardValueType>,
    pub(super) type_bindings: Vec<CheckedStandardTypeBinding>,
    pub(super) checked_executables: Vec<CheckedStandardExecutable>,
}

impl CheckedStandardLibrary {
    /// Returns the verified snapshot that this result reconciles.
    pub fn verified_snapshot(&self) -> &VerifiedStandardLibrarySnapshot {
        &self.verified_snapshot
    }

    /// Returns checked schemas in source order.
    pub fn schemas(&self) -> &[CheckedStandardSchema] {
        &self.schemas
    }

    /// Returns checked primitive and opaque value types in source order.
    pub fn value_types(&self) -> &[CheckedStandardValueType] {
        &self.value_types
    }

    /// Returns checked direct type bindings in source order.
    pub fn type_bindings(&self) -> &[CheckedStandardTypeBinding] {
        &self.type_bindings
    }

    /// Returns the checked standard executable facts in catalogue order.
    pub fn checked_executables(&self) -> &[CheckedStandardExecutable] {
        &self.checked_executables
    }

    /// Returns the checked V2 `std.invoke.echo` executable for compatibility.
    ///
    /// V1 snapshots carry no executable. V2 and later snapshots retain this
    /// historical accessor even when the standard snapshot contains additional
    /// executable functions.
    pub fn checked_executable(&self) -> Option<&CheckedStandardExecutable> {
        self.standard_executable(STD_INVOKE_ECHO_FUNCTION_ID)
    }

    /// Finds checked evidence for one standard executable function.
    pub fn standard_executable(
        &self,
        function_id: FunctionId,
    ) -> Option<&CheckedStandardExecutable> {
        self.checked_executables
            .iter()
            .find(|executable| executable.function_id() == function_id)
    }
}

/// The fixed ADR 0058 `orna.std/3` standard-library revision identity: `...03`.
pub const STANDARD_LIBRARY_V3_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);
/// The fixed ADR 0062 `orna.std/4` standard-library revision identity: `...04`.
pub const STANDARD_LIBRARY_V4_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04]);
/// The fixed ADR 0075 `orna.std/5` standard-library revision identity: `...05`.
pub const STANDARD_LIBRARY_V5_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);
/// The fixed `orna.std/11` standard-library revision identity.
pub const STANDARD_LIBRARY_V11_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0b]);
/// The fixed ADR 0079 `orna.std/6` standard-library revision identity: `...06`.
pub const STANDARD_LIBRARY_V6_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);
/// The fixed ADR 0019 `orna.std/7` standard-library revision identity.
pub const STANDARD_LIBRARY_V7_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x07]);
/// The fixed Work ADR 0087 `orna.std/8` standard-library revision identity.
pub const STANDARD_LIBRARY_V8_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08]);
/// The fixed Work ADR 0088 `orna.std/9` standard-library revision identity.
pub const STANDARD_LIBRARY_V9_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09]);
/// The fixed CLI-session `orna.std/10` standard-library revision identity: `...0A`.
pub const STANDARD_LIBRARY_V10_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0A]);
/// The fixed CLI-session `std/cli.orna` source-unit identity: `...0B`.
pub const STD_CLI_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0B]);
/// The fixed CLI-session `std.cli` schema identity: `...0A`.
pub const STD_CLI_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0A]);
/// The fixed source-authored `std.cli.repl` function identity: `...1C`.
pub const STD_CLI_REPL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1C]);
/// The fixed source-authored `std.cli.repl` function-revision identity: `...1C`.
pub const STD_CLI_REPL_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1C]);
/// The source-authored `std.cli.repl` function revision number.
pub const STD_CLI_REPL_REVISION_NUMBER: u64 = 1;
/// The fixed Work ADR 0088 `std/ui_constructors.orna` source-unit identity: `...0A`.
pub const STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0A]);
/// The fixed Work ADR 0087 `std/data.orna` source-unit identity: `...09`.
pub const STD_DATA_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09]);
/// The deterministic `std.rows` qualified type-binding identity (derived by
/// `TypeBinding::qualified` from the normalised unquoted source spelling
/// `std.Rows`).
pub const STD_DATA_ROWS_TYPE_BINDING_ID: TypeBindingId = TypeBindingId::from_bytes([
    0x04, 0xe2, 0x43, 0x98, 0x0b, 0x43, 0xc2, 0xaa, 0xa0, 0x0e, 0x0e, 0x79, 0xc4, 0xce, 0xea, 0x10,
]);
/// The fixed ADR 0019 `std/window.orna` source-unit identity: `...08`.
pub const STD_WINDOW_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08]);
/// The fixed ADR 0019 `std.ui.window` function identity: `...14`.
pub const STD_UI_WINDOW_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);
/// The fixed ADR 0019 `std.ui.window.title` parameter identity: `...14`.
pub const STD_UI_WINDOW_TITLE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);
/// The fixed ADR 0019 `std.ui.window.content` parameter identity: `...15`.
pub const STD_UI_WINDOW_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);
/// The fixed ADR 0019 `std.ui.window` function-revision identity: `...14`.
pub const STD_UI_WINDOW_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);
/// The `std.ui.window` function revision number.
pub const STD_UI_WINDOW_REVISION_NUMBER: u64 = 1;
/// The fixed ADR 0019 runtime contract identity.
pub const STD_UI_WINDOW_RUNTIME_CONTRACT: &str = "std.ui.window@1";
/// The fixed Work ADR 0088 `std.ui.text` function identity: `...15`.
pub const STD_UI_TEXT_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);
/// The fixed Work ADR 0088 `std.ui.text.text` parameter identity: `...16`.
pub const STD_UI_TEXT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
/// The fixed Work ADR 0088 `std.ui.text` function-revision identity: `...15`.
pub const STD_UI_TEXT_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);
/// The fixed Work ADR 0088 `std.ui.text@1` external contract.
pub const STD_UI_TEXT_RUNTIME_CONTRACT: &str = "std.ui.text@1";

/// The fixed Work ADR 0088 `std.ui.button` function identity: `...16`.
pub const STD_UI_BUTTON_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
/// The fixed Work ADR 0088 `std.ui.button.label` parameter identity: `...17`.
pub const STD_UI_BUTTON_LABEL_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);
/// The fixed Work ADR 0088 `std.ui.button.enabled` parameter identity: `...18`.
pub const STD_UI_BUTTON_ENABLED_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);
/// The fixed Work ADR 0088 `std.ui.button` function-revision identity: `...16`.
pub const STD_UI_BUTTON_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
/// The fixed Work ADR 0088 `std.ui.button@1` external contract.
pub const STD_UI_BUTTON_RUNTIME_CONTRACT: &str = "std.ui.button@1";

/// The fixed Work ADR 0088 `std.ui.panel` function identity: `...17`.
pub const STD_UI_PANEL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);
/// The fixed Work ADR 0088 `std.ui.panel.content` parameter identity: `...19`.
pub const STD_UI_PANEL_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);
/// The fixed Work ADR 0088 `std.ui.panel` function-revision identity: `...17`.
pub const STD_UI_PANEL_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);
/// The fixed Work ADR 0088 `std.ui.panel@1` external contract.
pub const STD_UI_PANEL_RUNTIME_CONTRACT: &str = "std.ui.panel@1";

/// The fixed Work ADR 0088 `std.ui.row` function identity: `...18`.
pub const STD_UI_ROW_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);
/// The fixed Work ADR 0088 `std.ui.row.content` parameter identity: `...1A`.
pub const STD_UI_ROW_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);
/// The fixed Work ADR 0088 `std.ui.row` function-revision identity: `...18`.
pub const STD_UI_ROW_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);
/// The fixed Work ADR 0088 `std.ui.row@1` external contract.
pub const STD_UI_ROW_RUNTIME_CONTRACT: &str = "std.ui.row@1";

/// The fixed Work ADR 0088 `std.ui.column` function identity: `...19`.
pub const STD_UI_COLUMN_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);
/// The fixed Work ADR 0088 `std.ui.column.content` parameter identity: `...1B`.
pub const STD_UI_COLUMN_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);
/// The fixed Work ADR 0088 `std.ui.column` function-revision identity: `...19`.
pub const STD_UI_COLUMN_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);
/// The fixed Work ADR 0088 `std.ui.column@1` external contract.
pub const STD_UI_COLUMN_RUNTIME_CONTRACT: &str = "std.ui.column@1";

/// The fixed Work ADR 0088 `std.ui.text_input` function identity: `...1A`.
pub const STD_UI_TEXT_INPUT_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);
/// The fixed Work ADR 0088 `std.ui.text_input.text` parameter identity: `...1C`.
pub const STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1C]);
/// The fixed Work ADR 0088 `std.ui.text_input.placeholder` parameter identity: `...1D`.
pub const STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1D]);
/// The fixed Work ADR 0088 `std.ui.text_input.enabled` parameter identity: `...1E`.
pub const STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1E]);
/// The fixed Work ADR 0088 `std.ui.text_input` function-revision identity: `...1A`.
pub const STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);
/// The fixed Work ADR 0088 `std.ui.text_input@1` external contract.
pub const STD_UI_TEXT_INPUT_RUNTIME_CONTRACT: &str = "std.ui.text_input@1";

/// The fixed Work ADR 0088 `std.ui.tabs` function identity: `...1B`.
pub const STD_UI_TABS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);
/// The fixed Work ADR 0088 `std.ui.tabs.content` parameter identity: `...1F`.
pub const STD_UI_TABS_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1F]);
/// The fixed Work ADR 0088 `std.ui.tabs` function-revision identity: `...1B`.
pub const STD_UI_TABS_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);
/// The fixed Work ADR 0088 `std.ui.tabs@1` external contract.
pub const STD_UI_TABS_RUNTIME_CONTRACT: &str = "std.ui.tabs@1";
/// The fixed initial TEXT value-type identity used by `std.ui.window`.
pub const STD_CHARACTER_LARGE_OBJECT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);
/// The fixed ADR 0062 `std/ui.orna` source-unit identity: `...05`.
pub const STD_UI_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);
/// The fixed ADR 0079 `std/action.orna` source-unit identity: `...07`.
pub const STD_ACTION_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x07]);
/// The fixed ADR 0079 `std.action` schema identity: `...09`.
pub const STD_ACTION_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09]);
/// The fixed ADR 0075 `std/json.orna` source-unit identity: `...06`.
pub const STD_JSON_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);
/// The fixed ADR 0062 `std.ui` schema identity: 15 zero bytes then `0x08`.
pub const STD_UI_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08]);
/// The fixed ADR 0075 `std.json.Value` kernel representation contract.
pub const STD_JSON_CONTRACT: &str = "orna.std.value.json@1";
/// The fixed ADR 0062 `std.ui.UI` value-type identity: `...19`.
pub const STD_UI_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13]);
/// The fixed ADR 0062 `std.ui.UI` kernel representation contract.
pub const STD_UI_CONTRACT: &str = "orna.std.value.ui@1";
/// The fixed ADR 0079 `std.action.Action` value-type identity: `reserved_id(20)`.
pub const STD_ACTION_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);
/// The fixed ADR 0079 `std.action.Action` kernel representation contract.
pub const STD_ACTION_CONTRACT: &str = "orna.std.value.action@1";
/// The fixed ADR 0055 `std.invoke` schema identity: 15 zero bytes then `0x03`.
pub const STD_INVOKE_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);
/// The fixed ADR 0055 `std.invoke.echo` function identity: 15 zero bytes then `0x10`.
pub const STD_INVOKE_ECHO_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The fixed ADR 0055 `std.invoke.echo.p_value` parameter identity: `...10`.
pub const STD_INVOKE_ECHO_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The fixed ADR 0055 `std.invoke.echo` function-revision identity: `...10`.
pub const STD_INVOKE_ECHO_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The `std.invoke.echo` revision number: version 1 (ADR 0055).
pub const STD_INVOKE_ECHO_REVISION_NUMBER: u64 = 1;
/// The fixed ADR 0055 `std/types.orna` source-unit identity: `...02`.
pub const STD_TYPES_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);
/// The fixed ADR 0055 `std/invoke.orna` source-unit identity: `...03`.
pub const STD_INVOKE_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);
/// The fixed ADR 0058 `std/output.orna` source-unit identity: `...04`.
pub const STD_OUTPUT_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04]);
/// The fixed ADR 0055 BOOLEAN value-type identity: `...01`.
pub const STD_BOOLEAN_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]);

/// The fixed ADR 0055 INTEGER value-type identity: `...02`.
pub const STD_INTEGER_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);
/// The fixed ADR 0058 `std.terminal.Document` value-type identity: `...15`.
pub const STD_TERMINAL_DOCUMENT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f]);
/// The fixed ADR 0058 `std.io.ByteStream` value-type identity: `...16`.
pub const STD_IO_BYTE_STREAM_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The fixed ADR 0057 `std.terminal` schema identity: `...04` (ADR 0058).
pub const STD_TERMINAL_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04]);
/// The fixed ADR 0057 `std.io` schema identity: `...05` (ADR 0058).
pub const STD_IO_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);
/// The fixed V5/ADR 0075 `std.json` schema identity: 15 zero bytes then `0x06`.
pub const STD_JSON_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);
/// The reserved, unregistered `std.data` schema identity: 15 zero bytes then `0x07`.
pub const STD_DATA_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x07]);
/// The `std.json.Value` value type introduced by V5/ADR 0075 has identity `...11`.
pub const STD_JSON_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The reserved, unregistered `std.data.Rows` value-type identity: `...12`.
pub const STD_DATA_ROWS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.json.encode` function identity retained by V5/ADR 0075:
/// 15 zero bytes then `0x11`.
pub const STD_JSON_ENCODE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.json.encode.p_value` parameter identity retained by
/// V5/ADR 0075: `...11`.
pub const STD_JSON_ENCODE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.json.encode` function-revision identity retained by
/// V5/ADR 0075: `...11`.
pub const STD_JSON_ENCODE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.terminal.present_table` function identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.terminal.present_table.p_rows` parameter identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.terminal.present_table` function-revision identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0067 `std.csv.encode` function identity: `...13`.
pub const STD_CSV_ENCODE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13]);

/// The checked executable facts for the one accepted standard parameter-echo
/// function (`std.invoke.echo`, ADR 0055).
///
/// The model carries the fixed function, parameter, and version-1 revision
/// identities, the complete 44-byte `orna.server-parameter-echo` artifact,
/// and the three ordered durable references (parameter type, result type,
/// body parameter read).
///
/// Step 6 (`feat(compiler): reconcile executable standard source`) wires the
/// checker into the standard source checker and consumes these facts to build
/// the `StandardExecutable` record: `FunctionRevisionRecord::new(function_id,
/// revision_id, STD_INVOKE_ECHO_REVISION_NUMBER, declaration_origin,
/// declaration_content_hash, semantic_hash, "orna.language/1", artifact)`
/// with this artifact and the exact reference sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardParameterEcho {
    pub(super) function_id: FunctionId,
    pub(super) parameter_id: ParameterId,
    pub(super) revision_id: FunctionRevisionId,
    pub(super) artifact: ExecutableArtifact,
    pub(super) references: Vec<DefinitionReference>,
}

impl CheckedStandardParameterEcho {
    /// Returns the fixed `std.invoke.echo` function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns the fixed `std.invoke.echo.p_value` parameter identity.
    pub const fn parameter_id(&self) -> ParameterId {
        self.parameter_id
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }

    /// Returns the complete server parameter-echo artifact.
    pub fn artifact(&self) -> &ExecutableArtifact {
        &self.artifact
    }

    /// Returns the ordered durable reference sequence for this executable.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }
}

/// The checked declaration facts for the JSON presenter function retained by
/// V5/ADR 0075 (`std.json.encode`).
///
/// The model carries the fixed function, parameter, and version-1 revision
/// identities. The exact-shape check rejects every variation of the closed
/// declaration before any artifact is constructed; ADR 0057 step 4
/// (`feat(artifact): encode terminal and json presenter plans`) consumes
/// these facts to build the 44-byte `orna.server-json-encode` artifact and
/// its ordered durable references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardJsonEncode {
    pub(super) function_id: FunctionId,
    pub(super) parameter_id: ParameterId,
    pub(super) revision_id: FunctionRevisionId,
}

impl CheckedStandardJsonEncode {
    /// Returns the fixed `std.json.encode` function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns the fixed `std.json.encode.p_value` parameter identity.
    pub const fn parameter_id(&self) -> ParameterId {
        self.parameter_id
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }
}
/// The checked declaration facts for the accepted ADR 0019 external
/// `std.ui.window` CLIENT function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardUiWindow {
    pub(super) function_id: FunctionId,
    pub(super) title_parameter_id: ParameterId,
    pub(super) content_parameter_id: ParameterId,
    pub(super) revision_id: FunctionRevisionId,
}

impl CheckedStandardUiWindow {
    /// Returns the fixed `std.ui.window` function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns the fixed `std.ui.window.title` parameter identity.
    pub const fn title_parameter_id(&self) -> ParameterId {
        self.title_parameter_id
    }

    /// Returns the fixed `std.ui.window.content` parameter identity.
    pub const fn content_parameter_id(&self) -> ParameterId {
        self.content_parameter_id
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }
}
/// The checked declaration facts for one Work ADR 0088 UI constructor.
///
/// The constructor set is closed and source-authored: each value retains the
/// exact function, ordered parameter, revision, and external-contract facts
/// checked against the V9 catalogue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardUiConstructor {
    pub(super) function_id: FunctionId,
    pub(super) parameter_ids: Vec<ParameterId>,
    pub(super) revision_id: FunctionRevisionId,
    pub(super) runtime_contract: &'static str,
}

impl CheckedStandardUiConstructor {
    /// Returns the fixed constructor function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns ordered constructor parameter identities.
    pub fn parameter_ids(&self) -> &[ParameterId] {
        &self.parameter_ids
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }

    /// Returns the exact external runtime-contract identity.
    pub const fn runtime_contract(&self) -> &'static str {
        self.runtime_contract
    }
}

/// The checked declaration facts for the one accepted ADR 0057 terminal
/// table presenter function (`std.terminal.present_table`).
///
/// The model carries the fixed function, parameter, and version-1 revision
/// identities. The exact-shape check rejects every variation of the closed
/// declaration before any artifact is constructed; ADR 0057 step 4
/// (`feat(artifact): encode terminal and json presenter plans`) consumes
/// these facts to build the `orna.server-terminal-table` artifact and its
/// ordered durable references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardTerminalPresentTable {
    pub(super) function_id: FunctionId,
    pub(super) parameter_id: ParameterId,
    pub(super) revision_id: FunctionRevisionId,
}

impl CheckedStandardTerminalPresentTable {
    /// Returns the fixed `std.terminal.present_table` function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns the fixed `std.terminal.present_table.p_rows` parameter identity.
    pub const fn parameter_id(&self) -> ParameterId {
        self.parameter_id
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }
}

/// The checked executable facts for the one V2 standard executable
/// (`std.invoke.echo`, ADR 0055), reconciled against both retained source
/// units and the verified snapshot executable evidence.
///
/// The model carries the fixed function, parameter, and version-1 revision
/// identities, the positive revision number, the checked declaration origin
/// and content hash, the version-2 semantic digest, the language version, the
/// complete 44-byte `orna.server-parameter-echo` artifact, the three ordered
/// durable references, and the three checked origins (the `std.invoke`
/// schema, the function declaration, and the `p_value` parameter declaration)
/// on the retained `std/invoke.orna` unit.
///
/// Step 10 reconstructs the durable `StandardExecutable` record from these
/// facts: `FunctionRevisionRecord::new(function_id, revision_id,
/// revision_number, declaration_origin, declaration_content_hash,
/// semantic_hash, language_version, artifact).with_semantic_hash_version(
/// semantic_hash_version)` with the exact reference sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardExecutable {
    pub(super) function_id: FunctionId,
    pub(super) parameter_ids: Vec<ParameterId>,
    pub(super) revision_id: FunctionRevisionId,
    pub(super) revision_number: u64,
    pub(super) declaration_origin: SourceOrigin,
    pub(super) declaration_content_hash: Sha256Digest,
    pub(super) semantic_hash: Sha256Digest,
    pub(super) semantic_hash_version: FunctionSemanticHashVersion,
    pub(super) language_version: String,
    pub(super) artifact: ExecutableArtifact,
    pub(super) references: Vec<DefinitionReference>,
    pub(super) schema_origin: SourceOrigin,
    pub(super) function_origin: SourceOrigin,
    pub(super) parameter_origins: Vec<SourceOrigin>,
}

impl CheckedStandardExecutable {
    /// Returns the fixed `std.invoke.echo` function identity.
    pub const fn function_id(&self) -> FunctionId {
        self.function_id
    }

    /// Returns all parameter identities in declaration order.
    pub fn parameter_ids(&self) -> &[ParameterId] {
        &self.parameter_ids
    }

    /// Returns the fixed version-1 function-revision identity.
    pub const fn revision_id(&self) -> FunctionRevisionId {
        self.revision_id
    }

    /// Returns the positive per-function revision number.
    pub const fn revision_number(&self) -> u64 {
        self.revision_number
    }

    /// Returns the checked declaration range in the retained invoke unit.
    pub const fn declaration_origin(&self) -> SourceOrigin {
        self.declaration_origin
    }

    /// Returns the exact declaration content hash.
    pub const fn declaration_content_hash(&self) -> Sha256Digest {
        self.declaration_content_hash
    }

    /// Returns the version-2 semantic digest.
    pub const fn semantic_hash(&self) -> Sha256Digest {
        self.semantic_hash
    }

    /// Returns the durable semantic-hash contract version.
    pub const fn semantic_hash_version(&self) -> FunctionSemanticHashVersion {
        self.semantic_hash_version
    }

    /// Returns the nonempty language version label.
    pub fn language_version(&self) -> &str {
        &self.language_version
    }

    /// Returns the complete server parameter-echo artifact.
    pub fn artifact(&self) -> &ExecutableArtifact {
        &self.artifact
    }

    /// Returns the ordered durable reference sequence for this executable.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }

    /// Returns the checked `CREATE SCHEMA std.invoke;` origin.
    pub const fn schema_origin(&self) -> SourceOrigin {
        self.schema_origin
    }

    /// Returns the checked `CREATE SERVER FUNCTION` declaration origin.
    pub const fn function_origin(&self) -> SourceOrigin {
        self.function_origin
    }

    /// Returns all parameter declaration origins in declaration order.
    pub fn parameter_origins(&self) -> &[SourceOrigin] {
        &self.parameter_origins
    }
}

/// A failure while reconciling retained source with a verified standard library.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardLibraryCheckError {
    /// The verified snapshot does not retain exactly one source unit.
    SourceUnitCount {
        /// The retained source-unit count.
        actual: usize,
    },
    /// The retained source contains compiler diagnostics.
    Diagnostics {
        /// The exact retained compiler diagnostics.
        diagnostics: Vec<CompilerDiagnostic>,
    },
    /// Source facts, catalogue facts, or origins do not agree.
    SourceMismatch,
    /// The declared server function name is not `std.invoke.echo`.
    UnexpectedName {
        /// The declared semantic function name.
        actual: QualifiedSemanticName,
    },
    /// The declaration does not declare exactly one parameter.
    UnexpectedParameterCount {
        /// The declared parameter count.
        actual: usize,
    },
    /// The single declared parameter is not `p_value`.
    UnexpectedParameterName {
        /// The declared semantic parameter name.
        actual: String,
    },
    /// The single parameter declares a default expression.
    ParameterDefault,
    /// The single parameter does not resolve to the fixed INTEGER value type.
    UnexpectedParameterType,
    /// The declared result is not one single value.
    UnexpectedResultShape,
    /// The single result does not resolve to the fixed INTEGER value type.
    UnexpectedResultType,
    /// The declaration omits the security mode.
    MissingSecurity,
    /// The declared security mode is not exactly `SECURITY INVOKER`.
    UnexpectedSecurity {
        /// The declared security mode.
        actual: SyntaxFunctionSecurity,
    },
    /// The declaration omits the transaction mode.
    MissingTransaction,
    /// The declared transaction mode is not exactly `TRANSACTION READ ONLY`.
    UnexpectedTransaction {
        /// The declared transaction mode.
        actual: SyntaxFunctionTransaction,
    },
    /// The declaration omits the volatility mode.
    MissingVolatility,
    /// The declared volatility mode is not exactly `VOLATILITY STABLE`.
    UnexpectedVolatility {
        /// The declared volatility mode.
        actual: SyntaxFunctionVolatility,
    },
    /// The declaration requires a capability clause.
    CapabilityClause,
    /// The body is not the exact closed `SELECT p_value` parameter-select form.
    UnexpectedBody,
    /// The no-input parameter select names an identifier other than `p_value`.
    UnexpectedBodyIdentifier {
        /// The identifier selected by the body.
        actual: String,
    },
    /// The catalogue does not contain the fixed `std.invoke` schema.
    MissingSchema,
    /// The schema at the fixed `std.invoke` identity has a different name.
    SchemaNameMismatch {
        /// The name of the schema at the fixed identity.
        actual: QualifiedSemanticName,
    },
    /// The catalogue does not contain the fixed `std.invoke.echo` function.
    MissingFunction,
    /// The function at the fixed identity has a different name.
    FunctionNameMismatch {
        /// The name of the function at the fixed identity.
        actual: QualifiedSemanticName,
    },
    /// The fixed function has no parameter at the fixed `p_value` identity.
    MissingParameter,
    /// The parameter at the fixed identity has a different name.
    ParameterNameMismatch {
        /// The name of the parameter at the fixed identity.
        actual: String,
    },
    /// The origins do not contain the fixed function declaration origin.
    MissingFunctionOrigin,
    /// The origins do not contain the fixed parameter declaration origin.
    MissingParameterOrigin,
    /// The origins do not contain the fixed `std.invoke` schema declaration origin.
    MissingSchemaOrigin,
    /// The function and parameter origins do not belong to the same source unit.
    OriginSourceUnitMismatch,
    /// The declared presenter function name is not the fixed presenter name.
    PresenterUnexpectedName {
        /// The exact expected presenter function name.
        expected: QualifiedSemanticName,
        /// The declared semantic function name.
        actual: QualifiedSemanticName,
    },
    /// The presenter declaration does not declare exactly one parameter.
    PresenterUnexpectedParameterCount {
        /// The declared parameter count.
        actual: usize,
    },
    /// The single presenter parameter is not the fixed parameter name.
    PresenterUnexpectedParameterName {
        /// The exact expected presenter parameter name.
        expected: String,
        /// The declared semantic parameter name.
        actual: String,
    },
    /// The single presenter parameter declares a default expression.
    PresenterParameterDefault,
    /// The presenter parameter does not resolve to the fixed value type.
    PresenterUnexpectedParameterType {
        /// The exact expected value-type identity.
        expected: TypeId,
    },
    /// The presenter result is not one single value.
    PresenterUnexpectedResultShape,
    /// The presenter result does not resolve to the fixed value type.
    PresenterUnexpectedResultType {
        /// The exact expected value-type identity.
        expected: TypeId,
    },
    /// The presenter declaration omits the security mode.
    PresenterMissingSecurity,
    /// The presenter security mode is not exactly `SECURITY INVOKER`.
    PresenterUnexpectedSecurity {
        /// The declared security mode.
        actual: SyntaxFunctionSecurity,
    },
    /// The presenter declaration omits the transaction mode.
    PresenterMissingTransaction,
    /// The presenter transaction mode is not exactly `TRANSACTION READ ONLY`.
    PresenterUnexpectedTransaction {
        /// The declared transaction mode.
        actual: SyntaxFunctionTransaction,
    },
    /// The presenter declaration omits the volatility mode.
    PresenterMissingVolatility,
    /// The presenter volatility mode is not exactly `VOLATILITY STABLE`.
    PresenterUnexpectedVolatility {
        /// The declared volatility mode.
        actual: SyntaxFunctionVolatility,
    },
    /// The presenter declaration requires a capability clause.
    PresenterCapabilityClause,
    /// The presenter body is not the exact closed parameter-select form.
    PresenterUnexpectedBody,
    /// The presenter parameter select names an identifier other than the
    /// fixed presenter parameter.
    PresenterUnexpectedBodyIdentifier {
        /// The exact expected body identifier.
        expected: String,
        /// The identifier selected by the body.
        actual: String,
    },
    /// The catalogue does not contain the fixed presenter schema identity.
    PresenterMissingSchema,
    /// The schema at the fixed presenter identity has a different name.
    PresenterSchemaNameMismatch {
        /// The exact expected schema name.
        expected: QualifiedSemanticName,
        /// The name of the schema at the fixed identity.
        actual: QualifiedSemanticName,
    },
    /// The catalogue does not contain the fixed presenter function identity.
    PresenterMissingFunction,
    /// The function at the fixed identity has a different name.
    PresenterFunctionNameMismatch {
        /// The exact expected function name.
        expected: QualifiedSemanticName,
        /// The name of the function at the fixed identity.
        actual: QualifiedSemanticName,
    },
    /// The function at the fixed identity is not a SERVER function.
    PresenterUnexpectedDomain {
        /// The declared function domain.
        actual: FunctionDomain,
    },
    /// The fixed presenter function has no parameter at the fixed identity.
    PresenterMissingParameter,
    /// The parameter at the fixed identity has a different name.
    PresenterParameterNameMismatch {
        /// The exact expected parameter name.
        expected: String,
        /// The name of the parameter at the fixed identity.
        actual: String,
    },
    /// The origins do not contain the fixed presenter function origin.
    PresenterMissingFunctionOrigin,
    /// The origins do not contain the fixed presenter parameter origin.
    PresenterMissingParameterOrigin,
    /// The verified executable standard snapshot does not carry exactly one
    /// executable record.
    ExecutableCount {
        /// The retained executable count.
        actual: usize,
    },
    /// A stored executable fact does not agree with the checked source facts.
    ExecutableMismatch,
    /// The server parameter-echo artifact could not be encoded.
    Artifact {
        /// The exact artifact encoder failure.
        source: ServerParameterEchoError,
    },
    /// A canonical digest (artifact payload, declaration content, or semantic
    /// hash) could not be computed.
    Digest {
        /// The exact canonical-hash failure.
        source: CanonicalHashError,
    },
    /// The executable facts violate a revision invariant.
    Revision {
        /// The exact revision invariant failure.
        source: RevisionInvariantError,
    },
}

impl fmt::Display for StandardLibraryCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceUnitCount { actual } => write!(
                formatter,
                "the verified standard library has {actual} source units, expected exactly one"
            ),
            Self::Diagnostics { .. } => {
                formatter.write_str("the verified standard library source has compiler diagnostics")
            }
            Self::SourceMismatch => formatter.write_str(
                "the verified standard library source does not match its catalogue and origins",
            ),
            Self::UnexpectedName { actual } => write!(
                formatter,
                "the standard parameter-echo declaration must be named std.invoke.echo, not {actual}"
            ),
            Self::UnexpectedParameterCount { actual } => write!(
                formatter,
                "the standard parameter-echo declaration must declare exactly one parameter, not {actual}"
            ),
            Self::UnexpectedParameterName { actual } => write!(
                formatter,
                "the standard parameter-echo parameter must be p_value, not {actual}"
            ),
            Self::ParameterDefault => formatter.write_str(
                "the standard parameter-echo parameter must not declare a default expression",
            ),
            Self::UnexpectedParameterType => formatter.write_str(
                "the standard parameter-echo parameter must resolve to the INTEGER value type",
            ),
            Self::UnexpectedResultShape => formatter
                .write_str("the standard parameter-echo declaration must return one single value"),
            Self::UnexpectedResultType => formatter.write_str(
                "the standard parameter-echo result must resolve to the INTEGER value type",
            ),
            Self::MissingSecurity => formatter
                .write_str("the standard parameter-echo declaration must declare SECURITY INVOKER"),
            Self::UnexpectedSecurity { actual } => write!(
                formatter,
                "the standard parameter-echo declaration must declare SECURITY INVOKER, not {actual:?}"
            ),
            Self::MissingTransaction => formatter.write_str(
                "the standard parameter-echo declaration must declare TRANSACTION READ ONLY",
            ),
            Self::UnexpectedTransaction { actual } => write!(
                formatter,
                "the standard parameter-echo declaration must declare TRANSACTION READ ONLY, not {actual:?}"
            ),
            Self::MissingVolatility => formatter.write_str(
                "the standard parameter-echo declaration must declare VOLATILITY STABLE",
            ),
            Self::UnexpectedVolatility { actual } => write!(
                formatter,
                "the standard parameter-echo declaration must declare VOLATILITY STABLE, not {actual:?}"
            ),
            Self::CapabilityClause => formatter
                .write_str("the standard parameter-echo declaration must not require a capability"),
            Self::UnexpectedBody => formatter.write_str(
                "the standard parameter-echo body must be the exact SELECT p_value form",
            ),
            Self::UnexpectedBodyIdentifier { actual } => write!(
                formatter,
                "the standard parameter-echo body must select p_value, not {actual}"
            ),
            Self::MissingSchema => formatter
                .write_str("the catalogue does not contain the fixed std.invoke schema identity"),
            Self::SchemaNameMismatch { actual } => write!(
                formatter,
                "the schema at the fixed identity is named {actual}, not std.invoke"
            ),
            Self::MissingFunction => formatter.write_str(
                "the catalogue does not contain the fixed std.invoke.echo function identity",
            ),
            Self::FunctionNameMismatch { actual } => write!(
                formatter,
                "the function at the fixed identity is named {actual}, not std.invoke.echo"
            ),
            Self::MissingParameter => formatter
                .write_str("the fixed function has no parameter at the fixed p_value identity"),
            Self::ParameterNameMismatch { actual } => write!(
                formatter,
                "the parameter at the fixed identity is named {actual}, not p_value"
            ),
            Self::MissingFunctionOrigin => formatter
                .write_str("the origins do not contain the fixed std.invoke.echo function origin"),
            Self::MissingParameterOrigin => formatter.write_str(
                "the origins do not contain the fixed std.invoke.echo.p_value parameter origin",
            ),
            Self::MissingSchemaOrigin => formatter.write_str(
                "the origins do not contain the fixed std.invoke schema declaration origin",
            ),
            Self::OriginSourceUnitMismatch => formatter.write_str(
                "the standard function and parameter origins must belong to the same source unit",
            ),
            Self::PresenterUnexpectedName { expected, actual } => write!(
                formatter,
                "the standard presenter declaration must be named {expected}, not {actual}"
            ),
            Self::PresenterUnexpectedParameterCount { actual } => write!(
                formatter,
                "the standard presenter declaration must declare exactly one parameter, not {actual}"
            ),
            Self::PresenterUnexpectedParameterName { expected, actual } => write!(
                formatter,
                "the standard presenter parameter must be {expected}, not {actual}"
            ),
            Self::PresenterParameterDefault => formatter.write_str(
                "the standard presenter parameter must not declare a default expression",
            ),
            Self::PresenterUnexpectedParameterType { expected } => write!(
                formatter,
                "the standard presenter parameter must resolve to the value type {expected:?}"
            ),
            Self::PresenterUnexpectedResultShape => formatter
                .write_str("the standard presenter declaration must return one single value"),
            Self::PresenterUnexpectedResultType { expected } => write!(
                formatter,
                "the standard presenter result must resolve to the value type {expected:?}"
            ),
            Self::PresenterMissingSecurity => formatter
                .write_str("the standard presenter declaration must declare SECURITY INVOKER"),
            Self::PresenterUnexpectedSecurity { actual } => write!(
                formatter,
                "the standard presenter declaration must declare SECURITY INVOKER, not {actual:?}"
            ),
            Self::PresenterMissingTransaction => formatter.write_str(
                "the standard presenter declaration must declare TRANSACTION READ ONLY",
            ),
            Self::PresenterUnexpectedTransaction { actual } => write!(
                formatter,
                "the standard presenter declaration must declare TRANSACTION READ ONLY, not {actual:?}"
            ),
            Self::PresenterMissingVolatility => formatter.write_str(
                "the standard presenter declaration must declare VOLATILITY STABLE",
            ),
            Self::PresenterUnexpectedVolatility { actual } => write!(
                formatter,
                "the standard presenter declaration must declare VOLATILITY STABLE, not {actual:?}"
            ),
            Self::PresenterCapabilityClause => formatter
                .write_str("the standard presenter declaration must not require a capability"),
            Self::PresenterUnexpectedBody => formatter.write_str(
                "the standard presenter body must be the exact closed parameter-select form",
            ),
            Self::PresenterUnexpectedBodyIdentifier { expected, actual } => write!(
                formatter,
                "the standard presenter body must select {expected}, not {actual}"
            ),
            Self::PresenterMissingSchema => formatter
                .write_str("the catalogue does not contain the fixed presenter schema identity"),
            Self::PresenterSchemaNameMismatch { expected, actual } => write!(
                formatter,
                "the schema at the fixed identity is named {actual}, not {expected}"
            ),
            Self::PresenterMissingFunction => formatter.write_str(
                "the catalogue does not contain the fixed presenter function identity",
            ),
            Self::PresenterFunctionNameMismatch { expected, actual } => write!(
                formatter,
                "the function at the fixed identity is named {actual}, not {expected}"
            ),
            Self::PresenterUnexpectedDomain { actual } => write!(
                formatter,
                "the function at the fixed identity must be a SERVER function, not {actual:?}"
            ),
            Self::PresenterMissingParameter => formatter
                .write_str("the fixed function has no parameter at the fixed presenter identity"),
            Self::PresenterParameterNameMismatch { expected, actual } => write!(
                formatter,
                "the parameter at the fixed identity is named {actual}, not {expected}"
            ),
            Self::PresenterMissingFunctionOrigin => formatter
                .write_str("the origins do not contain the fixed presenter function origin"),
            Self::PresenterMissingParameterOrigin => formatter
                .write_str("the origins do not contain the fixed presenter parameter origin"),
            Self::ExecutableCount { actual } => write!(
                formatter,
                "the verified executable standard library has {actual} executable records, expected exactly one"
            ),
            Self::ExecutableMismatch => formatter.write_str(
                "the verified executable standard library does not match its stored executable evidence",
            ),
            Self::Artifact { source } => write!(
                formatter,
                "the standard parameter-echo artifact could not be encoded: {source}"
            ),
            Self::Digest { source } => write!(
                formatter,
                "a canonical digest for the standard executable could not be computed: {source}"
            ),
            Self::Revision { source } => write!(
                formatter,
                "the standard parameter-echo executable facts violate a revision invariant: {source}"
            ),
        }
    }
}

impl Error for StandardLibraryCheckError {}

/// Authority required to check application source against a checked standard library.
#[derive(Clone, Copy, Debug)]
pub struct StandardApplicationCheckContext<'a> {
    pub(super) application: &'a CatalogueSnapshot,
    pub(super) standard: &'a CheckedStandardLibrary,
}

impl<'a> StandardApplicationCheckContext<'a> {
    /// Returns the application catalogue used for identity continuity.
    pub fn application_catalogue(&self) -> &'a CatalogueSnapshot {
        self.application
    }

    /// Returns the checked standard library used for type resolution.
    pub fn standard_library(&self) -> &'a CheckedStandardLibrary {
        self.standard
    }
}

/// A failure while establishing standard-backed application checking authority.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardApplicationContextError {
    /// An application schema has the same durable identity as a standard schema.
    SchemaIdentityConflict {
        /// The conflicting durable schema identity.
        id: SchemaId,
    },
    /// An application schema has the same semantic name as a standard schema.
    SchemaNameConflict {
        /// The conflicting semantic schema name.
        name: QualifiedSemanticName,
    },
    /// An application type has the same durable identity as a standard value type.
    TypeIdentityConflict {
        /// The conflicting durable type identity.
        id: TypeId,
    },
    /// An application binding has the same durable identity as a standard binding.
    TypeBindingIdentityConflict {
        /// The conflicting durable binding identity.
        id: TypeBindingId,
    },
    /// A standard value type uses a contract without compiler compatibility support.
    UnsupportedCompatibilityContract {
        /// The standard value type with the unsupported contract.
        type_id: TypeId,
        /// The unsupported representation contract.
        contract: String,
    },
    /// More than one standard type uses one supported compatibility contract.
    CompatibilityContractConflict {
        /// The duplicated representation contract.
        contract: String,
    },
}

impl fmt::Display for StandardApplicationContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaIdentityConflict { id } => write!(
                formatter,
                "the application catalogue conflicts with standard schema identity {id}"
            ),
            Self::SchemaNameConflict { name } => write!(
                formatter,
                "the application catalogue conflicts with standard schema name {name}"
            ),
            Self::TypeIdentityConflict { id } => write!(
                formatter,
                "the application catalogue conflicts with standard type identity {id}"
            ),
            Self::TypeBindingIdentityConflict { id } => write!(
                formatter,
                "the application catalogue conflicts with standard type binding identity {id}"
            ),
            Self::UnsupportedCompatibilityContract { type_id, contract } => write!(
                formatter,
                "the standard value type {type_id} uses unsupported compatibility contract {contract}"
            ),
            Self::CompatibilityContractConflict { contract } => write!(
                formatter,
                "the standard library uses compatibility contract {contract} for more than one type"
            ),
        }
    }
}

impl Error for StandardApplicationContextError {}

/// The kind and owner of one standard-backed application type use.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CheckedTypeUseKind {
    /// A direct type written on one object or record value field.
    Field {
        /// The checked object or record value type that owns the field.
        owner: CheckedTypeId,
        /// The checked field identity.
        field: CheckedFieldId,
    },
    /// A direct type written on one function parameter.
    Parameter {
        /// The checked function that owns the parameter.
        owner: CheckedFunctionId,
        /// The checked parameter identity.
        parameter: CheckedParameterId,
    },
    /// A direct type written on one CLIENT state slot.
    State {
        /// The checked CLIENT function that owns the state slot.
        owner: CheckedFunctionId,
        /// The zero-based state-slot ordinal in declaration order.
        ordinal: u32,
    },
    /// A direct type written on a function return or `ROWS` column.
    Return {
        /// The checked function that declares the return.
        owner: CheckedFunctionId,
        /// The zero-based scalar-return or `ROWS`-column ordinal.
        ordinal: u32,
    },
    /// One accepted value-producing function-body expression use.
    Expression {
        /// The checked function that owns the expression.
        owner: CheckedFunctionId,
        /// The deterministic body-expression ordinal.
        ordinal: u32,
    },
    /// One function-body result use.
    Result {
        /// The checked function that owns the result.
        owner: CheckedFunctionId,
        /// The zero-based declared result ordinal.
        ordinal: u32,
    },
}

/// One standard value-type use resolved through the checked standard library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedValueTypeUse {
    pub(super) type_id: TypeId,
    pub(super) kind: CheckedTypeUseKind,
    pub(super) location: SourceLocation,
}

impl CheckedValueTypeUse {
    /// Returns the durable standard value-type identity.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns the type-use kind.
    pub const fn kind(&self) -> CheckedTypeUseKind {
        self.kind
    }

    /// Returns the direct declaration location or complete body-expression location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One application object-reference type use in a standard-backed application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectReferenceUse {
    pub(super) target: CheckedTypeId,
    pub(super) kind: CheckedTypeUseKind,
    pub(super) location: SourceLocation,
}

impl CheckedObjectReferenceUse {
    /// Returns the checked application object-type target.
    pub const fn target(&self) -> CheckedTypeId {
        self.target
    }

    /// Returns the type-use kind.
    pub const fn kind(&self) -> CheckedTypeUseKind {
        self.kind
    }

    /// Returns the direct declaration location or complete body-expression location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The canonical public type use for one written slot in a standard-backed application.
///
/// This use is the canonical public resolved-type carrier for its slot. `Value` carries the
/// checked standard [`TypeId`], `Named` carries a checked application value type, and
/// `ObjectReference` carries the checked application object target. Separate signature
/// references are evidence about the same resolution, not another resolved type. The
/// compatibility [`SemanticType::Scalar`] is not a source-name or `TypeId` authority, and
/// declarations do not own a scalar-to-`TypeId` sidecar.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedApplicationTypeUse {
    /// A resolved standard value-type use with its checked standard [`TypeId`].
    Value(CheckedValueTypeUse),
    /// A resolved application value-type use with its checked application identity.
    Named {
        /// The checked application value-type target.
        target: CheckedTypeId,
        /// The type-use kind.
        kind: CheckedTypeUseKind,
        /// The direct declaration or complete expression location.
        location: SourceLocation,
    },
    /// A resolved application object-reference use with its checked application object target.
    ObjectReference(CheckedObjectReferenceUse),
}

impl CheckedApplicationTypeUse {
    /// Returns the value-type use when this use names a standard value type.
    pub fn value(&self) -> Option<&CheckedValueTypeUse> {
        match self {
            Self::Value(value) => Some(value),
            Self::Named { .. } | Self::ObjectReference(_) => None,
        }
    }

    /// Returns the checked application value-type target, when present.
    pub const fn named_type(&self) -> Option<CheckedTypeId> {
        match self {
            Self::Named { target, .. } => Some(*target),
            Self::Value(_) | Self::ObjectReference(_) => None,
        }
    }

    /// Returns the object-reference use when this use resolves an application object.
    pub fn object_reference(&self) -> Option<&CheckedObjectReferenceUse> {
        match self {
            Self::Value(_) | Self::Named { .. } => None,
            Self::ObjectReference(reference) => Some(reference),
        }
    }

    /// Returns the type-use kind.
    pub const fn kind(&self) -> CheckedTypeUseKind {
        match self {
            Self::Value(value) => value.kind,
            Self::Named { kind, .. } => *kind,
            Self::ObjectReference(reference) => reference.kind,
        }
    }

    /// Returns the direct declaration location or complete body-expression location.
    pub fn location(&self) -> &SourceLocation {
        match self {
            Self::Value(value) => &value.location,
            Self::Named { location, .. } => location,
            Self::ObjectReference(reference) => &reference.location,
        }
    }
}

/// One standard value-type signature reference derived from a canonical declaration use.
///
/// This does not duplicate object references or body type uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardTypeReference {
    pub(super) owner: CheckedFunctionId,
    pub(super) ordinal: u32,
    pub(super) target: TypeId,
    pub(super) location: SourceLocation,
}

impl CheckedStandardTypeReference {
    /// Returns the checked function that owns the signature slot.
    pub const fn owner(&self) -> CheckedFunctionId {
        self.owner
    }

    /// Returns the flattened zero-based signature ordinal, including unrecorded `REF` slots.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the durable checked standard value-type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the exact written location of the canonical value declaration use.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A standard-backed checking result that cannot convert to a legacy report.
#[derive(Clone)]
pub struct StandardApplicationCheckReport {
    pub(super) standard_library: CheckedStandardLibrary,
    pub(super) parse_report: ParseReport,
    pub(super) diagnostics: Vec<CompilerDiagnostic>,
    pub(super) checked_bundle: Option<CheckedStandardApplicationBundle>,
}

impl fmt::Debug for StandardApplicationCheckReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StandardApplicationCheckReport")
            .field("standard_library", &self.standard_library)
            .field("parse_report", &self.parse_report)
            .field("diagnostics", &self.diagnostics)
            .field("checked_bundle", &self.checked_bundle)
            .finish()
    }
}

/// Crate-private standard preparation input with no legacy-report escape.
pub(crate) struct StandardApplicationPreparationView<'a> {
    checked: &'a CheckedBundle,
    standard_catalogue_revision: CatalogueRevisionId,
    standard_library_revision: StandardLibraryRevisionId,
    standard_library_digest: Sha256Digest,
    uses: &'a [CheckedApplicationTypeUse],
    standard_type_references: &'a [CheckedStandardTypeReference],
    evidence: &'a StandardApplicationPreparationEvidence,
}

impl<'a> StandardApplicationPreparationView<'a> {
    fn new(bundle: &'a CheckedStandardApplicationBundle) -> Self {
        Self {
            checked: &bundle.inner,
            standard_catalogue_revision: bundle.standard_catalogue_revision,
            standard_library_revision: bundle.standard_library_revision,
            standard_library_digest: bundle.standard_library_digest,
            uses: &bundle.uses,
            standard_type_references: &bundle.standard_type_references,
            evidence: &bundle.preparation_evidence,
        }
    }

    pub(crate) const fn checked(&self) -> &'a CheckedBundle {
        self.checked
    }

    pub(crate) const fn standard_catalogue_revision(&self) -> CatalogueRevisionId {
        self.standard_catalogue_revision
    }

    pub(crate) const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        self.standard_library_revision
    }

    pub(crate) const fn standard_library_digest(&self) -> Sha256Digest {
        self.standard_library_digest
    }

    pub(crate) const fn uses(&self) -> &'a [CheckedApplicationTypeUse] {
        self.uses
    }

    pub(crate) const fn standard_type_references(&self) -> &'a [CheckedStandardTypeReference] {
        self.standard_type_references
    }

    pub(crate) const fn evidence(&self) -> &'a StandardApplicationPreparationEvidence {
        self.evidence
    }
}

/// Sealed canonical resolver evidence retained for standard preparation.
///
/// This is crate-private. It copies the canonical use and reference sequences
/// after resolver ordering. It does not provide lookup or resolution logic.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct StandardApplicationPreparationEvidence {
    pub(super) declaration_uses: Vec<CheckedApplicationTypeUse>,
    pub(super) type_uses: Vec<CheckedApplicationTypeUse>,
    pub(super) standard_type_references: Vec<CheckedStandardTypeReference>,
}

impl StandardApplicationPreparationEvidence {
    pub(super) fn from_canonical(
        type_uses: &[CheckedApplicationTypeUse],
        standard_type_references: &[CheckedStandardTypeReference],
    ) -> Self {
        Self {
            declaration_uses: type_uses
                .iter()
                .filter(|type_use| Self::is_declaration_use(type_use.kind()))
                .cloned()
                .collect(),
            type_uses: type_uses.to_vec(),
            standard_type_references: standard_type_references.to_vec(),
        }
    }

    fn is_declaration_use(kind: CheckedTypeUseKind) -> bool {
        match kind {
            CheckedTypeUseKind::Field { .. }
            | CheckedTypeUseKind::Parameter { .. }
            | CheckedTypeUseKind::State { .. }
            | CheckedTypeUseKind::Return { .. } => true,
            CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. } => false,
        }
    }

    pub(crate) fn declaration_uses(&self) -> &[CheckedApplicationTypeUse] {
        &self.declaration_uses
    }

    pub(crate) fn type_uses(&self) -> &[CheckedApplicationTypeUse] {
        &self.type_uses
    }

    pub(crate) fn standard_type_references(&self) -> &[CheckedStandardTypeReference] {
        &self.standard_type_references
    }
}

/// A checked standard-backed application bundle with one canonical type-use arena.
#[derive(Clone, Eq, PartialEq)]
pub struct CheckedStandardApplicationBundle {
    pub(super) inner: CheckedBundle,
    pub(super) standard_catalogue_revision: CatalogueRevisionId,
    pub(super) standard_library_revision: StandardLibraryRevisionId,
    pub(super) standard_library_digest: Sha256Digest,
    pub(super) uses: Vec<CheckedApplicationTypeUse>,
    pub(super) standard_type_references: Vec<CheckedStandardTypeReference>,
    pub(super) use_indices: HashMap<CheckedTypeUseKind, usize>,
    pub(super) preparation_evidence: StandardApplicationPreparationEvidence,
}

impl fmt::Debug for CheckedStandardApplicationBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let object_types = self.object_types().collect::<Vec<_>>();
        let record_value_types = self.record_value_types().collect::<Vec<_>>();
        let server_functions = self.server_functions().collect::<Vec<_>>();
        let client_functions = self.client_functions().collect::<Vec<_>>();

        formatter
            .debug_struct("CheckedStandardApplicationBundle")
            .field(
                "base_catalogue_revision",
                &self.inner.base_catalogue_revision,
            )
            .field(
                "standard_catalogue_revision",
                &self.standard_catalogue_revision,
            )
            .field("standard_library_revision", &self.standard_library_revision)
            .field("standard_library_digest", &self.standard_library_digest)
            .field("schemas", &self.inner.schemas)
            .field("object_types", &object_types)
            .field("record_value_types", &record_value_types)
            .field("server_functions", &server_functions)
            .field("client_functions", &client_functions)
            .field("uses", &self.uses)
            .field("standard_type_references", &self.standard_type_references)
            .finish()
    }
}

impl CheckedStandardApplicationBundle {
    /// Returns the application catalogue revision used for identity continuity.
    pub const fn base_catalogue_revision(&self) -> CatalogueRevisionId {
        self.inner.base_catalogue_revision
    }

    /// Returns the checked standard catalogue revision.
    pub const fn standard_catalogue_revision(&self) -> CatalogueRevisionId {
        self.standard_catalogue_revision
    }

    /// Returns the checked standard-library revision.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        self.standard_library_revision
    }

    /// Returns the checked standard-library digest.
    pub const fn standard_library_digest(&self) -> Sha256Digest {
        self.standard_library_digest
    }

    /// Returns every declared or body type use in canonical order.
    ///
    /// `Value` and `Named` entries retain standard and application value identities respectively.
    pub fn uses(&self) -> &[CheckedApplicationTypeUse] {
        &self.uses
    }

    /// Returns standard value-type uses without exposing the compatibility scalar.
    pub fn value_type_uses(&self) -> impl Iterator<Item = &CheckedValueTypeUse> + '_ {
        self.uses
            .iter()
            .filter_map(CheckedApplicationTypeUse::value)
    }

    /// Returns standard value-type signature references in source-unit insertion order.
    ///
    /// Each function's entries follow declaration order, while unrecorded `REF` slots may leave
    /// ordinal gaps.
    pub fn standard_type_references(&self) -> &[CheckedStandardTypeReference] {
        &self.standard_type_references
    }

    /// Returns submitted application schemas in source order.
    pub fn schemas(&self) -> &[CheckedSchema] {
        &self.inner.schemas
    }

    /// Returns scalar-free borrowed object views in source order.
    pub fn object_types(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationObjectType<'_>> + '_
    {
        self.inner
            .object_types
            .iter()
            .map(move |object| CheckedStandardApplicationObjectType {
                bundle: self,
                object,
            })
    }

    /// Returns scalar-free borrowed record value definitions in source order.
    pub fn record_value_types(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationRecordValueType<'_>> + '_
    {
        self.inner
            .record_value_types
            .iter()
            .map(
                move |record_value_type| CheckedStandardApplicationRecordValueType {
                    bundle: self,
                    record_value_type,
                },
            )
    }

    /// Returns scalar-free borrowed SERVER function views in source order.
    pub fn server_functions(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationServerFunction<'_>> + '_
    {
        self.inner.server_functions.iter().map(move |function| {
            CheckedStandardApplicationServerFunction {
                bundle: self,
                function,
            }
        })
    }

    /// Returns scalar-free borrowed CLIENT function views in source order.
    pub fn client_functions(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationClientFunction<'_>> + '_
    {
        self.inner.client_functions.iter().map(move |function| {
            CheckedStandardApplicationClientFunction {
                bundle: self,
                function,
            }
        })
    }
    /// Returns the checked source bundle for generic standard-client lowering.
    ///
    /// The bundle retains checked stable identities and source locations. The
    /// caller must use the standard catalogue and source authority supplied
    /// with the surrounding standard check.
    pub fn checked_bundle(&self) -> &CheckedBundle {
        &self.inner
    }

    fn type_use(&self, kind: CheckedTypeUseKind) -> &CheckedApplicationTypeUse {
        let index = self.use_indices[&kind];
        &self.uses[index]
    }
}

/// A scalar-free borrowed record value definition.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationRecordValueType<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    record_value_type: &'a CheckedRecordValueType,
}

impl fmt::Debug for CheckedStandardApplicationRecordValueType<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationRecordValueType")
            .field("id", &self.record_value_type.id)
            .field("name", &self.record_value_type.name)
            .field("location", &self.record_value_type.location)
            .finish()
    }
}

impl CheckedStandardApplicationRecordValueType<'_> {
    /// Returns the checked record value identity.
    pub const fn id(&self) -> CheckedTypeId {
        self.record_value_type.id
    }

    /// Returns the record value semantic name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.record_value_type.name
    }

    /// Returns scalar-free record field views in declaration order.
    pub fn fields(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationRecordValueField<'_>> + '_
    {
        self.record_value_type.fields.iter().map(move |field| {
            CheckedStandardApplicationRecordValueField {
                bundle: self.bundle,
                owner: self.record_value_type.id,
                field,
            }
        })
    }

    /// Returns the complete record value declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.record_value_type.location
    }
}

/// A scalar-free borrowed record value field.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationRecordValueField<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    owner: CheckedTypeId,
    field: &'a CheckedRecordValueField,
}

impl fmt::Debug for CheckedStandardApplicationRecordValueField<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationRecordValueField")
            .field("id", &self.field.id)
            .field("name", &self.field.name)
            .field("ordinal", &self.field.ordinal)
            .field("location", &self.field.location)
            .finish()
    }
}

impl CheckedStandardApplicationRecordValueField<'_> {
    /// Returns the checked record field identity.
    pub const fn id(&self) -> CheckedFieldId {
        self.field.id
    }

    /// Returns the record field name.
    pub fn name(&self) -> &str {
        &self.field.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.field.ordinal
    }

    /// Returns the canonical public type use for this record field.
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse {
        self.bundle.type_use(CheckedTypeUseKind::Field {
            owner: self.owner,
            field: self.field.id,
        })
    }

    /// Returns the complete record field declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.field.location
    }
}

/// A scalar-free borrowed object-type view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationObjectType<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    object: &'a CheckedObjectType,
}

impl fmt::Debug for CheckedStandardApplicationObjectType<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationObjectType")
            .field("id", &self.object.id)
            .field("name", &self.object.name)
            .field("location", &self.object.location)
            .finish()
    }
}

impl<'a> CheckedStandardApplicationObjectType<'a> {
    /// Returns the checked object identity.
    pub const fn id(&self) -> CheckedTypeId {
        self.object.id
    }

    /// Returns the object semantic name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.object.name
    }

    /// Returns scalar-free field views in declaration order.
    pub fn fields(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationField<'_>> + '_ {
        self.object
            .fields
            .iter()
            .map(move |field| CheckedStandardApplicationField {
                bundle: self.bundle,
                owner: self.object.id,
                field,
            })
    }

    /// Returns the complete object declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.object.location
    }
}

/// A scalar-free borrowed field view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationField<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    owner: CheckedTypeId,
    field: &'a CheckedField,
}

impl fmt::Debug for CheckedStandardApplicationField<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationField")
            .field("id", &self.field.id)
            .field("name", &self.field.name)
            .field("ordinal", &self.field.ordinal)
            .field("location", &self.field.location)
            .finish()
    }
}

impl CheckedStandardApplicationField<'_> {
    /// Returns the checked field identity.
    pub const fn id(&self) -> CheckedFieldId {
        self.field.id
    }

    /// Returns the field name.
    pub fn name(&self) -> &str {
        &self.field.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.field.ordinal
    }

    /// Returns the canonical public type use for this field's written slot.
    ///
    /// Standard and application value uses carry their checked identities. A standard
    /// declaration's compatibility [`SemanticType::Scalar`] is not a source-name or `TypeId`
    /// authority.
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse {
        self.bundle.type_use(CheckedTypeUseKind::Field {
            owner: self.owner,
            field: self.field.id,
        })
    }

    /// Reports whether the field accepts null values.
    pub const fn nullable(&self) -> bool {
        self.field.nullable
    }

    /// Reports whether the field is unique.
    pub const fn unique(&self) -> bool {
        self.field.unique
    }

    /// Returns the default value and location when present.
    pub fn default(&self) -> Option<(&ConstantValue, &SourceLocation)> {
        self.field
            .default
            .as_ref()
            .map(|default| (&default.value, &default.location))
    }

    /// Returns the declared delete action when present.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.field.on_delete
    }

    /// Returns the complete field declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.field.location
    }
}

/// A scalar-free borrowed SERVER function view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationServerFunction<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    function: &'a CheckedServerFunction,
}

impl fmt::Debug for CheckedStandardApplicationServerFunction<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationServerFunction")
            .field("id", &self.function.id)
            .field("name", &self.function.name)
            .field("location", &self.function.location)
            .finish()
    }
}

impl<'a> CheckedStandardApplicationServerFunction<'a> {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.function.id
    }

    /// Returns the function semantic name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.function.name
    }

    /// Returns scalar-free parameter views in declaration order.
    pub fn parameters(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationParameter<'_>> + '_
    {
        self.function
            .parameters
            .iter()
            .map(move |parameter| CheckedStandardApplicationParameter {
                bundle: self.bundle,
                owner: self.function.id,
                parameter,
            })
    }

    /// Returns scalar-free return-column views in declaration order.
    pub fn return_columns(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationReturnColumn<'_>> + '_
    {
        self.function.return_columns().iter().map(move |column| {
            CheckedStandardApplicationReturnColumn {
                bundle: self.bundle,
                owner: self.function.id,
                column,
            }
        })
    }

    /// Returns the function security mode.
    pub const fn security(&self) -> FunctionSecurity {
        self.function.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<FunctionTransaction> {
        self.function.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> FunctionVolatility {
        self.function.volatility
    }

    /// Returns the complete function declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.function.location
    }

    /// Returns application and object reference evidence in source order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.function.references
    }
}

/// A scalar-free borrowed CLIENT function view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationClientFunction<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    function: &'a CheckedClientFunction,
}

impl fmt::Debug for CheckedStandardApplicationClientFunction<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationClientFunction")
            .field("id", &self.function.id)
            .field("name", &self.function.name)
            .field("location", &self.function.location)
            .finish()
    }
}

impl<'a> CheckedStandardApplicationClientFunction<'a> {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.function.id
    }

    /// Returns the function semantic name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.function.name
    }

    /// Returns the function domain.
    pub const fn domain(&self) -> FunctionDomain {
        self.function.domain
    }

    /// Returns an empty scalar-free parameter iterator for the current CLIENT subset.
    pub fn parameters(
        &self,
    ) -> impl std::iter::ExactSizeIterator<Item = CheckedStandardApplicationParameter<'_>> + '_
    {
        self.function
            .parameters
            .iter()
            .map(move |parameter| CheckedStandardApplicationParameter {
                bundle: self.bundle,
                owner: self.function.id,
                parameter,
            })
    }

    /// Returns the canonical public type use for this CLIENT return slot.
    ///
    /// Standard and application value uses carry their checked identities. A standard
    /// declaration's compatibility [`SemanticType::Scalar`] is not a source-name or `TypeId`
    /// authority.
    pub fn return_type(&self) -> &CheckedApplicationTypeUse {
        self.bundle.type_use(CheckedTypeUseKind::Return {
            owner: self.function.id,
            ordinal: 0,
        })
    }

    /// Returns the function security mode.
    pub const fn security(&self) -> FunctionSecurity {
        self.function.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<FunctionTransaction> {
        self.function.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> FunctionVolatility {
        self.function.volatility
    }

    /// Returns the complete checked CLIENT body kind.
    pub fn body_kind(&self) -> CheckedClientBodyKind {
        self.function.body_kind()
    }

    /// Returns the complete function declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.function.location
    }

    /// Returns application and object reference evidence in source order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.function.references
    }
}

/// A scalar-free borrowed function-parameter view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationParameter<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    owner: CheckedFunctionId,
    parameter: &'a CheckedServerFunctionParameter,
}

impl fmt::Debug for CheckedStandardApplicationParameter<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationParameter")
            .field("id", &self.parameter.id)
            .field("name", &self.parameter.name)
            .field("ordinal", &self.parameter.ordinal)
            .field("location", &self.parameter.location)
            .finish()
    }
}

impl CheckedStandardApplicationParameter<'_> {
    /// Returns the checked parameter identity.
    pub const fn id(&self) -> CheckedParameterId {
        self.parameter.id
    }

    /// Returns the parameter name.
    pub fn name(&self) -> &str {
        &self.parameter.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.parameter.ordinal
    }

    /// Returns the canonical public type use for this parameter's written slot.
    ///
    /// Standard and application value uses carry their checked identities. A standard
    /// declaration's compatibility [`SemanticType::Scalar`] is not a source-name or `TypeId`
    /// authority.
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse {
        self.bundle.type_use(CheckedTypeUseKind::Parameter {
            owner: self.owner,
            parameter: self.parameter.id,
        })
    }

    /// Returns the complete parameter declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.parameter.location
    }
}

/// A scalar-free borrowed SERVER return-column view.
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationReturnColumn<'a> {
    bundle: &'a CheckedStandardApplicationBundle,
    owner: CheckedFunctionId,
    column: &'a CheckedServerFunctionReturnColumn,
}

impl fmt::Debug for CheckedStandardApplicationReturnColumn<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedStandardApplicationReturnColumn")
            .field("name", &self.column.name)
            .field("ordinal", &self.column.ordinal)
            .field("location", &self.column.location)
            .finish()
    }
}

impl CheckedStandardApplicationReturnColumn<'_> {
    /// Returns the return-column name.
    pub fn name(&self) -> &str {
        &self.column.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.column.ordinal
    }

    /// Returns the canonical public type use for this return column's written slot.
    ///
    /// Standard and application value uses carry their checked identities. A standard
    /// declaration's compatibility [`SemanticType::Scalar`] is not a source-name or `TypeId`
    /// authority.
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse {
        self.bundle.type_use(CheckedTypeUseKind::Return {
            owner: self.owner,
            ordinal: self.column.ordinal,
        })
    }

    /// Returns the complete return-column declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.column.location
    }
}

/// The result of parsing and checking a source bundle.
#[derive(Clone, Debug)]
pub struct CheckReport {
    pub(super) parse_report: ParseReport,
    pub(super) diagnostics: Vec<CompilerDiagnostic>,
    pub(super) checked_bundle: Option<CheckedBundle>,
}

impl CheckReport {
    /// Returns the retained parse report on both success and failure.
    pub fn parse_report(&self) -> &ParseReport {
        &self.parse_report
    }
    /// Returns syntax and semantic diagnostics in source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
    /// Returns whether checking produced any error-level diagnostics.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(CompilerDiagnostic::is_error)
    }
    /// Returns the number of error-level diagnostics.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .count()
    }
    /// Returns the number of warning-level diagnostics.
    pub fn warning_count(&self) -> usize {
        self.diagnostics.len() - self.error_count()
    }
    /// Returns checked Orna-owned definitions when checking succeeds.
    pub fn checked_bundle(&self) -> Option<&CheckedBundle> {
        self.checked_bundle.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn replace_checked_field_facts_for_test(
        &mut self,
        owner: CheckedTypeId,
        field: CheckedFieldId,
        semantic_type: SemanticType<CheckedTypeId>,
        nullable: bool,
        unique: bool,
    ) -> bool {
        let Some(checked) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(field) = checked
            .object_types
            .iter_mut()
            .find(|object_type| object_type.id == owner)
            .and_then(|object_type| {
                object_type
                    .fields
                    .iter_mut()
                    .find(|candidate| candidate.id == field)
            })
        else {
            return false;
        };

        field.semantic_type = semantic_type;
        field.nullable = nullable;
        field.unique = unique;
        true
    }
}
