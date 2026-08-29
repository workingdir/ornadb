//! Canonical `orna.server-mutation-plan` artifact formats.
//!
//! The format carries only stable Orna identities and the closed expression
//! set needed by the single-object `INSERT`, `UPDATE`, and `DELETE` execution
//! slices. It contains no source names, SQL, PostgreSQL names, runtime object
//! identities, or source locations.
//!
//! Version 1 encodes one scalar insert target, an ordered field-assignment list,
//! and the returned object type. Version 2 encodes one update target, its
//! selector parameter, the ordered field assignments, and the returned object
//! type. Version 3 encodes one delete target and its selector parameter in a
//! separate plan model because DELETE has a fixed BOOLEAN result and no
//! assignments. Version 4 extends INSERT with one nominal record-constructor
//! expression. Catalogue-dependent field, parameter, and result checks remain
//! outside this artifact boundary.

use std::fmt;

use orna_core::{
    FieldId, FunctionId, ParameterId, TypeId,
    types::{ResolvedType, StandardScalar},
};

use crate::artifact_codec::{DecodeError, Reader, Writer};
mod codec;
use codec::{validate_expression, validate_record_field_type, validate_supported_type};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.server-mutation-plan";
/// The Orna language version whose semantics this artifact version executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The version used by INSERT artifacts.
///
/// This name is retained for source compatibility with the first format. New
/// code that handles more than INSERT should select
/// [`INSERT_FORMAT_VERSION`], [`UPDATE_FORMAT_VERSION`], or
/// [`DELETE_FORMAT_VERSION`] explicitly and use [`ServerDeletePlan`] for the
/// distinct DELETE result shape.
pub const FORMAT_VERSION: u32 = 1;
/// The version used by INSERT artifacts.
pub const INSERT_FORMAT_VERSION: u32 = FORMAT_VERSION;
/// The version used by UPDATE artifacts.
pub const UPDATE_FORMAT_VERSION: u32 = 2;
/// The version used by DELETE artifacts.
pub const DELETE_FORMAT_VERSION: u32 = 3;
/// The version used by INSERT artifacts that contain a record constructor.
pub const RECORD_INSERT_FORMAT_VERSION: u32 = 4;
/// The exact first eight bytes of every server-mutation-plan artifact.
pub const MAGIC: [u8; 8] = *b"ORNAMP\0\0";
/// The maximum number of field assignments in one mutation plan.
pub const MAX_ASSIGNMENTS: u32 = 1_024;
/// The maximum number of fields in one record constructor.
pub const MAX_RECORD_FIELDS: u32 = 1_024;
/// The maximum accepted encoded artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

const INSERT_OPERATION_TAG: u8 = 1;
const UPDATE_OPERATION_TAG: u8 = 2;
const DELETE_OPERATION_TAG: u8 = 3;
const PARAMETER_EXPRESSION_TAG: u8 = 1;
const BOOLEAN_EXPRESSION_TAG: u8 = 2;
const TYPED_NULL_EXPRESSION_TAG: u8 = 3;
const RECORD_CONSTRUCTOR_EXPRESSION_TAG: u8 = 4;
const SCALAR_TYPE_TAG: u8 = 1;
const NAMED_TYPE_TAG: u8 = 2;
const REFERENCE_TYPE_TAG: u8 = 3;
const BOOLEAN_SCALAR_TAG: u8 = 1;
const INTEGER_SCALAR_TAG: u8 = 2;
const BIGINT_SCALAR_TAG: u8 = 3;
const FLOAT_SCALAR_TAG: u8 = 4;
const CLOB_SCALAR_TAG: u8 = 6;
const BLOB_SCALAR_TAG: u8 = 7;

/// A checked single-object mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerMutationPlan {
    operation: ServerMutationOperation,
    target: TypeId,
    assignments: Vec<FieldAssignment>,
    returned_object: TypeId,
}

/// A checked version-3 single-object DELETE plan.
///
/// DELETE is deliberately separate from [`ServerMutationPlan`]: it has no
/// field assignments or returned object type, and its operation fixes the
/// public result to zero or one BOOLEAN value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerDeletePlan {
    target: TypeId,
    selector: MutationSelector,
}

/// The operation represented by a server mutation plan.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerMutationOperation {
    /// A version-1 single-row insert.
    Insert,
    /// A version-2 single-object update.
    Update {
        /// The parameter that supplies the target object identity.
        selector: MutationSelector,
    },
}

/// The owner-qualified parameter that selects one object for UPDATE or DELETE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationSelector {
    owner: FunctionId,
    parameter: ParameterId,
}

impl MutationSelector {
    /// Creates an owner-qualified selector parameter.
    pub const fn new(owner: FunctionId, parameter: ParameterId) -> Self {
        Self { owner, parameter }
    }

