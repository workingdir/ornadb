//! Editor analysis built exclusively on the frozen Orna 1.0 syntax tree.
#![allow(deprecated)] // lsp-types 0.97 still requires DocumentSymbol::deprecated.

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, Diagnostic,
    DiagnosticRelatedInformation, DiagnosticSeverity, DocumentSymbol, Hover, Location,
    NumberOrString, ParameterInformation, ParameterLabel, Position, SignatureHelp,
    SignatureInformation, SymbolKind,
};
use orna_syntax_v1::{
    Argument, Declaration, Expr, Item, Keyword, Parse, Pattern, Statement,
    SyntaxSpan as SourceSpan, SyntaxTree, Token, TokenKind, TypeExpr, lex, parse_module_with_file,
};

use crate::documents::{Document, PositionMapper};

pub type EditorParse = Parse<SyntaxTree>;

/// Syntax diagnostics emitted by the 1.0 parser with their original byte spans.
pub fn check_document(document: &Document, mapper: &PositionMapper<'_>) -> Vec<Diagnostic> {
    let parse = parse_document(document);
    parse
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let related = diagnostic
                .labels
                .iter()
                .filter(|label| !label.primary)
                .map(|label| DiagnosticRelatedInformation {
                    location: Location {
                        uri: document.uri.clone(),
                        range: mapper.range(&label.span),
                    },
                    message: label.message.clone().unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            Diagnostic {
                range: mapper.range(&diagnostic.span),
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String(diagnostic.code.to_owned())),
                code_description: None,
                source: Some("orna-syntax-v1".to_owned()),
                message: diagnostic.message.clone(),
                related_information: (!related.is_empty()).then_some(related),
                tags: None,
                data: Some(serde_json::json!({
                    "title": diagnostic.title,
                    "help": diagnostic.help,
                    "notes": diagnostic.notes,
                })),
            }
        })
        .collect()
}

pub fn parse_document(document: &Document) -> EditorParse {
    parse_module_with_file(&document.text, document.logical_path())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorSymbolKind {
    Function,
    Type,
    Enum,
    Table,
    Protocol,
    Other,
}

#[derive(Debug, Clone)]
pub(crate) struct EditorSymbol {
    pub name: String,
    pub kind: EditorSymbolKind,
    pub selection: SourceSpan,
    pub full: SourceSpan,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub parameters: Vec<String>,
}

pub(crate) fn declaration_symbols(parse: &EditorParse, text: &str) -> Vec<EditorSymbol> {
    let Ok(tokens) = lex(text) else {
        return Vec::new();
    };
    let mut symbols = parse
        .value
        .items
        .iter()
        .filter_map(|item| symbol_for_item(item, text, &tokens))
        .collect::<Vec<_>>();
    symbols.sort_by_key(|symbol| symbol.selection.start);
    symbols
}

fn symbol_for_item(item: &Item, text: &str, tokens: &[Token]) -> Option<EditorSymbol> {
    let (name, kind, selection_scope, detail, parameters) = match &item.declaration {
        Declaration::Function { signature, .. } => (
            signature.name.as_str(),
            EditorSymbolKind::Function,
            &signature.span,
            Some(source_slice(text, &signature.span).to_owned()),
            signature
                .parameters
                .iter()
                .map(|parameter| source_slice(text, &parameter.span).to_owned())
                .collect(),
        ),
        Declaration::Table { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Table,
            &item.span,
            None,
            Vec::new(),
        ),
        Declaration::Protocol { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Protocol,
            &item.span,
            None,
            Vec::new(),
        ),
        Declaration::Enum { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Enum,
            &item.span,
            None,
            Vec::new(),
        ),
        Declaration::Type { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Type,
            &item.span,
            None,
            Vec::new(),
        ),
        Declaration::Dimension { name, .. } | Declaration::Unit { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Other,
            &item.span,
            None,
            Vec::new(),
        ),
        _ => return None,
    };
    let selection = tokens
        .iter()
        .find(|token| {
            token.span.start >= selection_scope.start
                && token.span.end <= selection_scope.end
                && token.text == name
                && matches!(token.kind, TokenKind::Identifier { .. })
        })?
        .span
        .clone();
    Some(EditorSymbol {
        name: name.to_owned(),
        kind,
        selection,
        full: item.span.clone(),
        detail,
        documentation: leading_doc_comment(text, item.span.start),
        parameters,
    })
}

fn leading_doc_comment(text: &str, before: usize) -> Option<String> {
    let prefix = text.get(..before)?;
    let mut lines = Vec::new();
    for line in prefix.lines().rev() {
        let trimmed = line.trim_start();
        if let Some(documentation) = trimmed.strip_prefix("///") {
            lines.push(documentation.strip_prefix(' ').unwrap_or(documentation));
        } else if trimmed.is_empty() && lines.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if lines.is_empty() {
        None
    } else {
        lines.reverse();
        Some(lines.join("\n"))
    }
}

fn source_slice<'a>(text: &'a str, span: &SourceSpan) -> &'a str {
    text.get(span.start..span.end).unwrap_or("")
}

