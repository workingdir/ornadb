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

impl ServerMutationPlan {
    /// Builds a checked version-1 insert plan.
    pub fn new_insert(
        target: TypeId,
        assignments: impl IntoIterator<Item = FieldAssignment>,
        returned_object: TypeId,
    ) -> Result<Self, ServerMutationPlanError> {
        let plan = Self {
            operation: ServerMutationOperation::Insert,
            target,
            assignments: collect_assignments(assignments)?,
            returned_object,
        };
        validate_plan(&plan)?;
        Ok(plan)
    }

    /// Builds a checked version-2 update plan.
    pub fn new_update(
        target: TypeId,
        selector: MutationSelector,
        assignments: impl IntoIterator<Item = FieldAssignment>,
        returned_object: TypeId,
    ) -> Result<Self, ServerMutationPlanError> {
        let plan = Self {
            operation: ServerMutationOperation::Update { selector },
            target,
            assignments: collect_assignments(assignments)?,
            returned_object,
        };
        validate_plan(&plan)?;
        Ok(plan)
    }

    /// Returns the mutation operation and any operation-specific evidence.
    pub const fn operation(&self) -> &ServerMutationOperation {
        &self.operation
    }

    /// Returns the selector for UPDATE, or `None` for INSERT.
    pub const fn selector(&self) -> Option<MutationSelector> {
        match self.operation {
            ServerMutationOperation::Insert => None,
            ServerMutationOperation::Update { selector } => Some(selector),
        }
    }

    /// Returns the canonical artifact version for this operation and expression set.
    pub fn format_version(&self) -> u32 {
        match self.operation {
            ServerMutationOperation::Insert if self.contains_record_constructor() => {
                RECORD_INSERT_FORMAT_VERSION
            }
            ServerMutationOperation::Insert => INSERT_FORMAT_VERSION,
            ServerMutationOperation::Update { .. } => UPDATE_FORMAT_VERSION,
        }
    }

    fn contains_record_constructor(&self) -> bool {
        self.assignments.iter().any(|assignment| {
            matches!(
                assignment.expression.kind,
                MutationExpressionKind::RecordConstructor { .. }
            )
        })
    }

    /// Returns the target object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns field assignments in their positional source order.
    pub fn assignments(&self) -> &[FieldAssignment] {
        &self.assignments
    }

    /// Returns the object type identity carried by the returned REF.
    pub const fn returned_object(&self) -> TypeId {
        self.returned_object
    }

