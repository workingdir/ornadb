use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationExecutionKind {
    Insert,
    Update,
    Delete,
}

impl MutationExecutionKind {
    const fn accepts_artifact_version(self, version: u32) -> bool {
        match self {
            Self::Insert => matches!(
                version,
                server_mutation_plan::INSERT_FORMAT_VERSION
                    | server_mutation_plan::RECORD_INSERT_FORMAT_VERSION
            ),
            Self::Update => version == server_mutation_plan::UPDATE_FORMAT_VERSION,
            Self::Delete => version == server_mutation_plan::DELETE_FORMAT_VERSION,
        }
    }
}

pub(super) fn validate_active_mutation<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
    operation: MutationExecutionKind,
) -> Result<ValidatedActiveMutation<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let returned =
        validate_function_signature_for_context(context, active.catalogue(), function, operation)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        operation,
    )?;
    let plan = ServerMutationPlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    validate_artifact_payload_version(function.id(), artifact.version(), &plan)?;
    let target = validate_plan_for_active(active, function, returned.target, &plan, operation)?;
    validate_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveMutation {
        returned,
        plan,
        target: target.target,
        unique_constraints: target.unique_constraints,
        arguments,
    })
}

pub(super) fn validate_active_delete<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<ValidatedActiveDelete<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let column =
        validate_delete_function_signature_with_context(context, active.catalogue(), function)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        MutationExecutionKind::Delete,
    )?;
    let plan = ServerDeletePlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let target = validate_delete_plan(active.catalogue(), function, &plan)?;
    validate_delete_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveDelete {
        column,
        plan,
        target,
        arguments,
    })
}

#[cfg(test)]
pub(super) fn validate_artifact_metadata(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
) -> Result<(), PostgresKernelError> {
    validate_artifact_metadata_for_operation(
        function,
        kind,
        format,
        version,
        language_version,
        MutationExecutionKind::Insert,
    )
}

pub(super) fn validate_artifact_metadata_for_operation(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    if kind != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function,
            "the active function must contain SERVER executable data",
        ));
    }
    if format != server_mutation_plan::FORMAT_IDENTITY
        || !operation.accepts_artifact_version(version)
    {
        return Err(artifact_error(
            function,
            match operation {
                MutationExecutionKind::Insert => {
                    "the active function must use INSERT mutation format version 1 or 4"
                }
                MutationExecutionKind::Update => {
                    "the active function must use the supported UPDATE mutation format version 2"
                }
                MutationExecutionKind::Delete => {
                    "the active function must use the supported DELETE mutation format version 3"
                }
            },
        ));
    }
    if language_version != server_mutation_plan::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function,
            "the active function must use orna.language/1",
        ));
    }
    Ok(())
}

pub(super) fn validate_artifact_payload_version(
    function: FunctionId,
    artifact_version: u32,
    plan: &ServerMutationPlan,
) -> Result<(), PostgresKernelError> {
    if artifact_version != plan.format_version() {
        return Err(artifact_error(
            function,
            "the active artifact metadata version must match its mutation payload",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_operation(catalogue, function, MutationExecutionKind::Insert)
}

#[cfg(test)]
pub(super) fn validate_function_signature_for_operation(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        operation,
    )
}

pub(super) fn validate_function_signature_for_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, operation)?;
    let reject = |rule| function_signature_error(function.id(), rule);
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let [column] = columns.as_slice() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let ResolvedRuntimeType::Reference(target) =
        resolve_runtime_type(context, column.resolved_type())
    else {
        return Err(reject(
            "the sole result column must be a non-null object reference",
        ));
    };
    if catalogue.object_type_by_id(target).is_none() {
        return Err(reject(
            "the result column must reference an active object type",
        ));
    }
    let column = ResultColumn::new(column.name(), ResolvedType::reference(target), false)
        .map_err(ServerInsertError::ResultRows)
        .map_err(server_error)?;
    Ok(ValidatedReturn { target, column })
}

#[cfg(test)]
pub(super) fn validate_delete_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_delete_function_signature_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
    )
}

