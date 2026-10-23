//! Inlay hints: parameter names at call sites, and `let` types that are
//! certain without inference.
//!
//! A type hint is shown only when the type follows from the source alone:
//! a literal other than `null` (which is an optional value of a type the
//! literal does not name), a comparison or logical operator, or a call to a
//! function whose declared result type is written in its signature. No type
//! is guessed from arithmetic or from function bodies.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};
use orna_syntax_v1::{
    Argument, Declaration, Expr, LiteralKind, Pattern, Statement, StringSegment, SyntaxSpan,
};

use crate::analysis;
use crate::documents::PositionMapper;
use crate::names::control_arms;
use crate::workspace::{FileAnalysis, Target, Workspace};

/// Returns in-range type and parameter-name hints for one module.
pub(crate) fn inlay_hints(workspace: &Workspace, module: usize, range: &Range) -> Vec<InlayHint> {
    let file = workspace.file(module);
    let mut collector = Collector {
        workspace,
        module,
        file,
        mapper: PositionMapper::new(file.text()),
        range,
        hints: Vec::new(),
    };
    for item in &file.parse.value.items {
        if let Declaration::Let {
            pattern: Pattern::Name(_, name),
            annotation: None,
            value,
        } = &item.declaration
        {
            collector.type_hint(name, value);
        }
        for body in analysis::item_bodies(item) {
            collector.expression(body);
        }
    }
    let mut hints = collector.hints;
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    hints
}

struct Collector<'a> {
    workspace: &'a Workspace,
    module: usize,
    file: &'a FileAnalysis,
    mapper: PositionMapper<'a>,
    range: &'a Range,
    hints: Vec<InlayHint>,
}

impl Collector<'_> {
    fn type_hint(&mut self, name: &SyntaxSpan, value: &Expr) {
        let Some(ty) = self.certain_type(value) else {
            return;
        };
        let position = self.mapper.position(name.end);
        if !contains(self.range, position) {
            return;
        }
        self.hints.push(InlayHint {
            position,
            label: InlayHintLabel::String(format!(": {ty}")),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }

    /// The type of `value` when the source states it, otherwise `None`.
    fn certain_type(&self, value: &Expr) -> Option<String> {
        match value {
            Expr::Literal { kind, .. } => Some(
                match kind {
                    LiteralKind::Integer => "Int",
                    LiteralKind::Decimal => "Decimal",
                    LiteralKind::Float => "Float",
                    LiteralKind::Date => "Date",
                    LiteralKind::Instant => "Instant",
                    LiteralKind::String => "Str",
                    LiteralKind::Boolean => "Bool",
                    // `null` is an optional value; its type is not written.
                    LiteralKind::Null => return None,
                }
                .to_owned(),
            ),
            Expr::InterpolatedString { .. } => Some("Str".to_owned()),
            Expr::Group { inner, .. } => self.certain_type(inner),
            Expr::Unary { op, .. } if op == "!" => Some("Bool".to_owned()),
            Expr::Binary { op, .. }
                if matches!(
                    op.as_str(),
                    "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||"
                ) =>
            {
                Some("Bool".to_owned())
            }
            Expr::Call { callee, .. } | Expr::GenericCall { callee, .. } => {
                self.declared_result(callee)
            }
            _ => None,
        }
    }

    /// The result type written in the signature of the called function.
    fn declared_result(&self, callee: &Expr) -> Option<String> {
        match self.callee_target(callee)? {
            Target::Declared { module, index } => {
                let declared = self.workspace.declaration(module, index);
                let symbol = analysis::declared_symbol(
                    &self.workspace.file(module).symbols,
                    &declared.selection,
                )?;
                symbol.result_type.clone()
            }
            Target::Standard { module, name } => {
                self.workspace.standard.function_result(&module, &name)
            }
            Target::Local { .. } | Target::Module(_) => None,
        }
    }

    fn callee_target(&self, callee: &Expr) -> Option<Target> {
        let name = analysis::callee_name_span(self.file, callee)?;
        let occurrence = self.file.names.occurrence(&name)?;
        self.workspace.resolve(self.module, occurrence).target()
    }

    fn parameter_hints(&mut self, callee: &Expr, arguments: &[Argument]) {
        let names = match self.callee_target(callee) {
            Some(Target::Declared { module, index }) => {
                let declared = self.workspace.declaration(module, index);
                match analysis::declared_symbol(
                    &self.workspace.file(module).symbols,
                    &declared.selection,
                ) {
                    Some(symbol) => symbol.parameter_names.clone(),
                    None => return,
                }
            }
            Some(Target::Standard { module, name }) => {
                match self.workspace.standard.function_parameters(&module, &name) {
                    Some(names) => names,
                    None => return,
                }
            }
            _ => return,
        };
        for (argument, parameter) in arguments.iter().zip(&names) {
            if argument.name.is_some() {
                continue;
            }
            let Some(name) = parameter else {
                continue;
            };
            let position = self.mapper.position(argument.value.span().start);
            if !contains(self.range, position) {
                continue;
            }
            self.hints.push(InlayHint {
                position,
                label: InlayHintLabel::String(format!("{name}: ")),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
    }

    fn expression(&mut self, expression: &Expr) {
        match expression {
            Expr::Call {
                callee, arguments, ..
            }
            | Expr::GenericCall {
                callee, arguments, ..
            } => {
                self.parameter_hints(callee, arguments);
                self.expression(callee);
                for argument in arguments {
                    self.expression(&argument.value);
                }
            }
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let StringSegment::Expression { value, .. } = segment {
                        self.expression(value);
                    }
                }
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => self.expression(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.expression(lhs);
                self.expression(rhs);
            }
            Expr::Range { lower, upper, .. } => {
                for value in [lower, upper].into_iter().flatten() {
                    self.expression(value);
                }
            }
            Expr::Index { base, index, .. } => {
                self.expression(base);
                self.expression(index);
            }
            Expr::Field { base, .. } => self.expression(base),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for value in elements {
                    self.expression(value);
                }
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                for field in fields {
                    self.expression(&field.value);
                }
            }
            Expr::Lambda { body, .. } => self.expression(body),
            Expr::Block {
                statements, tail, ..
            } => {
                for statement in statements {
                    self.statement(statement);
                }
                if let Some(tail) = tail {
                    self.expression(tail);
                }
            }
            Expr::Control {
                kind,
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                for value in [condition, body].into_iter().flatten() {
                    self.expression(value);
                }
                for arm in control_arms(*kind, arms) {
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                }
                if let Some(alternate) = alternate {
                    self.expression(alternate);
                }
            }
            Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                ..
            } => {
                if annotation.is_none()
                    && let Pattern::Name(_, name) = pattern
                {
                    self.type_hint(name, value);
                }
                self.expression(value);
            }
            Statement::Assert { value, .. }
            | Statement::Expression { value, .. }
            | Statement::Control { value, .. }
            | Statement::Assignment { value, .. } => self.expression(value),
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                if let Some(value) = value {
                    self.expression(value);
                }
            }
            Statement::Continue { .. } => {}
        }
    }
}