    /// Encodes this checked plan into canonical bytes for its operation.
    pub fn encode(&self) -> Result<Vec<u8>, ServerMutationPlanError> {
        validate_plan(self)?;
        let mut writer = Writer::new();
        writer.bytes(&MAGIC);
        writer.u32(self.format_version());
        match self.operation {
            ServerMutationOperation::Insert => writer.u8(INSERT_OPERATION_TAG),
            ServerMutationOperation::Update { .. } => writer.u8(UPDATE_OPERATION_TAG),
        }
        writer.type_id(self.target);
        if let ServerMutationOperation::Update { selector } = self.operation {
            writer.function_id(selector.owner);
            writer.parameter_id(selector.parameter);
        }
        writer.u32(u32::try_from(self.assignments.len()).map_err(|_| {
            ServerMutationPlanError::CollectionLimit {
                kind: "assignments",
                count: self.assignments.len(),
                maximum: MAX_ASSIGNMENTS,
            }
        })?);
        for assignment in &self.assignments {
            writer.type_id(assignment.owner);
            writer.field_id(assignment.field);
            encode_expression(
                &mut writer,
                &assignment.expression,
                self.format_version() == RECORD_INSERT_FORMAT_VERSION,
            )?;
        }
        writer.type_id(self.returned_object);
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical INSERT, record INSERT, or UPDATE artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerMutationPlanError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        let version = decode_versioned_header(&mut reader)?;
        if !matches!(
            version,
            INSERT_FORMAT_VERSION | UPDATE_FORMAT_VERSION | RECORD_INSERT_FORMAT_VERSION
        ) {
            return Err(ServerMutationPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        let is_update = match version {
            INSERT_FORMAT_VERSION | RECORD_INSERT_FORMAT_VERSION
                if operation == INSERT_OPERATION_TAG =>
            {
                false
            }
            UPDATE_FORMAT_VERSION if operation == UPDATE_OPERATION_TAG => true,
            INSERT_FORMAT_VERSION | UPDATE_FORMAT_VERSION | RECORD_INSERT_FORMAT_VERSION => {
                return Err(ServerMutationPlanError::InvalidEnumTag {
                    kind: "operation",
                    tag: operation,
                });
            }
            version => return Err(ServerMutationPlanError::UnsupportedVersion(version)),
        };
        let target = reader.type_id()?;
        let selector = if is_update {
            Some(MutationSelector {
                owner: reader.function_id()?,
                parameter: reader.parameter_id()?,
            })
        } else {
            None
        };
        let assignment_count = reader.count("assignments", MAX_ASSIGNMENTS)?;
        let mut assignments = Vec::with_capacity(assignment_count);
        for _ in 0..assignment_count {
            assignments.push(FieldAssignment {
                owner: reader.type_id()?,
                field: reader.field_id()?,
                expression: decode_expression(
                    &mut reader,
                    version == RECORD_INSERT_FORMAT_VERSION,
                )?,
            });
        }
        let returned_object = reader.type_id()?;
        reader.require_finished()?;
        let plan = match selector {
            Some(selector) => Self::new_update(target, selector, assignments, returned_object),
            None => Self::new_insert(target, assignments, returned_object),
        }?;
        let expected = plan.format_version();
        if version != expected {
            return Err(ServerMutationPlanError::NonCanonicalFormatVersion {
                expected,
                actual: version,
            });
        }
        Ok(plan)
    }
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

impl ServerDeletePlan {
    /// Builds a version-3 delete plan from stable Orna identities.
    pub const fn new(target: TypeId, selector: MutationSelector) -> Self {
        Self { target, selector }
    }

    /// Returns the canonical artifact version for DELETE.
    pub const fn format_version(&self) -> u32 {
        DELETE_FORMAT_VERSION
    }

    /// Returns the target object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the parameter that supplies the deleted object identity.
    pub const fn selector(&self) -> MutationSelector {
        self.selector
    }

    /// Encodes this checked DELETE plan into canonical version-3 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ServerMutationPlanError> {
        let mut writer = Writer::new();
        writer.bytes(&MAGIC);
        writer.u32(DELETE_FORMAT_VERSION);
        writer.u8(DELETE_OPERATION_TAG);
        writer.type_id(self.target);
        writer.function_id(self.selector.owner);
        writer.parameter_id(self.selector.parameter);
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-3 DELETE artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServerMutationPlanError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        let version = decode_versioned_header(&mut reader)?;
        if version != DELETE_FORMAT_VERSION {
            return Err(ServerMutationPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != DELETE_OPERATION_TAG {
            return Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: operation,
            });
        }
        let target = reader.type_id()?;
        let selector = MutationSelector::new(reader.function_id()?, reader.parameter_id()?);
        reader.require_finished()?;
        Ok(Self::new(target, selector))
    }
}

fn decode_versioned_header(reader: &mut Reader<'_>) -> Result<u32, ServerMutationPlanError> {
    if reader.array::<8>()? != MAGIC {
        return Err(ServerMutationPlanError::InvalidMagic);
    }
    reader.u32()
}

fn collect_assignments(
    assignments: impl IntoIterator<Item = FieldAssignment>,
) -> Result<Vec<FieldAssignment>, ServerMutationPlanError> {
    let mut collected = Vec::new();
    for (index, assignment) in assignments.into_iter().enumerate() {
        let count = index.saturating_add(1);
        if count > MAX_ASSIGNMENTS as usize {
            return Err(ServerMutationPlanError::CollectionLimit {
                kind: "assignments",
                count,
                maximum: MAX_ASSIGNMENTS,
            });
        }
        collected.push(assignment);
    }
    Ok(collected)
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

fn validate_plan(plan: &ServerMutationPlan) -> Result<(), ServerMutationPlanError> {
    if plan.assignments.is_empty() {
        return Err(ServerMutationPlanError::EmptyAssignments);
    }
    if plan.assignments.len() > MAX_ASSIGNMENTS as usize {
        return Err(ServerMutationPlanError::CollectionLimit {
            kind: "assignments",
            count: plan.assignments.len(),
            maximum: MAX_ASSIGNMENTS,
        });
    }
    if plan.target != plan.returned_object {
        return Err(ServerMutationPlanError::ReturnedObjectMismatch {
            target: plan.target,
            returned: plan.returned_object,
        });
    }
    let selector_owner = match plan.operation {
        ServerMutationOperation::Insert => None,
        ServerMutationOperation::Update { selector } => Some(selector.owner),
    };
    if selector_owner.is_some() && plan.contains_record_constructor() {
        return Err(ServerMutationPlanError::RecordConstructorRequiresInsert);
    }
    let mut first_parameter_owner = None;
    let mut first_parameter_assignment = 0;
    for (index, assignment) in plan.assignments.iter().enumerate() {
        if assignment.owner != plan.target {
            return Err(ServerMutationPlanError::AssignmentOwnerMismatch {
                assignment: index,
                target: plan.target,
                owner: assignment.owner,
            });
        }
        if let Some((first, _)) =
            plan.assignments
                .iter()
                .enumerate()
                .take(index)
                .find(|(_, previous)| {
                    previous.owner == assignment.owner && previous.field == assignment.field
                })
        {
            return Err(ServerMutationPlanError::DuplicateFieldAssignment {
                first,
                duplicate: index,
                owner: assignment.owner,
                field: assignment.field,
            });
        }
        validate_expression(
            &assignment.expression,
            matches!(plan.operation, ServerMutationOperation::Insert),
        )?;
        let mut parameter_owners = Vec::new();
        visit_parameter_owners(&assignment.expression, &mut |owner| {
            parameter_owners.push(owner);
        });
        for owner in parameter_owners {
            if let Some(selector_owner) = selector_owner
                && owner != selector_owner
            {
                return Err(ServerMutationPlanError::SelectorParameterOwnerMismatch {
                    assignment: index,
                    selector_owner,
                    assignment_owner: owner,
                });
            }
            match first_parameter_owner {
                None => {
                    first_parameter_owner = Some(owner);
                    first_parameter_assignment = index;
                }
                Some(expected) if expected != owner => {
                    return Err(ServerMutationPlanError::MixedParameterOwners {
                        first: first_parameter_assignment,
                        assignment: index,
                        expected,
                        actual: owner,
                    });
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

fn visit_parameter_owners(expression: &MutationExpression, visitor: &mut impl FnMut(FunctionId)) {
    match &expression.kind {
        MutationExpressionKind::Parameter { owner, .. } => visitor(*owner),
        MutationExpressionKind::RecordConstructor { fields } => {
            for field in fields {
                if let RecordFieldExpressionKind::Parameter { owner, .. } = field.kind {
                    visitor(owner);
                }
            }
        }
        MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull => {}
    }
}

fn validate_expression(
    expression: &MutationExpression,
    allow_record: bool,
) -> Result<(), ServerMutationPlanError> {
    match &expression.kind {
        MutationExpressionKind::Parameter { .. } => {
            validate_supported_type(expression.resolved_type)?;
            validate_nullability("parameter", false, expression.nullable)
        }
        MutationExpressionKind::BooleanLiteral { .. } => {
            let expected = ResolvedType::scalar(StandardScalar::Boolean);
            if !matches!(
                project_resolved_type(expression.resolved_type),
                MutationResolvedType::Scalar(StandardScalar::Boolean)
            ) {
                return Err(ServerMutationPlanError::ExpressionTypeMismatch {
                    expression_kind: "BOOLEAN literal",
                    expected,
                    actual: expression.resolved_type,
                });
            }
            validate_nullability("BOOLEAN literal", false, expression.nullable)
        }
        MutationExpressionKind::TypedNull => {
            validate_supported_type(expression.resolved_type)?;
            validate_nullability("typed NULL", true, expression.nullable)
        }
        MutationExpressionKind::RecordConstructor { fields } => {
            if !allow_record {
                return Err(ServerMutationPlanError::RecordConstructorRequiresInsert);
            }
            validate_record_constructor(expression.resolved_type, expression.nullable, fields)
        }
    }
}

fn validate_record_constructor(
    resolved_type: ResolvedType,
    nullable: bool,
    fields: &[RecordFieldExpression],
) -> Result<(), ServerMutationPlanError> {
    let Some(first) = fields.first() else {
        return Err(ServerMutationPlanError::EmptyRecordFields);
    };
    if fields.len() > MAX_RECORD_FIELDS as usize {
        return Err(ServerMutationPlanError::CollectionLimit {
            kind: "record fields",
            count: fields.len(),
            maximum: MAX_RECORD_FIELDS,
        });
    }
    let record_type = match project_resolved_type(resolved_type) {
        MutationResolvedType::Named(record_type) => record_type,
        _ => {
            return Err(ServerMutationPlanError::ExpressionTypeMismatch {
                expression_kind: "record constructor",
                expected: ResolvedType::named(first.owner),
                actual: resolved_type,
            });
        }
    };
    validate_nullability("record constructor", false, nullable)?;
    for (position, field) in fields.iter().enumerate() {
        if field.owner != record_type {
            return Err(ServerMutationPlanError::RecordFieldOwnerMismatch {
                position,
                expected: record_type,
                actual: field.owner,
            });
        }
        if let Some((first, _)) = fields
            .iter()
            .enumerate()
            .take(position)
            .find(|(_, previous)| previous.field == field.field)
        {
            return Err(ServerMutationPlanError::DuplicateRecordField {
                first,
                duplicate: position,
                owner: record_type,
                field: field.field,
            });
        }
        validate_record_field_expression(field)?;
    }
    Ok(())
}

fn validate_record_field_expression(
    field: &RecordFieldExpression,
) -> Result<(), ServerMutationPlanError> {
    match field.kind {
        RecordFieldExpressionKind::Parameter { .. } => {
            validate_record_field_type(field.resolved_type)
        }
        RecordFieldExpressionKind::BooleanLiteral { .. } => {
            let expected = ResolvedType::scalar(StandardScalar::Boolean);
            if field.resolved_type == expected {
                Ok(())
            } else {
                Err(ServerMutationPlanError::ExpressionTypeMismatch {
                    expression_kind: "record BOOLEAN literal",
                    expected,
                    actual: field.resolved_type,
                })
            }
        }
    }
}

/// The mutation-plan view of a resolved type.
///
/// Future resolved-type projections are deliberately not accepted here.
/// They remain an explicit fail-closed case until this format has a wire
/// contract for them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationResolvedType {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference(TypeId),
    Unsupported(ResolvedType),
}

const fn project_resolved_type(resolved_type: ResolvedType) -> MutationResolvedType {
    match (
        resolved_type.legacy_scalar(),
        resolved_type.named_type(),
        resolved_type.value_type(),
        resolved_type.reference_target(),
    ) {
        (Some(scalar), None, None, None) => MutationResolvedType::Scalar(scalar),
        (None, Some(type_id), None, None) => MutationResolvedType::Named(type_id),
        (None, None, None, Some(target)) => MutationResolvedType::Reference(target),
        _ => MutationResolvedType::Unsupported(resolved_type),
    }
}

fn validate_nullability(
    expression_kind: &'static str,
    expected: bool,
    actual: bool,
) -> Result<(), ServerMutationPlanError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind,
            expected,
            actual,
        })
    }
}

fn validate_supported_type(resolved_type: ResolvedType) -> Result<(), ServerMutationPlanError> {
    let supported = match project_resolved_type(resolved_type) {
        MutationResolvedType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        MutationResolvedType::Reference(_) => true,
        MutationResolvedType::Named(_) | MutationResolvedType::Unsupported(_) => false,
    };
    if supported {
        Ok(())
    } else {
        Err(ServerMutationPlanError::UnsupportedValueType { resolved_type })
    }
}

fn validate_record_field_type(resolved_type: ResolvedType) -> Result<(), ServerMutationPlanError> {
    let supported = match project_resolved_type(resolved_type) {
        MutationResolvedType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        MutationResolvedType::Named(_) => true,
        MutationResolvedType::Reference(_) | MutationResolvedType::Unsupported(_) => false,
    };
    if supported {
        Ok(())
    } else {
        Err(ServerMutationPlanError::UnsupportedValueType { resolved_type })
    }
}

fn encode_expression(
    writer: &mut Writer,
    expression: &MutationExpression,
    allow_record: bool,
) -> Result<(), ServerMutationPlanError> {
    validate_expression(expression, allow_record)?;
    let tag = match expression.kind {
        MutationExpressionKind::Parameter { .. } => PARAMETER_EXPRESSION_TAG,
        MutationExpressionKind::BooleanLiteral { .. } => BOOLEAN_EXPRESSION_TAG,
        MutationExpressionKind::TypedNull => TYPED_NULL_EXPRESSION_TAG,
        MutationExpressionKind::RecordConstructor { .. } => RECORD_CONSTRUCTOR_EXPRESSION_TAG,
    };
    writer.u8(tag);
    encode_resolved_type(
        writer,
        expression.resolved_type,
        matches!(
            expression.kind,
            MutationExpressionKind::RecordConstructor { .. }
        ),
    )?;
    writer.boolean(expression.nullable);
    match &expression.kind {
        MutationExpressionKind::Parameter { owner, parameter } => {
            writer.function_id(*owner);
            writer.parameter_id(*parameter);
        }
        MutationExpressionKind::BooleanLiteral { value } => writer.boolean(*value),
        MutationExpressionKind::TypedNull => {}
        MutationExpressionKind::RecordConstructor { fields } => {
            writer.u32(u32::try_from(fields.len()).map_err(|_| {
                ServerMutationPlanError::CollectionLimit {
                    kind: "record fields",
                    count: fields.len(),
                    maximum: MAX_RECORD_FIELDS,
                }
            })?);
            for field in fields {
                writer.type_id(field.owner);
                writer.field_id(field.field);
                encode_record_field_expression(writer, field)?;
            }
        }
    }
    Ok(())
}

fn encode_record_field_expression(
    writer: &mut Writer,
    field: &RecordFieldExpression,
) -> Result<(), ServerMutationPlanError> {
    validate_record_field_expression(field)?;
    match field.kind {
        RecordFieldExpressionKind::Parameter { owner, parameter } => {
            writer.u8(PARAMETER_EXPRESSION_TAG);
            encode_resolved_type(writer, field.resolved_type, true)?;
            writer.boolean(false);
            writer.function_id(owner);
            writer.parameter_id(parameter);
        }
        RecordFieldExpressionKind::BooleanLiteral { value } => {
            writer.u8(BOOLEAN_EXPRESSION_TAG);
            encode_resolved_type(writer, field.resolved_type, false)?;
            writer.boolean(false);
            writer.boolean(value);
        }
    }
    Ok(())
}

fn decode_expression(
    reader: &mut Reader<'_>,
    allow_record: bool,
) -> Result<MutationExpression, ServerMutationPlanError> {
    let tag = reader.u8()?;
    let resolved_type = decode_resolved_type(
        reader,
        allow_record && tag == RECORD_CONSTRUCTOR_EXPRESSION_TAG,
    )?;
    let nullable = reader.boolean("expression nullability")?;
    let kind = match tag {
        PARAMETER_EXPRESSION_TAG => MutationExpressionKind::Parameter {
            owner: reader.function_id()?,
            parameter: reader.parameter_id()?,
        },
        BOOLEAN_EXPRESSION_TAG => MutationExpressionKind::BooleanLiteral {
            value: reader.boolean("literal")?,
        },
        TYPED_NULL_EXPRESSION_TAG => MutationExpressionKind::TypedNull,
        RECORD_CONSTRUCTOR_EXPRESSION_TAG if allow_record => {
            let field_count = reader.count("record fields", MAX_RECORD_FIELDS)?;
            let mut fields = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                fields.push(decode_record_field_expression(reader)?);
            }
            MutationExpressionKind::RecordConstructor { fields }
        }
        tag => {
            return Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "mutation expression kind",
                tag,
            });
        }
    };
    let expression = MutationExpression {
        kind,
        resolved_type,
        nullable,
    };
    validate_expression(&expression, allow_record)?;
    Ok(expression)
}

