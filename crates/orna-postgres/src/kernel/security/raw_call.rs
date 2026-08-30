use super::*;

/// The owned value result of one authenticated raw call.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedRawCallResult {
    /// One value evaluated by the CLIENT runtime.
    Client(RuntimeValue),
    /// Zero or more values returned in SERVER execution result order.
    Server(Vec<RuntimeValue>),
}
impl AuthenticatedRawCallResult {
    /// Transfers result values without cloning their payloads.
    pub fn into_values(self) -> Vec<RuntimeValue> {
        match self {
            Self::Client(value) => vec![value],
            Self::Server(values) => values,
        }
    }
}
/// The closed result of raw record-argument preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordArgumentPreflight {
    /// The call contains no record argument and needs no PostgreSQL preflight.
    NotRequired,
    /// Every record is canonical for the transaction's active revision.
    Current,
    /// At least one record is stale or incompatible with the active revision.
    Stale,
}
pub(super) fn classify_raw_server_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Select(source),
            }
        }
        error => error,
    }
}

pub(super) fn classify_raw_identity_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw identity-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

pub(super) fn classify_raw_unique_text_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw unique-Text-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

pub(super) fn validate_raw_call_argument_shape(
    function: FunctionId,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    match arguments {
        [] => Ok(()),
        [argument] if raw_call_argument_is_supported(argument) => Ok(()),
        [first, second]
            if raw_call_argument_is_supported(first) && raw_call_argument_is_supported(second) =>
        {
            Ok(())
        }
        _ => Err(raw_call_target_unavailable(
            function,
            "raw calls accept zero arguments, one supported value, or one supported argument pair",
        )),
    }
}

fn raw_call_argument_is_supported(argument: &FunctionArgument) -> bool {
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

pub(super) fn raw_call_target_unavailable(
    function: FunctionId,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::RawCallTargetUnavailable { function, rule }
}

pub(super) fn classify_raw_server_insert_error(
    error: PostgresKernelError,
    arguments_present: bool,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_argument_target_is_unavailable(&source, arguments_present) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER INSERT argument target is unavailable",
            )
        }
        PostgresKernelError::ServerInsert(source) if arguments_present => {
            PostgresKernelError::ServerInsert(source)
        }
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_target_is_unavailable(&source) =>
        {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Insert(source),
            }
        }
        error => error,
    }
}

pub(super) fn classify_raw_server_reference_mutation_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerUpdate(source)
            if raw_server_update_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER UPDATE reference target is unavailable",
            )
        }
        PostgresKernelError::ServerDelete(source)
            if raw_server_delete_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER DELETE reference target is unavailable",
            )
        }
        error => error,
    }
}

fn raw_server_insert_argument_target_is_unavailable(
    error: &ServerInsertError,
    arguments_present: bool,
) -> bool {
    match error {
        ServerInsertError::NotCommitted { source, .. } => {
            raw_server_insert_argument_target_is_unavailable(source, arguments_present)
        }
        ServerInsertError::Argument { .. } => true,
        ServerInsertError::FunctionNotActive { .. }
        | ServerInsertError::FunctionSignature { .. }
        | ServerInsertError::Artifact { .. }
        | ServerInsertError::PlanDecode(_)
        | ServerInsertError::PlanInvariant { .. }
        | ServerInsertError::ReferenceEvidence { .. }
        | ServerInsertError::ComplexityLimit { .. } => arguments_present,
        _ => false,
    }
}
