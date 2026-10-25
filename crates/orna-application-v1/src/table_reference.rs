//! Static table reachability for admitted entries (ornadb-03ol).
//!
//! An admitted entry reads or writes a table only through a name that resolves
//! to that table. This walker collects every identifier in an expression,
//! pattern, type, or assignment target of the entry and of each admitted
//! function it names, and reports whether any of them is a table name. The
//! check over-approximates: a name that merely shares a table's spelling counts
//! as a reference, so a table read is never missed, and an entry whose
//! identifiers never match a table is safe to evaluate without table state.

use std::collections::BTreeSet;

use orna_evaluator_v1::Functions;
use orna_syntax_v1::{
    Argument, AssignmentTarget, CaseArm, Expr, LambdaParameter, NameSegment, Pattern, RecordField,
    Statement, StringSegment, TypeExpr,
};

/// Reports whether `entry`, or any admitted function it reaches by name,
/// mentions one of `tables`. An entry the admitted functions do not define
/// reports `true`, so the caller takes the table-aware path and fails closed.
pub(crate) fn entry_references_tables(
    functions: &Functions,
    entry: &str,
    tables: &BTreeSet<&str>,
) -> bool {
    if functions.get_entry(entry).is_none() {
        return true;
    }
    let mut pending = vec![entry];
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !visited.insert(name) {
            continue;
        }
        let Some(function) = functions.get_entry(name) else {
            continue;
        };
        let mut identifiers = BTreeSet::new();
        expr_identifiers(&function.body, &mut identifiers);
        if identifiers
            .iter()
            .any(|identifier| tables.contains(identifier))
        {
            return true;
        }
        for key in functions.visible_names() {
            if !visited.contains(key)
                && identifiers
                    .iter()
                    .any(|identifier| names_function(key, identifier))
            {
                pending.push(key);
            }
        }
    }
    false
}

/// An admitted function key is named by its full key or, for an alias key
/// `namespace::name`, by its final segment.
fn names_function(key: &str, identifier: &str) -> bool {
    key == identifier || key.rsplit("::").next() == Some(identifier)
}

/// Inserts `text` and each `::`-separated segment, so a qualified reference
/// matches both a full alias key and a bare table name.
fn insert_segments<'a>(text: &'a str, out: &mut BTreeSet<&'a str>) {
    out.insert(text);
    out.extend(text.split("::"));
}

fn optional_expr<'a>(expr: Option<&'a Expr>, out: &mut BTreeSet<&'a str>) {
    if let Some(expr) = expr {
        expr_identifiers(expr, out);
    }
}

fn expr_identifiers<'a>(expr: &'a Expr, out: &mut BTreeSet<&'a str>) {
    match expr {
        Expr::Name { text, .. } | Expr::ReplBinding { text, .. } => insert_segments(text, out),
        Expr::Literal { .. } => {}
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let StringSegment::Expression { value, .. } = segment {
                    expr_identifiers(value, out);
                }
            }
        }
        Expr::Unary { rhs, .. } => expr_identifiers(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            expr_identifiers(lhs, out);
            expr_identifiers(rhs, out);
        }
        Expr::Range { lower, upper, .. } => {
            optional_expr(lower.as_deref(), out);
            optional_expr(upper.as_deref(), out);
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            expr_identifiers(callee, out);
            arguments_identifiers(arguments, out);
        }
        Expr::GenericCall {
            callee,
            type_arguments,
            arguments,
            ..
        } => {
            expr_identifiers(callee, out);
            for ty in type_arguments {
                type_identifiers(ty, out);
            }
            arguments_identifiers(arguments, out);
        }
        Expr::Index { base, index, .. } => {
            expr_identifiers(base, out);
            expr_identifiers(index, out);
        }
        Expr::Field { base, name, .. } => {
            expr_identifiers(base, out);
            insert_segments(name, out);
        }
        Expr::Group { inner, .. } => expr_identifiers(inner, out),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                expr_identifiers(element, out);
            }
        }
        Expr::Record { fields, .. } => record_identifiers(fields, out),
        Expr::Nominal { path, fields, .. } => {
            path_identifiers(path, out);
            record_identifiers(fields, out);
        }
        Expr::Lambda {
            parameters, body, ..
        } => {
            for parameter in parameters {
                lambda_parameter_identifiers(parameter, out);
            }
            expr_identifiers(body, out);
        }
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                statement_identifiers(statement, out);
            }
            optional_expr(tail.as_deref(), out);
        }
        Expr::Control {
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(pattern) = binding {
                pattern_identifiers(pattern, out);
            }
            optional_expr(condition.as_deref(), out);
            optional_expr(body.as_deref(), out);
            for arm in arms {
                case_arm_identifiers(arm, out);
            }
            optional_expr(alternate.as_deref(), out);
        }
    }
}

