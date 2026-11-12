//! Unit-expression and interface-clause node types.

use crate::ast::NameSegment;
use crate::span::SourceSpan;

/// The unit expression of a unit variant: `unit_term ( ('*' | '/') unit_term )*`, evaluated left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitExpr {
    pub terms: Vec<UnitTerm>,
    pub span: SourceSpan,
}

/// One factor of a unit expression. `divide` is the operator written before the term; the first term
/// of an expression always has `divide == false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitTerm {
    pub divide: bool,
    /// The `RATIONAL` coefficient exactly as written, such as `1000` or `0.125`.
    pub coefficient: Option<String>,
    /// The family of a family-qualified name: `Length` in `Length.kilometre`.
    pub family: Option<String>,
    pub name: String,
    /// The exponent after `^`, exactly as written (`unit_term := RATIONAL? unit_name ('^' INT)?`).
    pub exponent: Option<String>,
    pub span: SourceSpan,
}

/// A string literal value of a clause, with its quotes removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseString {
    pub text: String,
    pub span: SourceSpan,
}

/// `removed "version" [use "path"] [reason "text"]`: the declaration is gone from `version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedClause {
    pub version: ClauseString,
    pub replacement: Option<ClauseString>,
    pub reason: Option<ClauseString>,
    pub span: SourceSpan,
}

/// `formerly "name" [diagnostic "code"]`: the name that this declaration replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormerlyClause {
    pub name: ClauseString,
    pub diagnostic: Option<ClauseString>,
    pub span: SourceSpan,
}

/// `deprecated "version" [use "path"]`: the declaration still works, and tools warn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecatedClause {
    pub version: ClauseString,
    pub replacement: Option<ClauseString>,
    pub span: SourceSpan,
}

/// The clauses that one declaration carries. Every field is empty or `None` when the clause is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterfaceClauses {
    /// `class read|invoke|admin`: the catalogue class of a native function.
    pub class: Option<NameSegment>,
    /// `fails a, b`: the failure codes a native function may raise, as enum names.
    pub fails: Vec<NameSegment>,
    /// `since "version"`: the first language version that has a native function.
    pub since: Option<ClauseString>,
    pub removed: Option<RemovedClause>,
    pub formerly: Option<FormerlyClause>,
    pub deprecated: Option<DeprecatedClause>,
    /// `spelling "sys.name(Type)"`: the 1.1 label of an overload member.
    pub spelling: Option<ClauseString>,
    /// `key a, b`: the key columns of a relation row type.
    pub key: Vec<NameSegment>,
    /// `availability "word"`: one of the five availability words of a relation row type.
    pub availability: Option<ClauseString>,
}
