//! Application-facing browser profile adapter over the shared evaluator.

use orna_foundation::CanonicalValue;

use crate::ApplicationError;

/// Evaluate an admitted browser source entry with the shared source/evaluator profile.
pub fn execute_browser_profile(
    source: &str,
    entry: &str,
) -> Result<CanonicalValue, ApplicationError> {
    orna_evaluator::execute_browser_profile(source, entry)
        .map_err(|error| ApplicationError::Evaluation(error.to_string()))
}