    /// Returns the function that owns the selector parameter.
    pub const fn owner(self) -> FunctionId {
        self.owner
    }

    /// Returns the selector parameter identity.
    pub const fn parameter(self) -> ParameterId {
        self.parameter
    }
}

/// One positional target-field assignment in a mutation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldAssignment {
    owner: TypeId,
    field: FieldId,
    expression: MutationExpression,
}

impl FieldAssignment {
    /// Creates an assignment. Plan construction performs cross-assignment
    /// validation such as owner, duplicate, and parameter-owner checks.
    pub const fn new(owner: TypeId, field: FieldId, expression: MutationExpression) -> Self {
        Self {
            owner,
            field,
            expression,
        }
    }

    /// Returns the object type that owns the assigned field.
    pub const fn owner(&self) -> TypeId {
        self.owner
    }

    /// Returns the assigned field identity.
    pub const fn field(&self) -> FieldId {
        self.field
    }

    /// Returns the checked value expression.
    pub const fn expression(&self) -> &MutationExpression {
        &self.expression
    }
}

/// One closed mutation assignment value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationExpression {
    kind: MutationExpressionKind,
    resolved_type: ResolvedType,
    nullable: bool,
}

impl MutationExpression {
    /// Creates a non-null parameter read.
    pub fn parameter(
        owner: FunctionId,
        parameter: ParameterId,
        resolved_type: ResolvedType,
    ) -> Result<Self, ServerMutationPlanError> {
        validate_supported_type(resolved_type)?;
        Ok(Self {
            kind: MutationExpressionKind::Parameter { owner, parameter },
            resolved_type,
            nullable: false,
        })
    }

    /// Creates a non-null BOOLEAN literal.
    pub const fn boolean_literal(value: bool) -> Self {
        Self {
            kind: MutationExpressionKind::BooleanLiteral { value },
            resolved_type: ResolvedType::Scalar(StandardScalar::Boolean),
            nullable: false,
        }
    }

    /// Creates a typed nullable NULL value.
    pub fn typed_null(resolved_type: ResolvedType) -> Result<Self, ServerMutationPlanError> {
        validate_supported_type(resolved_type)?;
        Ok(Self {
            kind: MutationExpressionKind::TypedNull,
            resolved_type,
            nullable: true,
        })
    }

    /// Creates one non-null nominal record constructor.
    pub fn record_constructor(
        record_type: TypeId,
        fields: impl IntoIterator<Item = RecordFieldExpression>,
    ) -> Result<Self, ServerMutationPlanError> {
        let fields = collect_record_fields(fields)?;
        let expression = Self {
            kind: MutationExpressionKind::RecordConstructor { fields },
            resolved_type: ResolvedType::named(record_type),
            nullable: false,
        };
        validate_expression(&expression, true)?;
        Ok(expression)
    }

    /// Returns the expression's closed semantic kind.
    pub const fn kind(&self) -> &MutationExpressionKind {
        &self.kind
    }

    /// Returns the expression's resolved semantic type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    /// Returns whether this expression may evaluate to NULL.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// The closed expression kinds accepted by mutation-plan versions 1, 2, and 4.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationExpressionKind {
    /// A declared non-null function parameter read.
    Parameter {
        /// The owning function identity.
        owner: FunctionId,
        /// The declared parameter identity.
        parameter: ParameterId,
    },
    /// A non-null BOOLEAN literal.
    BooleanLiteral {
        /// The literal value.
        value: bool,
    },
    /// A contextually typed NULL literal.
    TypedNull,
    /// One nominal record constructor with fields in declaration order.
    RecordConstructor {
        /// Owner-qualified fields in record declaration order.
        fields: Vec<RecordFieldExpression>,
    },
}

/// One owner-qualified field expression inside a record constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordFieldExpression {
    owner: TypeId,
    field: FieldId,
    kind: RecordFieldExpressionKind,
    resolved_type: ResolvedType,
}

impl RecordFieldExpression {
    /// Creates one non-null parameter field.
    pub fn parameter(
        owner: TypeId,
        field: FieldId,
        function: FunctionId,
        parameter: ParameterId,
        resolved_type: ResolvedType,
    ) -> Result<Self, ServerMutationPlanError> {
        validate_record_field_type(resolved_type)?;
        Ok(Self {
            owner,
            field,
            kind: RecordFieldExpressionKind::Parameter {
                owner: function,
                parameter,
            },
            resolved_type,
        })
    }

    /// Creates one non-null Boolean-literal field.
    pub const fn boolean_literal(owner: TypeId, field: FieldId, value: bool) -> Self {
        Self {
            owner,
            field,
            kind: RecordFieldExpressionKind::BooleanLiteral { value },
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
        }
    }

