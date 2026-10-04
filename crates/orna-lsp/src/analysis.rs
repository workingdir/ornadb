//! Editor analysis built exclusively on the frozen Orna 1.0 syntax tree.
#![allow(deprecated)] // lsp-types 0.97 still requires DocumentSymbol::deprecated.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::OnceLock,
};

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, Diagnostic,
    DiagnosticRelatedInformation, DiagnosticSeverity, DocumentLink, DocumentSymbol, Hover,
    Location, MarkupContent, MarkupKind, NumberOrString, ParameterInformation, ParameterLabel,
    Position, SignatureHelp, SignatureInformation, SymbolKind, Uri,
};
use orna_syntax_v1::{
    Argument, Declaration, Expr, ImportSegment, Item, Keyword, Parse, Pattern, Statement,
    SyntaxSpan as SourceSpan, SyntaxTree, Token, TokenKind, TypeExpr, UseTail, Visibility, lex,
    parse_module, parse_module_with_file,
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
                .filter(|label| {
                    !label.primary
                        && label
                            .message
                            .as_deref()
                            .is_some_and(|message| !message.trim().is_empty())
                })
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

/// Links explicit imports to a single matching open module document.
///
/// The parser supplies exact byte spans for the import path. Resolution is
/// intentionally limited to open `.orna` documents: a missing or ambiguous
/// target is left unlinked instead of guessing between the loader's flat-file
/// and directory-module layouts.
pub(crate) fn document_links(
    document: &Document,
    open_documents: &HashMap<Uri, Document>,
) -> Vec<DocumentLink> {
    if !document.uri.as_str().ends_with(".orna") {
        return Vec::new();
    }

    let parse = parse_document(document);
    if !parse.diagnostics.is_empty() {
        return Vec::new();
    }

    let mapper = PositionMapper::new(&document.text);
    parse
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
            let module = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let target = matching_open_module(document, path, open_documents)?;
            Some(DocumentLink {
                range: lsp_types::Range::new(
                    mapper.position(first.span.start),
                    mapper.position(last.span.end),
                ),
                target: Some(target),
                tooltip: Some(format!("Open module `{module}`")),
                data: None,
            })
        })
        .collect()
}

fn matching_open_module(
    source: &Document,
    path: &[ImportSegment],
    open_documents: &HashMap<Uri, Document>,
) -> Option<Uri> {
    let module_path = path
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let flat_suffix = format!("/{module_path}.orna");
    let directory_suffix = format!("/{module_path}/main.orna");
    let mut matching_target = None;

    for uri in open_documents.keys() {
        if uri == &source.uri {
            continue;
        }
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
    pub parameter_names: Vec<Option<String>>,
    pub result_type: Option<String>,
}

#[derive(Debug, Clone)]
struct StandardSymbol {
    module: String,
    symbol: EditorSymbol,
}

#[derive(Debug, Default)]
struct StandardLibraryIndex {
    modules: BTreeSet<String>,
    symbols: Vec<StandardSymbol>,
}

fn standard_library() -> &'static StandardLibraryIndex {
    static INDEX: OnceLock<StandardLibraryIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = StandardLibraryIndex::default();
        index.modules.insert("std".to_owned());
        for (path, source) in orna_standard_sources::reference_standard_sources_v1() {
            let Some(module) = standard_module_name(&path) else {
                continue;
            };
            let parsed = parse_module(&source);
            let Ok(tokens) = lex(&source) else {
                continue;
            };
            let mut prefix = String::new();
            for segment in module.split('.') {
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(segment);
                index.modules.insert(prefix.clone());
            }
            index.symbols.extend(
                parsed
                    .value
                    .items
                    .iter()
                    .filter(|item| matches!(&item.visibility, Visibility::Public { .. }))
                    .filter_map(|item| {
                        symbol_for_item(item, &source, &tokens).map(|symbol| StandardSymbol {
                            module: module.clone(),
                            symbol,
                        })
                    }),
            );
        }
        index.symbols.sort_by(|left, right| {
            left.module
                .cmp(&right.module)
                .then_with(|| left.symbol.name.cmp(&right.symbol.name))
        });
        index
    })
}