pub(super) fn validate_delete_function_signature_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, MutationExecutionKind::Delete)?;
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    if !runtime_types_match(
        context,
        column.resolved_type(),
        ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
    ) {
        return Err(function_signature_error(
            function.id(),
            "the sole DELETE result column must be BOOLEAN",
        ));
    }
    ResultColumn::new(
        column.name(),
        ResolvedType::Scalar(orna_core::types::StandardScalar::Boolean),
        false,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)
}

fn validate_mutation_function_header(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    let reject = |rule| function_signature_error(function.id(), rule);
    if function.domain() != FunctionDomain::Server {
        return Err(reject("this operation requires a SERVER function"));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => "an INSERT SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Update => "an UPDATE SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Delete => "a DELETE SERVER function must use SECURITY INVOKER",
        }));
    }
    if function.transaction() != Some(FunctionTransaction::Atomic) {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use exactly TRANSACTION ATOMIC"
            }
        }));
    }
    if function.volatility() != FunctionVolatility::Volatile {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use VOLATILITY VOLATILE"
            }
        }));
    }
    for parameter in function.parameters() {
        if parameter.default_expression().is_some() {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "INSERT SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Update => {
                    "UPDATE SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Delete => {
                    "DELETE SERVER function parameters cannot have default expressions"
                }
            }));
        }
        if !runtime_type_is_active(context, catalogue, parameter.resolved_type()) {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "every INSERT SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Update => {
                    "every UPDATE SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Delete => {
                    "every DELETE SERVER function parameter must use a supported active type"
                }
            }));
        }
    }
    Ok(())
}

pub(super) fn function_signature_error(
    function: FunctionId,
    rule: &'static str,
) -> PostgresKernelError {
    server_error(ServerMutationError::FunctionSignature { function, rule })
}

