use super::*;

/// Reports whether the pinned active artefact selects the narrow raw `INSERT` path.
///
/// This classification does not validate the target. Validation stays in the
/// authorised execution entry so that a rejected target can roll back only the
/// caller savepoint.
pub(crate) fn raw_server_insert_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    let Some(function) = active.catalogue().function_by_id(function_id) else {
        return false;
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    }) else {
        return false;
    };
    let artifact = revision.artifact();
    function.domain() == FunctionDomain::Server
        && artifact.kind() == ExecutableArtifactKind::Server
        && artifact.format() == server_mutation_plan::FORMAT_IDENTITY
        && matches!(
            artifact.version(),
            server_mutation_plan::INSERT_FORMAT_VERSION
                | server_mutation_plan::RECORD_INSERT_FORMAT_VERSION
        )
}

/// Executes one pinned parameter-free raw SERVER `INSERT` in the caller transaction.
///
/// The caller owns recovery, authorisation, audit, savepoint, and commit. This
/// entry neither opens a session nor starts or commits a transaction.
pub(crate) async fn execute_authorised_raw_server_insert(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
) -> Result<RuntimeValue, PostgresKernelError> {
    execute_authorised_raw_server_insert_with_arguments(transaction, active, authorisation, &[])
        .await
}

/// Executes one pinned raw SERVER `INSERT` with zero arguments, one accepted
/// scalar or Reference argument, or one bounded pair of those values.
///
/// The caller owns recovery, authorisation, audit, savepoint, and commit. This
/// entry validates the raw argument shape, then delegates stable identity and
/// type binding to the normal active INSERT executor.
pub(crate) async fn execute_authorised_raw_server_insert_with_arguments(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
) -> Result<RuntimeValue, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "active catalogue",
            record: target.function().canonical(),
            rule: "raw INSERT authorisation target must match the active pair",
        });
    }
    let function = active
        .catalogue()
        .function_by_id(target.function())
        .ok_or_else(|| {
            server_error(ServerInsertError::FunctionNotActive {
                pair: active.pair(),
                function: target.function(),
            })
        })?;
    validate_raw_server_insert_argument_shape(function, arguments)?;
    let context = ServerInsertContext::new(
        active.pair(),
        target.function(),
        function.current_revision(),
    );
    let validated = validate_active_raw_server_insert(active, function, arguments)
        .map_err(|error| not_committed(context, error))?;
    let (result, _) = execute_validated_active_insert(transaction, active, context, validated)
        .await
        .map_err(|error| not_committed(context, error))?;
    Ok(RuntimeValue::Reference {
        target: result.target(),
        object: result.object(),
    })
}

pub(super) fn validate_raw_server_insert_argument_shape(
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    if arguments.is_empty() {
        if function.parameters().is_empty() {
            return Ok(());
        }
        return Err(argument_error(
            None,
            "raw SERVER INSERT calls must have zero parameters",
        ));
    }

    match arguments {
        [argument] if raw_server_insert_argument_is_supported(argument) => Ok(()),
        [argument] => Err(argument_error(
            Some(argument.parameter()),
            "raw SERVER INSERT calls accept only one supported scalar or Reference argument",
        )),
        [first, second]
            if raw_server_insert_argument_is_supported(first)
                && raw_server_insert_argument_is_supported(second) =>
        {
            Ok(())
        }
        [first, second] => {
            let rejected = if raw_server_insert_argument_is_supported(first) {
                second
            } else {
                first
            };
            Err(argument_error(
                Some(rejected.parameter()),
                "raw SERVER INSERT argument pairs accept only supported scalar or Reference values",
            ))
        }
        _ => Err(argument_error(
            None,
            "raw SERVER INSERT calls accept at most two supported scalar or Reference arguments",
        )),
    }
}

fn raw_server_insert_argument_is_supported(argument: &FunctionArgument) -> bool {
    matches!(
        argument.value(),
        RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
    )
}