fn standard_module_name(path: &str) -> Option<String> {
    let relative = path.strip_prefix("std/")?.strip_suffix(".orna")?;
    let module_path = if relative == "main" {
        ""
    } else {
        relative.strip_suffix("/main").unwrap_or(relative)
    };
    if module_path.is_empty() {
        Some("std".to_owned())
    } else {
        Some(format!("std.{}", module_path.replace('/', ".")))
    }
}

fn standard_symbol(module: &str, name: &str) -> Option<&'static StandardSymbol> {
    standard_library()
        .symbols
        .iter()
        .find(|entry| entry.module == module && entry.symbol.name == name)
}

fn standard_imported_symbols(parse: &EditorParse) -> Vec<&'static StandardSymbol> {
    standard_imported_symbols_in_tree(&parse.value)
}

fn standard_imported_symbols_in_tree(tree: &SyntaxTree) -> Vec<&'static StandardSymbol> {
    let mut imported = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let module = path
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        match tail {
            UseTail::Names(names) => {
                for name in names {
                    if let Some(symbol) = standard_symbol(&module, &name.name) {
                        imported.insert((module.clone(), name.name.clone()), symbol);
                    }
                }
            }
            UseTail::Glob { .. } => {
                for symbol in standard_library()
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.module == module)
                {
                    imported.insert((module.clone(), symbol.symbol.name.clone()), symbol);
                }
            }
            UseTail::None | UseTail::Alias { .. } => {}
        }
    }
    imported.into_values().collect()
}

fn standard_module_aliases(parse: &EditorParse) -> BTreeMap<String, String> {
    standard_module_aliases_in_tree(&parse.value)
}

fn standard_module_aliases_in_tree(tree: &SyntaxTree) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let module = path
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        if !standard_library().modules.contains(&module) {
            continue;
        }
        let alias = match tail {
            UseTail::Alias { name, .. } => Some(name.as_str()),
            UseTail::None => path.last().map(|segment| segment.name.as_str()),
            UseTail::Glob { .. } | UseTail::Names(_) => None,
        };
        if let Some(alias) = alias {
            aliases.insert(alias.to_owned(), module);
        }
    }
    aliases
}

fn resolve_standard_module(parse: &EditorParse, qualifier: &str) -> Option<String> {
    resolve_standard_module_in_tree(&parse.value, qualifier)
}

fn resolve_standard_module_in_tree(tree: &SyntaxTree, qualifier: &str) -> Option<String> {
    if standard_library().modules.contains(qualifier) {
        return Some(qualifier.to_owned());
    }
    let (alias, child_path) = qualifier
        .split_once('.')
        .map_or((qualifier, None), |(alias, child)| (alias, Some(child)));
    let base = standard_module_aliases_in_tree(tree).remove(alias)?;
    let module = child_path.map_or(base.clone(), |child| format!("{base}.{child}"));
    standard_library()
        .modules
        .contains(&module)
        .then_some(module)
}

fn standard_symbol_at(
    parse: &EditorParse,
    text: &str,
    token: &Token,
) -> Option<&'static StandardSymbol> {
    let chain = identifier_chain_at_span(text, &token.span)?;
    if chain.len() > 1 {
        let name = chain.last()?;
        let qualifier = chain[..chain.len() - 1].join(".");
        if let Some(module) = resolve_standard_module(parse, &qualifier)
            && let Some(symbol) = standard_symbol(&module, name)
        {
            return Some(symbol);
        }
    }
    let mut matches = standard_imported_symbols(parse)
        .into_iter()
        .filter(|entry| entry.symbol.name == token.text);
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

