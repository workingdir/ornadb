//! Semantic token projection from syntax-v1 lexical tokens and resolved names.
//!
//! The lexer decides token boundaries and the fallback class of every token.
//! Name resolution refines identifiers: what a name refers to decides its
//! token type (a module is a `namespace`, a field a `property`, a variant an
//! `enumMember`), and names from the standard library or the intrinsic type
//! environment carry `defaultLibrary`.

use std::collections::HashMap;
use std::sync::LazyLock;

use lsp_types::{Range, SemanticToken, SemanticTokenModifier, SemanticTokenType};
use orna_syntax_v1::{SyntaxSpan, editor};

use crate::analysis::EditorSymbolKind;
use crate::documents::PositionMapper;
use crate::names::{DeclaredKind, LocalBindingKind, NameOccurrence, Role};
use crate::workspace::{Target, Workspace};

/// The intrinsic type environment (05-types, "Core type forms").
const INTRINSIC_TYPES: &[&str] = &[
    "Unit",
    "Bool",
    "Int",
    "Float",
    "Decimal",
    "Str",
    "Blob",
    "Path",
    "Digest",
    "Uuid",
    "Date",
    "Instant",
    "LocalDateTime",
    "TimeOfDay",
    "Duration",
    "TimeZone",
    "ZonedDateTime",
    "Error",
    "Range",
    "List",
    "Option",
    "Relation",
    "Query",
    "Predicate",
    "Stream",
    "Money",
    "Quantity",
];

/// Modifier bits, in the order of `editor::TOKEN_MODIFIERS`.
const DECLARATION: u32 = 1 << 0;
const READONLY: u32 = 1 << 1;
const MODIFICATION: u32 = 1 << 2;
const DEFAULT_LIBRARY: u32 = 1 << 3;

/// Legend indexes of the token types a resolved name can take.
struct TokenTypes {
    variable: u32,
    ty: u32,
    enumeration: u32,
    interface: u32,
    function: u32,
    parameter: u32,
    namespace: u32,
    enum_member: u32,
    property: u32,
}

static TOKEN_TYPES: LazyLock<TokenTypes> = LazyLock::new(|| {
    let index = |name: &str| {
        let position = editor::legend_token_types()
            .position(|candidate| candidate == name)
            .unwrap_or_else(|| panic!("the editor legend declares `{name}`"));
        u32::try_from(position).expect("legend index fits u32")
    };
    TokenTypes {
        variable: index("variable"),
        ty: index("type"),
        enumeration: index("enum"),
        interface: index("interface"),
        function: index("function"),
        parameter: index("parameter"),
        namespace: index("namespace"),
        enum_member: index("enumMember"),
        property: index("property"),
    }
});

/// LSP token legend supported by this server.
pub fn legend() -> Vec<SemanticTokenType> {
    editor::legend_token_types()
        .map(|token_type| SemanticTokenType::new(token_type))
        .collect()
}

/// LSP token modifiers supported by this server.
pub fn modifiers() -> Vec<SemanticTokenModifier> {
    editor::TOKEN_MODIFIERS
        .iter()
        .map(|modifier| SemanticTokenModifier::new(modifier))
        .collect()
}