fn runtime_type_is_active(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> bool {
    let runtime = resolve_mutation_runtime_type(context, catalogue, resolved_type);
    if postgres_type(runtime).is_none() {
        return false;
    }
    match runtime {
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::LegacyScalar(_) | ResolvedRuntimeType::VerifiedValue { .. } => true,
        ResolvedRuntimeType::CatalogueEnum(_) => true,
        ResolvedRuntimeType::Record(_) | ResolvedRuntimeType::Unsupported => false,
    }
}

fn validate_active_runtime_type(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let runtime = resolve_mutation_runtime_type(context, catalogue, resolved_type);
    if postgres_type(runtime).is_none() {
        return Err(plan_invariant(rule));
    }
    match runtime {
        ResolvedRuntimeType::Reference(target) if catalogue.object_type_by_id(target).is_none() => {
            return Err(plan_invariant(
                "every referenced object type must be active",
            ));
        }
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::CatalogueEnum(_)
        | ResolvedRuntimeType::Reference(_) => {}
        ResolvedRuntimeType::Record(_) | ResolvedRuntimeType::Unsupported => {
            return Err(plan_invariant(rule));
        }
    }
    Ok(())
}

fn resolve_mutation_runtime_type(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> ResolvedRuntimeType {
    let runtime = resolve_catalogue_runtime_type(catalogue, context, resolved_type);
    if runtime == ResolvedRuntimeType::Unsupported
        && resolved_type.named_type().is_some_and(|enum_type| {
            context
                .standard()
                .is_some_and(|standard| standard.catalogue().enum_type_by_id(enum_type).is_some())
        })
    {
        ResolvedRuntimeType::CatalogueEnum(
            resolved_type
                .named_type()
                .expect("standard enum identity was checked"),
        )
    } else {
        runtime
    }
}

#[cfg(test)]
pub(super) fn validate_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    Ok(validate_plan_for_operation(
        catalogue,
        function,
        returned_target,
        plan,
        MutationExecutionKind::Insert,
    )?
    .target)
}

#[cfg(test)]
pub(super) fn validate_plan_for_context<'a>(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_with_active(
        None,
        context,
        catalogue,
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_plan_for_active<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_with_active(
        Some(active),
        active.catalogue_hash_context(),
        active.catalogue(),
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_plan_with_active<'a>(
    active: Option<&'a ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    let operation_matches = matches!(
        (operation, plan.operation()),
        (
            MutationExecutionKind::Insert,
            ServerMutationOperation::Insert
        ) | (
            MutationExecutionKind::Update,
            ServerMutationOperation::Update { .. }
        )
    );
    if !operation_matches || !operation.accepts_artifact_version(plan.format_version()) {
        return Err(plan_invariant(
            "the payload operation and version must match the requested mutation",
        ));
    }
    if plan.returned_object() != plan.target() || plan.target() != returned_target {
        return Err(plan_invariant(
            "plan target, returned object, and declared result REF target must match",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("mutation target must be an active object type"))?;
    let unique_constraints = UniqueConstraints::from_target(context, target)?;
    for field in target.fields() {
        if field.default_expression().is_some() {
            return Err(plan_invariant(
                "mutation targets cannot contain field default expressions",
            ));
        }
        match resolve_runtime_type(context, field.resolved_type()) {
            ResolvedRuntimeType::Reference(target)
                if catalogue.object_type_by_id(target).is_none() =>
            {
                return Err(plan_invariant(
                    "every target-field REF type must name an active object type",
                ));
            }
            ResolvedRuntimeType::LegacyScalar(_)
            | ResolvedRuntimeType::VerifiedValue { .. }
            | ResolvedRuntimeType::Reference(_)
            | ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Unsupported => {}
        }
    }

    let mut assigned = BTreeMap::new();
    for assignment in plan.assignments() {
        if assignment.owner() != target.id() {
            return Err(plan_invariant(
                "every assignment owner must equal the mutation target",
            ));
        }
        let field = target
            .field_by_id(assignment.field())
            .ok_or_else(|| plan_invariant("every owner-qualified assigned field must be active"))?;
        if assigned.insert(field.id(), ()).is_some() {
            return Err(plan_invariant(
                "an owner-qualified field cannot be assigned more than once",
            ));
        }
        let expression = assignment.expression();
        if let MutationExpressionKind::RecordConstructor { fields } = expression.kind() {
            validate_record_constructor(
                active.ok_or_else(|| {
                    plan_invariant(
                        "record constructors require one complete active database revision",
                    )
                })?,
                function,
                field,
                expression,
                fields,
                operation,
            )?;
            continue;
        }
        validate_active_runtime_type(
            context,
            catalogue,
            expression.resolved_type(),
            "every assignment expression must use the active runtime subset",
        )?;
        if !runtime_types_match(context, expression.resolved_type(), field.resolved_type()) {
            return Err(plan_invariant(
                "assignment expression type must exactly equal its target field type",
            ));
        }
        if matches!(expression.kind(), MutationExpressionKind::TypedNull) && !field.nullable() {
            return Err(plan_invariant(
                "typed NULL can target only a nullable field",
            ));
        }
        if let MutationExpressionKind::Parameter { owner, parameter } = expression.kind() {
            if *owner != function.id() {
                return Err(plan_invariant(
                    "parameter expression owner must equal the active function",
                ));
            }
            let parameter = function.parameter_by_id(*parameter).ok_or_else(|| {
                plan_invariant("parameter expression must name an active declared parameter")
            })?;
            if parameter.default_expression().is_some()
                || !runtime_types_match(
                    context,
                    parameter.resolved_type(),
                    expression.resolved_type(),
                )
            {
                return Err(plan_invariant(
                    "parameter expression must exactly match a required active parameter",
                ));
            }
        }
    }
    if operation == MutationExecutionKind::Insert
        && target
            .fields()
            .iter()
            .any(|field| !field.nullable() && !assigned.contains_key(&field.id()))
    {
        return Err(plan_invariant(
            "every non-null target field must have an assignment",
        ));
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        if selector.owner() != function.id() {
            return Err(plan_invariant(
                "selector owner must equal the active function",
            ));
        }
        let parameter = function
            .parameter_by_id(selector.parameter())
            .ok_or_else(|| plan_invariant("selector must name an active declared parameter"))?;
        if parameter.default_expression().is_some()
            || parameter.resolved_type() != ResolvedType::reference(target.id())
        {
            return Err(plan_invariant(
                "selector must exactly match a required REF parameter for the target object",
            ));
        }
    }
    Ok(ValidatedMutationTarget {
        target,
        unique_constraints,
    })
}

fn validate_record_constructor(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    target_field: &orna_core::catalogue::FieldDefinition,
    expression: &server_mutation_plan::MutationExpression,
    fields: &[server_mutation_plan::RecordFieldExpression],
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    if operation != MutationExecutionKind::Insert {
        return Err(plan_invariant(
            "record constructors are accepted only in INSERT plans",
        ));
    }
    let record_type = expression
        .resolved_type()
        .named_type()
        .ok_or_else(|| plan_invariant("record constructor must retain its nominal record type"))?;
    if target_field.nullable() || target_field.resolved_type() != ResolvedType::named(record_type) {
        return Err(plan_invariant(
            "record constructor must target a non-null field of its exact nominal type",
        ));
    }
    let definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or_else(|| plan_invariant("record constructor type must be active"))?;
    if fields.len() != definition.fields().len() {
        return Err(plan_invariant(
            "record constructor field count must match its active definition",
        ));
    }
    for (field, declared) in fields.iter().zip(definition.fields()) {
        if field.owner() != record_type || field.field() != declared.id() {
            return Err(plan_invariant(
                "record constructor fields must retain active declaration order and identity",
            ));
        }
        let runtime_type = active
            .record_value_field_descriptor_runtime_type(declared.descriptor())
            .ok_or_else(|| plan_invariant("record constructor field type must be active"))?;
        if !runtime_types_match(
            active.catalogue_hash_context(),
            field.resolved_type(),
            runtime_type,
        ) {
            return Err(plan_invariant(
                "record constructor child type must match its active field type",
            ));
        }
        validate_active_runtime_type(
            active.catalogue_hash_context(),
            active.catalogue(),
            field.resolved_type(),
            "record constructor child type must be active",
        )?;
        match field.kind() {
            RecordFieldExpressionKind::Parameter { owner, parameter } => {
                if *owner != function.id() {
                    return Err(plan_invariant(
                        "record constructor parameter owner must equal the active function",
                    ));
                }
                let parameter = function.parameter_by_id(*parameter).ok_or_else(|| {
                    plan_invariant("record constructor parameter must be actively declared")
                })?;
                if parameter.default_expression().is_some()
                    || !runtime_types_match(
                        active.catalogue_hash_context(),
                        parameter.resolved_type(),
                        field.resolved_type(),
                    )
                {
                    return Err(plan_invariant(
                        "record constructor parameter must exactly match its artifact child",
                    ));
                }
            }
            RecordFieldExpressionKind::BooleanLiteral { .. } => {
                if !runtime_types_match(
                    active.catalogue_hash_context(),
                    field.resolved_type(),
                    ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
                ) {
                    return Err(plan_invariant(
                        "record constructor Boolean child must target a Boolean field",
                    ));
                }
            }
            _ => {
                return Err(plan_invariant(
                    "unknown future record constructor child kinds are unsupported",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_plan_for_operation<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        returned_target,
        plan,
        operation,
    )
}

pub(super) fn validate_delete_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    if plan.format_version() != server_mutation_plan::DELETE_FORMAT_VERSION {
        return Err(plan_invariant(
            "the DELETE payload must use mutation format version 3",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("DELETE target must be an active object type"))?;
    let selector = plan.selector();
    if selector.owner() != function.id() {
        return Err(plan_invariant(
            "DELETE selector owner must equal the active function",
        ));
    }
    let parameter = function
        .parameter_by_id(selector.parameter())
        .ok_or_else(|| plan_invariant("DELETE selector must name an active declared parameter"))?;
    if parameter.default_expression().is_some()
        || parameter.resolved_type() != ResolvedType::reference(target.id())
    {
        return Err(plan_invariant(
            "DELETE selector must exactly match a required REF parameter for the target object",
        ));
    }
    Ok(target)
}

fn validate_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and mutation body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and mutation body order"
            }
        };
        server_error(ServerInsertError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

fn validate_delete_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_delete_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and DELETE body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and DELETE body order"
            }
        };
        server_error(ServerMutationError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

pub(super) fn expected_delete_body_references(
    plan: &ServerDeletePlan,
) -> [ExpectedDefinitionReference; 3] {
    [
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector().owner(),
                parameter: plan.selector().parameter(),
            },
        ),
    ]
}

pub(super) fn expected_body_references(
    plan: &ServerMutationPlan,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = Vec::with_capacity(plan.assignments().len().saturating_mul(2) + 4);
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::WriteObject,
        DefinitionReferenceTarget::ObjectType(plan.target()),
    ));
    for assignment in plan.assignments() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: assignment.owner(),
                field: assignment.field(),
            },
        ));
        match assignment.expression().kind() {
            MutationExpressionKind::Parameter { owner, parameter } => {
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: *owner,
                        parameter: *parameter,
                    },
                ));
            }
            MutationExpressionKind::RecordConstructor { fields } => {
                let record_type = assignment
                    .expression()
                    .resolved_type()
                    .named_type()
                    .expect("validated record constructor must retain a named type");
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(record_type),
                ));
                for field in fields {
                    expected.push(ExpectedDefinitionReference::new(
                        DefinitionReferenceKind::WriteField,
                        DefinitionReferenceTarget::Field {
                            owner: field.owner(),
                            field: field.field(),
                        },
                    ));
                    if let RecordFieldExpressionKind::Parameter { owner, parameter } = field.kind()
                    {
                        expected.push(ExpectedDefinitionReference::new(
                            DefinitionReferenceKind::ParameterRead,
                            DefinitionReferenceTarget::Parameter {
                                owner: *owner,
                                parameter: *parameter,
                            },
                        ));
                    }
                }
            }
            MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull => {}
            _ => {}
        }
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ));
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: selector.owner(),
                parameter: selector.parameter(),
            },
        ));
    }
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.returned_object()),
    ));
    expected
}