pub(crate) fn standard_parameter_names_for_call(
    tree: &SyntaxTree,
    name: &str,
    callee: &Expr,
) -> Option<Vec<Option<String>>> {
    if callee_resolves_to_local(tree, callee) {
        return None;
    }
    let symbol = standard_symbol_for_call_in_tree(tree, name)?;
    if symbol.symbol.kind != EditorSymbolKind::Function {
        return None;
    }
    Some(symbol.symbol.parameter_names.clone())
}

pub(crate) fn standard_result_type_for_call(
    tree: &SyntaxTree,
    name: &str,
    callee: &Expr,
) -> Option<String> {
    if callee_resolves_to_local(tree, callee) {
        return None;
    }
    let symbol = standard_symbol_for_call_in_tree(tree, name)?;
    if symbol.symbol.kind == EditorSymbolKind::Function {
        symbol.symbol.result_type.clone()
    } else {
        None
    }
}

fn identifier_chain_at_span(text: &str, span: &SourceSpan) -> Option<Vec<String>> {
    let tokens = lex(text).ok()?;
    let index = tokens.iter().position(|token| {
        same_span(&token.span, span) && matches!(token.kind, TokenKind::Identifier { .. })
    })?;
    let mut first = index;
    while first >= 2
        && tokens[first - 1].text == "."
        && matches!(tokens[first - 2].kind, TokenKind::Identifier { .. })
    {
        first -= 2;
    }
    let mut last = index;
    while last + 2 < tokens.len()
        && tokens[last + 1].text == "."
        && matches!(tokens[last + 2].kind, TokenKind::Identifier { .. })
    {
        last += 2;
    }
    Some(
        tokens[first..=last]
            .iter()
            .step_by(2)
            .map(|token| token.text.clone())
            .collect(),
    )
}

/// A top-level function and the calls in its body, with source spans kept in
/// syntax byte offsets until the server maps them to LSP coordinates.
#[derive(Debug, Clone)]
pub(crate) struct FunctionDefinition {
    pub name: String,
    pub selection: SourceSpan,
    pub full: SourceSpan,
    pub detail: Option<String>,
    pub calls: Vec<FunctionCall>,
}

#[derive(Debug, Clone)]
pub(crate) struct FunctionCall {
    pub name: String,
    pub selection: SourceSpan,
}

/// Returns top-level function declarations and syntactic calls that are not
/// shadowed by a local binding. The caller decides whether declarations from
/// different open documents resolve uniquely.
pub(crate) fn function_definitions(parse: &EditorParse, text: &str) -> Vec<FunctionDefinition> {
    let symbols = declaration_symbols(parse, text);
    let mut definitions = Vec::new();
    for item in &parse.value.items {
        let Declaration::Function { signature, body } = &item.declaration else {
            continue;
        };
        let Some(symbol) = symbols.iter().find(|symbol| {
            symbol.kind == EditorSymbolKind::Function
                && same_span(&symbol.full, &item.span)
                && symbol.name == signature.name
        }) else {
            continue;
        };
        let mut call_sites = Vec::new();
        collect_calls(body, &mut call_sites);
        let calls = call_sites
            .into_iter()
            .filter(|call| !callee_resolves_to_local(&parse.value, &call.callee))
            .filter_map(|call| {
                Some(FunctionCall {
                    name: call.name.rsplit('.').next()?.to_owned(),
                    selection: expression_name_span(&call.callee)?,
                })
            })
            .collect();
        definitions.push(FunctionDefinition {
            name: symbol.name.clone(),
            selection: symbol.selection.clone(),
            full: symbol.full.clone(),
            detail: symbol.detail.clone(),
            calls,
        });
    }
    definitions.sort_by_key(|definition| definition.selection.start);
    definitions
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
        parameter_names,
        result_type,
    })
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

fn token_at(text: &str, byte: usize) -> Option<Token> {
    lex(text).ok()?.into_iter().find(|token| {
        matches!(token.kind, TokenKind::Identifier { .. })
            && token.span.start <= byte
            && byte < token.span.end
    })
}

