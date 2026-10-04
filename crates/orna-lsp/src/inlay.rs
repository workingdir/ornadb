//! Inlay hints derived from syntax-v1 declarations and expression structure.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, InlayHintTooltip, Position, Range};
use orna_syntax_v1::{
    Argument, ControlKind, Declaration, Expr, FunctionSignature, LiteralKind, Pattern, Statement,
    SyntaxSpan, SyntaxTree, TypeExpr,
};

use crate::{analysis, documents::PositionMapper, locals};

/// Returns in-range type and parameter-name hints for one parsed document.
pub fn inlay_hints(
    parse: &analysis::EditorParse,
    text: &str,
    mapper: &PositionMapper<'_>,
    range: &Range,
) -> Vec<InlayHint> {
    let tree = &parse.value;
    let mut hints = Vec::new();
    for item in &tree.items {
        match &item.declaration {
            Declaration::Function { body, .. } => {
                collect_expression_hints(body, tree, text, mapper, range, &mut hints);
            }
            Declaration::Let {
                pattern,
                annotation,
                value,
            } => {
                if annotation.is_none() {
                    if let Pattern::Name(_, name_span) = pattern {
                        add_inferred_type_hint(
                            name_span, value, tree, text, mapper, range, &mut hints,
                        );
                    }
                }
                collect_expression_hints(value, tree, text, mapper, range, &mut hints);
            }
            Declaration::Assertion { value } => {
                collect_expression_hints(value, tree, text, mapper, range, &mut hints);
            }
            _ => {}
        }
    }
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    hints
}

fn add_inferred_type_hint(
    name_span: &SyntaxSpan,
    value: &Expr,
    tree: &SyntaxTree,
    text: &str,
    mapper: &PositionMapper<'_>,
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    let Some(inferred) = infer_type(value, tree, text) else {
        return;
    };
    let position = mapper.position(name_span.end);
    if !contains(range, position) {
        return;
    }
    hints.push(InlayHint {
        position,
        label: InlayHintLabel::String(format!(": {inferred}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: Some(InlayHintTooltip::String(
            "Inferred from the initializer expression.".to_owned(),
        )),
        padding_left: Some(true),
        padding_right: None,
        data: None,
    });
}

fn collect_expression_hints(
    expression: &Expr,
    tree: &SyntaxTree,
    text: &str,
    mapper: &PositionMapper<'_>,
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    match expression {
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            add_parameter_hints(callee, arguments, tree, mapper, range, hints);
            collect_expression_hints(callee, tree, text, mapper, range, hints);
            for argument in arguments {
                collect_expression_hints(&argument.value, tree, text, mapper, range, hints);
            }
        }
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let orna_syntax_v1::StringSegment::Expression { value, .. } = segment {
                    collect_expression_hints(value, tree, text, mapper, range, hints);
                }
            }
        }
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
            collect_expression_hints(rhs, tree, text, mapper, range, hints);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expression_hints(lhs, tree, text, mapper, range, hints);
            collect_expression_hints(rhs, tree, text, mapper, range, hints);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(value) = lower {
                collect_expression_hints(value, tree, text, mapper, range, hints);
            }
            if let Some(value) = upper {
                collect_expression_hints(value, tree, text, mapper, range, hints);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expression_hints(base, tree, text, mapper, range, hints);
            collect_expression_hints(index, tree, text, mapper, range, hints);
        }
        Expr::Field { base, .. } => {
            collect_expression_hints(base, tree, text, mapper, range, hints);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for value in elements {
                collect_expression_hints(value, tree, text, mapper, range, hints);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_expression_hints(&field.value, tree, text, mapper, range, hints);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_expression_hints(body, tree, text, mapper, range, hints);
        }
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                match statement {
                    Statement::Let {
                        pattern,
                        annotation,
                        value,
                        ..
                    } => {
                        if annotation.is_none() {
                            if let Pattern::Name(_, name_span) = pattern {
                                add_inferred_type_hint(
                                    name_span, value, tree, text, mapper, range, hints,
                                );
                            }
                        }
                        collect_expression_hints(value, tree, text, mapper, range, hints);
                    }
                    Statement::Assert { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. } => {
                        collect_expression_hints(value, tree, text, mapper, range, hints);
                    }
                    Statement::Return { value, .. } | Statement::Break { value, .. } => {
                        if let Some(value) = value {
                            collect_expression_hints(value, tree, text, mapper, range, hints);
                        }
                    }
                    Statement::Assignment { value, .. } => {
                        collect_expression_hints(value, tree, text, mapper, range, hints);
                    }
                    Statement::Continue { .. } => {}
                }
            }
            if let Some(tail) = tail {
                collect_expression_hints(tail, tree, text, mapper, range, hints);
            }
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(condition) = condition {
                collect_expression_hints(condition, tree, text, mapper, range, hints);
            }
            if let Some(body) = body {
                collect_expression_hints(body, tree, text, mapper, range, hints);
            }
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expression_hints(guard, tree, text, mapper, range, hints);
                }
                collect_expression_hints(&arm.body, tree, text, mapper, range, hints);
            }
            if let Some(alternate) = alternate {
                collect_expression_hints(alternate, tree, text, mapper, range, hints);
            }
        }
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
    }
}

