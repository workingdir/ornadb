//! CLIENT procedural control-flow checking.

use super::*;

struct ClientControlFlowChecker<'a, 'b> {
    input: &'a ResolvedClientFunctionInput<'b>,
    submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &'a CatalogueSnapshot,
    server_names: &'a [QualifiedSemanticName],
    standard: Option<&'a CheckedStandardLibrary>,
    diagnostics: &'a mut Vec<CompilerDiagnostic>,
    locals: ClientLocalEnvironment,
    checked_locals: Vec<CheckedClientLocal>,
    references: Vec<CheckedDefinitionReference>,
    used_capabilities: HashSet<QualifiedSemanticName>,
    next_ordinal: u32,
    _source_lifetime: std::marker::PhantomData<&'b ()>,
}

impl<'a, 'b> ClientControlFlowChecker<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        input: &'a ResolvedClientFunctionInput<'b>,
        submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
        targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
        action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
        resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
        query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
        base: &'a CatalogueSnapshot,
        server_names: &'a [QualifiedSemanticName],
        standard: Option<&'a CheckedStandardLibrary>,
        diagnostics: &'a mut Vec<CompilerDiagnostic>,
    ) -> Self {
        Self {
            input,
            submitted_ids,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            locals: ClientLocalEnvironment::new(),
            checked_locals: Vec::new(),
            references: Vec::new(),
            used_capabilities: HashSet::new(),
            next_ordinal: 0,
            _source_lifetime: std::marker::PhantomData,
        }
    }

    fn expression(
        &mut self,
        expression: &ClientExpression,
    ) -> Option<(CheckedClientExpression, ClientExpressionType)> {
        check_client_expression(
            expression,
            self.input,
            self.targets,
            self.action_targets,
            self.resource_targets,
            self.query_catalogue,
            self.base,
            self.server_names,
            self.standard,
            self.diagnostics,
            &mut self.references,
            &mut self.used_capabilities,
            &self.locals,
        )
    }

    fn declare_local(
        &mut self,
        name: &NamePart,
        type_source: Option<&SourceSlice>,
        expression: &ClientExpression,
        span: &SourceSpan,
        pre_begin_resource: bool,
    ) -> Option<(u32, CheckedClientExpression)> {
        let local_name = semantic_part(name);
        if self.locals.contains_key(&local_name) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate CLIENT local definition {local_name} in {}",
                    self.input.name
                ),
                self.input.logical_path,
                &name.span,
            ));
            return None;
        }
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, true, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }

        let declared_resource_family = type_source.and_then(client_local_resource_family);
        let direct_resource = matches!(
            expression,
            ClientExpression::Call { callee, .. }
                if resource_constructor_kind(&semantic_name(callee)).is_some()
        );
        if pre_begin_resource && !direct_resource {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("CLIENT local {local_name} requires a resource constructor initializer"),
                self.input.logical_path,
                span,
            ));
            return None;
        }

        let (checked, expression_type, kind) = if declared_resource_family.is_some()
            || direct_resource
        {
            if !direct_resource {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT local {local_name} resource type requires a resource constructor"
                    ),
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            let (checked, expression_type) = check_resource_constructor(
                expression,
                self.input,
                self.targets,
                self.action_targets,
                self.resource_targets,
                self.query_catalogue,
                self.base,
                self.server_names,
                self.standard,
                self.diagnostics,
                &mut self.references,
                &mut self.used_capabilities,
                &self.locals,
            )?;
            let actual_kind = match &checked {
                CheckedClientExpression::Resource { operation } => operation.kind,
                _ => unreachable!("resource constructor checker returns a resource"),
            };
            if let Some(source) = type_source {
                let Some((expected_kind, descriptor)) = client_local_resource_type(source) else {
                    self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"
                            ),
                            self.input.logical_path,
                            &source.span,
                        ));
                    return None;
                };
                if reject_deferred_client_resource_descriptor(
                    descriptor.as_ref(),
                    &local_name,
                    self.input,
                    source,
                    self.diagnostics,
                ) {
                    return None;
                }
                if actual_kind != expected_kind {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} type does not match its resource constructor"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
                if let Some(descriptor) = descriptor {
                    let resolved = resolve_application_type_with_named_standard(
                        &descriptor,
                        self.submitted_ids,
                        self.input.logical_path,
                        self.diagnostics,
                        self.standard,
                        true,
                    )?;
                    let expected_type = ClientExpressionType {
                        semantic_type: resolved.semantic_type,
                        standard_value_type: resolved.standard_value_type,
                        result_shape: ClientExpressionResultShape::Value,
                    };
                    if !client_expression_types_compatible(expression_type, expected_type) {
                        self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!(
                                    "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                                ),
                                self.input.logical_path,
                                &source.span,
                            ));
                        return None;
                    }
                }
            }
            (
                checked,
                expression_type,
                CheckedClientLocalKind::Resource(actual_kind),
            )
        } else {
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_type_is_evaluable(expression_type, self.base, self.standard) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT local type is not supported by the local evaluator",
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            if let Some(source) = type_source {
                let Some(specification) = client_type_specification_from_source(source) else {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("unsupported CLIENT local type for {local_name}"),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                };
                let resolved = resolve_application_type_with_named_standard(
                    &specification,
                    self.submitted_ids,
                    self.input.logical_path,
                    self.diagnostics,
                    self.standard,
                    true,
                )?;
                let expected_type = ClientExpressionType {
                    semantic_type: resolved.semantic_type,
                    standard_value_type: resolved.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                };
                if !client_expression_types_compatible(expression_type, expected_type) {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} initializer does not match its declared type"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
            }
            (checked, expression_type, CheckedClientLocalKind::Value)
        };

        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.checked_add(1)?;
        self.checked_locals.push(CheckedClientLocal {
            ordinal,
            name: local_name.clone(),
            semantic_type: expression_type.semantic_type,
            standard_value_type: expression_type.standard_value_type,
            kind,
            location: location(self.input.logical_path, span),
        });
        self.locals.insert(
            local_name,
            ClientLocalBinding {
                checked: checked.clone(),
                expression_type,
                ordinal: Some(ordinal),
                kind,
            },
        );
        Some((ordinal, checked))
    }

    fn statements(
        &mut self,
        statements: &[orna_syntax::ClientProceduralStatement],
    ) -> Option<(Vec<CheckedClientControlFlowStatement>, bool)> {
        let mut checked = Vec::with_capacity(statements.len());
        let mut guaranteed_return = false;
        for statement in statements {
            let (statement, statement_returns) = match statement {
                orna_syntax::ClientProceduralStatement::Let(statement) => {
                    let (local, expression) = self.declare_local(
                        &statement.name,
                        statement.type_source.as_ref(),
                        &statement.expression,
                        &statement.span,
                        false,
                    )?;
                    (
                        CheckedClientControlFlowStatement::Let {
                            local,
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                    let local_name = semantic_part(&statement.target);
                    let Some(binding) = self.locals.get(&local_name).cloned() else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::UnknownQualifiedName,
                            format!("unknown CLIENT local {local_name}"),
                            self.input.logical_path,
                            &statement.target.span,
                        ));
                        return None;
                    };
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.expression,
                        true,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let direct_resource = matches!(
                        &statement.expression,
                        ClientExpression::Call { callee, .. }
                            if resource_constructor_kind(&semantic_name(callee)).is_some()
                    );
                    let (expression, expression_type) =
                        if matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            && direct_resource
                        {
                            check_resource_constructor(
                                &statement.expression,
                                self.input,
                                self.targets,
                                self.action_targets,
                                self.resource_targets,
                                self.query_catalogue,
                                self.base,
                                self.server_names,
                                self.standard,
                                self.diagnostics,
                                &mut self.references,
                                &mut self.used_capabilities,
                                &self.locals,
                            )?
                        } else {
                            self.expression(&statement.expression)?
                        };
                    if !client_expression_types_compatible(expression_type, binding.expression_type)
                        || (matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            != matches!(expression, CheckedClientExpression::Resource { .. }))
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT assignment to local {local_name} does not match its declared type"
                            ),
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    }
                    if let Some(binding) = self.locals.get_mut(&local_name) {
                        binding.checked = expression.clone();
                    }
                    (
                        CheckedClientControlFlowStatement::Assignment {
                            local: binding.ordinal.expect("control-flow local has ordinal"),
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Return(statement) => {
                    let expression = if let Some(expression) = statement.expression.as_ref() {
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            expression,
                            true,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (checked, expression_type) = self.expression(expression)?;
                        let expected = ClientExpressionType {
                            semantic_type: self.input.return_type,
                            standard_value_type: self.input.standard_value_type,
                            result_shape: self.input.result_shape,
                        };
                        if !client_expression_types_compatible(expression_type, expected) {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "this CLIENT RETURN expression does not match the declared value type",
                                self.input.logical_path,
                                expression.span(),
                            ));
                            return None;
                        }
                        Some(checked)
                    } else if self.input.return_type == SemanticType::scalar(StandardScalar::Void) {
                        None
                    } else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT RETURN without an expression requires a VOID return type",
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    };
                    (
                        CheckedClientControlFlowStatement::Return {
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        true,
                    )
                }
                orna_syntax::ClientProceduralStatement::If(statement) => {
                    let incoming = self.locals.clone();
                    let mut branches = Vec::with_capacity(1 + statement.elsif_branches.len());
                    let mut all_return = true;

                    self.locals = incoming.clone();
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT IF condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (then_statements, then_returns) =
                        self.statements(&statement.then_statements)?;
                    all_return &= then_returns;
                    branches.push(CheckedClientControlFlowBranch {
                        condition,
                        statements: then_statements,
                        location: location(self.input.logical_path, &statement.span),
                    });

                    for branch in &statement.elsif_branches {
                        self.locals = incoming.clone();
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            &branch.condition,
                            false,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (condition, condition_type) = self.expression(&branch.condition)?;
                        if control_flow_supported_scalar(condition_type)
                            != Some(StandardScalar::Boolean)
                        {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "CLIENT ELSIF condition must be BOOLEAN",
                                self.input.logical_path,
                                branch.condition.span(),
                            ));
                            return None;
                        }
                        self.locals = incoming.clone();
                        let (branch_statements, branch_returns) =
                            self.statements(&branch.statements)?;
                        all_return &= branch_returns;
                        branches.push(CheckedClientControlFlowBranch {
                            condition,
                            statements: branch_statements,
                            location: location(self.input.logical_path, &branch.span),
                        });
                    }

                    let (else_statements, else_returns) =
                        if let Some(statements) = statement.else_statements.as_ref() {
                            self.locals = incoming.clone();
                            let (statements, returns) = self.statements(statements)?;
                            (Some(statements), returns)
                        } else {
                            (None, false)
                        };
                    all_return &= else_returns;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::If {
                            branches,
                            else_statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        all_return,
                    )
                }
                orna_syntax::ClientProceduralStatement::While(statement) => {
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let incoming = self.locals.clone();
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT WHILE condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (statements, _) = self.statements(&statement.body)?;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::While {
                            condition,
                            statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
            };
            guaranteed_return |= statement_returns;
            checked.push(statement);
        }
        Some((checked, guaranteed_return))
    }

    fn finish_capabilities(&mut self) -> bool {
        for capability in self.input.capabilities {
            let capability_name = semantic_name(&capability.name);
            if !self.used_capabilities.contains(&capability_name) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::CapabilityRequirement,
                    format!("declared CLIENT capability {capability_name} is not exercised"),
                    self.input.logical_path,
                    &self.input.declaration_span,
                ));
                return false;
            }
        }
        true
    }

    fn finish_direct_expression(
        mut self,
        expression: &ClientExpression,
        allow_await: bool,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, allow_await, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }
        let (checked, expression_type) = self.expression(expression)?;
        let expected = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if !client_expression_types_compatible(expression_type, expected) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function must return the declared value type",
                self.input.logical_path,
                expression.span(),
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        let location = location(self.input.logical_path, expression.span());
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: Vec::new(),
                statements: vec![CheckedClientControlFlowStatement::Return {
                    expression: Some(checked),
                    location: location.clone(),
                }],
            },
            expression_type,
            location,
            self.references,
        ))
    }

    fn finish_block(
        mut self,
        block: &orna_syntax::ClientStateBlockBody,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        if !block.states.is_empty() {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT state blocks cannot contain programmable control flow",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }

        let mut checked_statements = Vec::new();
        for local in &block.locals {
            let (ordinal, expression) = self.declare_local(
                &local.name,
                Some(&local.type_source),
                &local.expression,
                &local.span,
                false,
            )?;
            checked_statements.push(CheckedClientControlFlowStatement::Let {
                local: ordinal,
                expression,
                location: location(self.input.logical_path, &local.span),
            });
        }
        let (statements, mut guaranteed_return) = self.statements(&block.statements)?;
        checked_statements.extend(statements);

        let mut body_type = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if let Some(expression) = block.return_expression.as_ref() {
            let diagnostics_before = self.diagnostics.len();
            validate_client_await_positions(expression, true, self.input, self.diagnostics);
            if self.diagnostics.len() != diagnostics_before {
                return None;
            }
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_types_compatible(
                expression_type,
                ClientExpressionType {
                    semantic_type: self.input.return_type,
                    standard_value_type: self.input.standard_value_type,
                    result_shape: self.input.result_shape,
                },
            ) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT function must return the declared value type",
                    self.input.logical_path,
                    expression.span(),
                ));
                return None;
            }
            body_type = expression_type;
            checked_statements.push(CheckedClientControlFlowStatement::Return {
                expression: Some(checked),
                location: location(self.input.logical_path, expression.span()),
            });
            guaranteed_return = true;
        }
        if !guaranteed_return {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT control-flow blocks must return on every path",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: self.checked_locals,
                statements: checked_statements,
            },
            body_type,
            location(self.input.logical_path, &block.span),
            self.references,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_client_control_flow_body(
    input: &ResolvedClientFunctionInput<'_>,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<(
    CheckedClientFunctionBody,
    ClientExpressionType,
    SourceLocation,
    Vec<CheckedDefinitionReference>,
)> {
    let checker = ClientControlFlowChecker::new(
        input,
        submitted_ids,
        targets,
        action_targets,
        resource_targets,
        query_catalogue,
        base,
        server_names,
        standard,
        diagnostics,
    );
    match input.body {
        orna_syntax::ClientFunctionBody::Expression { expression } => {
            checker.finish_direct_expression(expression, false)
        }
        orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            checker.finish_direct_expression(expression, true)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => checker.finish_block(block),
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => None,
        _ => None,
    }
}

pub(super) fn is_closed_client_boolean_return(specification: &TypeSpecification) -> bool {
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return false;
    }
    let spelling = &name.parts[0].text;
    spelling.eq_ignore_ascii_case("BOOLEAN") || spelling.eq_ignore_ascii_case("BOOL")
}

pub(super) fn is_standard_client_boolean_return(specification: &TypeSpecification) -> bool {
    if is_closed_client_boolean_return(specification) {
        return true;
    }
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    match semantic_name(name).parts() {
        [schema, value_type] => schema == "std" && value_type == "boolean",
        [schema, types, value_type] => {
            schema == "std" && types == "types" && value_type == "boolean"
        }
        _ => false,
    }
}