fn decode_record_field_expression(
    reader: &mut Reader<'_>,
) -> Result<RecordFieldExpression, ServerMutationPlanError> {
    let owner = reader.type_id()?;
    let field = reader.field_id()?;
    let tag = reader.u8()?;
    let resolved_type = decode_resolved_type(reader, tag == PARAMETER_EXPRESSION_TAG)?;
    let nullable = reader.boolean("record field nullability")?;
    validate_nullability("record field", false, nullable)?;
    let kind = match tag {
        PARAMETER_EXPRESSION_TAG => RecordFieldExpressionKind::Parameter {
            owner: reader.function_id()?,
            parameter: reader.parameter_id()?,
        },
        BOOLEAN_EXPRESSION_TAG => RecordFieldExpressionKind::BooleanLiteral {
            value: reader.boolean("record literal")?,
        },
        tag => {
            return Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "record field expression kind",
                tag,
            });
        }
    };
    let expression = RecordFieldExpression {
        owner,
        field,
        kind,
        resolved_type,
    };
    validate_record_field_expression(&expression)?;
    Ok(expression)
}

fn encode_resolved_type(
    writer: &mut Writer,
    resolved_type: ResolvedType,
    allow_named: bool,
) -> Result<(), ServerMutationPlanError> {
    match project_resolved_type(resolved_type) {
        MutationResolvedType::Scalar(scalar) => {
            writer.u8(SCALAR_TYPE_TAG);
            writer.u8(encode_scalar(scalar)?);
        }
        MutationResolvedType::Reference(target) => {
            writer.u8(REFERENCE_TYPE_TAG);
            writer.type_id(target);
        }
        MutationResolvedType::Named(type_id) if allow_named => {
            writer.u8(NAMED_TYPE_TAG);
            writer.type_id(type_id);
        }
        MutationResolvedType::Named(_) | MutationResolvedType::Unsupported(_) => {
            return Err(ServerMutationPlanError::UnsupportedValueType { resolved_type });
        }
    }
    Ok(())
}