/// Returns delta-encoded tokens with declaration and resolved-name context.
///
/// The frozen syntax-v1 lexer remains the source of token boundaries and
/// lexical fallback classes. Name resolution only refines identifier
/// classes, so incomplete editor buffers retain useful highlighting.
pub(crate) fn semantic_tokens(
    workspace: &Workspace,
    module: usize,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let text = workspace.file(module).text();
    let contextual = contextual_tokens(workspace, module);
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

fn contextual_tokens(workspace: &Workspace, module: usize) -> HashMap<(usize, usize), (u32, u32)> {
    let mut tokens = HashMap::new();
    let names = &workspace.file(module).names;
    for declared in names.declarations() {
        let (token_type, modifiers) = declared_token(declared.kind);
        insert(
            &mut tokens,
            &declared.selection,
            token_type,
            modifiers | DECLARATION,
        );
    }
    for binding in names.bindings() {
        insert(
            &mut tokens,
            &binding.selection,
            binding_token_type(binding.kind),
            DECLARATION,
        );
    }
    for occurrence in names.occurrences() {
        if matches!(occurrence.role, Role::Declaration)
            || tokens.contains_key(&(occurrence.span.start, occurrence.span.end))
        {
            continue;
        }
        let classified = match workspace.resolve(module, occurrence).target() {
            Some(target) => resolved_token(workspace, &target),
            None => intrinsic_type(occurrence),
        };
        let Some((token_type, modifiers)) = classified else {
            continue;
        };
        let write = matches!(
            occurrence.role,
            Role::Value { write: true } | Role::Member { write: true, .. }
        );
        insert(
            &mut tokens,
            &occurrence.span,
            token_type,
            if write {
                modifiers | MODIFICATION
            } else {
                modifiers
            },
        );
    }
    tokens
}

/// The token type and modifiers of a name that refers to `target`.
fn resolved_token(workspace: &Workspace, target: &Target) -> Option<(u32, u32)> {
    let types = &*TOKEN_TYPES;
    Some(match target {
        Target::Local { module, binding } => (
            binding_token_type(workspace.file(*module).names.binding(*binding).kind),
            0,
        ),
        Target::Declared { module, index } => {
            declared_token(workspace.declaration(*module, *index).kind)
        }
        Target::Module(path) => (
            types.namespace,
            if matches!(path.first().map(String::as_str), Some("std" | "sys")) {
                DEFAULT_LIBRARY
            } else {
                0
            },
        ),
        Target::Standard { module, name } => {
            let standard = workspace.standard.symbol(module, name)?;
            let (token_type, modifiers) = standard_token(standard.symbol.kind);
            (token_type, modifiers | DEFAULT_LIBRARY)
        }
    })
}

/// An unresolved single-segment type name from the intrinsic type
/// environment.
fn intrinsic_type(occurrence: &NameOccurrence) -> Option<(u32, u32)> {
    let Role::Path { path, .. } = &occurrence.role else {
        return None;
    };
    (path.len() == 1 && INTRINSIC_TYPES.contains(&occurrence.key.as_str()))
        .then_some((TOKEN_TYPES.ty, READONLY | DEFAULT_LIBRARY))
}

fn binding_token_type(kind: LocalBindingKind) -> u32 {
    match kind {
        LocalBindingKind::Parameter => TOKEN_TYPES.parameter,
        LocalBindingKind::Local | LocalBindingKind::Pattern => TOKEN_TYPES.variable,
    }
}

fn declared_token(kind: DeclaredKind) -> (u32, u32) {
    let types = &*TOKEN_TYPES;
    match kind {
        DeclaredKind::Type | DeclaredKind::Table | DeclaredKind::Dimension => (types.ty, READONLY),
        DeclaredKind::Enum => (types.enumeration, READONLY),
        DeclaredKind::Variant => (types.enum_member, READONLY),
        DeclaredKind::Protocol => (types.interface, READONLY),
        DeclaredKind::Function | DeclaredKind::Method => (types.function, READONLY),
        DeclaredKind::Field | DeclaredKind::KeyField | DeclaredKind::PayloadField => {
            (types.property, 0)
        }
        DeclaredKind::Static => (types.property, READONLY),
        DeclaredKind::Let | DeclaredKind::Unit => (types.variable, READONLY),
    }
}

fn standard_token(kind: EditorSymbolKind) -> (u32, u32) {
    let types = &*TOKEN_TYPES;
    match kind {
        EditorSymbolKind::Function | EditorSymbolKind::Method => (types.function, READONLY),
        EditorSymbolKind::Type | EditorSymbolKind::Table | EditorSymbolKind::Dimension => {
            (types.ty, READONLY)
        }
        EditorSymbolKind::Enum => (types.enumeration, READONLY),
        EditorSymbolKind::EnumVariant => (types.enum_member, READONLY),
        EditorSymbolKind::Protocol => (types.interface, READONLY),
        EditorSymbolKind::Field => (types.property, 0),
        EditorSymbolKind::Constant | EditorSymbolKind::Unit => (types.variable, READONLY),
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
    use std::sync::Arc;

    use super::*;
    use crate::documents::Document;
    use crate::workspace::FileAnalysis;

    const SOURCE: &str = include_str!("../tests/fixtures/editor-lsp-hints.orna");

    fn workspace(text: &str) -> Workspace {
        Workspace::detached(vec![Arc::new(FileAnalysis::new(Document::new(
            "file:///w/tokens.orna".parse().unwrap(),
            text.to_owned(),
            1,
        )))])
    }

    /// The token type name and modifier names of each token, by its text.
    fn classes(text: &str) -> Vec<(String, String, Vec<&'static str>)> {
        let workspace = workspace(text);
        let legend = editor::legend_token_types().collect::<Vec<_>>();
        let mapper = PositionMapper::new(text);
        let mut line = 0u32;
        let mut start = 0u32;
        let lines = text.split('\n').collect::<Vec<_>>();
        semantic_tokens(&workspace, 0, &mapper, None)
            .into_iter()
            .map(|token| {
                if token.delta_line > 0 {
                    line += token.delta_line;
                    start = token.delta_start;
                } else {
                    start += token.delta_start;
                }
                let utf16 = lines[line as usize].encode_utf16().collect::<Vec<_>>();
                let lexeme = String::from_utf16(
                    &utf16[start as usize..(start + token.length) as usize],
                )
                .unwrap();
                let modifiers = editor::TOKEN_MODIFIERS
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| token.token_modifiers_bitset & (1 << bit) != 0)
                    .map(|(_, modifier)| *modifier)
                    .collect();
                (lexeme, legend[token.token_type as usize].to_owned(), modifiers)
            })
            .collect()
    }

    fn class_of<'a>(
        classes: &'a [(String, String, Vec<&'static str>)],
        lexeme: &str,
        nth: usize,
    ) -> (&'a str, &'a [&'static str]) {
        let (_, token_type, modifiers) = classes
            .iter()
            .filter(|(text, _, _)| text == lexeme)
            .nth(nth)
            .unwrap_or_else(|| panic!("no token #{nth} `{lexeme}` in {classes:?}"));
        (token_type, modifiers)
    }

    #[test]
    fn advertised_legend_matches_checked_in_editor_attach_legend() {
        let artifact: serde_json::Value = serde_json::from_str(include_str!(
            "../../../editors/semantic-token-legend.json"
        ))
        .expect("editor legend artifact is JSON");
        let editor_types = artifact["tokenTypes"]
            .as_array()
            .expect("tokenTypes is an array")
            .iter()
            .map(|value| value.as_str().expect("token type is a string"))
            .collect::<Vec<_>>();
        assert_eq!(
            legend()
                .iter()
                .map(SemanticTokenType::as_str)
                .collect::<Vec<_>>(),
            editor_types,
            "editor attach legend must declare every token type the LSP advertises"
        );
        assert_eq!(
            editor::TOKEN_MODIFIERS,
            ["declaration", "readonly", "modification", "defaultLibrary"],
            "modifier bits follow the editor legend order"
        );
        assert_eq!(modifiers().len(), editor::TOKEN_MODIFIERS.len());
    }

    #[test]
    fn semantic_tokens_refine_lexical_classes_with_declarations_and_scoped_names() {
        let classes = classes(SOURCE);
        assert!(classes.iter().any(|(_, token_type, _)| token_type == "function"));
        assert!(classes.iter().any(|(_, token_type, _)| token_type == "parameter"));
        assert!(
            classes
                .iter()
                .any(|(_, _, modifiers)| modifiers.contains(&"declaration"))
        );
        assert!(
            classes
                .iter()
                .any(|(_, _, modifiers)| modifiers.contains(&"readonly"))
        );
    }

    #[test]
    fn resolved_names_take_the_class_of_what_they_refer_to() {
        const TEXT: &str = "pub dim Length;\n\
            pub unit metre : Length base;\n\
            pub unit kilometre : Length = 1000.metre;\n\
            pub enum Status { draft, published }\n\
            pub protocol Display { fn show(self): Str; }\n\
            pub type Badge { pub text: Str, }\n\
            pub table Post(id: Int) { state: Status, badge: Badge, }\n\
            pub fn label(s: Status): Str = case s { Status.draft: \"Draft\", Status.published: \"Published\", };\n\
            pub fn make(): Badge = Badge { text: \"x\" };\n\
            pub fn read(post: Post): Str = post.badge.text;\n";
        let parse = orna_syntax_v1::parse_module(TEXT);
        assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
        let classes = classes(TEXT);

        assert_eq!(class_of(&classes, "Length", 0), ("type", &["declaration", "readonly"][..]));
        assert_eq!(class_of(&classes, "Length", 1), ("type", &["readonly"][..]));
        assert_eq!(class_of(&classes, "metre", 0), ("variable", &["declaration", "readonly"][..]));
        assert_eq!(class_of(&classes, "metre", 1), ("variable", &["readonly"][..]));
        assert_eq!(class_of(&classes, "Display", 0).0, "interface");
        assert_eq!(class_of(&classes, "draft", 0), ("enumMember", &["declaration", "readonly"][..]));
        assert_eq!(class_of(&classes, "draft", 1), ("enumMember", &["readonly"][..]));
        assert_eq!(class_of(&classes, "Status", 2), ("enum", &["readonly"][..]));
        assert_eq!(class_of(&classes, "text", 0), ("property", &["declaration"][..]));
        assert_eq!(class_of(&classes, "text", 1), ("property", &[][..]));
        assert_eq!(class_of(&classes, "text", 2), ("property", &[][..]));
        assert_eq!(class_of(&classes, "badge", 1), ("property", &[][..]));
        assert_eq!(class_of(&classes, "Badge", 3), ("type", &["readonly"][..]));
        assert_eq!(
            class_of(&classes, "Str", 0),
            ("type", &["readonly", "defaultLibrary"][..])
        );
    }

    #[test]
    fn standard_modules_and_exports_are_default_library_names() {
        const TEXT: &str = "use std.math.{increment};\n\
            pub fn next(value: Int): Int = std.math.increment(increment(value));\n";
        let classes = classes(TEXT);
        assert_eq!(class_of(&classes, "std", 1), ("namespace", &["defaultLibrary"][..]));
        assert_eq!(class_of(&classes, "math", 1), ("namespace", &["defaultLibrary"][..]));
        assert_eq!(
            class_of(&classes, "increment", 1),
            ("function", &["readonly", "defaultLibrary"][..])
        );
        assert_eq!(
            class_of(&classes, "increment", 2),
            ("function", &["readonly", "defaultLibrary"][..])
        );
    }
}
