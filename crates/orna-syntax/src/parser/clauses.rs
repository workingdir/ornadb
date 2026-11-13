//! Interface clauses of engine-embedded declarations (`since`, `removed`, `class`, ...).
//!
//! Every clause word is contextual: none is a keyword, so `since` and `key` stay usable as names. A clause is
//! legal only in an engine-embedded interface module and only on the declaration forms that the clause table
//! names. The clauses sit between a declaration's head and its `;` or `{` (sys-as-native section 2.4).

use super::{Parser, TokenKind};
use orna_syntax_ast::*;

/// The declaration form that a clause is written on. A clause word outside its form's table is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClauseTarget {
    NativeFn,
    NativeStatic,
    NativeType,
    Type,
    Enum,
}

/// Every clause word, so that the parser can tell a clause from the next declaration.
const CLAUSE_WORDS: [&str; 9] = [
    "class",
    "fails",
    "since",
    "removed",
    "formerly",
    "deprecated",
    "spelling",
    "key",
    "availability",
];

/// The catalogue classes that a `class` clause names.
const CATALOGUE_CLASSES: [&str; 3] = ["read", "invoke", "admin"];

/// The five availability words of a relation row type.
const AVAILABILITY_WORDS: [&str; 5] = [
    "catalogue",
    "derived",
    "live",
    "local-durable",
    "durable-observation",
];

const INTERFACE_CLAUSE: &str = "ORNA091-E-INTERFACE-CLAUSE";

impl Parser {
    /// Parses the clauses that follow a declaration head, stopping at the first token that is not a clause word.
    /// Each clause is checked against its target and the module kind. A clause in a user module is reported and
    /// then discarded, so that one bad clause does not cascade.
    pub(super) fn parse_interface_clauses(&mut self, target: ClauseTarget) -> InterfaceClauses {
        let mut clauses = InterfaceClauses::default();
        let mut seen: Vec<String> = Vec::new();
        while self.at_clause_word() {
            let word = self.current().text.clone();
            let word_span = self.current().span.clone();
            self.bump();
            if !self.interface {
                self.error_here(
                    INTERFACE_CLAUSE,
                    "interface clauses are permitted only in engine-embedded modules",
                );
            } else if !clause_allowed(&word, target) {
                self.error_here(
                    INTERFACE_CLAUSE,
                    &format!("the `{word}` clause is not permitted on this declaration"),
                );
            } else if seen.contains(&word) {
                self.error_here(INTERFACE_CLAUSE, &format!("duplicate `{word}` clause"));
            }
            seen.push(word.clone());
            let span = word_span.join(self.previous().span.clone());
            self.parse_one_clause(&word, span, &mut clauses);
        }
        if self.interface {
            clauses
        } else {
            InterfaceClauses::default()
        }
    }

    fn at_clause_word(&self) -> bool {
        matches!(self.current().kind, TokenKind::Identifier { .. })
            && CLAUSE_WORDS.contains(&self.current().text.as_str())
    }

    /// Parses the value of one clause. The clause word is already consumed. Duplicates and wrong targets are
    /// reported by the caller, so this function only fills in the value.
    fn parse_one_clause(&mut self, word: &str, span: SourceSpan, clauses: &mut InterfaceClauses) {
        match word {
            "class" => {
                clauses.class = self.clause_word_value("class", &CATALOGUE_CLASSES);
            }
            "fails" => clauses.fails = self.clause_name_list(),
            "key" => clauses.key = self.clause_key_list(),
            "since" => clauses.since = self.clause_string("since"),
            "spelling" => clauses.spelling = self.clause_string("spelling"),
            "availability" => clauses.availability = self.clause_availability(),
            "removed" => {
                let version = self.clause_string("removed");
                let (replacement, reason) = self.optional_use_and_reason();
                if let Some(version) = version {
                    clauses.removed = Some(RemovedClause {
                        version,
                        replacement,
                        reason,
                        span,
                    });
                }
            }
            "formerly" => {
                let name = self.clause_string("formerly");
                let diagnostic = self.optional_named_string("diagnostic");
                if let Some(name) = name {
                    clauses.formerly = Some(FormerlyClause {
                        name,
                        diagnostic,
                        span,
                    });
                }
            }
            "deprecated" => {
                let version = self.clause_string("deprecated");
                let (replacement, _) = self.optional_use_and_reason();
                if let Some(version) = version {
                    clauses.deprecated = Some(DeprecatedClause {
                        version,
                        replacement,
                        span,
                    });
                }
            }
            _ => unreachable!("parse_one_clause is called only with a CLAUSE_WORDS entry"),
        }
    }