pub(super) fn validate_raw_reference_insert_parameter_use(
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let [argument] = arguments else {
        return Ok(());
    };
    if !matches!(argument.value(), RuntimeValue::Reference { .. }) {
        return Ok(());
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "raw Reference INSERT calls must declare exactly one parameter",
        ));
    };
    let reads_parameter = plan.assignments().iter().any(|assignment| {
        matches!(
            assignment.expression().kind(),
            MutationExpressionKind::Parameter { owner, parameter: read }
                if *owner == function.id() && *read == parameter.id()
        )
    });
    if !reads_parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw SERVER INSERT calls must read the sole Reference parameter",
        ));
    }
    Ok(())
}

pub(super) fn validate_raw_scalar_insert_parameter_use(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let [argument] = arguments else {
        return Ok(());
    };
    if !matches!(
        argument.value(),
        RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
    ) {
        return Ok(());
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "raw scalar INSERT calls must declare exactly one parameter",
        ));
    };
    let reads_parameter = plan.assignments().iter().any(|assignment| {
        let expression = assignment.expression();
        matches!(
            expression.kind(),
            MutationExpressionKind::Parameter { owner, parameter: read }
                if *owner == function.id() && *read == parameter.id()
        ) && runtime_types_match(
            active.catalogue_hash_context(),
            expression.resolved_type(),
            parameter.resolved_type(),
        )
    });
    if argument.parameter() != parameter.id() || !reads_parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw SERVER INSERT calls must directly read the sole scalar parameter",
        ));
    }
    Ok(())
}

pub(super) fn validate_raw_argument_pair_insert_parameter_use(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let Some([first, second]) = raw_argument_pair_in_parameter_order(arguments) else {
        return Ok(());
    };
    if first.parameter() == second.parameter() {
        return Err(argument_error(
            Some(second.parameter()),
            "raw SERVER INSERT argument pairs require two distinct parameter identities",
        ));
    }
    if function.parameters().len() != 2 {
        return Err(function_signature_error(
            function.id(),
            "raw SERVER INSERT argument pairs require exactly two parameters",
        ));
    }
    for argument in [first, second] {
        let parameter = function
            .parameter_by_id(argument.parameter())
            .ok_or_else(|| {
                argument_error(
                    Some(argument.parameter()),
                    "raw SERVER INSERT argument pairs must name declared parameters",
                )
            })?;
        let reads_parameter = plan.assignments().iter().any(|assignment| {
            let expression = assignment.expression();
            matches!(
                expression.kind(),
                MutationExpressionKind::Parameter { owner, parameter: read }
                    if *owner == function.id() && *read == parameter.id()
            ) && runtime_types_match(
                active.catalogue_hash_context(),
                expression.resolved_type(),
                parameter.resolved_type(),
            )
        });
        if !reads_parameter {
            return Err(argument_error(
                Some(argument.parameter()),
                "raw SERVER INSERT argument pairs must directly read both supplied parameters",
            ));
        }
    }
    Ok(())
}

fn raw_argument_pair_in_parameter_order(
    arguments: &[FunctionArgument],
) -> Option<[&FunctionArgument; 2]> {
    let [first, second] = arguments else {
        return None;
    };
    Some(if first.parameter() <= second.parameter() {
        [first, second]
    } else {
        [second, first]
    })
}

pub(super) fn validate_raw_text_insert_argument(
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let validate = |argument: &FunctionArgument| {
        if matches!(argument.value(), RuntimeValue::Text(value) if value.contains('\0')) {
            return Err(argument_error(
                Some(argument.parameter()),
                "raw Text INSERT arguments cannot contain U+0000",
            ));
        }
        Ok(())
    };
    if let Some(arguments) = raw_argument_pair_in_parameter_order(arguments) {
        for argument in arguments {
            validate(argument)?;
        }
    } else {
        for argument in arguments {
            validate(argument)?;
        }
    }
    Ok(())
}

