//! One index of every name occurrence and what it resolved to (design H1).
//!
//! Each module records one [`NameUse`] per name it writes: an identifier, a
//! path segment that names a definition, a pattern binding's constructor, or a
//! callee. The index is the single answer the language server's definition,
//! references, rename and hover queries read, and the answer the H1
//! resolution comparison checks against the old checker.
//!
//! The index is per module and in source order, so a consumer can binary-search
//! by span. A use that resolved to nothing is still recorded, with
//! [`NameTarget::Unresolved`], so a query never has to distinguish "absent from
//! the index" from "did not resolve".

use orna_syntax::SyntaxSpan as SourceSpan;

use crate::{DefId, LocalId, ModuleId, Name};

/// What one written name denotes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameTarget {
    /// A local binding slot of the enclosing body.
    Local(LocalId),
    /// One definition of this program.
    Def(DefId),
    /// One imported module, named by its root or alias.
    Module(ModuleId),
    /// A language-defined call form with no declaration (`fail`, `Some`, a
    /// relation stage). The stage text calls these `Intrinsic`; they move to
    /// [`NameTarget::Def`] when `core.orna` declares the native forms.
    Intrinsic,
    /// Two wildcard imports provided the name and no declaration or explicit
    /// import settled it (ORNA-S011).
    Ambiguous,
    /// Nothing provides the name (ORNA-S012).
    Unresolved,
}

/// Where a name was written.
///
/// The position decides which resolution rules apply and, for the H1
/// comparison, which occurrences the old checker can answer for: its
/// unresolved-name diagnostic names a value use, not a type or pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseContext {
    /// A value expression or a callee.
    Value,
    /// A type annotation, argument, alias or constructor path.
    Type,
    /// A binding pattern or a pattern constructor.
    Pattern,
}

/// One name occurrence and its resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameUse {
    /// The identifier's own span, without the path's other segments.
    pub span: SourceSpan,
    /// The normalized identifier as written.
    pub name: Name,
    /// Where it was written.
    pub context: UseContext,
    /// What it denotes.
    pub target: NameTarget,
}
