//! Analysis helpers and diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation::{Diagnostic, DiagnosticSeverity, SafeText};
use orna_syntax::{Expr, Statement, StringSegment};
use orna_value::{Decimal, path_decode_key_components};
use unicode_normalization::is_nfc;

use super::*;

/// Reports whether an assertion with `dependencies` must be evaluated for a write
/// that changed `changed_tables`. An incomplete dependency set applies to every write.
pub fn assertion_dependencies_apply(
    dependencies: &BTreeSet<String>,
    changed_tables: &BTreeSet<String>,
) -> bool {
    dependencies.contains(UNRESOLVED_TABLE_DEPENDENCY)
        || dependencies
            .iter()
            .any(|table| changed_tables.contains(table))
}

/// Records the table identities named by a path expression. Returns true when the
/// path is itself a table reference, so nothing beneath it contributes further.
pub(crate) fn table_path_dependency(
    e: &Expr,
    path: &[&str],
    names: &mut BTreeSet<String>,
    resolved_tables: Option<&BTreeMap<String, Type>>,
    scope: Option<&Scope>,
    local_names: &BTreeSet<String>,
) -> bool {
    if let Some(scope) = scope
        && !local_names.contains(path[0])
        && let Some(identity) = table_identity(e, scope, &BTreeMap::new())
        && resolved_tables.is_none_or(|tables| tables.contains_key(&identity))
    {
        names.insert(identity);
        return true;
    }
    let joined = path.join(".");
    if resolved_tables.is_some_and(|tables| tables.contains_key(&joined)) {
        names.insert(joined);
        return true;
    }
    if resolved_tables.is_none()
        && let Expr::Name { text, .. } = e
        && text.chars().next().is_some_and(char::is_uppercase)
    {
        names.insert(text.clone());
    }
    false
}

/// Adds the table reads of a helper used as a callee or function value. A helper
/// without a computed summary cannot be analysed, so the set becomes incomplete.
pub(crate) fn helper_dependencies(summary_key: &str, names: &mut BTreeSet<String>, scope: &Scope) {
    match scope.function_dependencies.borrow().get(summary_key) {
        Some(dependencies) => names.extend(dependencies.iter().cloned()),
        None => {
            names.insert(UNRESOLVED_TABLE_DEPENDENCY.to_owned());
        }
    }
}

