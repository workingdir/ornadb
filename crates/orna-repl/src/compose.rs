//! Composes the source the HIR lowers for one REPL input: the session's definitions plus one entry function.
//!
//! The REPL's `$_` is not Orna syntax, so every occurrence is rewritten to the parameter [`LAST`] before lowering.

use orna_syntax::visit::{Visit, walk_declaration, walk_expr};
use orna_syntax::{TokenKind, lex};
use orna_syntax_ast::SourceSpan;
use orna_syntax_ast::{Declaration, Expr, Item};

use crate::error::ReplError;

/// The name of the entry function one expression or `let` is wrapped in.
pub(crate) const ENTRY: &str = "__repl_input";
/// The parameter that carries the previous result, `$_`.
pub(crate) const LAST: &str = "__repl_last";
/// The spelling of the previous-result reference in the REPL.
const LAST_SPELLING: &str = "$_";
/// The prefix every name the session generates starts with. User names may not use it.
pub(crate) const RESERVED_PREFIX: &str = "__repl";

/// Collects the `$_` references in one syntax node, in traversal order.
#[derive(Default)]
struct References {
    spans: Vec<SourceSpan>,
    unsupported: bool,
}

impl Visit for References {
    fn visit_expr(&mut self, expression: &Expr) {
        if let Expr::ReplBinding { text, span } = expression {
            if text == LAST_SPELLING {
                self.spans.push(span.clone());
            } else {
                self.unsupported = true;
            }
        }
        walk_expr(self, expression);
    }
}

impl References {
    fn finish(mut self) -> Result<Vec<SourceSpan>, ReplError> {
        if self.unsupported {
            return Err(ReplError::new(
                "ORNA-EVAL-UNSUPPORTED",
                "only $_ refers to the previous result",
            ));
        }
        self.spans.sort_by_key(|span| span.start);
        Ok(self.spans)
    }
}

/// The `$_` spans of an expression, in source order.
pub(crate) fn expression_references(expression: &Expr) -> Result<Vec<SourceSpan>, ReplError> {
    let mut references = References::default();
    references.visit_expr(expression);
    references.finish()
}

/// The `$_` spans of a declaration, in source order.
pub(crate) fn declaration_references(
    declaration: &Declaration,
) -> Result<Vec<SourceSpan>, ReplError> {
    let mut references = References::default();
    walk_declaration(&mut references, declaration);
    references.finish()
}

/// The source text one span covers.
pub(crate) fn span_text<'a>(source: &'a str, span: &SourceSpan) -> Result<&'a str, ReplError> {
    source.get(span.start..span.end).ok_or_else(internal_span)
}

/// The identifier spellings in `text`, in source order.
pub(crate) fn identifiers(text: &str) -> Result<Vec<String>, ReplError> {
    let tokens = lex(text)
        .map_err(|_| ReplError::new("ORNA-REPL-INTERNAL", "the composed input does not lex"))?;
    Ok(tokens
        .into_iter()
        .filter(|token| matches!(token.kind, TokenKind::Identifier { .. }))
        .map(|token| token.text)
        .collect())
}

/// The text of a whole declaration, terminated by `;` as the module grammar requires.
pub(crate) fn declaration_text(source: &str, item: &Item) -> Result<String, ReplError> {
    let text = span_text(source, &item.span)?.trim_end();
    if text.ends_with(';') {
        Ok(text.to_owned())
    } else {
        Ok(format!("{text};"))
    }
}

/// The source of `span` with each `references` span replaced by the [`LAST`] parameter.
pub(crate) fn rewrite(
    source: &str,
    span: &SourceSpan,
    references: &[SourceSpan],
) -> Result<String, ReplError> {
    let mut text = String::new();
    let mut cursor = span.start;
    for reference in references {
        text.push_str(
            source
                .get(cursor..reference.start)
                .ok_or_else(internal_span)?,
        );
        text.push_str(LAST);
        cursor = reference.end;
    }
    text.push_str(source.get(cursor..span.end).ok_or_else(internal_span)?);
    Ok(text)
}

fn internal_span() -> ReplError {
    ReplError::new(
        "ORNA-REPL-INTERNAL",
        "an input span is not on a character boundary",
    )
}

/// The entry function: its parameters as `(name, printed type)`, its printed result type if any, and its body.
pub(crate) fn entry(parameters: &[(String, String)], result: Option<&str>, body: &str) -> String {
    let parameters = parameters
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = result.map(|ty| format!(": {ty}")).unwrap_or_default();
    format!("fn {ENTRY}({parameters}){result} = {body};")
}