fn validate_active_raw_server_insert<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<ValidatedActiveMutation<'a>, PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Insert)?;
    validate_raw_reference_insert_parameter_use(function, &validated.plan, arguments)?;
    validate_raw_scalar_insert_parameter_use(active, function, &validated.plan, arguments)?;
    validate_raw_argument_pair_insert_parameter_use(active, function, &validated.plan, arguments)?;
    validate_raw_text_insert_argument(arguments)?;
    Ok(validated)
}

/// Reports whether an INSERT failure is a closed raw target rejection.
pub(crate) const fn raw_server_insert_target_is_unavailable(error: &ServerInsertError) -> bool {
    match error {
        ServerInsertError::NotCommitted { source, .. } => {
            raw_server_insert_target_is_unavailable(source)
        }
        ServerInsertError::FunctionNotActive { .. }
        | ServerInsertError::FunctionSignature { .. }
        | ServerInsertError::Artifact { .. }
        | ServerInsertError::PlanDecode(_)
        | ServerInsertError::PlanInvariant { .. }
        | ServerInsertError::ReferenceEvidence { .. }
        | ServerInsertError::Argument { .. }
        | ServerInsertError::ComplexityLimit { .. }
        | ServerInsertError::ResultRows(_)
        | ServerInsertError::RecordValue(_)
        | ServerInsertError::ValueCodec(_)
        | ServerInsertError::UniqueReferenceConflict { .. } => true,
        ServerInsertError::Kernel { .. }
        | ServerInsertError::Database { .. }
        | ServerInsertError::CurrentRevision { .. }
        | ServerInsertError::PreparedResult { .. }
        | ServerInsertError::RowDecode { .. }
        | ServerInsertError::ValueInvariant { .. }
        | ServerInsertError::UniqueTextConflict { .. }
        | ServerInsertError::CommitRejected { .. }
        | ServerInsertError::CommitOutcomeUnknown { .. }
        | ServerInsertError::CommittedButShutdownFailed { .. } => false,
    }
}

/// The exact SERVER mutation family selected by one raw mutation call.
///
/// UPDATE accepts the retained one-Reference call or the bounded
/// selector/value pair. DELETE accepts only the retained one-Reference call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawServerReferenceMutation {
    /// One accepted version-2 identity-selected UPDATE.
    Update,
    /// One accepted version-3 identity-selected DELETE.
    Delete,
}

/// Selects a superficial raw reference-mutation artifact candidate.
///
/// This classification deliberately stops before decoding or validating the
/// target. The authorised caller opens a savepoint only for one of these two
/// artifact families, then the normal mutation validator remains authoritative.
pub(crate) fn raw_server_reference_mutation_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Option<RawServerReferenceMutation> {
    let function = active.catalogue().function_by_id(function_id)?;
    let revision = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    })?;
    let artifact = revision.artifact();
    if function.domain() != FunctionDomain::Server
        || artifact.kind() != ExecutableArtifactKind::Server
        || artifact.format() != server_mutation_plan::FORMAT_IDENTITY
    {
        return None;
    }
    match artifact.version() {
        server_mutation_plan::UPDATE_FORMAT_VERSION => Some(RawServerReferenceMutation::Update),
        server_mutation_plan::DELETE_FORMAT_VERSION => Some(RawServerReferenceMutation::Delete),
        _ => None,
    }
}

/// Reports whether one active artifact is a superficial version-2 UPDATE target.
///
/// This predicate does not inspect the function signature, plan payload, or
/// arguments. The authorised UPDATE entry remains the validation authority.
pub(crate) fn raw_server_reference_value_update_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    raw_server_reference_mutation_target(active, function_id)
        == Some(RawServerReferenceMutation::Update)
}