fn normalized_identifier(text: &str) -> String {
    lex(text)
        .ok()
        .and_then(|tokens| {
            tokens.into_iter().find_map(|token| match token.kind {
                TokenKind::Identifier { normalized } => Some(normalized),
                _ => None,
            })
        })
        .unwrap_or_else(|| text.to_owned())
}

fn token_at(text: &str, byte: usize) -> Option<Token> {
    lex(text).ok()?.into_iter().find(|token| {
        matches!(token.kind, TokenKind::Identifier { .. })
            && token.span.start <= byte
            && byte < token.span.end
    })
}

fn symbol_for_name<'a>(symbols: &'a [EditorSymbol], name: &str) -> Option<&'a EditorSymbol> {
    let key = normalized_identifier(name);
    let mut matches = symbols
        .iter()
        .filter(|symbol| normalized_identifier(&symbol.name) == key);
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

fn selected_symbol_owned(parse: &EditorParse, text: &str, byte: usize) -> Option<EditorSymbol> {
    let token = token_at(text, byte)?;
    let symbols = declaration_symbols(parse, text);
    symbol_for_name(&symbols, &token.text).cloned()
}

pub fn hover(
    document: &Document,
    parse: &EditorParse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Hover> {
    let byte = mapper.byte_offset(position);
    let token = token_at(&document.text, byte)?;
    let Some(symbol) = selected_symbol_owned(parse, &document.text, byte) else {
        return parameter_hover(parse, &document.text, byte);
    };
    let kind = match symbol.kind {
        EditorSymbolKind::Function => "function",
        EditorSymbolKind::Type => "type",
        EditorSymbolKind::Enum => "enum",
        EditorSymbolKind::Table => "table",
        EditorSymbolKind::Protocol => "protocol",
        EditorSymbolKind::Other => "declaration",
    };
    let _ = token;
    Some(crate::hover::declaration(
        kind,
        &symbol.name,
        symbol.detail.as_deref(),
        &symbol.parameters,
        symbol.documentation.as_deref(),
    ))
}

fn parameter_hover(parse: &EditorParse, text: &str, byte: usize) -> Option<Hover> {
    for item in &parse.value.items {
        let Declaration::Function { signature, .. } = &item.declaration else {
            continue;
        };
        for parameter in &signature.parameters {
            let Pattern::Name(name, span) = &parameter.pattern else {
                continue;
            };
            if span.start <= byte && byte < span.end {
                return Some(crate::hover::declaration(
                    "parameter",
                    name,
                    Some(source_slice(text, &parameter.span)),
                    &[],
                    leading_doc_comment(text, parameter.span.start).as_deref(),
                ));
            }
        }
    }
    None
}

pub fn signature_help(
    document: &Document,
    parse: &EditorParse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<SignatureHelp> {
    let byte = mapper.byte_offset(position);
    let mut calls = Vec::new();
    for item in &parse.value.items {
        if let Declaration::Function { body, .. } = &item.declaration {
            collect_calls(body, &mut calls);
        }
    }
    let call = calls
        .into_iter()
        .filter(|call| call.span.start <= byte && byte <= call.span.end)
        .min_by_key(|call| call.span.end.saturating_sub(call.span.start))?;
    let symbols = declaration_symbols(parse, &document.text);
    let function = symbol_for_name(&symbols, call.name.rsplit('.').next().unwrap_or(&call.name))?;
    if function.kind != EditorSymbolKind::Function {
        return None;
    }
    let active_parameter = call
        .arguments
        .iter()
        .take_while(|argument| argument.span.start < byte)
        .count()
        .min(function.parameters.len().saturating_sub(1)) as u32;
    let signature = function
        .detail
        .clone()
        .unwrap_or_else(|| function.name.clone());
    let parameters = function
        .parameters
        .iter()
        .cloned()
        .map(|label| ParameterInformation {
            label: ParameterLabel::Simple(label),
            documentation: None,
        })
        .collect();
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature,
            documentation: function.documentation.clone().map(|text| {
                lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                    kind: lsp_types::MarkupKind::Markdown,
                    value: text,
                })
            }),
            parameters: Some(parameters),
            active_parameter: Some(active_parameter),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

#[derive(Debug)]
struct CallSite {
    name: String,
    span: SourceSpan,
    arguments: Vec<Argument>,
}

fn collect_calls(expression: &Expr, output: &mut Vec<CallSite>) {
    match expression {
        Expr::Call {
            callee,
            arguments,
            span,
        } => {
            if let Some(name) = expression_name(callee) {
                output.push(CallSite {
                    name,
                    span: span.clone(),
                    arguments: arguments.clone(),
                });
            }
            collect_calls(callee, output);
            for argument in arguments {
                collect_calls(&argument.value, output);
            }
        }
        Expr::GenericCall {
            callee,
            arguments,
            span,
            ..
        } => {
            if let Some(name) = expression_name(callee) {
                output.push(CallSite {
                    name,
                    span: span.clone(),
                    arguments: arguments.clone(),
                });
            }
            collect_calls(callee, output);
            for argument in arguments {
                collect_calls(&argument.value, output);
            }
        }
        Expr::Unary { rhs, .. } => collect_calls(rhs, output),
        Expr::Binary { lhs, rhs, .. } => {
            collect_calls(lhs, output);
            collect_calls(rhs, output);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(value) = lower {
                collect_calls(value, output);
            }
            if let Some(value) = upper {
                collect_calls(value, output);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_calls(base, output);
            collect_calls(index, output);
        }
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => collect_calls(base, output),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for value in elements {
                collect_calls(value, output);
            }
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                collect_calls(&field.value, output);
            }
        }
        Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_calls(&field.value, output);
            }
        }
        Expr::Lambda { body, .. } => collect_calls(body, output),
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                collect_statement_calls(statement, output);
            }
            if let Some(tail) = tail {
                collect_calls(tail, output);
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
                collect_calls(condition, output);
            }
            if let Some(body) = body {
                collect_calls(body, output);
            }
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_calls(guard, output);
                }
                collect_calls(&arm.body, output);
            }
            if let Some(alternate) = alternate {
                collect_calls(alternate, output);
            }
        }
        Expr::Name { .. }
        | Expr::Literal { .. }
        | Expr::InterpolatedString { .. }
        | Expr::ReplBinding { .. } => {}
    }
}