    /// Returns the nominal record type that owns this field.
    pub const fn owner(&self) -> TypeId {
        self.owner
    }

    /// Returns the stable field identity.
    pub const fn field(&self) -> FieldId {
        self.field
    }

    /// Returns the closed child-expression kind.
    pub const fn kind(&self) -> &RecordFieldExpressionKind {
        &self.kind
    }

    /// Returns the child expression's resolved type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
}

/// The closed child expressions accepted by a record constructor artifact.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordFieldExpressionKind {
    /// A declared non-null function parameter read.
    Parameter {
        /// The owning function identity.
        owner: FunctionId,
        /// The declared parameter identity.
        parameter: ParameterId,
    },
    /// A non-null Boolean literal.
    BooleanLiteral {
        /// The literal value.
        value: bool,
    },
}

fn collect_record_fields(
    fields: impl IntoIterator<Item = RecordFieldExpression>,
) -> Result<Vec<RecordFieldExpression>, ServerMutationPlanError> {
    let mut collected = Vec::new();
    for (index, field) in fields.into_iter().enumerate() {
        let count = index.saturating_add(1);
        if count > MAX_RECORD_FIELDS as usize {
            return Err(ServerMutationPlanError::CollectionLimit {
                kind: "record fields",
                count,
                maximum: MAX_RECORD_FIELDS,
            });
        }
        collected.push(field);
    }
    if collected.is_empty() {
        return Err(ServerMutationPlanError::EmptyRecordFields);
    }
    Ok(collected)
}

/// An error returned when a mutation plan cannot be validated or decoded.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerMutationPlanError {
    /// The artifact does not start with the mutation-plan magic bytes.
    InvalidMagic,
    /// The artifact version is not supported.
    UnsupportedVersion(u32),
    /// The encoded version does not match the expression set.
    NonCanonicalFormatVersion {
        /// The version selected by the decoded plan.
        expected: u32,
        /// The version carried by the artifact.
        actual: u32,
    },
    /// An enum tag is not defined by this format version.
    InvalidEnumTag {
        /// The encoded enum category.
        kind: &'static str,
        /// The encoded tag.
        tag: u8,
    },
    /// A boolean byte was not zero or one.
    InvalidBoolean {
        /// The encoded boolean category.
        context: &'static str,
        /// The encoded byte.
        value: u8,
    },
    /// A collection exceeds the format limit.
    CollectionLimit {
        /// The collection category.
        kind: &'static str,
        /// The supplied count.
        count: usize,
        /// The largest valid count.
        maximum: u32,
    },
    /// The encoded artifact exceeds the format byte limit.
    ArtifactSizeLimit {
        /// The supplied artifact size.
        size: usize,
        /// The largest accepted artifact size.
        maximum: usize,
    },
    /// A mutation plan must contain at least one assignment.
    EmptyAssignments,
    /// A record constructor must contain at least one field.
    EmptyRecordFields,
    /// A record constructor is not accepted by UPDATE.
    RecordConstructorRequiresInsert,
    /// A record field owner differs from the outer record type.
    RecordFieldOwnerMismatch {
        /// The field's zero-based declaration position.
        position: usize,
        /// The outer record type.
        expected: TypeId,
        /// The field's supplied owner.
        actual: TypeId,
    },
    /// Two record entries address the same owner-qualified field.
    DuplicateRecordField {
        /// The first field's zero-based position.
        first: usize,
        /// The duplicate field's zero-based position.
        duplicate: usize,
        /// The record owner identity.
        owner: TypeId,
        /// The duplicate field identity.
        field: FieldId,
    },
    /// An assignment owner differs from the mutation target.
    AssignmentOwnerMismatch {
        /// The assignment's zero-based position.
        assignment: usize,
        /// The insert target identity.
        target: TypeId,
        /// The assignment owner identity.
        owner: TypeId,
    },
    /// Two assignments address the same owner-qualified field.
    DuplicateFieldAssignment {
        /// The first assignment's zero-based position.
        first: usize,
        /// The duplicate assignment's zero-based position.
        duplicate: usize,
        /// The field owner identity.
        owner: TypeId,
        /// The duplicate field identity.
        field: FieldId,
    },
    /// Parameter reads do not all belong to one function.
    MixedParameterOwners {
        /// The first parameter assignment's position.
        first: usize,
        /// The mixed-owner assignment's position.
        assignment: usize,
        /// The first parameter owner.
        expected: FunctionId,
        /// The mixed parameter owner.
        actual: FunctionId,
    },
    /// An UPDATE assignment reads a parameter from another function.
    SelectorParameterOwnerMismatch {
        /// The assignment's zero-based position.
        assignment: usize,
        /// The function that owns the selector parameter.
        selector_owner: FunctionId,
        /// The function that owns the assignment parameter.
        assignment_owner: FunctionId,
    },
    /// A parameter, typed NULL, or record child uses a type outside the closed set.
    UnsupportedValueType {
        /// The rejected resolved type.
        resolved_type: ResolvedType,
    },
    /// An expression carries a type different from its fixed kind type.
    ExpressionTypeMismatch {
        /// The expression kind being checked.
        expression_kind: &'static str,
        /// The required type.
        expected: ResolvedType,
        /// The supplied type.
        actual: ResolvedType,
    },
    /// An expression carries the wrong nullability.
    ExpressionNullabilityMismatch {
        /// The expression kind being checked.
        expression_kind: &'static str,
        /// The required nullability.
        expected: bool,
        /// The supplied nullability.
        actual: bool,
    },
    /// The returned reference target differs from the mutation target.
    ReturnedObjectMismatch {
        /// The insert target identity.
        target: TypeId,
        /// The returned reference target identity.
        returned: TypeId,
    },
    /// The artifact ends before a complete value can be read.
    Truncated,
    /// The artifact contains bytes after a complete plan.
    TrailingBytes,
}

