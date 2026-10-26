//! Editor analysis built exclusively on the frozen Orna 1.0 syntax tree.
#![allow(deprecated)] // lsp-types 0.97 still requires DocumentSymbol::deprecated.

use std::collections::{BTreeSet, HashMap};

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, Diagnostic,
    DiagnosticRelatedInformation, DiagnosticSeverity, DocumentLink, DocumentSymbol, Hover,
    Location, MarkupContent, MarkupKind, NumberOrString, ParameterInformation, ParameterLabel,
    SignatureHelp, SignatureInformation, SymbolKind, Uri,
};
use orna_syntax_v1::{
    Argument, Declaration, Expr, ImportSegment, Item, Keyword, Parse, Pattern, Statement,
    SyntaxSpan as SourceSpan, SyntaxTree, Token, TokenKind, TypeExpr, lex,
};

use crate::documents::PositionMapper;
use crate::names::{DeclaredKind, LocalBindingKind, key_of};
use crate::standard::{StandardLibrary, StandardSymbol};
use crate::workspace::{FileAnalysis, Target, Workspace};

pub type EditorParse = Parse<SyntaxTree>;

/// orna-syntax-v1 carries no severity on its diagnostics; every parser error is
/// a hard error, so the editor maps all of them to ERROR. Semantic analysis
/// likewise only produces errors.
const ERROR_SEVERITY: DiagnosticSeverity = DiagnosticSeverity::ERROR;

/// The `source` of every diagnostic this server publishes: the same checks
/// that `orna check` runs.
pub(crate) const DIAGNOSTIC_SOURCE: &str = "orna";

/// One error diagnostic for a project or semantic check, in the same shape as
/// the syntax diagnostics.
pub(crate) fn source_diagnostic(
    range: lsp_types::Range,
    code: &str,
    message: &str,
    help: &[&str],
) -> Diagnostic {
    let help = help
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    Diagnostic {
        range,
        severity: Some(ERROR_SEVERITY),
        code: Some(NumberOrString::String(code.to_owned())),
        code_description: None,
        source: Some(DIAGNOSTIC_SOURCE.to_owned()),
        message: lsp_diagnostic_message(message, message, &help, &[]),
        related_information: None,
        tags: None,
        data: Some(serde_json::json!({
            "title": message,
            "help": help,
            "notes": [],
        })),
    }
}