fn collect_statement_calls(statement: &Statement, output: &mut Vec<CallSite>) {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => collect_calls(value, output),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            if let Some(value) = value {
                collect_calls(value, output);
            }
        }
        Statement::Assignment { value, .. } => collect_calls(value, output),
        Statement::Continue { .. } => {}
    }
}

fn expression_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", expression_name(base)?, name)),
        _ => None,
    }
}

pub fn definition(
    document: &Document,
    parse: &EditorParse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Location> {
    let byte = mapper.byte_offset(position);
    let token = token_at(&document.text, byte)?;
    if let Some(binding) = crate::locals::binding_at(&parse.value, &token.text, &token.span) {
        return Some(Location {
            uri: document.uri.clone(),
            range: mapper.range(&binding.selection),
        });
    }
    let symbols = declaration_symbols(parse, &document.text);
    let symbol = symbol_for_name(&symbols, &token.text)?;
    Some(Location {
        uri: document.uri.clone(),
        range: mapper.range(&symbol.selection),
    })
}

pub fn references(
    document: &Document,
    parse: &EditorParse,
    position: Position,
    mapper: &PositionMapper<'_>,
    include_declaration: bool,
) -> Vec<Location> {
    let byte = mapper.byte_offset(position);
    let Some(token) = token_at(&document.text, byte) else {
        return Vec::new();
    };
    if let Some(binding) = crate::locals::binding_at(&parse.value, &token.text, &token.span) {
        let mut spans = crate::locals::references(&parse.value, &binding);
        if include_declaration {
            spans.push(binding.selection);
        }
        spans.sort_by_key(|span| (span.start, span.end));
        spans.dedup_by_key(|span| (span.start, span.end));
        return spans
            .into_iter()
            .map(|span| Location {
                uri: document.uri.clone(),
                range: mapper.range(&span),
            })
            .collect();
    }
    let symbols = declaration_symbols(parse, &document.text);
    let Some(symbol) = symbol_for_name(&symbols, &token.text) else {
        return Vec::new();
    };
    let name = normalized_identifier(&symbol.name);
    let local_references = crate::locals::resolved_reference_spans(&parse.value);
    let mut spans = reference_occurrences(&parse.value, &document.text)
        .into_iter()
        .filter(|(occurrence, span)| {
            normalized_identifier(occurrence) == name
                && !local_references.contains(&(span.start, span.end))
        })
        .map(|(_, span)| span)
        .collect::<Vec<_>>();
    if include_declaration {
        spans.push(symbol.selection.clone());
    }
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    spans
        .into_iter()
        .map(|span| Location {
            uri: document.uri.clone(),
            range: mapper.range(&span),
        })
        .collect()
}

pub(crate) fn local_declaration_spans(parse: &EditorParse) -> Vec<SourceSpan> {
    crate::locals::all_bindings(&parse.value)
        .into_iter()
        .map(|binding| binding.selection)
        .collect()
}

pub fn document_symbols(
    parse: &EditorParse,
    text: &str,
    mapper: &PositionMapper<'_>,
) -> Vec<DocumentSymbol> {
    declaration_symbols(parse, text)
        .into_iter()
        .map(|symbol| DocumentSymbol {
            name: symbol.name,
            detail: symbol.detail,
            kind: match symbol.kind {
                EditorSymbolKind::Function => SymbolKind::FUNCTION,
                EditorSymbolKind::Type => SymbolKind::STRUCT,
                EditorSymbolKind::Enum => SymbolKind::ENUM,
                EditorSymbolKind::Table => SymbolKind::CLASS,
                EditorSymbolKind::Protocol => SymbolKind::INTERFACE,
                EditorSymbolKind::Other => SymbolKind::NAMESPACE,
            },
            tags: None,
            deprecated: None,
            range: mapper.range(&symbol.full),
            selection_range: mapper.range(&symbol.selection),
            children: None,
        })
        .collect()
}

pub fn completion_at(
    parse: &EditorParse,
    text: &str,
    _byte: Option<usize>,
    _context: Option<&CompletionContext>,
) -> Vec<CompletionItem> {
    let mut completions = Keyword::ALL
        .iter()
        .map(|keyword| CompletionItem {
            label: keyword.spelling().to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Orna 1.0 keyword".to_owned()),
            sort_text: Some(format!("0-{}", keyword.spelling())),
            ..CompletionItem::default()
        })
        .collect::<Vec<_>>();
    for symbol in declaration_symbols(parse, text) {
        let kind = match symbol.kind {
            EditorSymbolKind::Function => CompletionItemKind::FUNCTION,
            EditorSymbolKind::Type => CompletionItemKind::STRUCT,
            EditorSymbolKind::Enum => CompletionItemKind::ENUM,
            EditorSymbolKind::Table => CompletionItemKind::CLASS,
            EditorSymbolKind::Protocol => CompletionItemKind::INTERFACE,
            EditorSymbolKind::Other => CompletionItemKind::REFERENCE,
        };
        let insert_text = if symbol.kind == EditorSymbolKind::Function {
            let placeholders = symbol
                .parameters
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    format!(
                        "${{{}:{}}}",
                        index + 1,
                        parameter_name(parameter).unwrap_or("arg")
                    )
                })
                .collect::<Vec<_>>();
            format!("{}({})", symbol.name, placeholders.join(", "))
        } else {
            symbol.name.clone()
        };
        let sort_text = format!("1-{}", symbol.name);
        completions.push(CompletionItem {
            label: symbol.name,
            kind: Some(kind),
            detail: symbol.detail,
            documentation: symbol.documentation.map(lsp_types::Documentation::String),
            insert_text: Some(insert_text),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            sort_text: Some(sort_text),
            ..CompletionItem::default()
        });
    }
    completions
}

