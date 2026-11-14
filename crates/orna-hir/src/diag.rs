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

use orna_syntax_ast::SourceSpan;
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
/// A `sys` member the host has not linked a production provider for (ORNA-SYS-008).
pub const SYS_VERSION_INCOMPATIBLE: &str = "sys.version.incompatible";
/// A unit name that names no unit of a family or unit declaration in scope (units.md ORNA-UNIT-UNRESOLVED).
pub const UNIT_UNRESOLVED: &str = "ORNA-UNIT-UNRESOLVED";
/// A unit family whose variants do not share the family's dimension (units.md ORNA-UNIT-010).
pub const UNIT_DIM: &str = "ORNA-UNIT-DIM";
/// A bare number, or a quantity outside its family, given a unit-family type (units.md ORNA-UNIT-020).
pub const UNIT_ANNOT: &str = "ORNA-UNIT-020";
/// A unit whose definition depends on itself (units.md ORNA-UNIT-009).
pub const UNIT_CYCLE: &str = "ORNA-UNIT-CYCLE";
/// A unit key whose exponent or rational leaves the evaluator's budget (units.md ORNA-UNIT-019).
pub const UNIT_LIMIT: &str = "ORNA-UNIT-LIMIT";
/// An exact conversion or fold that has no finite decimal form (units.md ORNA-UNIT-016).
pub const UNIT_INEXACT: &str = "ORNA-UNIT-INEXACT";
/// A power whose exponent is not a literal `Int` (units.md ORNA-UNIT-013).
pub const UNIT_POW: &str = "ORNA-UNIT-POW";
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

/// Two `Money` values of different currencies meet in one `+`, `-` or ordering operator (hir-host.md B5).
///
/// Published by the kernel (kernel-and-library.md 4.7); the library supplies only the text.
pub const MONEY_CURRENCY_MISMATCH: &str = "E5003";

/// A `Money` value is built from a `Float` (hir-host.md B5; the old checker's `E5004`).
pub const MONEY_FROM_FLOAT: &str = "E5004";

/// A `Money` value and a `Float` meet in one arithmetic or comparison operator (hir-host.md B5; `E5103`).
pub const MONEY_BINARY_FLOAT: &str = "E5103";

/// Two quantities of different dimensions meet in one `+` or `-` (hir-host.md B5; the operator-level `E5001`).
pub const QUANTITY_DIMENSIONS: &str = "E5001";

/// Two affine points (`core.temperature.Temperature`) meet in one `+` or `-` (hir-host.md B5; `E5002`).
pub const AFFINE_POINT_OPERATOR: &str = "E5002";

/// A fold (`sum`) over affine points (hir-host.md B5; `E5003`, the absolute-quantity sum class).
pub const AFFINE_POINT_SUM: &str = "E5003";

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

/// A `case` expression does not cover its scrutinee type: an enum variant is
/// duplicated or missing, or a boolean value is duplicated or uncovered.
///
/// The code is the checker's type-error class (ORNA-S021-TYPE), so the parity
/// report compares the same mistake under the same code.
pub const CASE_COVERAGE: &str = "ORNA-S021-TYPE";

/// Two protocol implementations of one type supply the same member call.
///
/// Overlap is a static error whatever the source order (ORNA-GENERIC-014), so
/// the call cannot pick one of them.
pub const GENERIC_OVERLAP: &str = "ORNA-GENERIC-014";

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
