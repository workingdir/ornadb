use super::*;

pub(super) fn encode_control_flow_plan(
    plan: &ControlFlowClientPlan,
) -> Result<Vec<u8>, ClientPlanError> {
    validate_control_flow_model(plan)?;

    let local_count = u32::try_from(plan.locals.len()).map_err(|_| {
        ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        }
    })?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&CONTROL_FLOW_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_CONTROL_FLOW_OPERATION);
    bytes.extend_from_slice(&local_count.to_be_bytes());
    for local in &plan.locals {
        bytes.extend_from_slice(&local.local.to_bytes());
        bytes.extend_from_slice(&local.type_id.to_bytes());
        bytes.push(local.kind.tag());
    }

    let mut writer = NodeWriter::new();
    let mut expression_count = 0usize;
    let mut resource_count = 0usize;
    let mut statement_count = 0usize;
    encode_control_flow_block(
        &plan.statements,
        0,
        &mut writer,
        &mut expression_count,
        &mut resource_count,
        &mut statement_count,
    )?;
    bytes.extend_from_slice(&writer.finish());
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(bytes)
}

pub(super) fn decode_control_flow_plan(
    bytes: &[u8],
) -> Result<ControlFlowClientPlan, ClientPlanError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    let mut reader = Reader::new(bytes);
    if reader.array::<8>()? != MAGIC {
        return Err(ClientPlanError::InvalidMagic);
    }
    let version = reader.u32()?;
    if version != CONTROL_FLOW_FORMAT_VERSION {
        return Err(ClientPlanError::UnsupportedVersion(version));
    }
    let operation = reader.u8()?;
    if operation != RETURN_CONTROL_FLOW_OPERATION {
        return Err(ClientPlanError::InvalidOperation(operation));
    }

    let local_count = reader.u32()? as usize;
    if local_count > MAX_CONTROL_FLOW_LOCALS {
        return Err(ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        });
    }
    let mut locals = Vec::with_capacity(local_count);
    for _ in 0..local_count {
        let local = LocalId::from_bytes(reader.array()?);
        if locals
            .iter()
            .any(|candidate: &ClientLocal| candidate.local == local)
        {
            return Err(ClientPlanError::DuplicateControlFlowLocal(local));
        }
        let type_id = TypeId::from_bytes(reader.array()?);
        let kind = ClientLocalKind::from_tag(reader.u8()?)?;
        locals.push(ClientLocal::new(local, type_id, kind));
    }

    let mut expression_count = 0usize;
    let mut resource_count = 0usize;
    let mut statement_count = 0usize;
    let statements = decode_control_flow_block(
        &mut reader,
        0,
        &mut expression_count,
        &mut resource_count,
        &mut statement_count,
    )?;
    reader.require_finished()?;

    let plan = ControlFlowClientPlan::new(locals, statements);
    validate_control_flow_model(&plan)?;
    Ok(plan)
}