fn parameter_name(source: &str) -> Option<&str> {
    let name = source.split(':').next()?.split('=').next()?.trim();
    (!name.is_empty()).then_some(name)
}

pub(crate) fn reference_occurrences(tree: &SyntaxTree, text: &str) -> Vec<(String, SourceSpan)> {
    let mut output = Vec::new();
    for item in &tree.items {
        match &item.declaration {
            Declaration::Function { signature, body } => {
                for parameter in &signature.parameters {
                    if let Some(ty) = &parameter.annotation {
                        collect_type_refs(ty, text, &mut output);
                    }
                }
                if let Some(result) = &signature.result {
                    collect_type_refs(result, text, &mut output);
                }
                collect_expr_refs(body, text, &mut output);
            }
            Declaration::Let {
                annotation, value, ..
            } => {
                if let Some(ty) = annotation {
                    collect_type_refs(ty, text, &mut output);
                }
                collect_expr_refs(value, text, &mut output);
            }
            Declaration::Assertion { value } => collect_expr_refs(value, text, &mut output),
            Declaration::Table { members, .. } => {
                for member in members {
                    if let orna_syntax_v1::TableMember::Field {
                        ty, initializer, ..
                    } = member
                    {
                        collect_type_refs(ty, text, &mut output);
                        if let Some(
                            orna_syntax_v1::FieldInitializer::Default(value)
                            | orna_syntax_v1::FieldInitializer::Computed(value),
                        ) = initializer
                        {
                            collect_expr_refs(value, text, &mut output);
                        }
                    } else if let orna_syntax_v1::TableMember::Assertion { value, .. } = member {
                        collect_expr_refs(value, text, &mut output);
                    }
                }
            }
            _ => {}
        }
    }
    output
}

