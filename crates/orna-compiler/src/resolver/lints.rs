use orna_syntax::{ClientIfStatement, ClientProceduralStatement, SourceSpan};

use super::{CompilerDiagnostic, DiagnosticCode, ParseReport, diagnostic, location};

/// Collects non-blocking diagnostics after semantic checking succeeds.
pub(super) fn collect_warnings(parse_report: &ParseReport) -> Vec<CompilerDiagnostic> {
    let mut warnings = Vec::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().client_functions() {
            let Some(statements) = declaration.body.procedural_statements() else {
                continue;
            };
            collect_sequence_warnings(unit.logical_path(), statements, &mut warnings);
        }
    }
    warnings
}

fn collect_sequence_warnings(
    logical_path: &str,
    statements: &[ClientProceduralStatement],
    warnings: &mut Vec<CompilerDiagnostic>,
) -> bool {
    let mut return_cause = None;
    let mut first_unreachable = None;

    for (index, statement) in statements.iter().enumerate() {
        if return_cause.is_some() {
            first_unreachable.get_or_insert(index);
            continue;
        }

        let returns = collect_statement_warnings(logical_path, statement, warnings);
        if returns {
            return_cause = Some(ReturnCause {
                span: statement_span(statement).clone(),
                label: match statement {
                    ClientProceduralStatement::Return(_) => {
                        "this statement returns from the function"
                    }
                    ClientProceduralStatement::If(_) => {
                        "every branch of this statement returns from the function"
                    }
                    ClientProceduralStatement::Let(_)
                    | ClientProceduralStatement::Assignment(_)
                    | ClientProceduralStatement::While(_) => {
                        unreachable!("only RETURN and IF can guarantee a return")
                    }
                },
            });
        }
    }

    if let (Some(first_unreachable), Some(cause)) = (first_unreachable, return_cause.as_ref()) {
        let unreachable = SourceSpan {
            start: statement_span(&statements[first_unreachable]).start,
            end: statement_span(
                statements
                    .last()
                    .expect("an unreachable statement implies a nonempty sequence"),
            )
            .end,
        };
        let count = statements.len() - first_unreachable;
        let message = if count == 1 {
            "unreachable statement".to_owned()
        } else {
            format!("{count} unreachable statements")
        };
        warnings.push(
            diagnostic(
                DiagnosticCode::UnreachableCode,
                message,
                logical_path,
                &unreachable,
            )
            .with_primary_label("unreachable code")
            .with_note("unreachable statements are still checked but can never execute")
            .with_related(location(logical_path, &cause.span), cause.label),
        );
    }

    return_cause.is_some()
}

fn collect_statement_warnings(
    logical_path: &str,
    statement: &ClientProceduralStatement,
    warnings: &mut Vec<CompilerDiagnostic>,
) -> bool {
    match statement {
        ClientProceduralStatement::Return(_) => true,
        ClientProceduralStatement::If(statement) => {
            collect_if_warnings(logical_path, statement, warnings)
        }
        ClientProceduralStatement::While(statement) => {
            collect_sequence_warnings(logical_path, statement.body(), warnings);
            false
        }
        ClientProceduralStatement::Let(_) | ClientProceduralStatement::Assignment(_) => false,
    }
}

fn collect_if_warnings(
    logical_path: &str,
    statement: &ClientIfStatement,
    warnings: &mut Vec<CompilerDiagnostic>,
) -> bool {
    let then_returns =
        collect_sequence_warnings(logical_path, statement.then_statements(), warnings);
    let elsif_return = statement
        .elsif_branches()
        .iter()
        .all(|branch| collect_sequence_warnings(logical_path, branch.statements(), warnings));
    let else_returns = statement
        .else_statements()
        .is_some_and(|statements| collect_sequence_warnings(logical_path, statements, warnings));
    then_returns && elsif_return && else_returns
}

fn statement_span(statement: &ClientProceduralStatement) -> &SourceSpan {
    match statement {
        ClientProceduralStatement::Let(statement) => &statement.span,
        ClientProceduralStatement::Assignment(statement) => &statement.span,
        ClientProceduralStatement::Return(statement) => statement.span(),
        ClientProceduralStatement::If(statement) => statement.span(),
        ClientProceduralStatement::While(statement) => statement.span(),
    }
}

struct ReturnCause {
    span: SourceSpan,
    label: &'static str,
}