fn decode_resolved_type(
    reader: &mut Reader<'_>,
    allow_named: bool,
) -> Result<ResolvedType, ServerMutationPlanError> {
    match reader.u8()? {
        SCALAR_TYPE_TAG => Ok(ResolvedType::scalar(decode_scalar(reader.u8()?)?)),
        NAMED_TYPE_TAG if allow_named => Ok(ResolvedType::named(reader.type_id()?)),
        REFERENCE_TYPE_TAG => Ok(ResolvedType::reference(reader.type_id()?)),
        tag => Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "resolved type kind",
            tag,
        }),
    }
}

fn encode_scalar(scalar: StandardScalar) -> Result<u8, ServerMutationPlanError> {
    let tag = match scalar {
        StandardScalar::Boolean => BOOLEAN_SCALAR_TAG,
        StandardScalar::Integer => INTEGER_SCALAR_TAG,
        StandardScalar::BigInt => BIGINT_SCALAR_TAG,
        StandardScalar::Float => FLOAT_SCALAR_TAG,
        StandardScalar::CharacterLargeObject => CLOB_SCALAR_TAG,
        StandardScalar::BinaryLargeObject => BLOB_SCALAR_TAG,
        _ => {
            return Err(ServerMutationPlanError::UnsupportedValueType {
                resolved_type: ResolvedType::scalar(scalar),
            });
        }
    };
    Ok(tag)
}

fn decode_scalar(tag: u8) -> Result<StandardScalar, ServerMutationPlanError> {
    match tag {
        BOOLEAN_SCALAR_TAG => Ok(StandardScalar::Boolean),
        INTEGER_SCALAR_TAG => Ok(StandardScalar::Integer),
        BIGINT_SCALAR_TAG => Ok(StandardScalar::BigInt),
        FLOAT_SCALAR_TAG => Ok(StandardScalar::Float),
        CLOB_SCALAR_TAG => Ok(StandardScalar::CharacterLargeObject),
        BLOB_SCALAR_TAG => Ok(StandardScalar::BinaryLargeObject),
        tag => Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "scalar type",
            tag,
        }),
    }
}

