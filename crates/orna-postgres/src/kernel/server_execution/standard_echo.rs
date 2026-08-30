//! Closed standard parameter-echo artifact execution and validation.

use super::*;

/// Executes one closed standard `orna.server-parameter-echo` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`] and its already bound [`FunctionArgument`]. It
/// dispatches purely by checked artifact kind, format, and version, then
/// validates the artifact against the pinned standard function signature:
/// decode pins the function's parameter identity and the resolved INTEGER
/// value type, and the signature validator requires the fixed ADR 0055 echo
/// shape. It never matches a function by Rust name or [`FunctionId`], executes
/// SQL, or opens a PostgreSQL row. The result is the already bound typed
/// integer.
///
/// The sealed `sys.invoke` execution step (ADR 0055 implementation order item
/// 11) is the sole caller (`dispatch_sealed_sys_invoke`).
pub(crate) fn execute_standard_parameter_echo(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    arguments: &[FunctionArgument],
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_parameter_echo::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-parameter-echo",
        ));
    }
    if artifact.version() != server_parameter_echo::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-parameter-echo version 1",
        ));
    }
    if revision.language_version() != server_parameter_echo::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the parameter-echo language version",
        ));
    }
    let parameter = validate_standard_parameter_echo_signature(function)?;
    ServerParameterEcho::decode(artifact.payload(), parameter, INTEGER_TYPE_ID)
        .map_err(ServerSelectError::ParameterEchoDecode)
        .map_err(server_error)?;
    validate_standard_parameter_echo_argument(parameter, arguments)
}

/// Validates one pinned function against the fixed ADR 0055 echo signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `INTEGER` parameter with no default expression, one single `INTEGER`
/// result, `SECURITY INVOKER`, `TRANSACTION READ ONLY`, and `VOLATILITY
/// STABLE`. Both the parameter and the result must resolve to the durable
/// INTEGER value type. Returns the pinned parameter identity the artifact must
/// carry.
fn validate_standard_parameter_echo_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        ));
    }
    let FunctionReturn::Single(result_type) = function.return_type() else {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must return a single INTEGER value",
        ));
    };
    if !is_standard_integer_type(&parameter.resolved_type())
        || !is_standard_integer_type(result_type)
    {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use STABLE volatility",
        ));
    }
    Ok(parameter.id())
}

/// Returns whether one resolved type is the standard INTEGER of the pinned
/// V2 context.
///
/// The retained standard catalogue declares the echo parameter and result as
/// the primitive `Scalar(Integer)` form, while the pinned echo artifact
/// carries the durable `Value(INTEGER_TYPE_ID)` identity. Both denote the
/// same standard INTEGER (`orna.std/2` value type `...02`), so the closed
/// signature validator admits exactly these two forms and nothing else.
fn is_standard_integer_type(resolved_type: &ResolvedType) -> bool {
    *resolved_type == ResolvedType::value(INTEGER_TYPE_ID)
        || *resolved_type == ResolvedType::scalar(StandardScalar::Integer)
}

/// Validates the exact bound argument of one standard parameter-echo call.
///
/// The engine accepts exactly one argument bound to the pinned parameter that
/// carries one non-null `RuntimeValue::Integer`, and returns that typed
/// integer. A typed null cannot cross the [`FunctionArgument`] boundary; the
/// explicit null arm keeps the closed-engine invariant independent of that
/// boundary.
fn validate_standard_parameter_echo_argument(
    parameter: ParameterId,
    arguments: &[FunctionArgument],
) -> Result<RuntimeValue, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "standard parameter echo calls require exactly one argument",
        ));
    };
    if argument.parameter() != parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "standard parameter echo arguments must bind the pinned parameter identity",
        ));
    }
    match argument.value() {
        RuntimeValue::Integer(value) => Ok(RuntimeValue::Integer(*value)),
        RuntimeValue::Null(_) => Err(argument_error(
            Some(parameter),
            "standard parameter echo arguments cannot be NULL",
        )),
        _ => Err(argument_error(
            Some(parameter),
            "standard parameter echo arguments must be one non-null INTEGER value",
        )),
    }
}