    /// Reads one identifier from `allowed`, for a clause such as `class read`.
    fn clause_word_value(&mut self, word: &str, allowed: &[&str]) -> Option<NameSegment> {
        let token = self.current().clone();
        if !matches!(token.kind, TokenKind::Identifier { .. })
            || !allowed.contains(&token.text.as_str())
        {
            self.error_here(
                INTERFACE_CLAUSE,
                &format!("the `{word}` clause takes one of: {}", allowed.join(", ")),
            );
            return None;
        }
        self.bump();
        Some(NameSegment {
            text: token.text,
            span: token.span,
        })
    }

    /// Reads a comma-separated list of identifiers, as the `fails` clause does.
    fn clause_name_list(&mut self) -> Vec<NameSegment> {
        let mut names = Vec::new();
        loop {
            if !matches!(self.current().kind, TokenKind::Identifier { .. }) {
                self.error_here(INTERFACE_CLAUSE, "expected a name in the clause list");
                break;
            }
            let token = self.current().clone();
            self.bump();
            names.push(NameSegment {
                text: token.text,
                span: token.span,
            });
            if !self.is_punct(",") {
                break;
            }
            self.bump();
        }
        names
    }

    /// Reads a comma-separated list of key paths, as the `key` clause does. A path is one or more identifiers joined
    /// by `.`, so a key can name a field of a nested row: `change.area` on `DiffEntry`.
    fn clause_key_list(&mut self) -> Vec<NameSegment> {
        let mut keys = Vec::new();
        loop {
            if !self.contextual() {
                self.error_here(INTERFACE_CLAUSE, "expected a name in the clause list");
                break;
            }
            let first = self.current().clone();
            self.bump();
            let mut text = first.text;
            let mut span = first.span;
            while self.is_punct(".") {
                self.bump();
                if !self.contextual() {
                    self.error_here(
                        INTERFACE_CLAUSE,
                        "expected a name after `.` in the clause list",
                    );
                    break;
                }
                let segment = self.current().clone();
                self.bump();
                text.push('.');
                text.push_str(&segment.text);
                span.end = segment.span.end;
            }
            keys.push(NameSegment { text, span });
            if !self.is_punct(",") {
                break;
            }
            self.bump();
        }
        keys
    }

    /// Reads one string literal value, with its quotes removed.
    fn clause_string(&mut self, word: &str) -> Option<ClauseString> {
        let token = self.current().clone();
        if !matches!(token.kind, TokenKind::String) {
            self.error_here(
                INTERFACE_CLAUSE,
                &format!("the `{word}` clause takes a string literal"),
            );
            return None;
        }
        self.bump();
        let inner = token
            .text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(&token.text);
        Some(ClauseString {
            text: inner.to_string(),
            span: token.span,
        })
    }

    /// Reads the optional `use "path"` and `reason "text"` that follow a version string. Each word is read when it
    /// is next, so the two may come in either order.
    fn optional_use_and_reason(&mut self) -> (Option<ClauseString>, Option<ClauseString>) {
        let mut replacement = None;
        let mut reason = None;
        loop {
            if self.keyword() == Some(crate::lexer::Keyword::Use) {
                self.bump();
                replacement = self.clause_string("use");
            } else if self.is_identifier_text("reason") {
                self.bump();
                reason = self.clause_string("reason");
            } else {
                break;
            }
        }
        (replacement, reason)
    }

    /// Reads an optional `name "text"` pair, such as `diagnostic "code"`.
    fn optional_named_string(&mut self, word: &str) -> Option<ClauseString> {
        if !self.is_identifier_text(word) {
            return None;
        }
        self.bump();
        self.clause_string(word)
    }

    /// Reads the `availability "word"` value and checks it against the five availability words.
    fn clause_availability(&mut self) -> Option<ClauseString> {
        let value = self.clause_string("availability")?;
        if !AVAILABILITY_WORDS.contains(&value.text.as_str()) {
            self.error_here(
                INTERFACE_CLAUSE,
                "the `availability` clause takes one of the five availability words",
            );
            return None;
        }
        Some(value)
    }

    fn is_identifier_text(&self, text: &str) -> bool {
        matches!(self.current().kind, TokenKind::Identifier { .. }) && self.current().text == text
    }
}

/// Whether a clause word is in the clause table for a declaration form.
fn clause_allowed(word: &str, target: ClauseTarget) -> bool {
    match word {
        "class" | "fails" | "since" | "spelling" => target == ClauseTarget::NativeFn,
        "formerly" => matches!(target, ClauseTarget::NativeFn | ClauseTarget::NativeStatic),
        "key" | "availability" => target == ClauseTarget::Type,
        "removed" | "deprecated" => true,
        _ => false,
    }
}