pub(crate) fn tables_referenced(
    expr: &Expr,
    resolved_tables: Option<&BTreeMap<String, Type>>,
    scope: Option<&Scope>,
    initial_local_names: Option<&BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let initial_local_names = initial_local_names.cloned().unwrap_or_default();
    fn visit(
        e: &Expr,
        names: &mut BTreeSet<String>,
        resolved_tables: Option<&BTreeMap<String, Type>>,
        scope: Option<&Scope>,
        local_names: &BTreeSet<String>,
    ) {
        // Dependency extraction follows resolved table identities and also
        // expands summaries for called pure helpers. The latter keeps
        // assertion planning tied to the resolver rather than source spelling.
        // A table reached through a module binding, a full namespace path or
        // an import contributes its qualified identity, so same-named tables
        // in different modules stay distinct dependencies.
        if let Some(path) = qualified_path(e) {
            if table_path_dependency(e, &path, names, resolved_tables, scope, local_names) {
                return;
            }
            // A helper reached as a callee or a function value contributes its summary.
            // Without a summary it cannot be analysed, so the set becomes incomplete.
            if let Some(scope) = scope
                && let Some(summary_key) = resolved_helper_summary_key(&path, local_names, scope)
            {
                helper_dependencies(&summary_key, names, scope);
            }
        }
        match e {
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let StringSegment::Expression { value, .. } = segment {
                        visit(value, names, resolved_tables, scope, local_names);
                    }
                }
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
                visit(rhs, names, resolved_tables, scope, local_names)
            }
            Expr::Binary { lhs, rhs, .. } => {
                visit(lhs, names, resolved_tables, scope, local_names);
                visit(rhs, names, resolved_tables, scope, local_names);
            }
            Expr::Call {
                callee, arguments, ..
            }
            | Expr::GenericCall {
                callee, arguments, ..
            } => {
                visit(callee, names, resolved_tables, scope, local_names);
                for argument in arguments {
                    visit(&argument.value, names, resolved_tables, scope, local_names);
                }
            }
            Expr::Field { base, .. } => visit(base, names, resolved_tables, scope, local_names),
            Expr::Index { base, index, .. } => {
                visit(base, names, resolved_tables, scope, local_names);
                visit(index, names, resolved_tables, scope, local_names);
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    visit(element, names, resolved_tables, scope, local_names);
                }
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                for field in fields {
                    visit(&field.value, names, resolved_tables, scope, local_names);
                }
            }
            Expr::Lambda {
                parameters, body, ..
            } => {
                let mut lambda_locals = local_names.clone();
                for parameter in parameters {
                    lambda_locals.extend(pattern_binding_names(&parameter.pattern));
                }
                visit(body, names, resolved_tables, scope, &lambda_locals);
            }
            Expr::Block {
                statements, tail, ..
            } => {
                let mut block_locals = local_names.clone();
                let mut terminated = false;
                for statement in statements {
                    match statement {
                        Statement::Let { pattern, value, .. } => {
                            visit(value, names, resolved_tables, scope, &block_locals);
                            block_locals.extend(pattern_binding_names(pattern));
                        }
                        Statement::Assert { value, .. }
                        | Statement::Expression { value, .. }
                        | Statement::Control { value, .. }
                        | Statement::Assignment { value, .. } => {
                            visit(value, names, resolved_tables, scope, &block_locals)
                        }
                        Statement::Return { value, .. } => {
                            if let Some(value) = value {
                                visit(value, names, resolved_tables, scope, &block_locals);
                            }
                            terminated = true;
                            break;
                        }
                        Statement::Break { value, .. } => {
                            if let Some(value) = value {
                                visit(value, names, resolved_tables, scope, &block_locals);
                            }
                        }
                        Statement::Continue { .. } => {}
                    }
                }
                if !terminated && let Some(tail) = tail {
                    visit(tail, names, resolved_tables, scope, &block_locals);
                }
            }
            Expr::Control {
                binding,
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                if let Some(condition) = condition {
                    visit(condition, names, resolved_tables, scope, local_names);
                }
                if let Some(body) = body {
                    let mut body_locals = local_names.clone();
                    if let Some(binding) = binding {
                        body_locals.extend(pattern_binding_names(binding));
                    }
                    visit(body, names, resolved_tables, scope, &body_locals);
                }
                if let Some(alternate) = alternate {
                    visit(alternate, names, resolved_tables, scope, local_names);
                }
                for arm in arms {
                    let mut arm_locals = local_names.clone();
                    arm_locals.extend(pattern_binding_names(&arm.pattern));
                    if let Some(guard) = &arm.guard {
                        visit(guard, names, resolved_tables, scope, &arm_locals);
                    }
                    visit(&arm.body, names, resolved_tables, scope, &arm_locals);
                }
            }
            // Names are resolved by the qualified-path check above; literals read no table.
            Expr::Name { .. } | Expr::Literal { .. } => {}
            // A REPL binding stands for a value captured outside this expression, so
            // its table reads cannot be enumerated here.
            Expr::ReplBinding { .. } => {
                names.insert(UNRESOLVED_TABLE_DEPENDENCY.to_owned());
            }
            Expr::Range { lower, upper, .. } => {
                for bound in [lower, upper].into_iter().flatten() {
                    visit(bound, names, resolved_tables, scope, local_names);
                }
            }
        }
    }
    visit(
        expr,
        &mut names,
        resolved_tables,
        scope,
        &initial_local_names,
    );
    names
}

pub(crate) fn valid_row_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 1024 && path.split('/').all(valid_row_path_component)
}

pub(crate) fn valid_row_path_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains('/')
        && !component.contains('\\')
        && is_nfc(component)
        && component.len() <= 200
}

pub(crate) fn valid_row_component(component: &str) -> bool {
    valid_row_path_component(component) && component.is_ascii()
}

pub(crate) fn decode_row_key_path(components: &[String]) -> Option<Vec<String>> {
    path_decode_key_components(components).ok()
}