fn arguments_identifiers<'a>(arguments: &'a [Argument], out: &mut BTreeSet<&'a str>) {
    for argument in arguments {
        if let Some(name) = &argument.name {
            out.insert(name.as_str());
        }
        expr_identifiers(&argument.value, out);
    }
}

fn record_identifiers<'a>(fields: &'a [RecordField], out: &mut BTreeSet<&'a str>) {
    for field in fields {
        out.insert(field.name.as_str());
        expr_identifiers(&field.value, out);
    }
}

fn path_identifiers<'a>(path: &'a [NameSegment], out: &mut BTreeSet<&'a str>) {
    for segment in path {
        insert_segments(&segment.text, out);
    }
}

fn lambda_parameter_identifiers<'a>(parameter: &'a LambdaParameter, out: &mut BTreeSet<&'a str>) {
    pattern_identifiers(&parameter.pattern, out);
    if let Some(annotation) = &parameter.annotation {
        type_identifiers(annotation, out);
    }
}

fn case_arm_identifiers<'a>(arm: &'a CaseArm, out: &mut BTreeSet<&'a str>) {
    pattern_identifiers(&arm.pattern, out);
    optional_expr(arm.guard.as_ref(), out);
    expr_identifiers(&arm.body, out);
}

fn statement_identifiers<'a>(statement: &'a Statement, out: &mut BTreeSet<&'a str>) {
    match statement {
        Statement::Let {
            pattern,
            annotation,
            value,
            ..
        } => {
            pattern_identifiers(pattern, out);
            if let Some(annotation) = annotation {
                type_identifiers(annotation, out);
            }
            expr_identifiers(value, out);
        }
        Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => expr_identifiers(value, out),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            optional_expr(value.as_ref(), out);
        }
        Statement::Continue { .. } => {}
        Statement::Assignment { target, value, .. } => {
            assignment_target_identifiers(target, out);
            expr_identifiers(value, out);
        }
    }
}

fn assignment_target_identifiers<'a>(target: &'a AssignmentTarget, out: &mut BTreeSet<&'a str>) {
    match target {
        AssignmentTarget::Name { name, .. } => insert_segments(name, out),
        AssignmentTarget::Field { base, name, .. } => {
            assignment_target_identifiers(base, out);
            insert_segments(name, out);
        }
        AssignmentTarget::Index { base, index, .. } => {
            assignment_target_identifiers(base, out);
            expr_identifiers(index, out);
        }
    }
}

fn pattern_identifiers<'a>(pattern: &'a Pattern, out: &mut BTreeSet<&'a str>) {
    match pattern {
        Pattern::Name(name, _) => insert_segments(name, out),
        Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for element in elements {
                pattern_identifiers(element, out);
            }
        }
        Pattern::Record { fields, .. } => {
            for (name, field_pattern, _) in fields {
                out.insert(name.as_str());
                if let Some(field_pattern) = field_pattern {
                    pattern_identifiers(field_pattern, out);
                }
            }
        }
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } => {
            path_identifiers(path, out);
            for argument in arguments {
                pattern_identifiers(argument, out);
            }
            for field in fields {
                out.insert(field.name.as_str());
                if let Some(field_pattern) = &field.pattern {
                    pattern_identifiers(field_pattern, out);
                }
            }
        }
    }
}

fn type_identifiers<'a>(ty: &'a TypeExpr, out: &mut BTreeSet<&'a str>) {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } => {
            for segment in path {
                insert_segments(segment, out);
            }
            for argument in arguments {
                type_identifiers(argument, out);
            }
        }
        TypeExpr::Optional { inner, .. } | TypeExpr::List { inner, .. } => {
            type_identifiers(inner, out);
        }
        TypeExpr::Product { lhs, rhs, .. } => {
            type_identifiers(lhs, out);
            type_identifiers(rhs, out);
        }
        TypeExpr::Record { fields, .. } => {
            for (name, field_type, _) in fields {
                out.insert(name.as_str());
                type_identifiers(field_type, out);
            }
        }
        TypeExpr::Tuple { elements, .. } => {
            for element in elements {
                type_identifiers(element, out);
            }
        }
        TypeExpr::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                type_identifiers(parameter, out);
            }
            type_identifiers(result, out);
        }
    }
}
