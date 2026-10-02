//! Evaluator bindings for native operations in the generated sys host registry.

use num_traits::ToPrimitive;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SafeText};
use orna_syntax_v1::Expr;
use orna_sys_v1::{
    ClockProvider, EnvironmentDispatchValue, EnvironmentProvider, EnvironmentProviderError,
    HostOperationDescriptor, ProcessProvider, system_host_operation_registry,
};

use crate::{CancellationToken, EffectHandler, EvaluationError, StepBudget};

/// Evaluator-facing registration of built-in sys host providers.
///
/// Operations are selected from the build-generated sys registry. Environment
/// values are limited to an explicit name snapshot, processes require exact
/// executable/root grants, and clock waits require a duration cap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SysHostBindingRegistry {
    environment: EnvironmentProvider,
    process: Option<ProcessProvider>,
    clock: Option<ClockProvider>,
}

impl SysHostBindingRegistry {
    /// Links the baked environment role to its native provider and a caller
    /// supplied, already allowlisted environment snapshot.
    #[must_use]
    pub fn new(environment: EnvironmentProvider) -> Self {
        Self {
            environment,
            process: None,
            clock: None,
        }
    }

    /// Adds an explicitly allowlisted native process provider.
    #[must_use]
    pub fn with_process_provider(mut self, provider: ProcessProvider) -> Self {
        self.process = Some(provider);
        self
    }

    /// Adds an explicitly bounded native clock provider for waits.
    #[must_use]
    pub fn with_clock_provider(mut self, provider: ClockProvider) -> Self {
        self.clock = Some(provider);
        self
    }

    /// Captures only names explicitly approved by the host.
    pub fn capture_environment(
        names: impl IntoIterator<Item = String>,
    ) -> Result<Self, EnvironmentProviderError> {
        EnvironmentProvider::capture_allowlisted(names).map(Self::new)
    }