pub(super) fn validate_arguments_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    let mut validated = BTreeMap::new();
    let mut variable_payload = 0usize;
    for argument in arguments {
        let parameter_id = argument.parameter();
        if validated.contains_key(&parameter_id) {
            return Err(argument_error(
                Some(parameter_id),
                "the same parameter was supplied twice",
            ));
        }
        let parameter = function.parameter_by_id(parameter_id).ok_or_else(|| {
            argument_error(
                Some(parameter_id),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
        let value = argument.value();
        if value.is_null() {
            return Err(argument_error(
                Some(parameter_id),
                "function arguments cannot be NULL",
            ));
        }
        let RuntimeType::Flat(value_type) = value.runtime_type() else {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        };
        if !runtime_type_is_active(context, catalogue, value_type) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        }
        if !runtime_types_match(context, value_type, parameter.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
            ));
        }
        if let RuntimeValue::Enum(value) = value
            && !enum_value_is_active(context, catalogue, value)
        {
            return Err(argument_error(
                Some(parameter_id),
                "the enum argument label is not active in the pinned catalogue",
            ));
        }
        variable_payload = variable_payload
            .checked_add(variable_payload_len(value)?)
            .ok_or_else(payload_limit_error)?;
        if variable_payload > VARIABLE_ARGUMENT_PAYLOAD_LIMIT {
            return Err(payload_limit_error());
        }
        validated.insert(parameter_id, BindValue::from_runtime(value, parameter_id)?);
    }
    for parameter in function.parameters() {
        if !validated.contains_key(&parameter.id()) {
            return Err(argument_error(
                Some(parameter.id()),
                "a required argument is missing",
            ));
        }
    }
    Ok(validated)
}