/// Executes one pinned raw SERVER UPDATE or DELETE.
///
/// UPDATE accepts either its retained constant-assignment Reference selector
/// or its Reference selector plus one caller value. DELETE accepts only its
/// retained Reference selector.
///
/// The caller owns recovery, authorisation, audit, savepoint, and commit. This
/// entry neither opens a session nor starts or commits a transaction.
pub(crate) async fn execute_authorised_raw_server_reference_mutation(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    operation: RawServerReferenceMutation,
    arguments: &[FunctionArgument],
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "active catalogue",
            record: target.function().canonical(),
            rule: "raw reference mutation authorisation target must match the active pair",
        });
    }
    let function = active
        .catalogue()
        .function_by_id(target.function())
        .ok_or_else(|| match operation {
            RawServerReferenceMutation::Update => {
                update_error(ServerUpdateError::FunctionNotActive {
                    pair: active.pair(),
                    function: target.function(),
                })
            }
            RawServerReferenceMutation::Delete => {
                delete_error(ServerDeleteError::FunctionNotActive {
                    pair: active.pair(),
                    function: target.function(),
                })
            }
        })?;
    let context = ServerMutationContext::new(
        active.pair(),
        target.function(),
        function.current_revision(),
    );
    match operation {
        RawServerReferenceMutation::Update => {
            let validated = if matches!(arguments, [_, _]) {
                validate_active_raw_server_reference_value_update(active, function, arguments)
            } else {
                validate_raw_reference_update_shape(active, function, arguments).and_then(|()| {
                    validate_active_mutation(
                        active,
                        function,
                        arguments,
                        MutationExecutionKind::Update,
                    )
                })
            }
            .map_err(|error| update_not_committed(context, error))?;
            let (result, _) =
                execute_validated_active_update(transaction, active, context, validated, arguments)
                    .await
                    .map_err(|error| update_not_committed(context, error))?;
            Ok(result_rows_values(result.rows()))
        }
        RawServerReferenceMutation::Delete => {
            validate_raw_reference_delete_shape(active, function, arguments)
                .map_err(|error| delete_not_committed(context, error))?;
            let result = execute_active_delete(transaction, active, function, context, arguments)
                .await
                .map_err(|error| delete_not_committed(context, error))?;
            Ok(result_rows_values(result.rows()))
        }
    }
}

fn validate_raw_reference_update_shape(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "a raw reference UPDATE must declare only its selector parameter",
        ));
    };
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "raw reference mutations accept exactly one reference argument",
        ));
    };
    let RuntimeValue::Reference { target, .. } = argument.value() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw reference mutations accept exactly one reference argument",
        ));
    };
    let artifact = active_function_artifact(active, function)?;
    let plan = ServerMutationPlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let Some(selector) = plan.selector() else {
        return Err(plan_invariant(
            "raw reference UPDATE must contain a selector",
        ));
    };
    if selector.owner() != function.id()
        || selector.parameter() != parameter.id()
        || argument.parameter() != parameter.id()
        || parameter.resolved_type() != ResolvedType::reference(plan.target())
        || *target != plan.target()
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw reference UPDATE selector must match its sole active parameter and target",
        ));
    }
    if plan.assignments().iter().any(|assignment| {
        !matches!(
            assignment.expression().kind(),
            MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull
        )
    }) {
        return Err(function_signature_error(
            function.id(),
            "raw reference UPDATE assignments must use only literal values",
        ));
    }
    Ok(())
}

fn validate_active_raw_server_reference_value_update<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<ValidatedActiveMutation<'a>, PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Update)?;
    let value_parameter =
        validate_raw_reference_value_update_shape(active, function, &validated.plan, arguments)?;
    validate_raw_reference_value_update_text_argument(value_parameter, arguments)?;
    Ok(validated)
}

