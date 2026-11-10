//! Unit families: the `= unit_expr` variant form and the `impl Unit {}` marker
//! (docs/design/units.md section 2, C13 and C16).

use super::{
    Diagnostic, EnumVariant, Implementation, Parser, SourceLines, SourceSpan, TypeExpr,
    annotate_span,
};
use crate::lexer::TokenKind;

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

impl Parser {
    /// Parses the `unit_expr` that follows the `=` of a unit variant.
    pub(super) fn parse_unit_expr(&mut self) -> Option<UnitExpr> {
        let start = self.current().span.clone();
        let mut terms = vec![self.parse_unit_term(false)?];
        while self.is_punct("*") || self.is_punct("/") {
            let divide = self.is_punct("/");
            self.bump();
            terms.push(self.parse_unit_term(divide)?);
        }
        Some(UnitExpr {
            terms,
            span: start.join(self.previous().span.clone()),
        })
    }

    fn parse_unit_term(&mut self, divide: bool) -> Option<UnitTerm> {
        let start = self.current().span.clone();
        let coefficient = if matches!(self.current().kind, TokenKind::Integer | TokenKind::Decimal)
        {
            let text = self.current().text.clone();
            self.bump();
            Some(text)
        } else {
            None
        };
        if !self.contextual() {
            self.error_here("ORNA-PARSE-001", "expected unit name in unit expression");
            return None;
        }
        let head = self.current().text.clone();
        self.bump();
        let (family, name) = if self.is_punct(".") {
            self.bump();
            if !self.contextual() {
                self.error_here("ORNA-PARSE-001", "expected unit name after `.`");
                return None;
            }
            let name = self.current().text.clone();
            self.bump();
            (Some(head), name)
        } else {
            (None, head)
        };
        let exponent = if self.is_punct("^") {
            self.bump();
            if !matches!(self.current().kind, TokenKind::Integer) {
                self.error_here(
                    "ORNA-PARSE-001",
                    "expected integer exponent in unit expression",
                );
                return None;
            }
            let value = self.current().text.clone();
            self.bump();
            Some(value)
        } else {
            None
        };
        Some(UnitTerm {
            divide,
            coefficient,
            family,
            name,
            exponent,
            span: start.join(self.previous().span.clone()),
        })
    }

    /// Settles the unit shape of an enum once its body is parsed. An enum is a unit family when its last
    /// implementation is the marker `impl Unit {}`; the marker is then removed from `implementations`.
    /// Returns whether the enum is a unit family.
    pub(super) fn settle_unit_family(
        &mut self,
        variants: &[EnumVariant],
        implementations: &mut Vec<Implementation>,
    ) -> bool {
        let Some(index) = implementations.iter().position(is_unit_marker) else {
            if let Some(variant) = variants.iter().find(|variant| variant.unit.is_some()) {
                self.errors.push(Diagnostic::error(
                    "ORNA-UNIT-009",
                    "a `=` variant needs the enum to be a unit family: end it with `impl Unit {}`",
                    variant.span.clone(),
                ));
            }
            return false;
        };
        let marker = implementations.remove(index);
        if index != implementations.len() {
            self.errors.push(Diagnostic::error(
                "ORNA-UNIT-009",
                "the `impl Unit {}` marker must be the last line of a unit family",
                marker.span.clone(),
            ));
        }
        if let Some(variant) = variants.iter().find(|variant| !variant.fields.is_empty()) {
            self.errors.push(Diagnostic::error(
                "ORNA-UNIT-009",
                "a unit family has no payload variants",
                variant.span.clone(),
            ));
        }
        true
    }
}

/// Whether an implementation is the marker `impl Unit {}`: protocol `Unit` with no arguments and no members.
fn is_unit_marker(implementation: &Implementation) -> bool {
    implementation.members.is_empty()
        && matches!(
            &implementation.protocol,
            TypeExpr::Name { path, arguments, .. } if path.len() == 1 && path[0] == "Unit" && arguments.is_empty()
        )
}

pub(super) fn annotate_unit_expr(expr: &mut UnitExpr, source: &SourceLines<'_>, file: &str) {
    for term in &mut expr.terms {
        annotate_span(&mut term.span, source, file);
    }
    annotate_span(&mut expr.span, source, file);
}
