//! Semantic token projection from syntax-v1 lexical tokens and resolved names.

use std::collections::HashMap;

use lsp_types::{Range, SemanticToken, SemanticTokenModifier, SemanticTokenType};
use orna_syntax_v1::{SyntaxSpan, editor, parse_module_with_file};

use crate::{analysis, documents::PositionMapper, locals};

const TOKEN_TYPES: &[&str] = &[
    "keyword",
    "variable",
    "number",
    "string",
    "comment",
    "operator",
    "type",
    "enum",
    "interface",
    "function",
    "parameter",
];

const TOKEN_MODIFIERS: &[&str] = &["declaration", "readonly"];
const TOKEN_VARIABLE: u32 = 1;
const TOKEN_TYPE: u32 = 6;
const TOKEN_ENUM: u32 = 7;
const TOKEN_INTERFACE: u32 = 8;
const TOKEN_FUNCTION: u32 = 9;
const TOKEN_PARAMETER: u32 = 10;
const DECLARATION: u32 = 1 << 0;
const READONLY: u32 = 1 << 1;

/// LSP token legend supported by this server.
pub fn legend() -> Vec<SemanticTokenType> {
    TOKEN_TYPES
        .iter()
        .map(|token_type| SemanticTokenType::new(token_type))
        .collect()
}

/// LSP token modifiers supported by this server.
pub fn modifiers() -> Vec<SemanticTokenModifier> {
    TOKEN_MODIFIERS
        .iter()
        .map(|modifier| SemanticTokenModifier::new(modifier))
        .collect()
}

/// Returns delta-encoded tokens with declaration and resolved-name context.
///
/// The frozen syntax-v1 lexer remains the source of token boundaries and
/// lexical fallback classes. Parse information only refines identifier
/// classes, so incomplete editor buffers retain useful highlighting.
pub fn semantic_tokens(
    text: &str,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let parse = parse_module_with_file(text, "<editor>");
    let contextual = contextual_tokens(&parse, text);
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in editor::highlight(text) {
        let Some(lexical_type) = editor::semantic_token_index(token.class) else {
            continue;
        };
        let (token_type, modifiers) = contextual
            .get(&(token.range.start, token.range.end))
            .copied()
            .unwrap_or((lexical_type as u32, 0));
        let span = SyntaxSpan::new(token.range.start, token.range.end);
        for (position, length) in mapper.segments(&span) {
            if length == 0 {
                continue;
            }
            if let Some(range) = range {
                let segment_end = lsp_types::Position {
                    line: position.line,
                    character: position.character.saturating_add(length),
                };
                if segment_end <= range.start || position >= range.end {
                    continue;
                }
            }
            let line = position.line;
            let start = position.character;
            let (delta_line, delta_start) = if data.is_empty() {
                (line, start)
            } else if line == previous_line {
                (0, start - previous_start)
            } else {
                (line - previous_line, start)
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: modifiers,
            });
            previous_line = line;
            previous_start = start;
        }
    }
    data
}

fn contextual_tokens(
    parse: &analysis::EditorParse,
    text: &str,
) -> HashMap<(usize, usize), (u32, u32)> {
    let mut tokens = HashMap::new();
    let tree = &parse.value;
    let symbols = analysis::declaration_symbols(parse, text);

    for symbol in &symbols {
        if let Some(token_type) = symbol_token_type(symbol.kind) {
            insert(
                &mut tokens,
                &symbol.selection,
                token_type,
                DECLARATION | READONLY,
            );
        }
    }

    let bindings = locals::all_bindings(tree);
    for binding in &bindings {
        insert(
            &mut tokens,
            &binding.selection,
            match binding.kind {
                locals::LocalBindingKind::Parameter => TOKEN_PARAMETER,
                locals::LocalBindingKind::Local | locals::LocalBindingKind::Pattern => {
                    TOKEN_VARIABLE
                }
            },
            DECLARATION,
        );
    }

    let local_references = locals::resolved_references(tree);
    for (span, kind) in &local_references {
        insert(
            &mut tokens,
            span,
            match kind {
                locals::LocalBindingKind::Parameter => TOKEN_PARAMETER,
                locals::LocalBindingKind::Local | locals::LocalBindingKind::Pattern => {
                    TOKEN_VARIABLE
                }
            },
            0,
        );
    }

    let local_spans = local_references
        .iter()
        .map(|(span, _)| (span.start, span.end))
        .collect::<std::collections::HashSet<_>>();
    for (name, span) in analysis::reference_occurrences(tree, text) {
        if local_spans.contains(&(span.start, span.end)) {
            continue;
        }
        let Some(symbol) = analysis::symbol_for_name(&symbols, &name) else {
            continue;
        };
        if let Some(token_type) = symbol_token_type(symbol.kind) {
            insert(&mut tokens, &span, token_type, READONLY);
        }
    }
    tokens
}

fn symbol_token_type(kind: analysis::EditorSymbolKind) -> Option<u32> {
    match kind {
        analysis::EditorSymbolKind::Type | analysis::EditorSymbolKind::Table => Some(TOKEN_TYPE),
        analysis::EditorSymbolKind::Enum => Some(TOKEN_ENUM),
        analysis::EditorSymbolKind::Protocol => Some(TOKEN_INTERFACE),
        analysis::EditorSymbolKind::Function => Some(TOKEN_FUNCTION),
        analysis::EditorSymbolKind::Other => None,
    }
}

fn insert(
    tokens: &mut HashMap<(usize, usize), (u32, u32)>,
    span: &SyntaxSpan,
    token_type: u32,
    modifiers: u32,
) {
    tokens.insert((span.start, span.end), (token_type, modifiers));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = include_str!("../tests/fixtures/editor-lsp-hints.orna");

    #[test]
    fn semantic_tokens_refine_lexical_classes_with_declarations_and_scoped_names() {
        assert_eq!(
            legend(),
            [
                "keyword",
                "variable",
                "number",
                "string",
                "comment",
                "operator",
                "type",
                "enum",
                "interface",
                "function",
                "parameter",
            ]
            .into_iter()
            .map(SemanticTokenType::new)
            .collect::<Vec<_>>()
        );
        assert_eq!(modifiers().len(), TOKEN_MODIFIERS.len());
        assert_eq!(
            &TOKEN_TYPES[..6],
            editor::semantic_token_types()
                .collect::<Vec<_>>()
                .as_slice()
        );

        let mapper = PositionMapper::new(SOURCE);
        let tokens = semantic_tokens(SOURCE, &mapper, None);
        assert!(
            tokens
                .iter()
                .any(|token| token.token_type == TOKEN_FUNCTION)
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.token_type == TOKEN_PARAMETER)
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.token_modifiers_bitset & DECLARATION != 0)
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.token_modifiers_bitset & READONLY != 0)
        );
    }
}
