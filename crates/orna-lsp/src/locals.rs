//! Lexical binding and reference collection for function-local names.

use std::collections::BTreeSet;

use orna_syntax_v1::{
    CaseArm, ControlKind, Declaration, Expr, Pattern, Statement, SyntaxSpan, SyntaxTree, TokenKind,
    lex,
};

#[derive(Clone, Debug)]
pub(crate) struct LocalBinding {
    pub name: String,
    pub selection: SyntaxSpan,
    pub context: SyntaxSpan,
    pub kind: LocalBindingKind,
    key: String,
    scope: SyntaxSpan,
    visible_after: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalBindingKind {
    Parameter,
    Local,
    Pattern,
}

#[derive(Clone, Debug)]
struct LocalOccurrence {
    key: String,
    span: SyntaxSpan,
}

pub(crate) fn bindings(tree: &SyntaxTree) -> Vec<LocalBinding> {
    let mut output = Vec::new();
    for item in &tree.items {
        let Declaration::Function { signature, body } = &item.declaration else {
            continue;
        };
        let body_span = body.span();
        for parameter in &signature.parameters {
            collect_pattern_bindings(
                &parameter.pattern,
                &body_span,
                body_span.start,
                &parameter.span,
                LocalBindingKind::Parameter,
                &mut output,
            );
        }
        collect_expr_bindings(body, &mut output);
    }
    output.sort_by_key(|binding| binding.selection.start);
    output
}

pub(crate) fn all_bindings(tree: &SyntaxTree) -> Vec<LocalBinding> {
    bindings(tree)
}

/// Returns the innermost visible binding for each name at a source position.
pub(crate) fn visible_bindings(tree: &SyntaxTree, position: usize) -> Vec<LocalBinding> {
    let bindings = bindings(tree);
    let keys = bindings
        .iter()
        .map(|binding| binding.key.clone())
        .collect::<BTreeSet<_>>();
    let mut visible = keys
        .into_iter()
        .filter_map(|key| resolve(&bindings, &key, position).cloned())
        .collect::<Vec<_>>();
    visible.sort_by_key(|binding| binding.selection.start);
    visible
}

/// Finds the local binding selected by an identifier token, either at its
/// declaration or at a name occurrence that resolves to it.
pub(crate) fn binding_at(tree: &SyntaxTree, name: &str, span: &SyntaxSpan) -> Option<LocalBinding> {
    let bindings = bindings(tree);
    let key = normalize(name);
    if let Some(binding) = bindings
        .iter()
        .find(|binding| same_span(&binding.selection, span) && binding.key == key)
    {
        return Some(binding.clone());
    }
    let occurrences = occurrences(tree);
    let occurrence = occurrences
        .iter()
        .find(|occurrence| same_span(&occurrence.span, span) && occurrence.key == key)?;
    resolve(&bindings, &occurrence.key, occurrence.span.start).cloned()
}

pub(crate) fn references(tree: &SyntaxTree, binding: &LocalBinding) -> Vec<SyntaxSpan> {
    let bindings = bindings(tree);
    occurrences(tree)
        .into_iter()
        .filter(|occurrence| {
            occurrence.key == binding.key
                && resolve(&bindings, &binding.key, occurrence.span.start)
                    .is_some_and(|resolved| resolved.selection == binding.selection)
        })
        .map(|occurrence| occurrence.span)
        .collect()
}

pub(crate) fn resolved_reference_spans(tree: &SyntaxTree) -> BTreeSet<(usize, usize)> {
    let bindings = bindings(tree);
    occurrences(tree)
        .into_iter()
        .filter(|occurrence| resolve(&bindings, &occurrence.key, occurrence.span.start).is_some())
        .map(|occurrence| (occurrence.span.start, occurrence.span.end))
        .collect()
}

fn resolve<'a>(
    bindings: &'a [LocalBinding],
    key: &str,
    position: usize,
) -> Option<&'a LocalBinding> {
    bindings
        .iter()
        .filter(|binding| {
            binding.key == key
                && binding.visible_after <= position
                && binding.scope.start <= position
                && position < binding.scope.end
        })
        .min_by_key(|binding| {
            (
                binding.scope.end.saturating_sub(binding.scope.start),
                usize::MAX - binding.visible_after,
            )
        })
}

fn same_span(left: &SyntaxSpan, right: &SyntaxSpan) -> bool {
    left.start == right.start && left.end == right.end
}

fn normalize(name: &str) -> String {
    lex(name)
        .ok()
        .and_then(|tokens| {
            tokens.into_iter().find_map(|token| match token.kind {
                TokenKind::Identifier { normalized } => Some(normalized),
                _ => None,
            })
        })
        .unwrap_or_else(|| name.to_owned())
}