fn add_parameter_hints(
    callee: &Expr,
    arguments: &[Argument],
    tree: &SyntaxTree,
    mapper: &PositionMapper<'_>,
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    let parameter_names = match callee {
        Expr::Name {
            text: callee_name,
            span: callee_span,
        } => {
            if locals::binding_at(tree, callee_name, callee_span).is_some() {
                return;
            }
            if let Some(signature) = function_signature(tree, callee_name) {
                signature
                    .parameters
                    .iter()
                    .map(|parameter| match &parameter.pattern {
                        Pattern::Name(name, _) => Some(name.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            } else {
                let Some(name) = analysis::expression_name(callee) else {
                    return;
                };
                let Some(parameters) =
                    analysis::standard_parameter_names_for_call(tree, &name, callee)
                else {
                    return;
                };
                parameters.into_iter().map(Some).collect()
            }
        }
        _ => {
            let Some(name) = analysis::expression_name(callee) else {
                return;
            };
            let Some(parameters) = analysis::standard_parameter_names_for_call(tree, &name, callee)
            else {
                return;
            };
            parameters.into_iter().map(Some).collect()
        }
    };
    for (argument, parameter_name) in arguments.iter().zip(&parameter_names) {
        if argument.name.is_some() {
            continue;
        }
        let Some(name) = parameter_name else {
            continue;
        };
        let position = mapper.position(argument.value.span().start);
        if !contains(range, position) {
            continue;
        }
        hints.push(InlayHint {
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

fn function_signature<'a>(tree: &'a SyntaxTree, name: &str) -> Option<&'a FunctionSignature> {
    let key = analysis::normalized_identifier(name);
    let mut found = tree.items.iter().filter_map(|item| {
        let Declaration::Function { signature, .. } = &item.declaration else {
            return None;
        };
        (analysis::normalized_identifier(&signature.name) == key).then_some(signature)
    });
    let signature = found.next()?;
    found.next().is_none().then_some(signature)
}

fn infer_type(expression: &Expr, tree: &SyntaxTree, text: &str) -> Option<String> {
    match expression {
        Expr::Literal { kind, .. } => Some(
            match kind {
                LiteralKind::Integer => "Int",
                LiteralKind::Decimal => "Decimal",
                LiteralKind::Float => "Float",
                LiteralKind::Date => "Date",
                LiteralKind::Instant => "Instant",
                LiteralKind::String => "Str",
                LiteralKind::Boolean => "Bool",
                LiteralKind::Null => "Null",
            }
            .to_owned(),
        ),
        Expr::InterpolatedString { .. } => Some("Str".to_owned()),
        Expr::Group { inner, .. } => infer_type(inner, tree, text),
        Expr::Unary { op, rhs, .. } if op == "not" => Some("Bool".to_owned()),
        Expr::Unary { rhs, .. } => infer_type(rhs, tree, text),
        Expr::Binary { op, .. }
            if matches!(
                op.as_str(),
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "and" | "or"
            ) =>
        {
            Some("Bool".to_owned())
        }
        Expr::Binary { lhs, rhs, .. } => {
            let left = infer_type(lhs, tree, text)?;
            let right = infer_type(rhs, tree, text)?;
            (left == right).then_some(left)
        }
        Expr::Call { callee, .. } | Expr::GenericCall { callee, .. } => {
            if let Expr::Name { text: name, .. } = callee.as_ref()
                && let Some(signature) = function_signature(tree, name)
            {
                return signature
                    .result
                    .as_ref()
                    .and_then(|ty| source_type(text, ty))
                    .or_else(|| infer_type_from_body(tree, signature, text));
            }
            let name = analysis::expression_name(callee)?;
            analysis::standard_result_type_for_call(tree, &name, callee)
        }
        Expr::Control {
            kind: ControlKind::If,
            body: Some(body),
            alternate: Some(alternate),
            ..
        } => {
            let body = infer_type(body, tree, text)?;
            let alternate = infer_type(alternate, tree, text)?;
            (body == alternate).then_some(body)
        }
        Expr::Block {
            tail: Some(tail), ..
        } => infer_type(tail, tree, text),
        _ => None,
    }
}

fn infer_type_from_body(
    tree: &SyntaxTree,
    signature: &FunctionSignature,
    text: &str,
) -> Option<String> {
    let item = tree.items.iter().find(|item| {
        matches!(
            &item.declaration,
            Declaration::Function { signature: candidate, .. }
                if std::ptr::eq(candidate, signature)
        )
    })?;
    let Declaration::Function { body, .. } = &item.declaration else {
        return None;
    };
    infer_type(body, tree, text)
}

fn source_type(text: &str, ty: &TypeExpr) -> Option<String> {
    let span = type_span(ty);
    text.get(span.start..span.end)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn type_span(ty: &TypeExpr) -> &SyntaxSpan {
    match ty {
        TypeExpr::Name { span, .. }
        | TypeExpr::Optional { span, .. }
        | TypeExpr::Product { span, .. }
        | TypeExpr::List { span, .. }
        | TypeExpr::Record { span, .. }
        | TypeExpr::Tuple { span, .. }
        | TypeExpr::Function { span, .. } => span,
    }
}

fn contains(range: &Range, position: Position) -> bool {
    range.start <= position && position < range.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_syntax_v1::parse_module_with_file;

    const SOURCE: &str = include_str!("../tests/fixtures/editor-lsp-hints.orna");

    fn full_range(mapper: &PositionMapper<'_>) -> Range {
        Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: mapper.position(SOURCE.len()),
        }
    }

    #[test]
    fn local_types_and_parameter_names_are_inferred_from_syntax() {
        let parse = parse_module_with_file(SOURCE, "<fixture>");
        assert!(parse.is_ok(), "{:?}", parse.diagnostics);
        let mapper = PositionMapper::new(SOURCE);
        let hints = inlay_hints(&parse, SOURCE, &mapper, &full_range(&mapper));
        let labels = hints
            .iter()
            .filter_map(|hint| match &hint.label {
                InlayHintLabel::String(label) => Some(label.as_str()),
                InlayHintLabel::LabelParts(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(labels.contains(&": Int"), "{labels:?}");
        assert!(labels.contains(&"left: "), "{labels:?}");
        assert!(labels.contains(&"right: "), "{labels:?}");

        let one_line = Range {
            start: Position {
                line: 4,
                character: 0,
            },
            end: Position {
                line: 4,
                character: 3,
            },
        };
        assert!(inlay_hints(&parse, SOURCE, &mapper, &one_line).is_empty());
    }
}