fn collect_type_refs(ty: &TypeExpr, text: &str, output: &mut Vec<(String, SourceSpan)>) {
    match ty {
        TypeExpr::Name {
            path,
            span,
            arguments,
        } => {
            let tokens = lex(text).unwrap_or_default();
            for token in tokens.into_iter().filter(|token| {
                token.span.start >= span.start
                    && token.span.end <= span.end
                    && matches!(token.kind, TokenKind::Identifier { .. })
            }) {
                output.push((token.text, token.span));
            }
            for argument in arguments {
                collect_type_refs(argument, text, output);
            }
            let _ = path;
        }
        TypeExpr::Optional { inner, .. } | TypeExpr::List { inner, .. } => {
            collect_type_refs(inner, text, output)
        }
        TypeExpr::Product { lhs, rhs, .. } => {
            collect_type_refs(lhs, text, output);
            collect_type_refs(rhs, text, output);
        }
        TypeExpr::Record { fields, .. } => {
            for (_, ty, _) in fields {
                collect_type_refs(ty, text, output);
            }
        }
        TypeExpr::Tuple { elements, .. } => {
            for ty in elements {
                collect_type_refs(ty, text, output);
            }
        }
        TypeExpr::Function {
            parameters, result, ..
        } => {
            for ty in parameters {
                collect_type_refs(ty, text, output);
            }
            collect_type_refs(result, text, output);
        }
    }
}

fn collect_expr_refs(expression: &Expr, text: &str, output: &mut Vec<(String, SourceSpan)>) {
    match expression {
        Expr::Name { text: name, span } => output.push((name.clone(), span.clone())),
        Expr::Field { base, name, span } => {
            output.push((
                name.clone(),
                SourceSpan::new(span.end.saturating_sub(name.len()), span.end),
            ));
            collect_expr_refs(base, text, output);
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            collect_expr_refs(callee, text, output);
            for arg in arguments {
                collect_expr_refs(&arg.value, text, output);
            }
        }
        Expr::GenericCall {
            callee,
            type_arguments,
            arguments,
            ..
        } => {
            collect_expr_refs(callee, text, output);
            for ty in type_arguments {
                collect_type_refs(ty, text, output);
            }
            for arg in arguments {
                collect_expr_refs(&arg.value, text, output);
            }
        }
        Expr::Unary { rhs, .. } => collect_expr_refs(rhs, text, output),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_refs(lhs, text, output);
            collect_expr_refs(rhs, text, output);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(expr) = lower {
                collect_expr_refs(expr, text, output);
            }
            if let Some(expr) = upper {
                collect_expr_refs(expr, text, output);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expr_refs(base, text, output);
            collect_expr_refs(index, text, output);
        }
        Expr::Group { inner, .. } => collect_expr_refs(inner, text, output),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for expr in elements {
                collect_expr_refs(expr, text, output);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_expr_refs(&field.value, text, output);
            }
        }
        Expr::Lambda { body, .. } => collect_expr_refs(body, text, output),
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                match statement {
                    Statement::Let {
                        annotation, value, ..
                    } => {
                        if let Some(ty) = annotation {
                            collect_type_refs(ty, text, output);
                        }
                        collect_expr_refs(value, text, output);
                    }
                    Statement::Assert { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. } => collect_expr_refs(value, text, output),
                    Statement::Return { value, .. } | Statement::Break { value, .. } => {
                        if let Some(value) = value {
                            collect_expr_refs(value, text, output);
                        }
                    }
                    Statement::Assignment { value, .. } => collect_expr_refs(value, text, output),
                    Statement::Continue { .. } => {}
                }
            }
            if let Some(tail) = tail {
                collect_expr_refs(tail, text, output);
            }
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(expr) = condition {
                collect_expr_refs(expr, text, output);
            }
            if let Some(expr) = body {
                collect_expr_refs(expr, text, output);
            }
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_refs(guard, text, output);
                }
                collect_expr_refs(&arm.body, text, output);
            }
            if let Some(expr) = alternate {
                collect_expr_refs(expr, text, output);
            }
        }
        Expr::Literal { .. } | Expr::InterpolatedString { .. } | Expr::ReplBinding { .. } => {}
    }
}

#[cfg(test)]
mod tests;
