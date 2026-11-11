use std::fmt;

/// A REPL rejection: a stable code and a message that quotes no source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplError {
    code: &'static str,
    message: String,
}

impl ReplError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The stable code, for example `ORNA-EVAL-NAME`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The explanation, with no source text quoted.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ReplError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReplError {}