impl From<DecodeError> for ServerMutationPlanError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Truncated => Self::Truncated,
            DecodeError::TrailingBytes => Self::TrailingBytes,
        }
    }
}

impl fmt::Display for ServerMutationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => {
                formatter.write_str("invalid orna.server-mutation-plan artifact magic")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.server-mutation-plan artifact version {version}"
            ),
            Self::NonCanonicalFormatVersion { expected, actual } => write!(
                formatter,
                "server mutation plan version {actual} is not canonical for content requiring version {expected}"
            ),
            Self::InvalidEnumTag { kind, tag } => write!(formatter, "invalid {kind} tag {tag}"),
            Self::InvalidBoolean { context, value } => {
                write!(formatter, "invalid {context} boolean byte {value}")
            }
            Self::CollectionLimit {
                kind,
                count,
                maximum,
            } => write!(
                formatter,
                "{kind} count {count} exceeds the limit {maximum}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "server mutation plan artifact size {size} exceeds the limit {maximum}"
            ),
            Self::EmptyAssignments => {
                formatter.write_str("a server mutation plan must contain at least one assignment")
            }
            Self::EmptyRecordFields => {
                formatter.write_str("a record constructor must contain at least one field")
            }
            Self::RecordConstructorRequiresInsert => {
                formatter.write_str("record constructors are accepted only by INSERT plans")
            }
            Self::RecordFieldOwnerMismatch {
                position,
                expected,
                actual,
            } => write!(
                formatter,
                "record field {position} owner {actual} differs from record type {expected}"
            ),
            Self::DuplicateRecordField {
                first,
                duplicate,
                owner,
                field,
            } => write!(
                formatter,
                "record field {duplicate} duplicates owner-qualified field {owner}.{field} from field {first}"
            ),
            Self::AssignmentOwnerMismatch {
                assignment,
                target,
                owner,
            } => write!(
                formatter,
                "assignment {assignment} owner {owner} differs from mutation target {target}"
            ),
            Self::DuplicateFieldAssignment {
                first,
                duplicate,
                owner,
                field,
            } => write!(
                formatter,
                "assignment {duplicate} duplicates owner-qualified field {owner}.{field} from assignment {first}"
            ),
            Self::MixedParameterOwners {
                first,
                assignment,
                expected,
                actual,
            } => write!(
                formatter,
                "assignment {assignment} parameter owner {actual} differs from assignment {first} owner {expected}"
            ),
            Self::SelectorParameterOwnerMismatch {
                assignment,
                selector_owner,
                assignment_owner,
            } => write!(
                formatter,
                "assignment {assignment} parameter owner {assignment_owner} differs from selector owner {selector_owner}"
            ),
            Self::UnsupportedValueType { resolved_type } => {
                write!(
                    formatter,
                    "unsupported mutation value type {resolved_type:?}"
                )
            }
            Self::ExpressionTypeMismatch {
                expression_kind,
                expected,
                actual,
            } => write!(
                formatter,
                "{expression_kind} expression requires type {expected:?}, found {actual:?}"
            ),
            Self::ExpressionNullabilityMismatch {
                expression_kind,
                expected,
                actual,
            } => write!(
                formatter,
                "{expression_kind} expression requires nullable={expected}, found nullable={actual}"
            ),
            Self::ReturnedObjectMismatch { target, returned } => write!(
                formatter,
                "returned object {returned} differs from mutation target {target}"
            ),
            Self::Truncated => formatter.write_str("truncated orna.server-mutation-plan artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.server-mutation-plan artifact")
            }
        }
    }
}

impl std::error::Error for ServerMutationPlanError {}

#[cfg(test)]
#[path = "server_mutation_plan/tests.rs"]
mod tests;