fn collect_pattern_bindings(
    pattern: &Pattern,
    scope: &SyntaxSpan,
    visible_after: usize,
    context: &SyntaxSpan,
    kind: LocalBindingKind,
    output: &mut Vec<LocalBinding>,
) {
    match pattern {
        Pattern::Name(name, span) => {
            push_binding(name, span, scope, visible_after, context, kind, output);
        }
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for element in elements {
                collect_pattern_bindings(element, scope, visible_after, context, kind, output);
            }
        }
        Pattern::Record { fields, .. } => {
            for (name, pattern, span) in fields {
                if let Some(pattern) = pattern {
                    collect_pattern_bindings(pattern, scope, visible_after, context, kind, output);
                } else {
                    push_binding(name, span, scope, visible_after, context, kind, output);
                }
            }
        }
        Pattern::Constructor {
            arguments, fields, ..
        } => {
            for pattern in arguments {
                collect_pattern_bindings(pattern, scope, visible_after, context, kind, output);
            }
            for field in fields {
                if let Some(pattern) = &field.pattern {
                    collect_pattern_bindings(pattern, scope, visible_after, context, kind, output);
                } else {
                    push_binding(
                        &field.name,
                        &field.span,
                        scope,
                        visible_after,
                        context,
                        kind,
                        output,
                    );
                }
            }
        }
        Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
    }
}

fn push_binding(
    name: &str,
    selection: &SyntaxSpan,
    scope: &SyntaxSpan,
    visible_after: usize,
    context: &SyntaxSpan,
    kind: LocalBindingKind,
    output: &mut Vec<LocalBinding>,
) {
    output.push(LocalBinding {
        name: name.to_owned(),
        selection: selection.clone(),
        context: context.clone(),
        kind,
        key: normalize(name),
        scope: scope.clone(),
        visible_after,
    });
}

fn collect_expr_bindings(expression: &Expr, output: &mut Vec<LocalBinding>) {
    match expression {
        Expr::Lambda {
            parameters, body, ..
        } => {
            let body_span = body.span();
            for parameter in parameters {
                collect_pattern_bindings(
                    &parameter.pattern,
                    &body_span,
                    body_span.start,
                    &parameter.span,
                    LocalBindingKind::Parameter,
                    output,
                );
            }
            collect_expr_bindings(body, output);
        }
        Expr::Block {
            statements,
            tail,
            span,
        } => {
            for statement in statements {
                if let Statement::Let {
                    pattern,
                    value,
                    span: statement_span,
                    ..
                } = statement
                {
                    collect_expr_bindings(value, output);
                    collect_pattern_bindings(
                        pattern,
                        span,
                        statement_span.end,
                        statement_span,
                        LocalBindingKind::Local,
                        output,
                    );
                } else {
                    collect_statement_bindings(statement, output);
                }
            }
            if let Some(tail) = tail {
                collect_expr_bindings(tail, output);
            }
        }
        Expr::Control {
            kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(condition) = condition {
                collect_expr_bindings(condition, output);
            }
            if *kind == ControlKind::For {
                if let (Some(pattern), Some(body)) = (binding, body) {
                    let body_span = body.span();
                    collect_pattern_bindings(
                        pattern,
                        &body_span,
                        pattern_span(pattern).end,
                        &pattern_span(pattern),
                        LocalBindingKind::Pattern,
                        output,
                    );
                }
            }
            if let Some(body) = body {
                collect_expr_bindings(body, output);
            }
            for arm in arms {
                if *kind == ControlKind::For {
                    continue;
                }
                collect_arm_bindings(arm, output);
            }
            if let Some(alternate) = alternate {
                collect_expr_bindings(alternate, output);
            }
        }
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let orna_syntax_v1::StringSegment::Expression { value, .. } = segment {
                    collect_expr_bindings(value, output);
                }
            }
        }
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
        Expr::Unary { rhs, .. } => collect_expr_bindings(rhs, output),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_bindings(lhs, output);
            collect_expr_bindings(rhs, output);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(value) = lower {
                collect_expr_bindings(value, output);
            }
            if let Some(value) = upper {
                collect_expr_bindings(value, output);
            }
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            collect_expr_bindings(callee, output);
            for argument in arguments {
                collect_expr_bindings(&argument.value, output);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expr_bindings(base, output);
            collect_expr_bindings(index, output);
        }
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => {
            collect_expr_bindings(base, output);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for value in elements {
                collect_expr_bindings(value, output);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_expr_bindings(&field.value, output);
            }
        }
    }
}

fn collect_arm_bindings(arm: &CaseArm, output: &mut Vec<LocalBinding>) {
    let visible_after = pattern_span(&arm.pattern).end;
    collect_pattern_bindings(
        &arm.pattern,
        &arm.span,
        visible_after,
        &arm.span,
        LocalBindingKind::Pattern,
        output,
    );
    if let Some(guard) = &arm.guard {
        collect_expr_bindings(guard, output);
    }
    collect_expr_bindings(&arm.body, output);
}

