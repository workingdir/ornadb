//! Diagnostics produced while lowering one module.
//!
//! Lowering is total, so every construct it cannot represent is recorded here
//! instead of aborting. A [`HirDiagnostic`] carries the classification, the
//! message and the source span; it is deliberately not an
//! [`orna_value::Diagnostic`], because that type pins a repository snapshot and
//! lowering has none. The admission boundary, which owns the snapshot and the
//! source path, converts a diagnostic with [`HirDiagnostic::to_value`].
//!
//! The codes are the checker's resolution vocabulary
//! (`crates/orna-check/src/diagnostics.rs`), because the HIR resolver replaces
//! that resolver: a consumer must see the same classification for the same
//! mistake.

use orna_syntax::SyntaxSpan as SourceSpan;
use orna_value::{Diagnostic, DiagnosticSeverity, DiagnosticSpan, SafeText, Snapshot};

/// A name that no declaration, import, local or language form provides.
pub const UNRESOLVED: &str = "ORNA-S012-UNRESOLVED";
/// A name that two imports provide and no local declaration settles.
pub const AMBIGUOUS: &str = "ORNA-S011-AMBIGUOUS";
/// An import that names no module, no public definition, or an invalid tail.
pub const IMPORT: &str = "ORNA-S010-IMPORT";
/// Two declarations of one name in one scope.
pub const DUPLICATE: &str = "ORNA-S013-DUPLICATE";
/// A construct the HIR does not represent yet.
pub const UNSUPPORTED: &str = "ORNA-S022-UNSUPPORTED";
/// A `native fn` outside the engine-embedded `core` and `sys` modules (D12).
pub const NATIVE_ADMISSION: &str = "ORNA-NATIVE-004";
/// A call whose arguments do not bind to its parameters.
///
/// The classifier keeps the evaluator's code so a consumer sees one
/// classification for the same mistake; the binder moves the check from run
/// time to lowering (design section 2.7), and stage H3 decides whether the
/// vocabulary changes.
pub const ARGUMENT: &str = "ORNA-EVAL-ARGUMENT";

/// Two constraints on one expression disagree.
///
/// The classification is the specification requirement that governs the
/// report: "Inference errors MUST show the conflicting constraints and their
/// source spans" (ORNA-INFER-011).
pub const INFER_CONFLICT: &str = "ORNA-INFER-011";

/// An inference variable is still unsolved after its body was checked.
///
/// The classification is the requirement that asks inference to name the
/// smallest useful annotation site instead of demanding annotations everywhere
/// (ORNA-INFER-007).
pub const INFER_AMBIGUOUS: &str = "ORNA-INFER-007";

/// A public function parameter omits its type annotation.
///
/// Decision D1 requires every `pub` parameter to be annotated; only private
/// parameters may omit the annotation and have it solved from the body and
/// defaults. The classification matches the old checker's annotation
/// diagnostic so the two agree on the same mistake.
pub const PUBLIC_ANNOTATION: &str = "ORNA-S020-ANNOTATION";

/// A source module whose first segment is the reserved `sys` or `std` root (S003).
pub const RESERVED: &str = "ORNA-S003-RESERVED";
/// Two source namespaces that Unicode folding makes siblings, or a repeated
/// module namespace (S002).
pub const NAMESPACE: &str = "ORNA-S002-NAMESPACE";
/// A table named `sys`, which is reserved (E1008).
pub const RESERVED_SYS: &str = "E1008";
/// A table named `std`, which is reserved (E1009).
pub const RESERVED_STD: &str = "E1009";
/// A module assertion that depends on no table (A091-012).
pub const ASSERTION_SCOPE: &str = "ORNA-A091-012";
/// A module assertion that depends on exactly one table (A091-003).
pub const ASSERTION_ONE_TABLE: &str = "ORNA-A091-003";
/// An assertion that performs an effect or can fail (A091-007).
pub const ASSERTION_EFFECT: &str = "ORNA-A091-007";
/// A table assertion that pipes its own table into the predicate (A091-002).
pub const ASSERTION_OWNER_PIPE: &str = "ORNA-A091-002";

/// One diagnostic raised while lowering a module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirDiagnostic {
    /// The stable primary classification.
    pub code: &'static str,
    /// The explanation, with no source text quoted.
    pub message: Box<str>,
    /// Where it applies.
    pub span: SourceSpan,
}

impl HirDiagnostic {
    /// Builds one diagnostic.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<Box<str>>, span: SourceSpan) -> Self {
        Self {
            code,
            message: message.into(),
            span,
        }
    }

    /// Converts to the portable diagnostic form once a snapshot and the
    /// module's path are known.
    ///
    /// Returns `None` when the text cannot be represented in the portable
    /// record, which the admission boundary reports as an internal failure
    /// rather than silently dropping.
    #[must_use]
    pub fn to_value(&self, snapshot: &Snapshot, file: &str) -> Option<Diagnostic> {
        let start = u64::try_from(self.span.start).ok()?;
        let end = u64::try_from(self.span.end).ok()?;
        Some(
            Diagnostic::new(
                SafeText::new(self.code).ok()?,
                DiagnosticSeverity::Error,
                SafeText::new(self.message.to_string()).ok()?,
            )
            .ok()?
            .with_span(DiagnosticSpan::new(snapshot.clone(), file, start.into(), end.into()).ok()?),
        )
    }
}
