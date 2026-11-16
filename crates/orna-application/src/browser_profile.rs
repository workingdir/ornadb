//! Application-facing browser profile adapter over the shared evaluator.

use orna_value::{CanonicalValue, Diagnostic, DiagnosticSeverity, SafeText};

use crate::ApplicationError;

/// Evaluate an admitted browser source entry with the shared source/evaluator profile.
pub fn execute_browser_profile(
    source: &str,
    entry: &str,
) -> Result<CanonicalValue, ApplicationError> {
    orna_evaluator::execute_browser_profile(source, entry).map_err(|error| {
        let code =
            SafeText::new(error.stable_code().to_owned()).expect("browser profile codes are safe");
        ApplicationError::Evaluation(Box::new(
            Diagnostic::new(code, DiagnosticSeverity::Error, SafeText::redacted())
                .expect("safe diagnostic code")
                .redacted(),
        ))
    })
}
