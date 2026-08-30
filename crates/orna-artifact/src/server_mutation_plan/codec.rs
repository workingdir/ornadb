use super::*;
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
        let assignment_count = decode_count(&mut reader, "assignments", MAX_ASSIGNMENTS)?;
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
    Ok(reader.u32()?)
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

pub(super) fn validate_expression(
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

pub(super) fn validate_supported_type(
    resolved_type: ResolvedType,
) -> Result<(), ServerMutationPlanError> {
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

pub(super) fn validate_record_field_type(
    resolved_type: ResolvedType,
) -> Result<(), ServerMutationPlanError> {
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
    let nullable = decode_boolean(reader, "expression nullability")?;
    let kind = match tag {
        PARAMETER_EXPRESSION_TAG => MutationExpressionKind::Parameter {
            owner: reader.function_id()?,
            parameter: reader.parameter_id()?,
        },
        BOOLEAN_EXPRESSION_TAG => MutationExpressionKind::BooleanLiteral {
            value: decode_boolean(reader, "literal")?,
        },
        TYPED_NULL_EXPRESSION_TAG => MutationExpressionKind::TypedNull,
        RECORD_CONSTRUCTOR_EXPRESSION_TAG if allow_record => {
            let field_count = decode_count(reader, "record fields", MAX_RECORD_FIELDS)?;
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
    let nullable = decode_boolean(reader, "record field nullability")?;
    validate_nullability("record field", false, nullable)?;
    let kind = match tag {
        PARAMETER_EXPRESSION_TAG => RecordFieldExpressionKind::Parameter {
            owner: reader.function_id()?,
            parameter: reader.parameter_id()?,
        },
        BOOLEAN_EXPRESSION_TAG => RecordFieldExpressionKind::BooleanLiteral {
            value: decode_boolean(reader, "record literal")?,
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

fn decode_boolean(
    reader: &mut Reader<'_>,
    context: &'static str,
) -> Result<bool, ServerMutationPlanError> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(ServerMutationPlanError::InvalidBoolean { context, value }),
    }
}

fn decode_count(
    reader: &mut Reader<'_>,
    kind: &'static str,
    maximum: u32,
) -> Result<usize, ServerMutationPlanError> {
    let count = reader.u32()? as usize;
    if count > maximum as usize {
        return Err(ServerMutationPlanError::CollectionLimit {
            kind,
            count,
            maximum,
        });
    }
    Ok(count)
}