fn collect_statement_bindings(statement: &Statement, output: &mut Vec<LocalBinding>) {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => collect_expr_bindings(value, output),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            if let Some(value) = value {
                collect_expr_bindings(value, output);
            }
        }
        Statement::Assignment { target, value, .. } => {
            collect_assignment_target_bindings(target, output);
            collect_expr_bindings(value, output);
        }
        Statement::Continue { .. } => {}
    }
}

fn collect_assignment_target_bindings(
    target: &orna_syntax_v1::AssignmentTarget,
    output: &mut Vec<LocalBinding>,
) {
    if let orna_syntax_v1::AssignmentTarget::Index { index, base, .. } = target {
        collect_assignment_target_bindings(base, output);
        collect_expr_bindings(index, output);
    } else if let orna_syntax_v1::AssignmentTarget::Field { base, .. } = target {
        collect_assignment_target_bindings(base, output);
    }
}

fn pattern_span(pattern: &Pattern) -> SyntaxSpan {
    match pattern {
        Pattern::Name(_, span) | Pattern::Wildcard(span) | Pattern::Literal { span, .. } => {
            span.clone()
        }
        Pattern::Tuple { span, .. }
        | Pattern::List { span, .. }
        | Pattern::Record { span, .. }
        | Pattern::Constructor { span, .. } => span.clone(),
    }
}

fn occurrences(tree: &SyntaxTree) -> Vec<LocalOccurrence> {
    let mut output = Vec::new();
    for item in &tree.items {
        if let Declaration::Function { body, .. } = &item.declaration {
            collect_expr_occurrences(body, &mut output);
        }
    }
    output.sort_by_key(|occurrence| occurrence.span.start);
    output.dedup_by(|left, right| left.span == right.span);
    output
}

fn collect_expr_occurrences(expression: &Expr, output: &mut Vec<LocalOccurrence>) {
    match expression {
        Expr::Name { text, span } => output.push(LocalOccurrence {
            key: normalize(text),
            span: span.clone(),
        }),
        Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let orna_syntax_v1::StringSegment::Expression { value, .. } = segment {
                    collect_expr_occurrences(value, output);
                }
            }
        }
        Expr::Unary { rhs, .. } => collect_expr_occurrences(rhs, output),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_occurrences(lhs, output);
            collect_expr_occurrences(rhs, output);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(value) = lower {
                collect_expr_occurrences(value, output);
            }
            if let Some(value) = upper {
                collect_expr_occurrences(value, output);
            }
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            collect_expr_occurrences(callee, output);
            for argument in arguments {
                collect_expr_occurrences(&argument.value, output);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expr_occurrences(base, output);
            collect_expr_occurrences(index, output);
        }
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => {
            collect_expr_occurrences(base, output);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for value in elements {
                collect_expr_occurrences(value, output);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_expr_occurrences(&field.value, output);
            }
        }
        Expr::Lambda { body, .. } => collect_expr_occurrences(body, output),
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                collect_statement_occurrences(statement, output);
            }
            if let Some(tail) = tail {
                collect_expr_occurrences(tail, output);
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
            if let Some(condition) = condition {
                collect_expr_occurrences(condition, output);
            }
            if let Some(body) = body {
                collect_expr_occurrences(body, output);
            }
            if *kind != ControlKind::For {
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        collect_expr_occurrences(guard, output);
                    }
                    collect_expr_occurrences(&arm.body, output);
                }
            }
            if let Some(alternate) = alternate {
                collect_expr_occurrences(alternate, output);
            }
        }
    }
}

fn collect_statement_occurrences(statement: &Statement, output: &mut Vec<LocalOccurrence>) {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => collect_expr_occurrences(value, output),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            if let Some(value) = value {
                collect_expr_occurrences(value, output);
            }
        }
        Statement::Assignment { target, value, .. } => {
            collect_assignment_target_occurrences(target, output);
            collect_expr_occurrences(value, output);
        }
        Statement::Continue { .. } => {}
    }
}

fn collect_assignment_target_occurrences(
    target: &orna_syntax_v1::AssignmentTarget,
    output: &mut Vec<LocalOccurrence>,
) {
    match target {
        orna_syntax_v1::AssignmentTarget::Name { name, span } => {
            output.push(LocalOccurrence {
                key: normalize(name),
                span: span.clone(),
            });
        }
        orna_syntax_v1::AssignmentTarget::Field { base, .. } => {
            collect_assignment_target_occurrences(base, output);
        }
        orna_syntax_v1::AssignmentTarget::Index { base, index, .. } => {
            collect_assignment_target_occurrences(base, output);
            collect_expr_occurrences(index, output);
        }
    }
}