fn encode_control_flow_block(
    statements: &[ControlFlowStatement],
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
    statement_count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    if statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }
    let count = u32::try_from(statements.len()).map_err(|_| {
        ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        }
    })?;
    writer.extend(&count.to_be_bytes());
    for statement in statements {
        *statement_count = statement_count.saturating_add(1);
        if *statement_count > MAX_CONTROL_FLOW_STATEMENTS {
            return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
                limit: MAX_CONTROL_FLOW_STATEMENTS,
            });
        }
        match statement {
            ControlFlowStatement::Let { local, expression } => {
                writer.push(CONTROL_FLOW_STATEMENT_LET);
                writer.extend(&local.to_bytes());
                encode_control_flow_expression(
                    expression,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
            ControlFlowStatement::Assignment { local, expression } => {
                writer.push(CONTROL_FLOW_STATEMENT_ASSIGNMENT);
                writer.extend(&local.to_bytes());
                encode_control_flow_expression(
                    expression,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
            ControlFlowStatement::Return(return_statement) => {
                writer.push(CONTROL_FLOW_STATEMENT_RETURN);
                match return_statement.expression.as_ref() {
                    None => writer.push(CONTROL_FLOW_RETURN_NONE),
                    Some(expression) => {
                        writer.push(CONTROL_FLOW_RETURN_EXPRESSION);
                        encode_control_flow_expression(
                            expression,
                            0,
                            writer,
                            expression_count,
                            resource_count,
                        )?;
                    }
                }
            }
            ControlFlowStatement::If(if_statement) => {
                if if_statement.branches.is_empty() {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if if_statement.branches.len() > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                writer.push(CONTROL_FLOW_STATEMENT_IF);
                let branch_count = u32::try_from(if_statement.branches.len()).map_err(|_| {
                    ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    }
                })?;
                writer.extend(&branch_count.to_be_bytes());
                for branch in &if_statement.branches {
                    encode_control_flow_expression(
                        &branch.condition,
                        0,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                    encode_control_flow_block(
                        &branch.statements,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?;
                }
                match if_statement.else_statements.as_ref() {
                    None => writer.push(CONTROL_FLOW_ELSE_NONE),
                    Some(statements) => {
                        writer.push(CONTROL_FLOW_ELSE_BODY);
                        encode_control_flow_block(
                            statements,
                            depth + 1,
                            writer,
                            expression_count,
                            resource_count,
                            statement_count,
                        )?;
                    }
                }
            }
            ControlFlowStatement::While(while_statement) => {
                writer.push(CONTROL_FLOW_STATEMENT_WHILE);
                encode_control_flow_expression(
                    &while_statement.condition,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
                encode_control_flow_block(
                    &while_statement.statements,
                    depth + 1,
                    writer,
                    expression_count,
                    resource_count,
                    statement_count,
                )?;
            }
        }
    }
    Ok(())
}

fn decode_control_flow_block(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
    statement_count: &mut usize,
) -> Result<Vec<ControlFlowStatement>, ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    let count = reader.u32()? as usize;
    if count > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }
    let mut statements = Vec::with_capacity(count);
    for _ in 0..count {
        *statement_count = statement_count.saturating_add(1);
        if *statement_count > MAX_CONTROL_FLOW_STATEMENTS {
            return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
                limit: MAX_CONTROL_FLOW_STATEMENTS,
            });
        }
        let tag = reader.u8()?;
        let statement = match tag {
            CONTROL_FLOW_STATEMENT_LET | CONTROL_FLOW_STATEMENT_ASSIGNMENT => {
                let local = LocalId::from_bytes(reader.array()?);
                let expression =
                    decode_control_flow_expression(reader, 0, expression_count, resource_count)?;
                if tag == CONTROL_FLOW_STATEMENT_LET {
                    ControlFlowStatement::let_(local, expression)
                } else {
                    ControlFlowStatement::assignment(local, expression)
                }
            }
            CONTROL_FLOW_STATEMENT_RETURN => {
                let expression = match reader.u8()? {
                    CONTROL_FLOW_RETURN_NONE => None,
                    CONTROL_FLOW_RETURN_EXPRESSION => Some(decode_control_flow_expression(
                        reader,
                        0,
                        expression_count,
                        resource_count,
                    )?),
                    tag => return Err(ClientPlanError::InvalidControlFlowReturnTag(tag)),
                };
                ControlFlowStatement::Return(ControlFlowReturnStatement::new(expression))
            }
            CONTROL_FLOW_STATEMENT_IF => {
                let branch_count = reader.u32()? as usize;
                if branch_count == 0 {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if branch_count > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                let mut branches = Vec::with_capacity(branch_count);
                for _ in 0..branch_count {
                    let condition = decode_control_flow_expression(
                        reader,
                        0,
                        expression_count,
                        resource_count,
                    )?;
                    let statements = decode_control_flow_block(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?;
                    branches.push(ControlFlowIfBranch::new(condition, statements));
                }
                let else_statements = match reader.u8()? {
                    CONTROL_FLOW_ELSE_NONE => None,
                    CONTROL_FLOW_ELSE_BODY => Some(decode_control_flow_block(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?),
                    tag => return Err(ClientPlanError::InvalidControlFlowElseTag(tag)),
                };
                ControlFlowStatement::If(ControlFlowIfStatement::new(branches, else_statements))
            }
            CONTROL_FLOW_STATEMENT_WHILE => {
                let condition =
                    decode_control_flow_expression(reader, 0, expression_count, resource_count)?;
                let statements = decode_control_flow_block(
                    reader,
                    depth + 1,
                    expression_count,
                    resource_count,
                    statement_count,
                )?;
                ControlFlowStatement::While(ControlFlowWhileStatement::new(condition, statements))
            }
            tag => return Err(ClientPlanError::InvalidControlFlowStatement(tag)),
        };
        statements.push(statement);
    }
    Ok(statements)
}
fn encode_control_flow_expression(
    node: &ControlFlowExpression,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *expression_count = expression_count.saturating_add(1);
    if *expression_count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }

    match node {
        ClientExpressionNode::Await { expression } => {
            writer.push(NODE_AWAIT);
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Resource { operation } => {
            encode_control_flow_resource_operation(
                operation,
                depth,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Action { operation } => {
            encode_control_flow_action_operation(
                operation,
                depth,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Inspect { operation } => {
            writer.push(NODE_INSPECT);
            match operation {
                InspectOperationNode::Snapshot { target, options } => {
                    writer.push(INSPECT_OPERATION_SNAPSHOT);
                    encode_control_flow_expression(
                        target,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                    if options.is_some() {
                        return Err(ClientPlanError::UnsupportedInspectOptions);
                    }
                    writer.push(0);
                }
                InspectOperationNode::Projection {
                    projection,
                    snapshot,
                } => {
                    writer.push(INSPECT_OPERATION_PROJECTION);
                    writer.push(projection.tag());
                    encode_control_flow_expression(
                        snapshot,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            writer.push(NODE_CALL);
            writer.extend(&function.to_bytes());
            let length = u32::try_from(arguments.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for (parameter, value) in arguments {
                writer.extend(&parameter.to_bytes());
                encode_control_flow_expression(
                    value,
                    depth + 1,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
        }
        ClientExpressionNode::String { value } => {
            writer.push(NODE_STRING);
            let bytes = value.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::Integer { value } => {
            writer.push(NODE_INTEGER);
            writer.extend(&value.to_be_bytes());
        }
        ClientExpressionNode::Boolean { value } => {
            writer.push(NODE_BOOLEAN);
            writer.push(u8::from(*value));
        }
        ClientExpressionNode::ParameterRead { parameter } => {
            writer.push(NODE_PARAMETER_READ);
            writer.extend(&parameter.to_bytes());
        }
        ClientExpressionNode::LocalRead { local } => {
            writer.push(NODE_LOCAL_READ);
            writer.extend(&local.to_bytes());
        }
        ClientExpressionNode::FieldPath { root, fields } => {
            if fields.is_empty() || fields.len() > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            writer.push(NODE_FIELD_PATH);
            writer.extend(&root.to_bytes());
            let length = u32::try_from(fields.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for field in fields {
                writer.extend(&field.to_bytes());
            }
        }
        ClientExpressionNode::Concat { left, right } => {
            writer.push(NODE_CONCAT);
            encode_control_flow_expression(
                left,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
            encode_control_flow_expression(
                right,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::ExternalContract { identity } => {
            validate_external_contract_identity(identity)?;
            writer.push(NODE_EXTERNAL_CONTRACT);
            let bytes = identity.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::Unary {
            operator,
            expression,
        } => {
            writer.push(NODE_UNARY);
            writer.push(operator.tag());
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Binary {
            operator,
            left,
            right,
        } => {
            writer.push(NODE_BINARY);
            writer.push(operator.tag());
            encode_control_flow_expression(
                left,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
            encode_control_flow_expression(
                right,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::SourceIntrospection => writer.push(NODE_SOURCE_INTROSPECTION),
        ClientExpressionNode::Input => writer.push(NODE_INPUT),
        ClientExpressionNode::Evaluate { expression } => {
            writer.push(NODE_EVALUATE);
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
    }
    Ok(())
}

fn encode_control_flow_resource_operation(
    operation: &ResourceOperationNode,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    *resource_count = resource_count.saturating_add(1);
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        validate_resource_identity(identity)?;
    }
    validate_resource_arguments(&operation.arguments)?;
    writer.push(NODE_RESOURCE);
    writer.push(operation.kind.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    let argument_count = u32::try_from(operation.arguments.len()).map_err(|_| {
        ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        }
    })?;
    writer.extend(&argument_count.to_be_bytes());
    for (parameter, value) in &operation.arguments {
        writer.extend(&parameter.to_bytes());
        encode_control_flow_expression(value, depth + 1, writer, expression_count, resource_count)?;
    }
    writer.extend(&operation.result_type.to_bytes());
    Ok(())
}

fn encode_control_flow_action_operation(
    operation: &ActionOperationNode,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    validate_action_arguments(&operation.arguments)?;
    writer.push(NODE_ACTION);
    writer.push(operation.domain.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    writer.extend(&operation.result_type.to_bytes());
    let argument_count = u32::try_from(operation.arguments.len()).map_err(|_| {
        ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        }
    })?;
    writer.extend(&argument_count.to_be_bytes());
    for (parameter, value) in &operation.arguments {
        if parameter.to_bytes() == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
        writer.extend(&parameter.to_bytes());
        encode_control_flow_expression(value, depth + 1, writer, expression_count, resource_count)?;
    }
    Ok(())
}

fn decode_control_flow_expression(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ControlFlowExpression, ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *expression_count = expression_count.saturating_add(1);
    if *expression_count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }

    let tag = reader.u8()?;
    match tag {
        NODE_AWAIT => Ok(ClientExpressionNode::Await {
            expression: Box::new(decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?),
        }),
        NODE_RESOURCE => Ok(ClientExpressionNode::Resource {
            operation: decode_control_flow_resource_operation(
                reader,
                depth,
                expression_count,
                resource_count,
            )?,
        }),
        NODE_ACTION => Ok(ClientExpressionNode::Action {
            operation: decode_control_flow_action_operation(
                reader,
                depth,
                expression_count,
                resource_count,
            )?,
        }),
        NODE_INSPECT => {
            let operation = match reader.u8()? {
                INSPECT_OPERATION_SNAPSHOT => InspectOperationNode::Snapshot {
                    target: Box::new(decode_control_flow_expression(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                    )?),
                    options: match reader.u8()? {
                        0 => None,
                        1 => return Err(ClientPlanError::UnsupportedInspectOptions),
                        tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
                    },
                },
                INSPECT_OPERATION_PROJECTION => {
                    let projection = InspectProjection::from_tag(reader.u8()?)?;
                    InspectOperationNode::Projection {
                        projection,
                        snapshot: Box::new(decode_control_flow_expression(
                            reader,
                            depth + 1,
                            expression_count,
                            resource_count,
                        )?),
                    }
                }
                tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
            };
            Ok(ClientExpressionNode::Inspect { operation })
        }
        NODE_CALL => {
            let function = FunctionId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            let mut arguments = Vec::with_capacity(length);
            for _ in 0..length {
                let parameter = ParameterId::from_bytes(reader.array()?);
                let value = decode_control_flow_expression(
                    reader,
                    depth + 1,
                    expression_count,
                    resource_count,
                )?;
                arguments.push((parameter, value));
            }
            Ok(ClientExpressionNode::Call {
                function,
                arguments,
            })
        }
        NODE_STRING => {
            let length = reader.u32()? as usize;
            let value = std::str::from_utf8(reader.take(length)?)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_STRING))?
                .to_owned();
            Ok(ClientExpressionNode::String { value })
        }
        NODE_INTEGER => Ok(ClientExpressionNode::Integer {
            value: i64::from_be_bytes(reader.array()?),
        }),
        NODE_BOOLEAN => match reader.u8()? {
            0 => Ok(ClientExpressionNode::Boolean { value: false }),
            1 => Ok(ClientExpressionNode::Boolean { value: true }),
            _ => Err(ClientPlanError::InvalidExpressionNode(NODE_BOOLEAN)),
        },
        NODE_PARAMETER_READ => Ok(ClientExpressionNode::ParameterRead {
            parameter: ParameterId::from_bytes(reader.array()?),
        }),
        NODE_LOCAL_READ => Ok(ClientExpressionNode::LocalRead {
            local: LocalId::from_bytes(reader.array()?),
        }),
        NODE_FIELD_PATH => {
            let root = ParameterId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length == 0 {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            if length > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                });
            }
            let mut fields = Vec::with_capacity(length);
            for _ in 0..length {
                fields.push(FieldId::from_bytes(reader.array()?));
            }
            Ok(ClientExpressionNode::FieldPath { root, fields })
        }
        NODE_CONCAT => {
            let left = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            let right = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Concat {
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        NODE_SOURCE_INTROSPECTION => Ok(ClientExpressionNode::SourceIntrospection),
        NODE_INPUT => Ok(ClientExpressionNode::Input),
        NODE_EVALUATE => Ok(ClientExpressionNode::Evaluate {
            expression: Box::new(decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?),
        }),
        NODE_EXTERNAL_CONTRACT => {
            let length = reader.u32()? as usize;
            let identity = std::str::from_utf8(reader.take(length)?)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_EXTERNAL_CONTRACT))?;
            validate_external_contract_identity(identity)?;
            Ok(ClientExpressionNode::ExternalContract {
                identity: identity.to_owned(),
            })
        }
        NODE_UNARY => {
            let operator = ControlFlowUnaryOperator::from_tag(reader.u8()?)?;
            let expression = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Unary {
                operator,
                expression: Box::new(expression),
            })
        }
        NODE_BINARY => {
            let operator = ControlFlowBinaryOperator::from_tag(reader.u8()?)?;
            let left = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            let right = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        tag => Err(ClientPlanError::InvalidExpressionNode(tag)),
    }
}

fn decode_control_flow_resource_operation(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ResourceOperationNode, ClientPlanError> {
    *resource_count = resource_count.saturating_add(1);
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    let kind = match reader.u8()? {
        RESOURCE_KIND_SCALAR => ResourceKind::Scalar,
        RESOURCE_KIND_STREAM => ResourceKind::Stream,
        tag => return Err(ClientPlanError::InvalidResourceKind(tag)),
    };
    let target = FunctionId::from_bytes(read_resource_identity(reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_resource_identity(reader)?),
        CatalogueRevisionId::from_bytes(read_resource_identity(reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_resource_identity(reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_resource_identity(reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalResourceArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateResourceArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value =
            decode_control_flow_expression(reader, depth + 1, expression_count, resource_count)?;
        arguments.push((parameter, value));
    }
    let result_type = TypeId::from_bytes(read_resource_identity(reader)?);
    Ok(ResourceOperationNode::new(
        kind,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    ))
}

fn decode_control_flow_action_operation(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ActionOperationNode, ClientPlanError> {
    let domain = match reader.u8()? {
        1 => ActionTargetDomain::Client,
        2 => ActionTargetDomain::Server,
        tag => return Err(ClientPlanError::InvalidActionDomain(tag)),
    };
    let target = FunctionId::from_bytes(read_action_identity(reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_action_identity(reader)?),
        CatalogueRevisionId::from_bytes(read_action_identity(reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_action_identity(reader)?);
    let result_type = TypeId::from_bytes(read_action_identity(reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_ACTION_ARGUMENTS {
        return Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_action_identity(reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalActionArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateActionArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value =
            decode_control_flow_expression(reader, depth + 1, expression_count, resource_count)?;
        arguments.push((parameter, value));
    }
    Ok(ActionOperationNode::new(
        domain,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    ))
}

fn validate_control_flow_model(plan: &ControlFlowClientPlan) -> Result<(), ClientPlanError> {
    if plan.locals.len() > MAX_CONTROL_FLOW_LOCALS {
        return Err(ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        });
    }
    if plan.statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }

    let mut seen_locals = Vec::with_capacity(plan.locals.len());
    for local in &plan.locals {
        if seen_locals.contains(&local.local) {
            return Err(ClientPlanError::DuplicateControlFlowLocal(local.local));
        }
        seen_locals.push(local.local);
    }

    let mut initialized = Vec::new();
    let mut let_seen = Vec::new();
    validate_control_flow_block(
        &plan.statements,
        0,
        &plan.locals,
        &mut initialized,
        &mut let_seen,
    )?;
    for local in &plan.locals {
        if !let_seen.contains(&local.local) {
            return Err(ClientPlanError::MissingControlFlowLet(local.local));
        }
    }
    Ok(())
}

fn validate_control_flow_block(
    statements: &[ControlFlowStatement],
    depth: usize,
    locals: &[ClientLocal],
    initialized: &mut Vec<LocalId>,
    let_seen: &mut Vec<LocalId>,
) -> Result<(), ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    if statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }

    for statement in statements {
        match statement {
            ControlFlowStatement::Let { local, expression } => {
                let declaration = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                if let_seen.contains(local) {
                    return Err(ClientPlanError::DuplicateControlFlowLet(*local));
                }
                let allow_resource_root = matches!(declaration.kind, ClientLocalKind::Resource(_));
                validate_control_flow_expression(
                    expression,
                    locals,
                    initialized,
                    allow_resource_root,
                    true,
                    matches!(declaration.kind, ClientLocalKind::Value),
                )?;
                validate_control_flow_initializer_kind(declaration, expression, locals)?;
                let_seen.push(*local);
                if !initialized.contains(local) {
                    initialized.push(*local);
                }
            }
            ControlFlowStatement::Assignment { local, expression } => {
                let declaration = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                if !initialized.contains(local) {
                    return Err(ClientPlanError::ControlFlowAssignmentBeforeLet(*local));
                }
                let allow_resource_root = matches!(declaration.kind, ClientLocalKind::Resource(_));
                validate_control_flow_expression(
                    expression,
                    locals,
                    initialized,
                    allow_resource_root,
                    true,
                    matches!(declaration.kind, ClientLocalKind::Value),
                )?;
                validate_control_flow_initializer_kind(declaration, expression, locals)?;
            }
            ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression.as_ref() {
                    validate_control_flow_expression(
                        expression,
                        locals,
                        initialized,
                        false,
                        true,
                        true,
                    )?;
                }
            }
            ControlFlowStatement::If(if_statement) => {
                if if_statement.branches.is_empty() {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if if_statement.branches.len() > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                let incoming = initialized.clone();
                let mut branch_exits = Vec::with_capacity(if_statement.branches.len() + 1);
                for branch in &if_statement.branches {
                    validate_control_flow_expression(
                        &branch.condition,
                        locals,
                        &incoming,
                        false,
                        true,
                        true,
                    )?;
                    let mut branch_initialized = incoming.clone();
                    validate_control_flow_block(
                        &branch.statements,
                        depth + 1,
                        locals,
                        &mut branch_initialized,
                        let_seen,
                    )?;
                    branch_exits.push(branch_initialized);
                }
                if let Some(statements) = if_statement.else_statements.as_ref() {
                    let mut else_initialized = incoming.clone();
                    validate_control_flow_block(
                        statements,
                        depth + 1,
                        locals,
                        &mut else_initialized,
                        let_seen,
                    )?;
                    branch_exits.push(else_initialized);
                    initialized
                        .retain(|local| branch_exits.iter().all(|exit| exit.contains(local)));
                } else {
                    *initialized = incoming;
                }
            }
            ControlFlowStatement::While(while_statement) => {
                let incoming = initialized.clone();
                validate_control_flow_expression(
                    &while_statement.condition,
                    locals,
                    &incoming,
                    false,
                    true,
                    true,
                )?;
                let mut body_initialized = incoming.clone();
                validate_control_flow_block(
                    &while_statement.statements,
                    depth + 1,
                    locals,
                    &mut body_initialized,
                    let_seen,
                )?;
                // A WHILE body may execute zero times, so no local initialized
                // only in that body is definite after the loop.
                *initialized = incoming;
            }
        }
    }
    Ok(())
}

fn validate_control_flow_initializer_kind(
    declaration: &ClientLocal,
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
) -> Result<(), ClientPlanError> {
    let actual = control_flow_resource_kind(expression, locals);
    match declaration.kind {
        ClientLocalKind::Value if actual.is_some() => Err(
            ClientPlanError::ControlFlowLocalKindMismatch(declaration.local),
        ),
        ClientLocalKind::Resource(expected) if actual != Some(expected) => Err(
            ClientPlanError::ControlFlowLocalKindMismatch(declaration.local),
        ),
        ClientLocalKind::Value | ClientLocalKind::Resource(_) => Ok(()),
    }
}

fn control_flow_resource_kind(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
) -> Option<ResourceKind> {
    match expression {
        ClientExpressionNode::Resource { operation } => Some(operation.kind()),
        ClientExpressionNode::LocalRead { local } => locals
            .iter()
            .find(|candidate| candidate.local == *local)
            .and_then(|candidate| match candidate.kind {
                ClientLocalKind::Resource(kind) => Some(kind),
                ClientLocalKind::Value => None,
            }),
        _ => None,
    }
}

fn validate_control_flow_expression(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
    initialized: &[LocalId],
    allow_resource_root: bool,
    allow_await_root: bool,
    value_position: bool,
) -> Result<(), ClientPlanError> {
    validate_external_contract_placement(expression, true)?;
    validate_control_flow_expression_shape(
        expression,
        locals,
        initialized,
        allow_resource_root,
        allow_await_root,
        value_position,
    )
}

fn validate_control_flow_expression_shape(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
    initialized: &[LocalId],
    allow_resource_root: bool,
    allow_await_root: bool,
    value_position: bool,
) -> Result<(), ClientPlanError> {
    match expression {
        ClientExpressionNode::Await { expression } => {
            if !allow_await_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            match expression.as_ref() {
                ClientExpressionNode::Resource { operation } => {
                    validate_control_flow_resource_operation(operation, locals, initialized)?;
                }
                ClientExpressionNode::LocalRead { local } => {
                    let declaration = locals
                        .iter()
                        .find(|candidate| candidate.local == *local)
                        .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                    if !matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                        return Err(ClientPlanError::InvalidAwaitOperand(*local));
                    }
                    if !initialized.contains(local) {
                        return Err(ClientPlanError::ControlFlowLocalReadBeforeLet(*local));
                    }
                }
                _ => return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT)),
            }
        }
        ClientExpressionNode::Resource { operation } => {
            if !allow_resource_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            validate_control_flow_resource_operation(operation, locals, initialized)?;
        }
        ClientExpressionNode::Action { operation } => {
            validate_control_flow_action_operation(operation, locals, initialized)?;
        }
        ClientExpressionNode::Inspect { operation } => match operation {
            InspectOperationNode::Snapshot { target, options } => {
                if options.is_some() {
                    return Err(ClientPlanError::UnsupportedInspectOptions);
                }
                validate_control_flow_expression_shape(
                    target,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
            InspectOperationNode::Projection { snapshot, .. } => {
                validate_control_flow_expression_shape(
                    snapshot,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
        },
        ClientExpressionNode::LocalRead { local } => {
            let declaration = locals
                .iter()
                .find(|candidate| candidate.local == *local)
                .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
            if !initialized.contains(local) {
                return Err(ClientPlanError::ControlFlowLocalReadBeforeLet(*local));
            }
            if value_position && matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                return Err(ClientPlanError::UnawaitedResourceLocal(*local));
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            for (_, value) in arguments {
                validate_control_flow_expression_shape(
                    value,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            validate_control_flow_expression_shape(left, locals, initialized, false, false, true)?;
            validate_control_flow_expression_shape(right, locals, initialized, false, false, true)?;
        }
        ClientExpressionNode::Unary { expression, .. } => {
            validate_control_flow_expression_shape(
                expression,
                locals,
                initialized,
                false,
                false,
                true,
            )?;
        }
        ClientExpressionNode::Input => {}
        ClientExpressionNode::Evaluate { expression } => {
            validate_control_flow_expression_shape(
                expression,
                locals,
                initialized,
                false,
                false,
                true,
            )?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
    Ok(())
}

fn validate_control_flow_resource_operation(
    operation: &ResourceOperationNode,
    locals: &[ClientLocal],
    initialized: &[LocalId],
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        validate_resource_identity(identity)?;
    }
    validate_resource_arguments(&operation.arguments)?;
    for (_, value) in &operation.arguments {
        validate_control_flow_expression_shape(value, locals, initialized, false, false, true)?;
    }
    Ok(())
}

fn validate_control_flow_action_operation(
    operation: &ActionOperationNode,
    locals: &[ClientLocal],
    initialized: &[LocalId],
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    validate_action_arguments(&operation.arguments)?;
    for (_, value) in &operation.arguments {
        validate_control_flow_expression_shape(value, locals, initialized, false, false, true)?;
    }
    Ok(())
}