fn contains(range: &Range, position: Position) -> bool {
    range.start <= position && position < range.end
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::documents::Document;

    fn hints(source: &str) -> Vec<String> {
        let workspace = Workspace::detached(vec![Arc::new(FileAnalysis::new(Document::new(
            "file:///w/hints.orna".parse().unwrap(),
            source.to_owned(),
            1,
        )))]);
        let mapper = PositionMapper::new(source);
        let range = Range {
            start: Position::default(),
            end: mapper.position(source.len()),
        };
        inlay_hints(&workspace, 0, &range)
            .into_iter()
            .map(|hint| {
                let InlayHintLabel::String(label) = hint.label else {
                    panic!("string labels only");
                };
                format!("{label}@{}:{}", hint.position.line, hint.position.character)
            })
            .collect()
    }

    #[test]
    fn local_types_and_parameter_names_follow_the_source() {
        const SOURCE: &str = include_str!("../tests/fixtures/editor-lsp-hints.orna");
        let labels = hints(SOURCE);
        assert!(labels.iter().any(|label| label.starts_with(": Int@")), "{labels:?}");
        assert!(labels.iter().any(|label| label.starts_with("left: @")), "{labels:?}");
        assert!(labels.iter().any(|label| label.starts_with("right: @")), "{labels:?}");
    }

    #[test]
    fn null_arithmetic_and_bodies_never_invent_a_type() {
        let source = "pub fn add(left: Int, right: Int): Int = left + right;\n\
            pub fn untyped(value: Int) = value;\n\
            pub fn demo(): Int {\n    let nothing = null;\n    let sum = 1 + 2;\n    \
            let flag = !true;\n    let same = 1 == 2;\n    let typed = add(1, 2);\n    \
            let guessed = untyped(1);\n    typed\n}\n";
        let labels = hints(source)
            .into_iter()
            .filter(|label| label.starts_with(": "))
            .collect::<Vec<_>>();
        assert_eq!(labels, [": Bool@5:12", ": Bool@6:12", ": Int@7:13"], "{labels:?}");
    }

    #[test]
    fn for_loop_bodies_are_hinted_once() {
        let source = "pub fn add(left: Int, right: Int): Int = left + right;\n\
            pub fn total(values: [Int]): Int {\n    let sum = 0;\n    \
            for value in values {\n        sum = add(sum, value);\n    }\n    sum\n}\n";
        let labels = hints(source);
        let parameters = labels
            .iter()
            .filter(|label| !label.starts_with(": "))
            .collect::<Vec<_>>();
        assert_eq!(parameters, ["left: @4:18", "right: @4:23"], "{labels:?}");
    }
}