#[cfg(test)]
pub(super) fn validate_arguments(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    validate_arguments_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        arguments,
    )
}

pub(super) fn selector_object(
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let selector = plan
        .selector()
        .ok_or_else(|| plan_invariant("UPDATE plan must contain one selector parameter"))?;
    selector_argument_object(plan.target(), selector, arguments)
}

pub(super) fn selector_argument_object(
    target: TypeId,
    selector: MutationSelector,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let argument = arguments
        .iter()
        .find(|argument| argument.parameter() == selector.parameter())
        .ok_or_else(|| plan_invariant("validated selector argument must be present"))?;
    match argument.value() {
        RuntimeValue::Reference {
            target: actual,
            object,
        } if *actual == target => Ok(*object),
        _ => Err(plan_invariant(
            "validated selector argument must be an exact target object reference",
        )),
    }
}

fn variable_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    match value {
        RuntimeValue::Text(value) => Ok(value.len()),
        RuntimeValue::Bytes(value) => Ok(value.len()),
        RuntimeValue::Enum(value) => Ok(value.label().len()),
        RuntimeValue::Null(_)
        | RuntimeValue::Boolean(_)
        | RuntimeValue::Integer(_)
        | RuntimeValue::BigInt(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Reference { .. } => Ok(0),
        _ => Err(argument_error(None, "the argument type is unsupported")),
    }
}