pub(crate) fn row_key_component_types(
    admission: &TableAdmission,
    analysis: &Analysis,
) -> Option<Vec<Type>> {
    // Editable paths contain scalar key leaves in declaration order. Flatten
    // tuple and table-reference types recursively: a reference contributes
    // its target key path, never target-row data or a table identity segment.
    fn append(
        ty: &Type,
        analysis: &Analysis,
        visiting: &mut BTreeSet<String>,
        output: &mut Vec<Type>,
    ) -> bool {
        match ty {
            Type::Tuple(elements) if !elements.is_empty() => elements
                .iter()
                .all(|element| append(element, analysis, visiting, output)),
            Type::Tuple(_) => false,
            Type::Named(name) if matches!(name.as_str(), "Uuid" | "UUID" | "std.UUID") => {
                output.push(ty.clone());
                true
            }
            Type::Named(name) => {
                let Some(symbol) = analysis_named_symbol(name, analysis) else {
                    output.push(ty.clone());
                    return true;
                };
                if symbol.kind != SymbolKind::Table {
                    output.push(ty.clone());
                    return true;
                }
                if !visiting.insert(name.clone()) {
                    return false;
                }
                let Some(referenced_keys) = symbol
                    .table_schema
                    .as_ref()
                    .and_then(|schema| schema.admission.as_ref())
                    .map(|schema| schema.keys.clone())
                    .filter(|keys| !keys.is_empty())
                else {
                    visiting.remove(name);
                    return false;
                };
                let valid = referenced_keys
                    .iter()
                    .all(|(_, key_type)| append(key_type, analysis, visiting, output));
                visiting.remove(name);
                valid
            }
            _ => {
                output.push(ty.clone());
                true
            }
        }
    }

    let mut output = Vec::new();
    let mut visiting = BTreeSet::new();
    for (_, ty) in &admission.keys {
        if !append(ty, analysis, &mut visiting, &mut output) {
            return None;
        }
    }
    Some(output)
}

pub(crate) fn analysis_named_symbol<'a>(name: &str, analysis: &'a Analysis) -> Option<&'a Symbol> {
    let mut exact = Vec::new();
    let mut qualified = Vec::new();
    for (namespace, module) in &analysis.modules {
        for (symbol_name, symbol) in &module.symbols {
            if symbol.ty == Type::Named(name.to_owned()) {
                exact.push(symbol);
            }
            let full_name = if namespace.0.is_empty() {
                symbol_name.clone()
            } else {
                format!("{}.{symbol_name}", namespace.0.join("."))
            };
            if full_name == name {
                qualified.push(symbol);
            }
        }
    }
    let matches = if qualified.is_empty() {
        exact
    } else {
        qualified
    };
    (matches.len() == 1).then(|| matches[0])
}

pub(crate) fn row_key_matches_type(value: &str, ty: &Type, analysis: &Analysis) -> bool {
    match ty {
        Type::Text => true,
        Type::Int => canonical_integer(value),
        Type::Decimal => Decimal::from_canonical_text(value).is_ok(),
        Type::Bool => matches!(value, "true" | "false"),
        Type::Date => canonical_date(value),
        Type::Instant => canonical_instant(value),
        Type::Named(name) => {
            if matches!(name.as_str(), "Uuid" | "UUID" | "std.UUID") {
                canonical_uuid(value)
            } else {
                analysis_named_symbol(name, analysis).is_some_and(|symbol| {
                    symbol.kind == SymbolKind::Enum && symbol.enum_variants.contains(value)
                })
            }
        }
        _ => false,
    }
}

pub(crate) fn canonical_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if value.starts_with('-') && digits == "0" {
        return false;
    }
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    digits == "0" || !digits.starts_with('0')
}

pub(crate) fn canonical_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[..4].parse::<u32>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=9999).contains(&year) && (1..=max_day).contains(&day)
}

pub(crate) fn canonical_instant(value: &str) -> bool {
    // Row paths carry one stable UTC spelling. Offsets are accepted in source
    // literals but normalize before storage, so they are not alternate keys.
    let Some(body) = value.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = body.split_once('T') else {
        return false;
    };
    if !canonical_date(date) {
        return false;
    }
    let (clock, fraction) = time
        .split_once('.')
        .map_or((time, None), |(clock, fraction)| (clock, Some(fraction)));
    let clock_bytes = clock.as_bytes();
    if clock_bytes.len() != 8
        || clock_bytes[2] != b':'
        || clock_bytes[5] != b':'
        || !clock_bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 5) || byte.is_ascii_digit())
    {
        return false;
    }
    let hour = clock[..2].parse::<u32>().ok();
    let minute = clock[3..5].parse::<u32>().ok();
    let second = clock[6..8].parse::<u32>().ok();
    if !matches!((hour, minute, second), (Some(h), Some(m), Some(s)) if h < 24 && m < 60 && s < 60)
    {
        return false;
    }
    match fraction {
        None => true,
        Some(fraction) => {
            !fraction.is_empty()
                && fraction.len() <= 9
                && fraction.bytes().all(|byte| byte.is_ascii_digit())
                && !fraction.ends_with('0')
        }
    }
}

pub(crate) fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                .then_some(byte == b'-')
                .unwrap_or(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(crate) fn diag(code: &'static str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        SafeText::new(code).expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(message.into()).expect("valid diagnostic message"),
    )
    .expect("valid diagnostic")
}
