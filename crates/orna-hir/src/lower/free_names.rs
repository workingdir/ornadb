//! Free names of a lambda body, for slot capture (design section 2.4, D10).
//!
//! A closure shares the slot of a captured variable, so its body needs one
//! inner slot per free name it reads from an enclosing body, plus one
//! [`crate::Capture`] edge naming the outer slot. This module finds those
//! names by a scoped walk of the syntax: a name is free when no lambda
//! parameter, `let` pattern, `case` arm pattern or `for` binding inside the
//! lambda binds it.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    AssignmentTarget, ControlKind, Expr, LambdaParameter, Pattern, Statement, SyntaxSpan,
};

/// The free names of `expr`, each with the first span it was written at.
pub(crate) fn free_names(expr: &Expr) -> BTreeMap<String, SyntaxSpan> {
    let mut found = BTreeMap::new();
    let mut bound = vec![BTreeSet::new()];
    walk(expr, &mut bound, &mut found);
    found
}

fn is_bound(bound: &[BTreeSet<String>], name: &str) -> bool {
    bound.iter().rev().any(|scope| scope.contains(name))
}

fn bind(bound: &mut [BTreeSet<String>], name: &str) {
    if let Some(scope) = bound.last_mut() {
        scope.insert(name.to_owned());
    }
}

fn note(
    bound: &[BTreeSet<String>],
    found: &mut BTreeMap<String, SyntaxSpan>,
    name: &str,
    span: &SyntaxSpan,
) {
    if !is_bound(bound, name) {
        found.entry(name.to_owned()).or_insert_with(|| span.clone());
    }
}

/// Records the names one pattern binds.
fn bind_pattern(bound: &mut [BTreeSet<String>], pattern: &Pattern) {
    match pattern {
        Pattern::Name(name, _) => bind(bound, name),
        Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for element in elements {
                bind_pattern(bound, element);
            }
        }
        Pattern::Record { fields, .. } => {
            for (name, pattern, _) in fields {
                match pattern {
                    Some(pattern) => bind_pattern(bound, pattern),
                    None => bind(bound, name),
                }
            }
        }
        Pattern::Constructor {
            arguments, fields, ..
        } => {
            for argument in arguments {
                bind_pattern(bound, argument);
            }
            for field in fields {
                match &field.pattern {
                    Some(pattern) => bind_pattern(bound, pattern),
                    None => bind(bound, &field.name),
                }
            }
        }
    }
}

fn walk(expr: &Expr, bound: &mut Vec<BTreeSet<String>>, found: &mut BTreeMap<String, SyntaxSpan>) {
    match expr {
        Expr::Name { text, span } => note(bound, found, text, span),
        Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let orna_syntax::StringSegment::Expression { value, .. } = segment {
                    walk(value, bound, found);
                }
            }
        }
        Expr::Unary { rhs, .. } => walk(rhs, bound, found),
        Expr::Binary { lhs, rhs, .. } => {
            walk(lhs, bound, found);
            walk(rhs, bound, found);
        }
        Expr::Range { lower, upper, .. } => {
            for operand in [lower, upper].into_iter().flatten() {
                walk(operand, bound, found);
            }
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            walk(callee, bound, found);
            for argument in arguments {
                walk(&argument.value, bound, found);
            }
        }
        Expr::Index { base, index, .. } => {
            walk(base, bound, found);
            walk(index, bound, found);
        }
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => walk(base, bound, found),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                walk(element, bound, found);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                walk(&field.value, bound, found);
            }
        }
        Expr::Lambda {
            parameters, body, ..
        } => walk_lambda(parameters, body, bound, found),
        Expr::Block {
            statements, tail, ..
        } => walk_block(statements, tail.as_deref(), bound, found),
        Expr::Control {
            kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => walk_control(
            *kind,
            binding.as_ref(),
            condition.as_deref(),
            body.as_deref(),
            arms,
            alternate.as_deref(),
            bound,
            found,
        ),
    }
}

/// Records the free names of a lambda body.
fn walk_lambda(
    parameters: &[LambdaParameter],
    body: &Expr,
    bound: &mut Vec<BTreeSet<String>>,
    found: &mut BTreeMap<String, SyntaxSpan>,
) {
    bound.push(BTreeSet::new());
    for LambdaParameter { pattern, .. } in parameters {
        bind_pattern(bound, pattern);
    }
    walk(body, bound, found);
    bound.pop();
}

/// Records the free names of a block.
fn walk_block(
    statements: &[Statement],
    tail: Option<&Expr>,
    bound: &mut Vec<BTreeSet<String>>,
    found: &mut BTreeMap<String, SyntaxSpan>,
) {
    bound.push(BTreeSet::new());
    for statement in statements {
        walk_statement(statement, bound, found);
    }
    if let Some(tail) = tail {
        walk(tail, bound, found);
    }
    bound.pop();
}

/// Records the free names of one statement.
fn walk_statement(
    statement: &Statement,
    bound: &mut Vec<BTreeSet<String>>,
    found: &mut BTreeMap<String, SyntaxSpan>,
) {
    match statement {
        Statement::Let { pattern, value, .. } => {
            walk(value, bound, found);
            bind_pattern(bound, pattern);
        }
        Statement::Assert { value, .. } | Statement::Expression { value, .. } => {
            walk(value, bound, found);
        }
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            if let Some(value) = value {
                walk(value, bound, found);
            }
        }
        Statement::Continue { .. } => {}
        Statement::Assignment { target, value, .. } => {
            walk(value, bound, found);
            walk_place(target, bound, found);
        }
        Statement::Control { value, .. } => walk(value, bound, found),
    }
}

/// Records the free names of one control expression.
#[allow(clippy::too_many_arguments)]
fn walk_control(
    kind: ControlKind,
    binding: Option<&Pattern>,
    condition: Option<&Expr>,
    body: Option<&Expr>,
    arms: &[orna_syntax::CaseArm],
    alternate: Option<&Expr>,
    bound: &mut Vec<BTreeSet<String>>,
    found: &mut BTreeMap<String, SyntaxSpan>,
) {
    if let Some(condition) = condition {
        walk(condition, bound, found);
    }
    match kind {
        ControlKind::If | ControlKind::While => {
            if let Some(body) = body {
                walk(body, bound, found);
            }
            if let Some(alternate) = alternate {
                walk(alternate, bound, found);
            }
        }
        ControlKind::Loop => {
            if let Some(body) = body {
                walk(body, bound, found);
            }
        }
        ControlKind::For => {
            bound.push(BTreeSet::new());
            if let Some(pattern) = binding {
                bind_pattern(bound, pattern);
            }
            if let Some(body) = body {
                walk(body, bound, found);
            }
            bound.pop();
        }
        ControlKind::Case => {
            for arm in arms {
                bound.push(BTreeSet::new());
                bind_pattern(bound, &arm.pattern);
                if let Some(guard) = &arm.guard {
                    walk(guard, bound, found);
                }
                walk(&arm.body, bound, found);
                bound.pop();
            }
        }
    }
}

/// Records the names an assignment target reads.
fn walk_place(
    target: &AssignmentTarget,
    bound: &mut Vec<BTreeSet<String>>,
    found: &mut BTreeMap<String, SyntaxSpan>,
) {
    match target {
        AssignmentTarget::Name { .. } => {}
        AssignmentTarget::Field { base, .. } => walk_place(base, bound, found),
        AssignmentTarget::Index { base, index, .. } => {
            walk_place(base, bound, found);
            walk(index, bound, found);
        }
    }
}