fn payload_limit_error() -> PostgresKernelError {
    server_error(ServerInsertError::ComplexityLimit {
        category: "total size of text and binary arguments",
        maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum BindValue {
    Boolean(bool),
    Integer(i32),
    BigInt(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Enum { value: EnumValue, label: String },
}

impl BindValue {
    pub(super) fn from_runtime(
        value: &RuntimeValue,
        parameter: ParameterId,
    ) -> Result<Self, PostgresKernelError> {
        match value {
            RuntimeValue::Boolean(value) => Ok(Self::Boolean(*value)),
            RuntimeValue::Integer(value) => Ok(Self::Integer(*value)),
            RuntimeValue::BigInt(value) => Ok(Self::BigInt(*value)),
            RuntimeValue::Float(value) => Ok(Self::Float(value.value())),
            RuntimeValue::Text(value) => Ok(Self::Text(value.clone())),
            RuntimeValue::Bytes(value) => Ok(Self::Bytes(value.clone())),
            RuntimeValue::Reference { object, .. } => Ok(Self::Bytes(object.to_bytes().to_vec())),
            RuntimeValue::Enum(value) => Ok(Self::Enum {
                value: value.clone(),
                label: value.label().to_owned(),
            }),
            RuntimeValue::Null(_) => Err(argument_error(
                Some(parameter),
                "function arguments cannot be NULL",
            )),
            _ => Err(argument_error(
                Some(parameter),
                "the argument type is unsupported",
            )),
        }
    }

    pub(super) fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Integer(value) => value,
            Self::BigInt(value) => value,
            Self::Float(value) => value,
            Self::Text(value) => value,
            Self::Bytes(value) => value,
            Self::Enum { label, .. } => label,
        }
    }

    pub(super) fn to_runtime(&self) -> RuntimeValue {
        match self {
            Self::Boolean(value) => RuntimeValue::Boolean(*value),
            Self::Integer(value) => RuntimeValue::Integer(*value),
            Self::BigInt(value) => RuntimeValue::BigInt(*value),
            Self::Float(value) => RuntimeValue::Float(
                orna_core::value::RuntimeFloat::new(*value)
                    .expect("validated bind float must remain finite"),
            ),
            Self::Text(value) => RuntimeValue::Text(value.clone()),
            Self::Bytes(value) => RuntimeValue::Bytes(value.clone()),
            Self::Enum { value, .. } => RuntimeValue::Enum(value.clone()),
        }
    }
}

fn enum_value_is_active(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    value: &EnumValue,
) -> bool {
    catalogue
        .enum_type_by_id(value.enum_type())
        .or_else(|| {
            context
                .standard()
                .and_then(|standard| standard.catalogue().enum_type_by_id(value.enum_type()))
        })
        .is_some_and(|definition| {
            definition
                .labels()
                .iter()
                .any(|label| label == value.label())
        })
}