pub(crate) fn identifier_at(
    document: &Document,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<(String, SourceSpan)> {
    let token = token_at(&document.text, mapper.byte_offset(position))?;
    Some((token.text, token.span))
}

pub(crate) fn symbol_for_name<'a>(
    symbols: &'a [EditorSymbol],
    name: &str,
) -> Option<&'a EditorSymbol> {
    let key = normalized_identifier(name);
    let mut matches = symbols
        .iter()
        .filter(|symbol| normalized_identifier(&symbol.name) == key);
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

pub fn hover(
    document: &Document,
    parse: &EditorParse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Hover> {
    let byte = mapper.byte_offset(position);
    let token = token_at(&document.text, byte)?;
    if let Some(binding) = crate::locals::binding_at(&parse.value, &token.text, &token.span) {
        let kind = match binding.kind {
            crate::locals::LocalBindingKind::Parameter => "parameter",
            crate::locals::LocalBindingKind::Local => "local",
            crate::locals::LocalBindingKind::Pattern => "pattern binding",
        };
        let context = source_slice(&document.text, &binding.context);
        return Some(crate::hover::declaration(
            kind,
            &binding.name,
            (!context.trim().is_empty()).then_some(context),
            &[],
            None,
        ));
    }
    let symbols = declaration_symbols(parse, &document.text);
    let symbol = symbols
        .iter()
        .find(|symbol| same_span(&symbol.selection, &token.span))
        .or_else(|| symbol_for_name(&symbols, &token.text));
    if let Some(symbol) = symbol {
        return Some(hover_symbol(symbol));
    }
    let symbol = standard_symbol_at(parse, &document.text, &token)?;
    Some(hover_symbol(&symbol.symbol))
}

fn hover_symbol(symbol: &EditorSymbol) -> Hover {
    let kind = match symbol.kind {
        EditorSymbolKind::Function => "function",
        EditorSymbolKind::Type => "type",
        EditorSymbolKind::Enum => "enum",
        EditorSymbolKind::Table => "table",
        EditorSymbolKind::Protocol => "protocol",
        EditorSymbolKind::Other => "declaration",
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
    if callee_resolves_to_local(&parse.value, &call.callee) {
        return None;
    }
    let symbols = declaration_symbols(parse, &document.text);
    let function = symbol_for_name(&symbols, call.name.rsplit('.').next().unwrap_or(&call.name))
        .cloned()
        .or_else(|| {
            standard_symbol_for_call(parse, &call.name).map(|entry| entry.symbol.clone())
        })?;
    if function.kind != EditorSymbolKind::Function {
        return None;
    }
    let active_argument = active_argument_index(&call, &document.text, byte);
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

fn standard_symbol_for_call(parse: &EditorParse, name: &str) -> Option<&'static StandardSymbol> {
    standard_symbol_for_call_in_tree(&parse.value, name)
}

fn standard_symbol_for_call_in_tree(
    tree: &SyntaxTree,
    name: &str,
) -> Option<&'static StandardSymbol> {
    if let Some((qualifier, symbol_name)) = name.rsplit_once('.') {
        let module = resolve_standard_module_in_tree(tree, qualifier)?;
        return standard_symbol(&module, symbol_name);
    }

    let mut matches = standard_imported_symbols_in_tree(tree)
        .into_iter()
        .filter(|entry| entry.symbol.name == name);
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

#[derive(Debug)]
struct CallSite {
    name: String,
    callee: Expr,
    span: SourceSpan,
    arguments: Vec<Argument>,
}

fn active_argument_index(call: &CallSite, text: &str, position: usize) -> usize {
    let Ok(tokens) = lex(text) else {
        return 0;
    };
    let is_comma_before = |start: usize, end: usize| {
        tokens.iter().any(|token| {
            matches!(token.kind, TokenKind::Punct(","))
                && token.span.start >= start
                && token.span.end <= end
                && token.span.start < position
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

fn callee_resolves_to_local(tree: &SyntaxTree, callee: &Expr) -> bool {
    match callee {
        Expr::Name { text, span } => crate::locals::binding_at(tree, text, span).is_some(),
        Expr::Field { base, .. } | Expr::Group { inner: base, .. } => {
            callee_resolves_to_local(tree, base)
        }
        _ => false,
    }
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

pub(crate) fn expression_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", expression_name(base)?, name)),
        _ => None,
    }
}

fn expression_name_span(expression: &Expr) -> Option<SourceSpan> {
    match expression {
        Expr::Name { span, .. } => Some(span.clone()),
        Expr::Field { name, span, .. } => {
            Some(SourceSpan::new(span.end.checked_sub(name.len())?, span.end))
        }
        Expr::Group { inner, .. } => expression_name_span(inner),
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
    byte: Option<usize>,
    _context: Option<&CompletionContext>,
) -> Vec<CompletionItem> {
    let (prefix, qualifier) = byte
        .map(|byte| completion_parts(text, byte))
        .unwrap_or_default();
    let prefix_key = normalized_identifier(&prefix).to_ascii_lowercase();
    let import_module = byte.and_then(|byte| import_module_before_cursor(text, byte));
    let qualified_module = qualifier
        .as_deref()
        .and_then(|value| resolve_standard_module(parse, value));
    let module_parent = qualifier
        .as_deref()
        .filter(|value| *value == "std" || standard_library().modules.contains(*value));
    let standard_context = import_module.is_some() || module_parent.is_some();
    let mut completions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut shadowed = BTreeSet::new();

    if !standard_context {
        if let Some(byte) = byte {
            for binding in crate::locals::visible_bindings(&parse.value, byte) {
                let key = normalized_identifier(&binding.name);
                if !completion_matches(&key, &prefix_key) || !seen.insert(key.clone()) {
                    continue;
                }
                shadowed.insert(key.clone());
                let detail = source_slice(text, &binding.context).trim();
                let mut item = CompletionItem {
                    label: binding.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(match binding.kind {
                        crate::locals::LocalBindingKind::Parameter => "Parameter".to_owned(),
                        crate::locals::LocalBindingKind::Local => "Local variable".to_owned(),
                        crate::locals::LocalBindingKind::Pattern => "Pattern binding".to_owned(),
                    }),
                    insert_text: Some(binding.name),
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

        for symbol in declaration_symbols(parse, text) {
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
            standard_library()
                .symbols
                .iter()
                .filter(|entry| entry.module == module),
            &prefix_key,
            1,
            false,
        );
    } else if let Some(module) = qualified_module.as_deref() {
        append_standard_symbols(
            &mut completions,
            &mut seen,
            standard_library()
                .symbols
                .iter()
                .filter(|entry| entry.module == module),
            &prefix_key,
            1,
            true,
        );
        append_standard_modules(&mut completions, &mut seen, module, &prefix_key);
    } else if let Some(parent) = module_parent {
        append_standard_modules(&mut completions, &mut seen, parent, &prefix_key);
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
            standard_imported_symbols(parse).into_iter(),
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
                    detail: Some("Orna 1.0 keyword".to_owned()),
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
            EditorSymbolKind::Table => CompletionItemKind::CLASS,
            EditorSymbolKind::Protocol => CompletionItemKind::INTERFACE,
            EditorSymbolKind::Other => CompletionItemKind::REFERENCE,
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
    completions: &mut Vec<CompletionItem>,
    seen: &mut BTreeSet<String>,
    parent: &str,
    prefix: &str,
) {
    let stem = format!("{parent}.");
    for module in &standard_library().modules {
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

fn import_module_before_cursor(text: &str, byte: usize) -> Option<String> {
    let before_cursor = text.get(..byte)?;
    let line_start = before_cursor.rfind('\n').map_or(0, |offset| offset + 1);
    let line = before_cursor.get(line_start..)?.trim_start();
    let path = line.strip_prefix("use ")?;
    let brace = path.rfind('{')?;
    let module = path[..brace].trim().trim_end_matches('.').trim();
    standard_library()
        .modules
        .contains(module)
        .then(|| module.to_owned())
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