fn validate_raw_reference_value_update_shape(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<ParameterId, PostgresKernelError> {
    let Some(arguments) = raw_argument_pair_in_parameter_order(arguments) else {
        return Err(argument_error(
            None,
            "a raw reference value UPDATE requires exactly two arguments",
        ));
    };
    if arguments[0].parameter() == arguments[1].parameter() {
        return Err(argument_error(
            Some(arguments[1].parameter()),
            "a raw reference value UPDATE requires two distinct parameter identities",
        ));
    }
    if function.parameters().len() != 2 {
        return Err(function_signature_error(
            function.id(),
            "a raw reference value UPDATE must declare exactly two parameters",
        ));
    }
    let selector = plan
        .selector()
        .ok_or_else(|| plan_invariant("raw reference value UPDATE must contain a selector"))?;
    if selector.owner() != function.id() {
        return Err(plan_invariant(
            "raw reference value UPDATE selector owner must match the active function",
        ));
    }

    let mut value_parameter = None;
    for argument in arguments {
        let parameter = function
            .parameter_by_id(argument.parameter())
            .ok_or_else(|| {
                argument_error(
                    Some(argument.parameter()),
                    "raw reference value UPDATE arguments must name declared parameters",
                )
            })?;
        if argument.parameter() == selector.parameter() {
            if parameter.resolved_type() != ResolvedType::reference(plan.target())
                || !matches!(
                    argument.value(),
                    RuntimeValue::Reference { target, .. } if *target == plan.target()
                )
            {
                return Err(argument_error(
                    Some(argument.parameter()),
                    "raw reference value UPDATE selector must be an exact target reference",
                ));
            }
            if plan.assignments().iter().any(|assignment| {
                matches!(
                    assignment.expression().kind(),
                    MutationExpressionKind::Parameter { owner, parameter }
                        if *owner == function.id() && *parameter == argument.parameter()
                )
            }) {
                return Err(argument_error(
                    Some(argument.parameter()),
                    "raw reference value UPDATE cannot assign from its selector parameter",
                ));
            }
        } else {
            let reads_value = plan.assignments().iter().any(|assignment| {
                let expression = assignment.expression();
                matches!(
                    expression.kind(),
                    MutationExpressionKind::Parameter { owner, parameter }
                        if *owner == function.id() && *parameter == argument.parameter()
                ) && runtime_types_match(
                    active.catalogue_hash_context(),
                    expression.resolved_type(),
                    parameter.resolved_type(),
                )
            });
            if !reads_value {
                return Err(argument_error(
                    Some(argument.parameter()),
                    "raw reference value UPDATE must directly read its value parameter",
                ));
            }
            value_parameter = Some(argument.parameter());
        }
    }
    let value_parameter = value_parameter.ok_or_else(|| {
        argument_error(
            None,
            "raw reference value UPDATE must supply one selector and one value",
        )
    })?;
    if plan.assignments().iter().any(|assignment| {
        !matches!(
            assignment.expression().kind(),
            MutationExpressionKind::Parameter { owner, parameter }
                if *owner == function.id() && *parameter == value_parameter
        ) && !matches!(
            assignment.expression().kind(),
            MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull
        )
    }) {
        return Err(function_signature_error(
            function.id(),
            "raw reference value UPDATE assignments must use the value parameter or literal values",
        ));
    }
    Ok(value_parameter)
}

fn validate_raw_reference_value_update_text_argument(
    value_parameter: ParameterId,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let Some(arguments) = raw_argument_pair_in_parameter_order(arguments) else {
        return Err(argument_error(
            None,
            "a raw reference value UPDATE requires exactly two arguments",
        ));
    };
    for argument in arguments {
        if argument.parameter() == value_parameter
            && matches!(argument.value(), RuntimeValue::Text(value) if value.contains('\0'))
        {
            return Err(argument_error(
                Some(argument.parameter()),
                "raw Text UPDATE arguments cannot contain U+0000",
            ));
        }
    }
    Ok(())
}

fn validate_raw_reference_delete_shape(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "a raw reference DELETE must declare only its selector parameter",
        ));
    };
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "raw reference mutations accept exactly one reference argument",
        ));
    };
    let RuntimeValue::Reference { target, .. } = argument.value() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw reference mutations accept exactly one reference argument",
        ));
    };
    let artifact = active_function_artifact(active, function)?;
    let plan = ServerDeletePlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let selector = plan.selector();
    if selector.owner() != function.id()
        || selector.parameter() != parameter.id()
        || argument.parameter() != parameter.id()
        || parameter.resolved_type() != ResolvedType::reference(plan.target())
        || *target != plan.target()
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "raw reference DELETE selector must match its sole active parameter and target",
        ));
    }
    Ok(())
}

