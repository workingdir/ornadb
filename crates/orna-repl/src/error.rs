use orna_hir::diag::UNRESOLVED;
use orna_syntax::ParseFailure;
use std::fmt;
use std::ops::Range;

/// A REPL rejection: a stable code and a message that quotes no source text, plus, when the
/// offending text is located, where it is and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplError {
    code: &'static str,
    message: String,
    detail: Option<Box<ReplDetail>>,
}

/// Explanation of a rejected REPL input, located in its source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplDetail {
    /// Parser or semantic code, for example `ORNA-PARSE-001`.
    pub code: String,
    /// What the user did wrong, naming the offending identifier when known.
    pub message: String,
    /// Byte range of the offending text in the submitted source, when located.
    pub span: Option<Range<usize>>,
    /// Supporting facts about the failure.
    pub notes: Vec<String>,
    /// Concrete remedies the user can apply.
    pub help: Vec<String>,
}

impl ReplError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub(crate) fn with_detail(mut self, detail: ReplDetail) -> Self {
        self.detail = Some(Box::new(detail));
        self
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

    /// The located explanation, when the failure has one.
    #[must_use]
    pub fn detail(&self) -> Option<&ReplDetail> {
        self.detail.as_deref()
    }
}

impl fmt::Display for ReplError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReplError {}

impl From<ParseFailure> for ReplDetail {
    fn from(failure: ParseFailure) -> Self {
        Self {
            code: failure.code.to_owned(),
            message: failure.message,
            span: Some(failure.span),
            notes: failure.notes,
            help: failure.help,
        }
    }
}

/// The rejection of a name the session does not declare. Its span is located by the caller, which knows the
/// input. `modules` are the standard modules that export the name. Each gets a `use` remedy, then the
/// `declare` remedy follows, as the old REPL path orders them; the HIR path never has namespace hints.
pub(crate) fn unresolved(name: &str, modules: &[String]) -> ReplError {
    let message = format!("name `{name}` cannot be resolved");
    let mut help: Vec<String> = modules
        .iter()
        .map(|module| format!("add `use {module}.{name}` to bring `{name}` into scope"))
        .collect();
    help.push(format!(
        "declare `{name}` before using it, for example `let {name} = <value>`"
    ));
    ReplError::new(UNRESOLVED, message.clone()).with_detail(ReplDetail {
        code: UNRESOLVED.to_owned(),
        message,
        span: None,
        notes: vec![format!(
            "`{name}` is not declared in this session and is not imported"
        )],
        help,
    })
}