/// The parser diagnostics of one parse, as LSP diagnostics.
pub(crate) fn syntax_diagnostics(
    uri: &Uri,
    parse: &EditorParse,
    mapper: &PositionMapper<'_>,
) -> Vec<Diagnostic> {
    parse
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let related = diagnostic
                .labels
                .iter()
                .filter(|label| {
                    !label.primary
                        && label
                            .message
                            .as_deref()
                            .is_some_and(|message| !message.trim().is_empty())
                })
                .map(|label| DiagnosticRelatedInformation {
                    location: Location {
                        uri: uri.clone(),
                        range: mapper.range(&label.span),
                    },
                    message: label.message.clone().unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            Diagnostic {
                range: mapper.range(&diagnostic.span),
                severity: Some(ERROR_SEVERITY),
                code: Some(NumberOrString::String(diagnostic.code.to_owned())),
                code_description: None,
                source: Some(DIAGNOSTIC_SOURCE.to_owned()),
                message: lsp_diagnostic_message(
                    &diagnostic.title,
                    &diagnostic.message,
                    &diagnostic.help,
                    &diagnostic.notes,
                ),
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

/// Links each `use` of a module to the module's source.
///
/// In a project, the import path names a module of the project's module
/// graph, open or not. Outside a project, it links to the one other open
/// document whose path ends in the module path, as a flat file or a
/// directory's `main.orna`; a missing or ambiguous target stays unlinked.
pub(crate) fn document_links(workspace: &Workspace, module: usize) -> Vec<DocumentLink> {
    let file = workspace.file(module);
    if !file.parse.diagnostics.is_empty() {
        return Vec::new();
    }
    let in_project = workspace.modules[module].logical_path.is_some();
    let mapper = PositionMapper::new(file.text());
    file.parse
        .value
        .items
        .iter()
        .filter_map(|item| {
            let Declaration::Use { path, .. } = &item.declaration else {
                return None;
            };
            let first = path.first()?;
            if matches!(first.name.as_str(), "std" | "sys") {
                return None;
            }
            let last = path.last()?;
            let target = if in_project {
                let keys = path
                    .iter()
                    .map(|segment| key_of(&segment.name))
                    .collect::<Vec<_>>();
                workspace
                    .modules
                    .iter()
                    .find(|candidate| candidate.namespace == keys)?
                    .file
                    .uri()
                    .clone()
            } else {
                matching_open_module(workspace, module, path)?
            };
            let module_name = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            Some(DocumentLink {
                range: lsp_types::Range::new(
                    mapper.position(first.span.start),
                    mapper.position(last.span.end),
                ),
                target: Some(target),
                tooltip: Some(format!("Open module `{module_name}`")),
                data: None,
            })
        })
        .collect()
}

fn matching_open_module(workspace: &Workspace, module: usize, path: &[ImportSegment]) -> Option<Uri> {
    let module_path = path
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let flat_suffix = format!("/{module_path}.orna");
    let directory_suffix = format!("/{module_path}/main.orna");
    let mut matching_target = None;
    for (index, candidate) in workspace.modules.iter().enumerate() {
        if index == module {
            continue;
        }
        let uri = candidate.file.uri();
        let uri_path = uri.as_str().split(['?', '#']).next().unwrap_or_default();
        if uri_path.ends_with(&flat_suffix) || uri_path.ends_with(&directory_suffix) {
            if matching_target.is_some() {
                return None;
            }
            matching_target = Some(uri.clone());
        }
    }
    matching_target
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorSymbolKind {
    Function,
    Type,
    Enum,
    EnumVariant,
    Table,
    Protocol,
    Method,
    Field,
    Constant,
    Dimension,
    Unit,
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
    pub parameter_names: Vec<Option<String>>,
    pub result_type: Option<String>,
    pub children: Vec<EditorSymbol>,
}

pub(crate) fn declaration_symbols(parse: &EditorParse, text: &str) -> Vec<EditorSymbol> {
    let Ok(tokens) = lex(text) else {
        return Vec::new();
    };
    declaration_symbols_from_tokens(parse, text, &tokens)
}

/// Declaration symbols from an already lexed text.
pub(crate) fn declaration_symbols_from_tokens(
    parse: &EditorParse,
    text: &str,
    tokens: &[Token],
) -> Vec<EditorSymbol> {
    let mut symbols = parse
        .value
        .items
        .iter()
        .filter_map(|item| symbol_for_item(item, text, tokens))
        .collect::<Vec<_>>();
    symbols.sort_by_key(|symbol| symbol.selection.start);
    symbols
}

pub(crate) fn symbol_for_item(item: &Item, text: &str, tokens: &[Token]) -> Option<EditorSymbol> {
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
        Declaration::Dimension { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Dimension,
            &item.span,
            None,
            Vec::new(),
        ),
        Declaration::Unit { name, .. } => (
            name.as_str(),
            EditorSymbolKind::Unit,
            &item.span,
            None,
            Vec::new(),
        ),
        _ => return None,
    };
    let result_type = match &item.declaration {
        Declaration::Function { signature, .. } => signature
            .result
            .as_ref()
            .and_then(|result| source_type(text, result)),
        _ => None,
    };
    let parameter_names = match &item.declaration {
        Declaration::Function { signature, .. } => signature
            .parameters
            .iter()
            .map(|parameter| match &parameter.pattern {
                Pattern::Name(name, _) => Some(name.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut symbol = symbol_from_scope(
        name,
        kind,
        &item.span,
        selection_scope,
        detail,
        parameters,
        parameter_names,
        result_type,
        text,
        tokens,
    )?;
    symbol.children = declaration_child_symbols(&item.declaration, text, tokens);
    Some(symbol)
}

fn symbol_from_scope(
    name: &str,
    kind: EditorSymbolKind,
    full: &SourceSpan,
    selection_scope: &SourceSpan,
    detail: Option<String>,
    parameters: Vec<String>,
    parameter_names: Vec<Option<String>>,
    result_type: Option<String>,
    text: &str,
    tokens: &[Token],
) -> Option<EditorSymbol> {
    let first = tokens.partition_point(|token| token.span.start < selection_scope.start);
    let selection = tokens[first..]
        .iter()
        .take_while(|token| token.span.end <= selection_scope.end)
        .find(|token| token.text == name && matches!(token.kind, TokenKind::Identifier { .. }))?
        .span
        .clone();
    Some(EditorSymbol {
        name: name.to_owned(),
        kind,
        selection,
        full: full.clone(),
        detail,
        documentation: leading_doc_comment(text, full.start),
        parameters,
        parameter_names,
        result_type,
        children: Vec::new(),
    })
}

fn declaration_child_symbols(
    declaration: &Declaration,
    text: &str,
    tokens: &[Token],
) -> Vec<EditorSymbol> {
    let mut children = match declaration {
        Declaration::Table { members, .. } => members
            .iter()
            .flat_map(|member| match member {
                orna_syntax_v1::TableMember::Field { name, span, .. } => symbol_from_scope(
                    name,
                    EditorSymbolKind::Field,
                    span,
                    span,
                    None,
                    Vec::new(),
                    Vec::new(),
                    None,
                    text,
                    tokens,
                )
                .into_iter()
                .collect(),
                orna_syntax_v1::TableMember::Implementation { implementation, .. } => {
                    implementation_symbols(implementation, text, tokens)
                }
                orna_syntax_v1::TableMember::Assertion { .. } => Vec::new(),
            })
            .collect(),
        Declaration::Protocol { members, .. } => members
            .iter()
            .filter_map(|member| match member {
                orna_syntax_v1::ProtocolMember::Function { signature, span } => symbol_from_scope(
                    &signature.name,
                    EditorSymbolKind::Method,
                    span,
                    &signature.span,
                    Some(source_slice(text, &signature.span).to_owned()),
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| source_slice(text, &parameter.span).to_owned())
                        .collect(),
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| match &parameter.pattern {
                            Pattern::Name(name, _) => Some(name.clone()),
                            _ => None,
                        })
                        .collect(),
                    signature
                        .result
                        .as_ref()
                        .and_then(|result| source_type(text, result)),
                    text,
                    tokens,
                ),
                orna_syntax_v1::ProtocolMember::Static { name, span, .. } => symbol_from_scope(
                    name,
                    EditorSymbolKind::Constant,
                    span,
                    span,
                    None,
                    Vec::new(),
                    Vec::new(),
                    None,
                    text,
                    tokens,
                ),
            })
            .collect(),
        Declaration::Enum { variants, .. } => variants
            .iter()
            .filter_map(|variant| {
                let mut symbol = symbol_from_scope(
                    &variant.name,
                    EditorSymbolKind::EnumVariant,
                    &variant.span,
                    &variant.span,
                    None,
                    Vec::new(),
                    Vec::new(),
                    None,
                    text,
                    tokens,
                )?;
                symbol.children = variant
                    .fields
                    .iter()
                    .filter_map(|field| {
                        symbol_from_scope(
                            &field.name,
                            EditorSymbolKind::Field,
                            &field.span,
                            &field.span,
                            None,
                            Vec::new(),
                            Vec::new(),
                            None,
                            text,
                            tokens,
                        )
                    })
                    .collect();
                Some(symbol)
            })
            .collect(),
        Declaration::Type { representation, .. } => match representation {
            orna_syntax_v1::TypeRepresentation::Alias { refinements, .. } => {
                type_member_symbols(refinements, text, tokens)
            }
            orna_syntax_v1::TypeRepresentation::Nominal { members } => {
                type_member_symbols(members, text, tokens)
            }
        },
        _ => Vec::new(),
    };
    children.sort_by_key(|symbol| symbol.selection.start);
    children
}

fn type_member_symbols(
    members: &[orna_syntax_v1::TypeMember],
    text: &str,
    tokens: &[Token],
) -> Vec<EditorSymbol> {
    let mut children = members
        .iter()
        .flat_map(|member| match member {
            orna_syntax_v1::TypeMember::Field { name, span, .. } => symbol_from_scope(
                name,
                EditorSymbolKind::Field,
                span,
                span,
                None,
                Vec::new(),
                Vec::new(),
                None,
                text,
                tokens,
            )
            .into_iter()
            .collect(),
            orna_syntax_v1::TypeMember::Implementation { implementation, .. } => {
                implementation_symbols(implementation, text, tokens)
            }
            orna_syntax_v1::TypeMember::Assertion { .. } => Vec::new(),
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|symbol| symbol.selection.start);
    children
}

fn implementation_symbols(
    implementation: &orna_syntax_v1::Implementation,
    text: &str,
    tokens: &[Token],
) -> Vec<EditorSymbol> {
    let mut children = implementation
        .members
        .iter()
        .filter_map(|member| match member {
            orna_syntax_v1::ImplMember::Function {
                signature, span, ..
            } => symbol_from_scope(
                &signature.name,
                EditorSymbolKind::Method,
                span,
                &signature.span,
                Some(source_slice(text, &signature.span).to_owned()),
                signature
                    .parameters
                    .iter()
                    .map(|parameter| source_slice(text, &parameter.span).to_owned())
                    .collect(),
                signature
                    .parameters
                    .iter()
                    .map(|parameter| match &parameter.pattern {
                        Pattern::Name(name, _) => Some(name.clone()),
                        _ => None,
                    })
                    .collect(),
                signature
                    .result
                    .as_ref()
                    .and_then(|result| source_type(text, result)),
                text,
                tokens,
            ),
            orna_syntax_v1::ImplMember::Static { name, span, .. } => symbol_from_scope(
                name,
                EditorSymbolKind::Constant,
                span,
                span,
                None,
                Vec::new(),
                Vec::new(),
                None,
                text,
                tokens,
            ),
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|symbol| symbol.selection.start);
    children
}

fn source_type(text: &str, ty: &TypeExpr) -> Option<String> {
    let span = match ty {
        TypeExpr::Name { span, .. }
        | TypeExpr::Optional { span, .. }
        | TypeExpr::Product { span, .. }
        | TypeExpr::List { span, .. }
        | TypeExpr::Record { span, .. }
        | TypeExpr::Tuple { span, .. }
        | TypeExpr::Function { span, .. } => span,
    };
    let source = source_slice(text, span).trim();
    (!source.is_empty()).then(|| source.to_owned())
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

fn lsp_diagnostic_message(title: &str, message: &str, help: &[String], notes: &[String]) -> String {
    let mut value = if !title.trim().is_empty() && title != message {
        format!("{title}: {message}")
    } else {
        message.to_owned()
    };
    for (heading, details) in [("Help", help), ("Note", notes)] {
        for detail in details.iter().filter(|detail| !detail.trim().is_empty()) {
            value.push_str("\n\n");
            value.push_str(heading);
            value.push_str(": ");
            value.push_str(detail.trim());
        }
    }
    value
}

fn source_slice<'a>(text: &'a str, span: &SourceSpan) -> &'a str {
    text.get(span.start..span.end).unwrap_or("")
}

pub(crate) fn normalized_identifier(text: &str) -> String {
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

/// Documentation shared by keyword hover cards and keyword completion items.
const KEYWORD_DOCUMENTATION: &str = "Reserved word of the Orna language (ORNA-LEX-007).";

/// The hover card for the name or keyword at `byte`.
pub(crate) fn hover(workspace: &Workspace, module: usize, byte: usize) -> Option<Hover> {
    let file = workspace.file(module);
    // Reserved words cannot name a binding or symbol, so the lexer's keyword
    // table decides the card before any name lookup.
    if let Some(keyword) = file.keyword_at(byte) {
        return Some(crate::hover::declaration(
            "keyword",
            keyword.spelling(),
            None,
            &[],
            Some(KEYWORD_DOCUMENTATION),
        ));
    }
    let (_, resolved) = workspace.resolve_at(module, byte)?;
    match resolved.target()? {
        Target::Local { module, binding } => {
            let file = workspace.file(module);
            let binding = file.names.binding(binding);
            let kind = match binding.kind {
                LocalBindingKind::Parameter => "parameter",
                LocalBindingKind::Local => "local",
                LocalBindingKind::Pattern => "pattern binding",
            };
            let context = source_slice(file.text(), &binding.context);
            Some(crate::hover::declaration(
                kind,
                &binding.name,
                (!context.trim().is_empty()).then_some(context),
                &[],
                None,
            ))
        }
        Target::Declared { module, index } => {
            let file = workspace.file(module);
            let declared = file.names.declaration(index);
            if let Some(symbol) = declared_symbol(&file.symbols, &declared.selection) {
                return Some(hover_symbol(symbol));
            }
            let detail = match &declared.ty {
                Some(ty) => format!("{}: {}", declared.name, source_slice(file.text(), type_span(ty))),
                None => source_slice(file.text(), &declared.full).trim().to_owned(),
            };
            Some(crate::hover::declaration(
                declared_noun(declared.kind),
                &declared.name,
                Some(&detail),
                &[],
                None,
            ))
        }
        Target::Standard { module, name } => workspace
            .standard
            .symbol(&module, &name)
            .map(|standard| hover_symbol(&standard.symbol)),
        Target::Module(path) => Some(crate::hover::declaration(
            "module",
            &path.join("."),
            None,
            &[],
            None,
        )),
    }
}

/// The editor symbol declared at `selection`, including nested members.
pub(crate) fn declared_symbol<'a>(
    symbols: &'a [EditorSymbol],
    selection: &SourceSpan,
) -> Option<&'a EditorSymbol> {
    symbols.iter().find_map(|symbol| {
        if same_span(&symbol.selection, selection) {
            Some(symbol)
        } else {
            declared_symbol(&symbol.children, selection)
        }
    })
}

fn declared_noun(kind: DeclaredKind) -> &'static str {
    match kind {
        DeclaredKind::Function => "function",
        DeclaredKind::Table => "table",
        DeclaredKind::Type => "type",
        DeclaredKind::Enum => "enum",
        DeclaredKind::Protocol => "protocol",
        DeclaredKind::Dimension => "dimension",
        DeclaredKind::Unit => "unit",
        DeclaredKind::Let => "value",
        DeclaredKind::Field => "field",
        DeclaredKind::KeyField => "key field",
        DeclaredKind::Variant => "enum variant",
        DeclaredKind::PayloadField => "field",
        DeclaredKind::Method => "method",
        DeclaredKind::Static => "constant",
    }
}

fn type_span(ty: &TypeExpr) -> &SourceSpan {
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

fn hover_symbol(symbol: &EditorSymbol) -> Hover {
    let kind = match symbol.kind {
        EditorSymbolKind::Function => "function",
        EditorSymbolKind::Type => "type",
        EditorSymbolKind::Enum => "enum",
        EditorSymbolKind::EnumVariant => "enum variant",
        EditorSymbolKind::Table => "table",
        EditorSymbolKind::Protocol => "protocol",
        EditorSymbolKind::Method => "method",
        EditorSymbolKind::Field => "field",
        EditorSymbolKind::Constant => "constant",
        EditorSymbolKind::Dimension => "dimension",
        EditorSymbolKind::Unit => "unit",
    };
    crate::hover::declaration(
        kind,
        &symbol.name,
        symbol.detail.as_deref(),
        &symbol.parameters,
        symbol.documentation.as_deref(),
    )
}

fn same_span(left: &SourceSpan, right: &SourceSpan) -> bool {
    left.start == right.start && left.end == right.end
}

/// Signature help for the innermost call around `byte`.
pub(crate) fn signature_help(
    workspace: &Workspace,
    module: usize,
    byte: usize,
) -> Option<SignatureHelp> {
    let file = workspace.file(module);
    let mut calls = Vec::new();
    for item in &file.parse.value.items {
        for body in item_bodies(item) {
            collect_calls(body, &mut calls);
        }
    }
    let call = calls
        .into_iter()
        .filter(|call| call.span.start <= byte && byte <= call.span.end)
        .min_by_key(|call| call.span.end.saturating_sub(call.span.start))?;
    let name = callee_name_span(file, &call.callee)?;
    let occurrence = file.names.occurrence(&name)?;
    let functions = match workspace.resolve(module, occurrence).target()? {
        Target::Declared {
            module: declared_module,
            index,
        } => {
            let declared = workspace.declaration(declared_module, index);
            let symbols = &workspace.file(declared_module).symbols;
            if declared.owner.is_none() {
                // Every same-name top-level declaration is a candidate signature.
                symbols
                    .iter()
                    .filter(|symbol| normalized_identifier(&symbol.name) == declared.key)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                declared_symbol(symbols, &declared.selection)
                    .into_iter()
                    .cloned()
                    .collect()
            }
        }
        Target::Standard { module, name } => workspace
            .standard
            .symbol(&module, &name)
            .map(|standard| standard.symbol.clone())
            .into_iter()
            .collect(),
        Target::Local { .. } | Target::Module(_) => return None,
    };
    let functions = functions
        .into_iter()
        .filter(|function| matches!(function.kind, EditorSymbolKind::Function | EditorSymbolKind::Method))
        .collect::<Vec<_>>();
    if functions.is_empty() {
        return None;
    }
    let active_argument = active_argument_index(&call, &file.tokens, byte);
    let signatures = functions
        .iter()
        .map(|function| {
            let active_parameter = call
                .arguments
                .get(active_argument)
                .and_then(|argument| argument.name.as_deref())
                .and_then(|name| {
                    let name = normalized_identifier(name);
                    function.parameters.iter().position(|parameter| {
                        parameter_name(parameter)
                            .is_some_and(|parameter| normalized_identifier(parameter) == name)
                    })
                })
                .unwrap_or(active_argument)
                .min(function.parameters.len().saturating_sub(1)) as u32;
            let parameters = function
                .parameters
                .iter()
                .cloned()
                .map(|label| ParameterInformation {
                    label: ParameterLabel::Simple(label),
                    documentation: None,
                })
                .collect();
            SignatureInformation {
                label: function
                    .detail
                    .clone()
                    .unwrap_or_else(|| function.name.clone()),
                documentation: function.documentation.clone().map(|text| {
                    lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value: text,
                    })
                }),
                parameters: Some(parameters),
                active_parameter: Some(active_parameter),
            }
        })
        .collect::<Vec<_>>();
    let active_parameter = signatures[0].active_parameter;
    Some(SignatureHelp {
        signatures,
        active_signature: Some(0),
        active_parameter,
    })
}

/// The span of the name a call's callee ends in.
pub(crate) fn callee_name_span(file: &FileAnalysis, callee: &Expr) -> Option<SourceSpan> {
    match callee {
        Expr::Name { span, .. } => Some(span.clone()),
        Expr::Field { base, span, .. } => file
            .tokens_in(&SourceSpan::new(base.span().end, span.end))
            .iter()
            .rev()
            .find(|token| matches!(token.kind, TokenKind::Identifier { .. }))
            .map(|token| token.span.clone()),
        Expr::Group { inner, .. } => callee_name_span(file, inner),
        _ => None,
    }
}

/// Every expression body an item declares: function bodies, implementation
/// members, field initializers, assertions and values.
pub(crate) fn item_bodies(item: &Item) -> Vec<&Expr> {
    fn implementation<'a>(implementation: &'a orna_syntax_v1::Implementation, bodies: &mut Vec<&'a Expr>) {
        for member in &implementation.members {
            match member {
                orna_syntax_v1::ImplMember::Function { body, .. } => bodies.push(body),
                orna_syntax_v1::ImplMember::Static { value, .. } => bodies.push(value),
            }
        }
    }
    let mut bodies = Vec::new();
    match &item.declaration {
        Declaration::Function { body, .. } => bodies.push(body),
        Declaration::Let { value, .. } | Declaration::Assertion { value } => bodies.push(value),
        Declaration::Table { members, .. } => {
            for member in members {
                match member {
                    orna_syntax_v1::TableMember::Field {
                        initializer:
                            Some(
                                orna_syntax_v1::FieldInitializer::Default(value)
                                | orna_syntax_v1::FieldInitializer::Computed(value),
                            ),
                        ..
                    }
                    | orna_syntax_v1::TableMember::Assertion { value, .. } => bodies.push(value),
                    orna_syntax_v1::TableMember::Implementation { implementation: inner, .. } => {
                        implementation(inner, &mut bodies);
                    }
                    orna_syntax_v1::TableMember::Field { .. } => {}
                }
            }
        }
        Declaration::Type { representation, .. } => {
            let members = match representation {
                orna_syntax_v1::TypeRepresentation::Alias { refinements, .. } => refinements,
                orna_syntax_v1::TypeRepresentation::Nominal { members } => members,
            };
            for member in members {
                match member {
                    orna_syntax_v1::TypeMember::Field {
                        initializer: Some(value),
                        ..
                    }
                    | orna_syntax_v1::TypeMember::Assertion { value, .. } => bodies.push(value),
                    orna_syntax_v1::TypeMember::Implementation { implementation: inner, .. } => {
                        implementation(inner, &mut bodies);
                    }
                    orna_syntax_v1::TypeMember::Field { .. } => {}
                }
            }
        }
        Declaration::Unit {
            definition: orna_syntax_v1::UnitDefinition::Derived { value, offset, .. },
            ..
        } => {
            bodies.push(value);
            bodies.extend(offset.iter());
        }
        Declaration::Use { .. }
        | Declaration::Protocol { .. }
        | Declaration::Enum { .. }
        | Declaration::Dimension { .. }
        | Declaration::Unit { .. } => {}
    }
    bodies
}

#[derive(Debug)]
struct CallSite {
    name: String,
    callee: Expr,
    span: SourceSpan,
    arguments: Vec<Argument>,
}

fn active_argument_index(call: &CallSite, tokens: &[Token], position: usize) -> usize {
    let is_comma_before = |start: usize, end: usize| {
        let first = tokens.partition_point(|token| token.span.start < start);
        tokens[first..]
            .iter()
            .take_while(|token| token.span.end <= end)
            .any(|token| {
                matches!(token.kind, TokenKind::Punct(",")) && token.span.start < position
            })
    };
    let separators = call
        .arguments
        .windows(2)
        .filter(|pair| is_comma_before(pair[0].span.end, pair[1].span.start))
        .count();
    let trailing_separator = call
        .arguments
        .last()
        .is_some_and(|last| is_comma_before(last.span.end, call.span.end));
    separators + usize::from(trailing_separator)
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
                    callee: callee.as_ref().clone(),
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
                    callee: callee.as_ref().clone(),
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
            kind,
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
            for arm in crate::names::control_arms(*kind, arms) {
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

pub(crate) fn expression_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", expression_name(base)?, name)),
        _ => None,
    }
}
/// The declaration of the name at `byte`.
pub(crate) fn definition(workspace: &Workspace, module: usize, byte: usize) -> Option<Location> {
    let (_, resolved) = workspace.resolve_at(module, byte)?;
    target_location(workspace, &resolved.target()?)
}

/// Where `target` is declared.
pub(crate) fn target_location(workspace: &Workspace, target: &Target) -> Option<Location> {
    match target {
        Target::Local { module, binding } => Some(location(
            workspace,
            *module,
            &workspace.file(*module).names.binding(*binding).selection,
        )),
        Target::Declared { module, index } => Some(location(
            workspace,
            *module,
            &workspace.declaration(*module, *index).selection,
        )),
        Target::Standard { module, name } => {
            let standard = workspace.standard.symbol(module, name)?;
            workspace.standard.location(standard)
        }
        Target::Module(path) => workspace
            .modules
            .iter()
            .find(|module| &module.namespace == path)
            .map(|module| Location {
                uri: module.file.uri().clone(),
                range: lsp_types::Range::default(),
            }),
    }
}

/// The LSP location of a byte span in one module.
pub(crate) fn location(workspace: &Workspace, module: usize, span: &SourceSpan) -> Location {
    locations(workspace, std::slice::from_ref(&(module, span.clone())))
        .pop()
        .expect("one location")
}

/// LSP locations of byte spans, building each module's line index once.
pub(crate) fn locations(workspace: &Workspace, spans: &[(usize, SourceSpan)]) -> Vec<Location> {
    let mut mappers = HashMap::<usize, PositionMapper<'_>>::new();
    spans
        .iter()
        .map(|(module, span)| {
            let file = workspace.file(*module);
            let mapper = mappers
                .entry(*module)
                .or_insert_with(|| PositionMapper::new(file.text()));
            Location {
                uri: file.uri().clone(),
                range: mapper.range(span),
            }
        })
        .collect()
}

/// Every occurrence that resolves to what the name at `byte` refers to.
pub(crate) fn references(
    workspace: &Workspace,
    module: usize,
    byte: usize,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(target) = workspace
        .resolve_at(module, byte)
        .and_then(|(_, resolved)| resolved.target())
    else {
        return Vec::new();
    };
    let declaration = target_location(workspace, &target);
    let mut locations = locations(workspace, &workspace.search(&target).found);
    match (&target, include_declaration, declaration) {
        (Target::Standard { .. }, true, Some(declaration)) => locations.push(declaration),
        (_, false, Some(declaration)) => locations.retain(|location| location != &declaration),
        _ => {}
    }
    locations
}

pub fn document_symbols(symbols: &[EditorSymbol], mapper: &PositionMapper<'_>) -> Vec<DocumentSymbol> {
    symbols
        .iter()
        .cloned()
        .map(|symbol| document_symbol(symbol, mapper))
        .collect()
}

fn document_symbol(symbol: EditorSymbol, mapper: &PositionMapper<'_>) -> DocumentSymbol {
    DocumentSymbol {
        name: symbol.name,
        detail: symbol.detail,
        kind: match symbol.kind {
            EditorSymbolKind::Function => SymbolKind::FUNCTION,
            EditorSymbolKind::Type => SymbolKind::STRUCT,
            EditorSymbolKind::Enum => SymbolKind::ENUM,
            EditorSymbolKind::EnumVariant => SymbolKind::ENUM_MEMBER,
            EditorSymbolKind::Table => SymbolKind::CLASS,
            EditorSymbolKind::Protocol => SymbolKind::INTERFACE,
            EditorSymbolKind::Method => SymbolKind::METHOD,
            EditorSymbolKind::Field => SymbolKind::FIELD,
            EditorSymbolKind::Constant => SymbolKind::CONSTANT,
            EditorSymbolKind::Dimension => SymbolKind::STRUCT,
            EditorSymbolKind::Unit => SymbolKind::CONSTANT,
        },
        tags: None,
        deprecated: None,
        range: mapper.range(&symbol.full),
        selection_range: mapper.range(&symbol.selection),
        children: (!symbol.children.is_empty()).then(|| {
            symbol
                .children
                .into_iter()
                .map(|child| document_symbol(child, mapper))
                .collect()
        }),
    }
}

pub(crate) fn completion_at(
    workspace: &Workspace,
    module: usize,
    byte: Option<usize>,
    _context: Option<&CompletionContext>,
) -> Vec<CompletionItem> {
    let file = workspace.file(module);
    let text = file.text();
    let parse = &file.parse;
    let (prefix, qualifier) = byte
        .map(|byte| completion_parts(text, byte))
        .unwrap_or_default();
    let prefix_key = normalized_identifier(&prefix).to_ascii_lowercase();
    let standard = &workspace.standard;
    let import_module = byte.and_then(|byte| import_module_before_cursor(standard, text, byte));
    let qualified_module = qualifier
        .as_deref()
        .and_then(|value| standard.resolve_module(&parse.value, value));
    let module_parent = qualifier
        .as_deref()
        .filter(|value| *value == "std" || standard.module_exists(value));
    let standard_context = import_module.is_some() || module_parent.is_some();
    let mut completions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut shadowed = BTreeSet::new();

    if !standard_context {
        if let Some(byte) = byte {
            for binding in file.names.visible_bindings(byte) {
                let key = binding.key.clone();
                if !completion_matches(&key, &prefix_key) || !seen.insert(key.clone()) {
                    continue;
                }
                shadowed.insert(key.clone());
                let detail = source_slice(text, &binding.context).trim();
                let mut item = CompletionItem {
                    label: binding.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(match binding.kind {
                        LocalBindingKind::Parameter => "Parameter".to_owned(),
                        LocalBindingKind::Local => "Local variable".to_owned(),
                        LocalBindingKind::Pattern => "Pattern binding".to_owned(),
                    }),
                    insert_text: Some(binding.name.clone()),
                    sort_text: Some(completion_sort_text(&prefix_key, 0, &key)),
                    ..CompletionItem::default()
                };
                if !detail.is_empty() {
                    item.documentation =
                        Some(lsp_types::Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("```orna\n{detail}\n```"),
                        }));
                }
                completions.push(item);
            }
        }

        for symbol in workspace.completion_symbols(module) {
            let symbol = symbol.clone();
            let key = normalized_identifier(&symbol.name);
            if !completion_matches(&key, &prefix_key)
                || shadowed.contains(&key)
                || !seen.insert(key.clone())
            {
                continue;
            }
            let kind = match symbol.kind {
                EditorSymbolKind::Function => CompletionItemKind::FUNCTION,
                EditorSymbolKind::Type => CompletionItemKind::STRUCT,
                EditorSymbolKind::Enum => CompletionItemKind::ENUM,
                EditorSymbolKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
                EditorSymbolKind::Table => CompletionItemKind::CLASS,
                EditorSymbolKind::Protocol => CompletionItemKind::INTERFACE,
                EditorSymbolKind::Method => CompletionItemKind::METHOD,
                EditorSymbolKind::Field => CompletionItemKind::FIELD,
                EditorSymbolKind::Constant => CompletionItemKind::CONSTANT,
                EditorSymbolKind::Dimension => CompletionItemKind::STRUCT,
                EditorSymbolKind::Unit => CompletionItemKind::UNIT,
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
            completions.push(CompletionItem {
                label: symbol.name,
                kind: Some(kind),
                detail: symbol.detail,
                documentation: symbol.documentation.map(|value| {
                    lsp_types::Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
                insert_text: Some(insert_text),
                insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
                sort_text: Some(completion_sort_text(&prefix_key, 1, &key)),
                ..CompletionItem::default()
            });
        }
    }

    if let Some(module) = import_module.as_deref() {
        append_standard_symbols(
            &mut completions,
            &mut seen,
            standard.symbols_in(module).iter(),
            &prefix_key,
            1,
            false,
        );
    } else if let Some(module) = qualified_module.as_deref() {
        append_standard_symbols(
            &mut completions,
            &mut seen,
            standard.symbols_in(module).iter(),
            &prefix_key,
            1,
            true,
        );
        append_standard_modules(standard, &mut completions, &mut seen, module, &prefix_key);
    } else if let Some(parent) = module_parent {
        append_standard_modules(standard, &mut completions, &mut seen, parent, &prefix_key);
    } else {
        if !prefix_key.is_empty() && completion_matches("std", &prefix_key) {
            completions.push(CompletionItem {
                label: "std".to_owned(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some("Orna standard library".to_owned()),
                insert_text: Some("std".to_owned()),
                sort_text: Some(completion_sort_text(&prefix_key, 1, "std")),
                ..CompletionItem::default()
            });
            seen.insert("std".to_owned());
        }
        append_standard_symbols(
            &mut completions,
            &mut seen,
            standard.imported_symbols(&parse.value).into_iter(),
            &prefix_key,
            1,
            true,
        );
    }

    if !standard_context {
        for keyword in Keyword::ALL {
            let name = keyword.spelling();
            let key = normalized_identifier(name);
            if completion_matches(&key, &prefix_key) && seen.insert(key.clone()) {
                completions.push(CompletionItem {
                    label: name.to_owned(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    detail: Some("keyword".to_owned()),
                    documentation: Some(lsp_types::Documentation::String(
                        KEYWORD_DOCUMENTATION.to_owned(),
                    )),
                    insert_text: Some(name.to_owned()),
                    sort_text: Some(completion_sort_text(&prefix_key, 2, &key)),
                    ..CompletionItem::default()
                });
            }
        }
    }
    completions.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    if let Some(first) = completions.first_mut() {
        first.preselect = Some(true);
    }
    completions
}

fn append_standard_symbols<'a>(
    completions: &mut Vec<CompletionItem>,
    seen: &mut BTreeSet<String>,
    symbols: impl Iterator<Item = &'a StandardSymbol>,
    prefix: &str,
    group: u8,
    snippets: bool,
) {
    for entry in symbols {
        let key = normalized_identifier(&entry.symbol.name);
        if !completion_matches(&key, prefix) || !seen.insert(key.clone()) {
            continue;
        }
        let symbol = &entry.symbol;
        let kind = match symbol.kind {
            EditorSymbolKind::Function => CompletionItemKind::FUNCTION,
            EditorSymbolKind::Type => CompletionItemKind::STRUCT,
            EditorSymbolKind::Enum => CompletionItemKind::ENUM,
            EditorSymbolKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
            EditorSymbolKind::Table => CompletionItemKind::CLASS,
            EditorSymbolKind::Protocol => CompletionItemKind::INTERFACE,
            EditorSymbolKind::Method => CompletionItemKind::METHOD,
            EditorSymbolKind::Field => CompletionItemKind::FIELD,
            EditorSymbolKind::Constant => CompletionItemKind::CONSTANT,
            EditorSymbolKind::Dimension => CompletionItemKind::STRUCT,
            EditorSymbolKind::Unit => CompletionItemKind::UNIT,
        };
        let insert_text = if snippets && symbol.kind == EditorSymbolKind::Function {
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
        completions.push(CompletionItem {
            label: symbol.name.clone(),
            kind: Some(kind),
            detail: Some(format!(
                "{} · {}",
                entry.module,
                symbol.detail.as_deref().unwrap_or(&symbol.name)
            )),
            documentation: symbol.documentation.clone().map(|value| {
                lsp_types::Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                })
            }),
            insert_text: Some(insert_text),
            insert_text_format: snippets.then_some(lsp_types::InsertTextFormat::SNIPPET),
            sort_text: Some(completion_sort_text(prefix, group, &key)),
            ..CompletionItem::default()
        });
    }
}

fn append_standard_modules(
    standard: &StandardLibrary,
    completions: &mut Vec<CompletionItem>,
    seen: &mut BTreeSet<String>,
    parent: &str,
    prefix: &str,
) {
    let stem = format!("{parent}.");
    for module in standard.modules() {
        let Some(child) = module.strip_prefix(&stem) else {
            continue;
        };
        if child.contains('.') || !completion_matches(child, prefix) || !seen.insert(child.into()) {
            continue;
        }
        completions.push(CompletionItem {
            label: child.to_owned(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some(format!("Standard library module {module}")),
            insert_text: Some(child.to_owned()),
            sort_text: Some(completion_sort_text(prefix, 0, child)),
            ..CompletionItem::default()
        });
    }
}

fn completion_parts(text: &str, byte: usize) -> (String, Option<String>) {
    let Some(prefix_text) = text.get(..byte) else {
        return (String::new(), None);
    };
    let line_start = prefix_text.rfind('\n').map_or(0, |offset| offset + 1);
    let bytes = text.as_bytes();
    let mut prefix_start = byte;
    while prefix_start > line_start && is_identifier_byte(bytes[prefix_start - 1]) {
        prefix_start -= 1;
    }
    let prefix = text.get(prefix_start..byte).unwrap_or_default().to_owned();
    let mut cursor = prefix_start;
    let mut segments = Vec::new();
    while cursor > line_start && bytes[cursor - 1] == b'.' {
        cursor -= 1;
        let end = cursor;
        while cursor > line_start && is_identifier_byte(bytes[cursor - 1]) {
            cursor -= 1;
        }
        if cursor == end {
            break;
        }
        segments.push(text[cursor..end].to_owned());
    }
    segments.reverse();
    let qualifier = (!segments.is_empty()).then(|| segments.join("."));
    (prefix, qualifier)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn import_module_before_cursor(standard: &StandardLibrary, text: &str, byte: usize) -> Option<String> {
    let before_cursor = text.get(..byte)?;
    let line_start = before_cursor.rfind('\n').map_or(0, |offset| offset + 1);
    let line = before_cursor.get(line_start..)?.trim_start();
    let path = line.strip_prefix("use ")?;
    let brace = path.rfind('{')?;
    let module = path[..brace].trim().trim_end_matches('.').trim();
    standard.module_exists(module).then(|| module.to_owned())
}

fn completion_matches(candidate: &str, prefix: &str) -> bool {
    prefix.is_empty() || candidate.to_ascii_lowercase().starts_with(prefix)
}

fn completion_sort_text(prefix: &str, group: u8, key: &str) -> String {
    let exact = !prefix.is_empty() && key.eq_ignore_ascii_case(prefix);
    format!("{}-{group}-{key}", if exact { 0 } else { 1 })
}

fn parameter_name(source: &str) -> Option<&str> {
    let name = source.split(':').next()?.split('=').next()?.trim();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests;
