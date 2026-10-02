//! Evaluator bindings for native operations in the generated sys host registry.

use orna_foundation_v1::{CanonicalValue, OvbRaw, SafeText};
use orna_syntax_v1::Expr;
use orna_sys_v1::{
    EnvironmentDispatchValue, EnvironmentProvider, EnvironmentProviderError,
    system_host_operation_registry,
};

use crate::{EffectHandler, EvaluationError, StepBudget};

/// Evaluator-facing registration of built-in sys host providers.
///
/// Operations are selected from the build-generated sys registry. The
/// environment provider receives only an explicit host allowlist/snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SysHostBindingRegistry {
    environment: EnvironmentProvider,
}

impl SysHostBindingRegistry {
    /// Links the baked environment role to its native provider and a caller
    /// supplied, already allowlisted environment snapshot.
    #[must_use]
    pub fn new(environment: EnvironmentProvider) -> Self {
        Self { environment }
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
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let registry = system_host_operation_registry();
        let Some(operation) = registry.operation(operation_name) else {
            return Ok(None);
        };
        if operation.effects.iter().any(|effect| effect != "read") {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let role = registry
            .role(&operation.role)
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        if role.provider != operation.provider || role.provider != "orna.sys.host.environment.v1" {
            return Ok(None);
        }
        if arguments.len() != 1 {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        }
        let OvbRaw::Text(name) = arguments[0].raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let value = self
            .environment
            .dispatch(operation, name)
            .map_err(|failure| self.failure(operation_name, failure))?;
        let raw = match value {
            EnvironmentDispatchValue::Optional(Some(value))
            | EnvironmentDispatchValue::Required(value) => OvbRaw::Text(value),
            EnvironmentDispatchValue::Optional(None) => OvbRaw::Null,
        };
        CanonicalValue::new(raw)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn failure(&self, operation_name: &str, failure: EnvironmentProviderError) -> EvaluationError {
        let declares_failure = system_host_operation_registry()
            .operation(operation_name)
            .is_some_and(|operation| operation.failures.iter().any(|code| code == failure.code()));
        if declares_failure {
            redacted_error("ORNA-EVAL-ERROR")
        } else {
            redacted_error("ORNA-EVAL-UNSUPPORTED")
        }
    }
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
        self.dispatch(operation, arguments)
    }
}

fn redacted_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static evaluator error code is safe"))
}