fn validate_artifact_size(size: usize) -> Result<(), ServerMutationPlanError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(ServerMutationPlanError::ArtifactSizeLimit {
            size,
            maximum: MAX_ARTIFACT_BYTES,
        })
    } else {
        Ok(())
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn boolean(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn type_id(&mut self, value: TypeId) {
        self.bytes(&value.to_bytes());
    }

    fn field_id(&mut self, value: FieldId) {
        self.bytes(&value.to_bytes());
    }

    fn function_id(&mut self, value: FunctionId) {
        self.bytes(&value.to_bytes());
    }

    fn parameter_id(&mut self, value: ParameterId) {
        self.bytes(&value.to_bytes());
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ServerMutationPlanError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ServerMutationPlanError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(ServerMutationPlanError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], ServerMutationPlanError> {
        self.take(LENGTH)?
            .try_into()
            .map_err(|_| ServerMutationPlanError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, ServerMutationPlanError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ServerMutationPlanError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn boolean(&mut self, context: &'static str) -> Result<bool, ServerMutationPlanError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(ServerMutationPlanError::InvalidBoolean { context, value }),
        }
    }

    fn count(
        &mut self,
        kind: &'static str,
        maximum: u32,
    ) -> Result<usize, ServerMutationPlanError> {
        let count = self.u32()? as usize;
        if count > maximum as usize {
            return Err(ServerMutationPlanError::CollectionLimit {
                kind,
                count,
                maximum,
            });
        }
        Ok(count)
    }

    fn type_id(&mut self) -> Result<TypeId, ServerMutationPlanError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    fn field_id(&mut self) -> Result<FieldId, ServerMutationPlanError> {
        Ok(FieldId::from_bytes(self.array()?))
    }

    fn function_id(&mut self) -> Result<FunctionId, ServerMutationPlanError> {
        Ok(FunctionId::from_bytes(self.array()?))
    }

    fn parameter_id(&mut self) -> Result<ParameterId, ServerMutationPlanError> {
        Ok(ParameterId::from_bytes(self.array()?))
    }

    fn require_finished(&self) -> Result<(), ServerMutationPlanError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ServerMutationPlanError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: TypeId = TypeId::from_bytes([1; 16]);
    const OTHER_TARGET: TypeId = TypeId::from_bytes([2; 16]);
    const FIELD_A: FieldId = FieldId::from_bytes([3; 16]);
    const FIELD_B: FieldId = FieldId::from_bytes([4; 16]);
    const FUNCTION_A: FunctionId = FunctionId::from_bytes([5; 16]);
    const FUNCTION_B: FunctionId = FunctionId::from_bytes([6; 16]);
    const PARAMETER_A: ParameterId = ParameterId::from_bytes([7; 16]);
    const PARAMETER_B: ParameterId = ParameterId::from_bytes([8; 16]);
    const REF_TARGET: TypeId = TypeId::from_bytes([9; 16]);
    const RECORD_TYPE: TypeId = TypeId::from_bytes([10; 16]);
    const RECORD_FIELD_A: FieldId = FieldId::from_bytes([11; 16]);
    const RECORD_FIELD_B: FieldId = FieldId::from_bytes([12; 16]);
    const ENUM_TYPE: TypeId = TypeId::from_bytes([13; 16]);

    fn assignment(field: FieldId, expression: MutationExpression) -> FieldAssignment {
        FieldAssignment::new(TARGET, field, expression)
    }

    fn plan_with(
        expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
    ) -> ServerMutationPlan {
        mutation_plan_with(None, expressions)
    }

    fn update_plan_with(
        expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
    ) -> ServerMutationPlan {
        mutation_plan_with(
            Some(MutationSelector::new(FUNCTION_A, PARAMETER_A)),
            expressions,
        )
    }

    fn mutation_plan_with(
        selector: Option<MutationSelector>,
        expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
    ) -> ServerMutationPlan {
        let assignments = expressions
            .into_iter()
            .map(|(field, expression)| assignment(field, expression));
        match selector {
            Some(selector) => {
                ServerMutationPlan::new_update(TARGET, selector, assignments, TARGET).unwrap()
            }
            None => ServerMutationPlan::new_insert(TARGET, assignments, TARGET).unwrap(),
        }
    }

    fn record_expression() -> MutationExpression {
        MutationExpression::record_constructor(
            RECORD_TYPE,
            [
                RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, true),
                RecordFieldExpression::parameter(
                    RECORD_TYPE,
                    RECORD_FIELD_B,
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::named(ENUM_TYPE),
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn round_trips_all_expression_kinds_and_supported_types() {
        let expressions = [
            (FIELD_A, MutationExpression::boolean_literal(true)),
            (
                FIELD_B,
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::scalar(StandardScalar::Integer),
                )
                .unwrap(),
            ),
            (
                FieldId::from_bytes([10; 16]),
                MutationExpression::typed_null(ResolvedType::reference(REF_TARGET)).unwrap(),
            ),
            (
                FieldId::from_bytes([11; 16]),
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_B,
                    ResolvedType::scalar(StandardScalar::BigInt),
                )
                .unwrap(),
            ),
            (
                FieldId::from_bytes([12; 16]),
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::scalar(StandardScalar::Float),
                )
                .unwrap(),
            ),
            (
                FieldId::from_bytes([13; 16]),
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_B,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            ),
            (
                FieldId::from_bytes([14; 16]),
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                )
                .unwrap(),
            ),
            (
                FieldId::from_bytes([15; 16]),
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_B,
                    ResolvedType::reference(REF_TARGET),
                )
                .unwrap(),
            ),
        ];
        let plan = plan_with(expressions);
        let encoded = plan.encode().unwrap();
        let decoded = ServerMutationPlan::decode(&encoded).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(decoded.encode(), Ok(encoded));
        assert_eq!(decoded.target(), TARGET);
        assert_eq!(decoded.returned_object(), TARGET);
        assert_eq!(decoded.assignments()[1].owner(), TARGET);
        assert_eq!(decoded.assignments()[1].field(), FIELD_B);
        assert_eq!(
            decoded.assignments()[1].expression().resolved_type(),
            ResolvedType::scalar(StandardScalar::Integer)
        );
        assert!(!decoded.assignments()[1].expression().nullable());
    }

    #[test]
    fn round_trips_update_with_exact_selector_and_operation() {
        let plan = update_plan_with([
            (
                FIELD_A,
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_B,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            ),
            (FIELD_B, MutationExpression::boolean_literal(false)),
        ]);

        let encoded = plan.encode().unwrap();
        let decoded = ServerMutationPlan::decode(&encoded).unwrap();

        assert_eq!(decoded, plan);
        assert_eq!(decoded.encode(), Ok(encoded));
        assert_eq!(decoded.format_version(), UPDATE_FORMAT_VERSION);
        assert_eq!(
            decoded.operation(),
            &ServerMutationOperation::Update {
                selector: MutationSelector::new(FUNCTION_A, PARAMETER_A)
            }
        );
        assert_eq!(
            decoded.selector(),
            Some(MutationSelector::new(FUNCTION_A, PARAMETER_A))
        );
        assert_eq!(decoded.selector().unwrap().owner(), FUNCTION_A);
        assert_eq!(decoded.selector().unwrap().parameter(), PARAMETER_A);
    }

    #[test]
    fn record_constructor_insert_has_exact_version_four_bytes_and_round_trips() {
        let plan = plan_with([(FIELD_A, record_expression())]);
        let encoded = plan.encode().unwrap();

        assert_eq!(plan.format_version(), RECORD_INSERT_FORMAT_VERSION);
        assert_eq!(
            encoded,
            vec![
                79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 2, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
                10, 10, 10, 10, 10, 0, 0, 0, 0, 2, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
                10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 2,
                1, 1, 0, 1, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 12, 12,
                12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 1, 2, 13, 13, 13, 13, 13,
                13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 0, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1,
            ]
        );

        let decoded = ServerMutationPlan::decode(&encoded).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(decoded.encode(), Ok(encoded));
        let MutationExpressionKind::RecordConstructor { fields } =
            decoded.assignments()[0].expression().kind()
        else {
            panic!("version-4 assignment must retain its record constructor");
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].owner(), RECORD_TYPE);
        assert_eq!(fields[0].field(), RECORD_FIELD_A);
        assert_eq!(fields[1].resolved_type(), ResolvedType::named(ENUM_TYPE));
        assert!(matches!(
            fields[1].kind(),
            RecordFieldExpressionKind::Parameter {
                owner: FUNCTION_A,
                parameter: PARAMETER_A,
            }
        ));
    }

    #[test]
    fn record_constructor_versions_and_structure_fail_closed() {
        let valid = plan_with([(FIELD_A, record_expression())])
            .encode()
            .unwrap();

        let mut legacy_version = valid.clone();
        legacy_version[8..12].copy_from_slice(&INSERT_FORMAT_VERSION.to_be_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&legacy_version),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "resolved type kind",
                tag: NAMED_TYPE_TAG,
            })
        );

        let mut wrong_operation = valid.clone();
        wrong_operation[12] = UPDATE_OPERATION_TAG;
        assert_eq!(
            ServerMutationPlan::decode(&wrong_operation),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: UPDATE_OPERATION_TAG,
            })
        );

        let mut noncanonical = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
            .encode()
            .unwrap();
        noncanonical[8..12].copy_from_slice(&RECORD_INSERT_FORMAT_VERSION.to_be_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&noncanonical),
            Err(ServerMutationPlanError::NonCanonicalFormatVersion {
                expected: INSERT_FORMAT_VERSION,
                actual: RECORD_INSERT_FORMAT_VERSION,
            })
        );

        let mut empty = valid.clone();
        empty[84..88].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&empty),
            Err(ServerMutationPlanError::EmptyRecordFields)
        );

        let mut over_count = valid.clone();
        over_count[84..88].copy_from_slice(&(MAX_RECORD_FIELDS + 1).to_be_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&over_count),
            Err(ServerMutationPlanError::CollectionLimit {
                kind: "record fields",
                count: MAX_RECORD_FIELDS as usize + 1,
                maximum: MAX_RECORD_FIELDS,
            })
        );

        let mut wrong_owner = valid.clone();
        wrong_owner[88..104].copy_from_slice(&OTHER_TARGET.to_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&wrong_owner),
            Err(ServerMutationPlanError::RecordFieldOwnerMismatch {
                position: 0,
                expected: RECORD_TYPE,
                actual: OTHER_TARGET,
            })
        );

        let mut duplicate = valid.clone();
        duplicate[141..157].copy_from_slice(&RECORD_FIELD_A.to_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&duplicate),
            Err(ServerMutationPlanError::DuplicateRecordField {
                first: 0,
                duplicate: 1,
                owner: RECORD_TYPE,
                field: RECORD_FIELD_A,
            })
        );

        let mut child_kind = valid.clone();
        child_kind[120] = TYPED_NULL_EXPRESSION_TAG;
        assert_eq!(
            ServerMutationPlan::decode(&child_kind),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "record field expression kind",
                tag: TYPED_NULL_EXPRESSION_TAG,
            })
        );

        let mut child_nullable = valid.clone();
        child_nullable[123] = 1;
        assert_eq!(
            ServerMutationPlan::decode(&child_nullable),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "record field",
                expected: false,
                actual: true,
            })
        );

        let mut child_boolean = valid.clone();
        child_boolean[124] = 2;
        assert_eq!(
            ServerMutationPlan::decode(&child_boolean),
            Err(ServerMutationPlanError::InvalidBoolean {
                context: "record literal",
                value: 2,
            })
        );

        let mut outer_nullable = valid.clone();
        outer_nullable[83] = 1;
        assert_eq!(
            ServerMutationPlan::decode(&outer_nullable),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "record constructor",
                expected: false,
                actual: true,
            })
        );

        for prefix in 0..valid.len() {
            assert_eq!(
                ServerMutationPlan::decode(&valid[..prefix]),
                Err(ServerMutationPlanError::Truncated),
                "prefix {prefix}"
            );
        }
        let mut trailing = valid;
        trailing.push(0);
        assert_eq!(
            ServerMutationPlan::decode(&trailing),
            Err(ServerMutationPlanError::TrailingBytes)
        );
    }

    #[test]
    fn record_constructor_builders_enforce_the_closed_child_and_plan_shapes() {
        assert_eq!(
            MutationExpression::record_constructor(RECORD_TYPE, Vec::new()),
            Err(ServerMutationPlanError::EmptyRecordFields)
        );
        assert_eq!(
            MutationExpression::record_constructor(
                RECORD_TYPE,
                [RecordFieldExpression::boolean_literal(
                    OTHER_TARGET,
                    RECORD_FIELD_A,
                    true,
                )],
            ),
            Err(ServerMutationPlanError::RecordFieldOwnerMismatch {
                position: 0,
                expected: RECORD_TYPE,
                actual: OTHER_TARGET,
            })
        );
        assert_eq!(
            MutationExpression::record_constructor(
                RECORD_TYPE,
                [
                    RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, true,),
                    RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, false,),
                ],
            ),
            Err(ServerMutationPlanError::DuplicateRecordField {
                first: 0,
                duplicate: 1,
                owner: RECORD_TYPE,
                field: RECORD_FIELD_A,
            })
        );
        assert_eq!(
            RecordFieldExpression::parameter(
                RECORD_TYPE,
                RECORD_FIELD_A,
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::reference(REF_TARGET),
            ),
            Err(ServerMutationPlanError::UnsupportedValueType {
                resolved_type: ResolvedType::reference(REF_TARGET),
            })
        );

        let update = ServerMutationPlan::new_update(
            TARGET,
            MutationSelector::new(FUNCTION_A, PARAMETER_A),
            [assignment(FIELD_A, record_expression())],
            TARGET,
        );
        assert_eq!(
            update,
            Err(ServerMutationPlanError::RecordConstructorRequiresInsert)
        );

        let mixed_owners = MutationExpression::record_constructor(
            RECORD_TYPE,
            [
                RecordFieldExpression::parameter(
                    RECORD_TYPE,
                    RECORD_FIELD_A,
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::scalar(StandardScalar::Integer),
                )
                .unwrap(),
                RecordFieldExpression::parameter(
                    RECORD_TYPE,
                    RECORD_FIELD_B,
                    FUNCTION_B,
                    PARAMETER_B,
                    ResolvedType::named(ENUM_TYPE),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(
            ServerMutationPlan::new_insert(TARGET, [assignment(FIELD_A, mixed_owners)], TARGET,),
            Err(ServerMutationPlanError::MixedParameterOwners {
                first: 0,
                assignment: 0,
                expected: FUNCTION_A,
                actual: FUNCTION_B,
            })
        );

        let too_many = (0..=MAX_RECORD_FIELDS).map(|index| {
            RecordFieldExpression::boolean_literal(
                RECORD_TYPE,
                FieldId::from_bytes([(index % 251) as u8; 16]),
                true,
            )
        });
        assert!(matches!(
            MutationExpression::record_constructor(RECORD_TYPE, too_many),
            Err(ServerMutationPlanError::CollectionLimit {
                kind: "record fields",
                count,
                maximum: MAX_RECORD_FIELDS,
            }) if count == MAX_RECORD_FIELDS as usize + 1
        ));
    }

    #[test]
    fn round_trips_delete_with_exact_target_selector_and_version() {
        let plan = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A));

        let encoded = plan.encode().unwrap();
        let decoded = ServerDeletePlan::decode(&encoded).unwrap();

        assert_eq!(decoded, plan);
        assert_eq!(decoded.encode(), Ok(encoded));
        assert_eq!(decoded.format_version(), DELETE_FORMAT_VERSION);
        assert_eq!(decoded.target(), TARGET);
        assert_eq!(decoded.selector().owner(), FUNCTION_A);
        assert_eq!(decoded.selector().parameter(), PARAMETER_A);
    }

    #[test]
    fn encodes_minimal_boolean_golden_exactly() {
        let plan = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))]);
        assert_eq!(
            plan.encode().unwrap(),
            vec![
                79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1,
            ]
        );
    }

    #[test]
    fn encodes_minimal_update_golden_exactly() {
        let plan = update_plan_with([(FIELD_A, MutationExpression::boolean_literal(false))]);
        assert_eq!(
            plan.encode().unwrap(),
            vec![
                79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1,
            ]
        );
    }

    #[test]
    fn encodes_minimal_delete_golden_exactly() {
        let plan = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A));
        assert_eq!(
            plan.encode().unwrap(),
            vec![
                79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 3, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 7, 7, 7, 7,
            ]
        );
    }

    #[test]
    fn encodes_rich_parameter_and_typed_null_golden_exactly() {
        let plan = plan_with([
            (
                FIELD_A,
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::reference(REF_TARGET),
                )
                .unwrap(),
            ),
            (
                FIELD_B,
                MutationExpression::typed_null(ResolvedType::scalar(
                    StandardScalar::CharacterLargeObject,
                ))
                .unwrap(),
            ),
        ]);
        let encoded = plan.encode().unwrap();
        assert_eq!(&encoded[..8], b"ORNAMP\0\0");
        assert_eq!(&encoded[8..13], &[0, 0, 0, 1, 1]);
        assert_eq!(
            encoded,
            vec![
                79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 0, 0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 3, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
                9, 0, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 7, 7, 7, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 3, 1, 6, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1,
            ]
        );
        assert!(
            encoded
                .windows(b"INSERT".len())
                .all(|window| window != b"INSERT")
        );
    }

    #[test]
    fn private_tags_are_append_only() {
        assert_eq!(INSERT_OPERATION_TAG, 1);
        assert_eq!(UPDATE_OPERATION_TAG, 2);
        assert_eq!(DELETE_OPERATION_TAG, 3);
        assert_eq!(INSERT_FORMAT_VERSION, 1);
        assert_eq!(UPDATE_FORMAT_VERSION, 2);
        assert_eq!(DELETE_FORMAT_VERSION, 3);
        assert_eq!(RECORD_INSERT_FORMAT_VERSION, 4);
        assert_eq!(
            [
                PARAMETER_EXPRESSION_TAG,
                BOOLEAN_EXPRESSION_TAG,
                TYPED_NULL_EXPRESSION_TAG,
                RECORD_CONSTRUCTOR_EXPRESSION_TAG,
            ],
            [1, 2, 3, 4]
        );
        assert_eq!(
            [SCALAR_TYPE_TAG, NAMED_TYPE_TAG, REFERENCE_TYPE_TAG],
            [1, 2, 3]
        );
        assert_eq!(
            [
                BOOLEAN_SCALAR_TAG,
                INTEGER_SCALAR_TAG,
                BIGINT_SCALAR_TAG,
                FLOAT_SCALAR_TAG,
                CLOB_SCALAR_TAG,
                BLOB_SCALAR_TAG,
            ],
            [1, 2, 3, 4, 6, 7]
        );
    }

    #[test]
    fn rejects_corruption_truncation_trailing_and_limits() {
        let valid = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
            .encode()
            .unwrap();
        let mut magic = valid.clone();
        magic[0] = b'X';
        assert_eq!(
            ServerMutationPlan::decode(&magic),
            Err(ServerMutationPlanError::InvalidMagic)
        );
        let mut version = valid.clone();
        version[11] = 3;
        assert_eq!(
            ServerMutationPlan::decode(&version),
            Err(ServerMutationPlanError::UnsupportedVersion(3))
        );
        let mut operation = valid.clone();
        operation[12] = 99;
        assert_eq!(
            ServerMutationPlan::decode(&operation),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: 99
            })
        );
        let mut expression_tag = valid.clone();
        expression_tag[65] = 99;
        assert_eq!(
            ServerMutationPlan::decode(&expression_tag),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "mutation expression kind",
                tag: 99
            })
        );
        let mut type_tag = valid.clone();
        type_tag[66] = NAMED_TYPE_TAG;
        assert_eq!(
            ServerMutationPlan::decode(&type_tag),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "resolved type kind",
                tag: NAMED_TYPE_TAG
            })
        );
        let mut scalar_tag = valid.clone();
        scalar_tag[67] = 99;
        assert_eq!(
            ServerMutationPlan::decode(&scalar_tag),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "scalar type",
                tag: 99
            })
        );
        let mut nullability = valid.clone();
        nullability[68] = 2;
        assert_eq!(
            ServerMutationPlan::decode(&nullability),
            Err(ServerMutationPlanError::InvalidBoolean {
                context: "expression nullability",
                value: 2
            })
        );
        let mut boolean = valid.clone();
        boolean[69] = 2;
        assert_eq!(
            ServerMutationPlan::decode(&boolean),
            Err(ServerMutationPlanError::InvalidBoolean {
                context: "literal",
                value: 2
            })
        );
        for prefix in 0..valid.len() {
            assert_eq!(
                ServerMutationPlan::decode(&valid[..prefix]),
                Err(ServerMutationPlanError::Truncated)
            );
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert_eq!(
            ServerMutationPlan::decode(&trailing),
            Err(ServerMutationPlanError::TrailingBytes)
        );
        let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
        assert_eq!(
            ServerMutationPlan::decode(&oversized),
            Err(ServerMutationPlanError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );
        let mut over_count = valid.clone();
        over_count[29..33].copy_from_slice(&(MAX_ASSIGNMENTS + 1).to_be_bytes());
        assert_eq!(
            ServerMutationPlan::decode(&over_count),
            Err(ServerMutationPlanError::CollectionLimit {
                kind: "assignments",
                count: MAX_ASSIGNMENTS as usize + 1,
                maximum: MAX_ASSIGNMENTS,
            })
        );
        let mut empty = valid.clone();
        empty[29..33].copy_from_slice(&0_u32.to_be_bytes());
        empty.drain(33..70);
        assert_eq!(
            ServerMutationPlan::decode(&empty),
            Err(ServerMutationPlanError::EmptyAssignments)
        );

        let rich = plan_with([
            (
                FIELD_A,
                MutationExpression::parameter(
                    FUNCTION_A,
                    PARAMETER_A,
                    ResolvedType::reference(REF_TARGET),
                )
                .unwrap(),
            ),
            (
                FIELD_B,
                MutationExpression::typed_null(ResolvedType::scalar(
                    StandardScalar::CharacterLargeObject,
                ))
                .unwrap(),
            ),
        ])
        .encode()
        .unwrap();
        let mut nullable_parameter = rich.clone();
        nullable_parameter[83] = 1;
        assert_eq!(
            ServerMutationPlan::decode(&nullable_parameter),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "parameter",
                expected: false,
                actual: true
            })
        );
        let mut nonnullable_null = rich;
        nonnullable_null[151] = 0;
        assert_eq!(
            ServerMutationPlan::decode(&nonnullable_null),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "typed NULL",
                expected: true,
                actual: false
            })
        );
    }

    #[test]
    fn rejects_update_version_operation_and_selector_corruption() {
        let valid = update_plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
            .encode()
            .unwrap();

        let mut insert_operation = valid.clone();
        insert_operation[12] = INSERT_OPERATION_TAG;
        assert_eq!(
            ServerMutationPlan::decode(&insert_operation),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: INSERT_OPERATION_TAG,
            })
        );

        let mut update_operation_in_v1 = valid.clone();
        update_operation_in_v1[11] = INSERT_FORMAT_VERSION as u8;
        assert_eq!(
            ServerMutationPlan::decode(&update_operation_in_v1),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: UPDATE_OPERATION_TAG,
            })
        );

        for prefix in 0..valid.len() {
            assert_eq!(
                ServerMutationPlan::decode(&valid[..prefix]),
                Err(ServerMutationPlanError::Truncated)
            );
        }
    }

    #[test]
    fn rejects_delete_header_truncation_and_trailing_corruption() {
        let valid = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A))
            .encode()
            .unwrap();

        let mut magic = valid.clone();
        magic[0] = b'X';
        assert_eq!(
            ServerDeletePlan::decode(&magic),
            Err(ServerMutationPlanError::InvalidMagic)
        );

        for version in [INSERT_FORMAT_VERSION, UPDATE_FORMAT_VERSION, 99] {
            let mut wrong_version = valid.clone();
            wrong_version[8..12].copy_from_slice(&version.to_be_bytes());
            assert_eq!(
                ServerDeletePlan::decode(&wrong_version),
                Err(ServerMutationPlanError::UnsupportedVersion(version))
            );
        }

        for operation in [INSERT_OPERATION_TAG, UPDATE_OPERATION_TAG, 99] {
            let mut wrong_operation = valid.clone();
            wrong_operation[12] = operation;
            assert_eq!(
                ServerDeletePlan::decode(&wrong_operation),
                Err(ServerMutationPlanError::InvalidEnumTag {
                    kind: "operation",
                    tag: operation,
                })
            );
        }

        assert_eq!(
            ServerMutationPlan::decode(&valid),
            Err(ServerMutationPlanError::UnsupportedVersion(
                DELETE_FORMAT_VERSION
            ))
        );
        for prefix in 0..valid.len() {
            assert_eq!(
                ServerDeletePlan::decode(&valid[..prefix]),
                Err(ServerMutationPlanError::Truncated)
            );
        }
        let mut trailing = valid;
        trailing.push(0);
        assert_eq!(
            ServerDeletePlan::decode(&trailing),
            Err(ServerMutationPlanError::TrailingBytes)
        );
        let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
        assert_eq!(
            ServerDeletePlan::decode(&oversized),
            Err(ServerMutationPlanError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );
    }

    #[test]
    fn rejects_all_plan_and_expression_invariants() {
        assert_eq!(
            ServerMutationPlan::new_insert(TARGET, Vec::new(), TARGET),
            Err(ServerMutationPlanError::EmptyAssignments)
        );
        assert_eq!(
            ServerMutationPlan::new_update(
                TARGET,
                MutationSelector::new(FUNCTION_A, PARAMETER_A),
                Vec::new(),
                TARGET,
            ),
            Err(ServerMutationPlanError::EmptyAssignments)
        );
        let too_many = (0..=MAX_ASSIGNMENTS).map(|index| {
            assignment(
                FieldId::from_bytes([index as u8; 16]),
                MutationExpression::boolean_literal(true),
            )
        });
        assert!(matches!(
            ServerMutationPlan::new_insert(TARGET, too_many, TARGET),
            Err(ServerMutationPlanError::CollectionLimit { .. })
        ));
        assert!(matches!(
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    OTHER_TARGET,
                    FIELD_A,
                    MutationExpression::boolean_literal(true)
                )],
                TARGET,
            ),
            Err(ServerMutationPlanError::AssignmentOwnerMismatch { assignment: 0, .. })
        ));
        assert!(matches!(
            ServerMutationPlan::new_insert(
                TARGET,
                [
                    assignment(FIELD_A, MutationExpression::boolean_literal(true)),
                    assignment(FIELD_A, MutationExpression::boolean_literal(false)),
                ],
                TARGET,
            ),
            Err(ServerMutationPlanError::DuplicateFieldAssignment {
                first: 0,
                duplicate: 1,
                ..
            })
        ));
        assert_eq!(
            ServerMutationPlan::new_update(
                TARGET,
                MutationSelector::new(FUNCTION_A, PARAMETER_A),
                [assignment(
                    FIELD_A,
                    MutationExpression::parameter(
                        FUNCTION_B,
                        PARAMETER_B,
                        ResolvedType::scalar(StandardScalar::Integer),
                    )
                    .unwrap(),
                )],
                TARGET,
            ),
            Err(ServerMutationPlanError::SelectorParameterOwnerMismatch {
                assignment: 0,
                selector_owner: FUNCTION_A,
                assignment_owner: FUNCTION_B,
            })
        );
        assert!(matches!(
            ServerMutationPlan::new_insert(
                TARGET,
                [
                    assignment(
                        FIELD_A,
                        MutationExpression::parameter(
                            FUNCTION_A,
                            PARAMETER_A,
                            ResolvedType::scalar(StandardScalar::Integer)
                        )
                        .unwrap()
                    ),
                    assignment(
                        FIELD_B,
                        MutationExpression::parameter(
                            FUNCTION_B,
                            PARAMETER_B,
                            ResolvedType::scalar(StandardScalar::Integer)
                        )
                        .unwrap()
                    ),
                ],
                TARGET,
            ),
            Err(ServerMutationPlanError::MixedParameterOwners {
                first: 0,
                assignment: 1,
                ..
            })
        ));
        assert_eq!(
            ServerMutationPlan::new_insert(
                TARGET,
                [assignment(
                    FIELD_A,
                    MutationExpression::boolean_literal(true)
                )],
                OTHER_TARGET
            ),
            Err(ServerMutationPlanError::ReturnedObjectMismatch {
                target: TARGET,
                returned: OTHER_TARGET
            })
        );
        for resolved_type in [
            ResolvedType::scalar(StandardScalar::Decimal),
            ResolvedType::scalar(StandardScalar::Uuid),
            ResolvedType::scalar(StandardScalar::Date),
            ResolvedType::scalar(StandardScalar::Time),
            ResolvedType::scalar(StandardScalar::Timestamp),
            ResolvedType::scalar(StandardScalar::Duration),
            ResolvedType::scalar(StandardScalar::Void),
            ResolvedType::named(OTHER_TARGET),
        ] {
            assert!(matches!(
                MutationExpression::parameter(FUNCTION_A, PARAMETER_A, resolved_type),
                Err(ServerMutationPlanError::UnsupportedValueType { .. })
            ));
            assert!(matches!(
                MutationExpression::typed_null(resolved_type),
                Err(ServerMutationPlanError::UnsupportedValueType { .. })
            ));
        }
        assert!(matches!(
            MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer)),
            Ok(expression) if expression.nullable()
        ));
        let wrong_boolean_type = ServerMutationPlan {
            operation: ServerMutationOperation::Insert,
            target: TARGET,
            assignments: vec![assignment(
                FIELD_A,
                MutationExpression {
                    kind: MutationExpressionKind::BooleanLiteral { value: true },
                    resolved_type: ResolvedType::scalar(StandardScalar::Integer),
                    nullable: false,
                },
            )],
            returned_object: TARGET,
        };
        assert_eq!(
            wrong_boolean_type.encode(),
            Err(ServerMutationPlanError::ExpressionTypeMismatch {
                expression_kind: "BOOLEAN literal",
                expected: ResolvedType::scalar(StandardScalar::Boolean),
                actual: ResolvedType::scalar(StandardScalar::Integer),
            })
        );
        let nullable_parameter = ServerMutationPlan {
            operation: ServerMutationOperation::Insert,
            target: TARGET,
            assignments: vec![assignment(
                FIELD_A,
                MutationExpression {
                    kind: MutationExpressionKind::Parameter {
                        owner: FUNCTION_A,
                        parameter: PARAMETER_A,
                    },
                    resolved_type: ResolvedType::scalar(StandardScalar::Integer),
                    nullable: true,
                },
            )],
            returned_object: TARGET,
        };
        assert_eq!(
            nullable_parameter.encode(),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "parameter",
                expected: false,
                actual: true,
            })
        );
        let nullable_boolean = ServerMutationPlan {
            operation: ServerMutationOperation::Insert,
            target: TARGET,
            assignments: vec![assignment(
                FIELD_A,
                MutationExpression {
                    kind: MutationExpressionKind::BooleanLiteral { value: true },
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: true,
                },
            )],
            returned_object: TARGET,
        };
        assert_eq!(
            nullable_boolean.encode(),
            Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
                expression_kind: "BOOLEAN literal",
                expected: false,
                actual: true,
            })
        );
    }

    #[test]
    fn no_source_or_backend_names_enter_encoded_bytes() {
        let encoded_plans = [
            plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
                .encode()
                .unwrap(),
            update_plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
                .encode()
                .unwrap(),
            ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A))
                .encode()
                .unwrap(),
        ];
        for encoded in encoded_plans {
            for forbidden in [
                b"INSERT".as_slice(),
                b"UPDATE".as_slice(),
                b"DELETE".as_slice(),
                b"_orna".as_slice(),
                b"source".as_slice(),
                b"tasks".as_slice(),
                b"created".as_slice(),
                b"updated".as_slice(),
                b"deleted".as_slice(),
            ] {
                assert!(
                    !encoded
                        .windows(forbidden.len())
                        .any(|window| window == forbidden)
                );
            }
        }
    }
}