fn active_function_artifact<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
) -> Result<&'a orna_core::revision::ExecutableArtifact, PostgresKernelError> {
    active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .map(|revision| revision.artifact())
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })
}

fn result_rows_values(rows: &ResultRows) -> Vec<RuntimeValue> {
    rows.rows()
        .iter()
        .flat_map(|row| row.values().iter().cloned())
        .collect()
}

/// Reports whether an UPDATE failure is a closed raw target rejection.
pub(crate) const fn raw_server_update_target_is_unavailable(error: &ServerUpdateError) -> bool {
    match error {
        ServerUpdateError::FunctionNotActive { .. } => true,
        ServerUpdateError::NotCommitted { source, .. } => {
            raw_reference_mutation_failure_is_unavailable(source)
        }
        ServerUpdateError::Unavailable { .. }
        | ServerUpdateError::CommitRejected { .. }
        | ServerUpdateError::CommitOutcomeUnknown { .. }
        | ServerUpdateError::CommittedButShutdownFailed { .. } => false,
    }
}

/// Reports whether a DELETE failure is a closed raw target rejection.
pub(crate) const fn raw_server_delete_target_is_unavailable(error: &ServerDeleteError) -> bool {
    match error {
        ServerDeleteError::FunctionNotActive { .. } => true,
        ServerDeleteError::NotCommitted { source, .. } => {
            raw_reference_mutation_failure_is_unavailable(source)
        }
        ServerDeleteError::Unavailable { .. }
        | ServerDeleteError::DeleteRestricted { .. }
        | ServerDeleteError::CommitRejected { .. }
        | ServerDeleteError::CommitOutcomeUnknown { .. }
        | ServerDeleteError::CommittedButShutdownFailed { .. } => false,
    }
}

pub(super) const fn raw_reference_mutation_failure_is_unavailable(
    error: &ServerMutationError,
) -> bool {
    match error {
        ServerMutationError::FunctionNotActive { .. }
        | ServerMutationError::FunctionSignature { .. }
        | ServerMutationError::Artifact { .. }
        | ServerMutationError::PlanDecode(_)
        | ServerMutationError::PlanInvariant { .. }
        | ServerMutationError::ReferenceEvidence { .. }
        | ServerMutationError::Argument { .. }
        | ServerMutationError::ComplexityLimit { .. } => true,
        ServerMutationError::NotCommitted { source, .. } => {
            raw_reference_mutation_failure_is_unavailable(source)
        }
        ServerMutationError::Kernel { .. }
        | ServerMutationError::Database { .. }
        | ServerMutationError::CurrentRevision { .. }
        | ServerMutationError::PreparedResult { .. }
        | ServerMutationError::RowDecode { .. }
        | ServerMutationError::ValueInvariant { .. }
        | ServerMutationError::ResultRows(_)
        | ServerMutationError::RecordValue(_)
        | ServerMutationError::ValueCodec(_)
        | ServerMutationError::UniqueReferenceConflict { .. }
        | ServerMutationError::UniqueTextConflict { .. }
        | ServerMutationError::CommitRejected { .. }
        | ServerMutationError::CommitOutcomeUnknown { .. }
        | ServerMutationError::CommittedButShutdownFailed { .. } => false,
    }
}