    fn dispatch(
        &self,
        operation_name: &str,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let registry = system_host_operation_registry();
        let Some(operation) = registry.operation(operation_name) else {
            return Ok(None);
        };
        if operation
            .effects
            .iter()
            .any(|effect| !matches!(effect.as_str(), "read" | "invoke"))
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let role = registry
            .role(&operation.role)
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        if role.provider != operation.provider
            || !role
                .operations
                .iter()
                .any(|candidate| candidate == operation_name)
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        match role.provider.as_str() {
            "orna.sys.host.environment.v1" => self.dispatch_environment(operation, arguments),
            "orna.sys.host.process.v1" => self.dispatch_process(operation, arguments),
            "orna.sys.host.clock.v1" => self.dispatch_clock(operation, arguments, cancellation),
            _ => Err(redacted_error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn dispatch_environment(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if arguments.len() != 1 {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        }
        let OvbRaw::Text(name) = arguments[0].raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let value = self
            .environment
            .dispatch(operation, name)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        let raw = match value {
            EnvironmentDispatchValue::Optional(value) => {
                let mut fields = vec![OvbRaw::Int(if value.is_some() {
                    1.into()
                } else {
                    0.into()
                })];
                if let Some(value) = value {
                    fields.push(OvbRaw::Text(value));
                }
                OvbRaw::Tag(60013, Box::new(OvbRaw::Array(fields)))
            }
            EnvironmentDispatchValue::Required(value) => OvbRaw::Text(value),
        };
        CanonicalValue::new(raw)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_process(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation.name != "std.io.process.run"
            || operation.implementation != "run"
            || operation.role != "host.std.io.process@1.0"
            || operation.provider != "orna.sys.host.process.v1"
            || operation.effects != ["invoke"]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .process
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let [
            executable,
            args,
            working_directory,
            environment,
            input,
            timeout,
            output_limit,
        ] = arguments
        else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let executable =
            raw_text(executable.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let working_directory =
            raw_text(working_directory.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let args = raw_text_array(args.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let environment =
            raw_text_pairs(environment.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let input = optional_raw(input.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let input = match input {
            None | Some(OvbRaw::Null) => None,
            Some(OvbRaw::Bytes(bytes)) => Some(bytes.as_slice()),
            Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
        };
        let timeout =
            optional_raw(timeout.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let timeout = match timeout {
            None | Some(OvbRaw::Null) => None,
            Some(OvbRaw::Tag(60005, value)) => {
                Some(raw_duration(value).ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?)
            }
            Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
        };
        let OvbRaw::Int(output_limit) = output_limit.raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let output_limit = output_limit
            .to_usize()
            .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
        let output = provider
            .run(
                executable,
                &args,
                working_directory,
                &environment,
                input,
                timeout,
                output_limit,
            )
            .map_err(|failure| self.failure(operation, failure.code()))?;
        let exit_status = match output.exit_status {
            Some(status) => OvbRaw::Tag(
                60013,
                Box::new(OvbRaw::Array(vec![
                    OvbRaw::Int(1.into()),
                    OvbRaw::Int(status.into()),
                ])),
            ),
            None => OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![OvbRaw::Int(0.into())]))),
        };
        CanonicalValue::new(OvbRaw::Array(vec![
            exit_status,
            OvbRaw::Bytes(output.stdout),
            OvbRaw::Bytes(output.stderr),
        ]))
        .map(Some)
        .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_clock(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation.name != "std.concurrent.sleep"
            || operation.implementation != "sleep"
            || operation.role != "host.std.concurrent.clock@1.0"
            || operation.provider != "orna.sys.host.clock.v1"
            || operation.effects != ["invoke"]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .clock
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let [duration] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let OvbRaw::Tag(60005, raw) = duration.raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let duration = raw_duration(raw).ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
        provider
            .sleep_with_cancellation(duration, || {
                cancellation.is_some_and(CancellationToken::is_requested)
            })
            .map_err(|failure| self.failure(operation, failure.code()))?;
        CanonicalValue::new(OvbRaw::Null)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn failure(&self, operation: &HostOperationDescriptor, code: &str) -> EvaluationError {
        let declares_failure = operation.failures.iter().any(|declared| declared == code);
        if declares_failure && code == "sys.host.clock.cancelled" {
            return redacted_error("ORNA-EVAL-CANCELLED");
        }
        if declares_failure {
            redacted_error("ORNA-EVAL-ERROR")
        } else {
            redacted_error("ORNA-EVAL-UNSUPPORTED")
        }
    }
}

fn raw_text(value: &OvbRaw) -> Option<&str> {
    match value {
        OvbRaw::Text(value) => Some(value),
        _ => None,
    }
}

fn raw_text_array(value: &OvbRaw) -> Option<Vec<String>> {
    let OvbRaw::Array(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| raw_text(value).map(str::to_owned))
        .collect()
}

fn raw_text_pairs(value: &OvbRaw) -> Option<Vec<(String, String)>> {
    let OvbRaw::Array(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| {
            let OvbRaw::Array(pair) = value else {
                return None;
            };
            let [name, value] = pair.as_slice() else {
                return None;
            };
            Some((raw_text(name)?.to_owned(), raw_text(value)?.to_owned()))
        })
        .collect()
}

fn optional_raw(value: &OvbRaw) -> Option<Option<&OvbRaw>> {
    match value {
        OvbRaw::Null => Some(None),
        OvbRaw::Tag(60013, raw) => {
            let OvbRaw::Array(parts) = raw.as_ref() else {
                return None;
            };
            match parts.as_slice() {
                [OvbRaw::Int(tag)] if tag == &0.into() => Some(None),
                [OvbRaw::Int(tag), value] if tag == &1.into() => Some(Some(value)),
                _ => None,
            }
        }
        _ => Some(Some(value)),
    }
}

fn raw_duration(raw: &OvbRaw) -> Option<std::time::Duration> {
    let OvbRaw::Array(parts) = raw else {
        return None;
    };
    let [OvbRaw::Int(seconds), OvbRaw::Int(nanoseconds)] = parts.as_slice() else {
        return None;
    };
    let nanoseconds = nanoseconds
        .to_u32()
        .filter(|nanoseconds| *nanoseconds < 1_000_000_000)?;
    Some(std::time::Duration::new(seconds.to_u64()?, nanoseconds))
}

impl EffectHandler for SysHostBindingRegistry {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        _callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.dispatch(operation, arguments, None)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        _callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.dispatch(operation, arguments, cancellation)
    }
}

fn redacted_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static evaluator error code is safe"))
}
